//! The two budgets a script runs under, as the operator sets them: the
//! instruction limit, and the wall clock on a query or procedure's `handle`.
//! Each resolves database value, then environment variable, then default.

use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use sqlx::AnyPool;

use crate::admin::settings::try_get_setting;
use crate::db::DatabaseBackend;

pub const INSTRUCTION_LIMIT_KEY: &str = "script_instruction_limit";
pub const WALL_CLOCK_SECONDS_KEY: &str = "script_wall_clock_seconds";

pub const DEFAULT_INSTRUCTION_LIMIT: u32 = 1_000_000;

/// The instruction budget counts Lua alone, so a script whose time goes to
/// awaiting the host (a loop of HTTP calls, say) would otherwise hold its
/// caller's connection for as long as the host keeps answering. The event
/// runners have no connection to hold and sit inside a retry loop that a
/// spent budget can only delay, so they get no clock.
pub const DEFAULT_WALL_CLOCK_SECONDS: u32 = 10;

/// The values a budget may take. The dashboard mirrors both ranges in
/// `web/src/types/settings.ts`, pinned by a test below.
///
/// The sandbox's own guards run under the hook before any script does, so a
/// budget below the floor is spent building the VM and every run fails with
/// an error about the VM rather than the budget. The ceilings keep a slip of
/// the keyboard (a wall clock typed in milliseconds) from turning a limit into
/// no limit at all: a thousand million instructions is minutes of Lua, and
/// five minutes is longer than any caller waits.
pub const INSTRUCTION_LIMIT_RANGE: RangeInclusive<u32> = 1_000..=1_000_000_000;
pub const WALL_CLOCK_SECONDS_RANGE: RangeInclusive<u32> = 1..=300;

/// How long a change made on another instance sharing the database takes to
/// reach this one.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(10);

/// The budgets as the runners read them. Two atomics rather than a lock or an
/// `ArcSwap`: each value is one word, the runners read one on every sandbox
/// they build, and nothing reads the pair together, so there is nothing for a
/// lock or a swap to keep consistent.
pub struct ScriptLimits {
    instruction_limit: AtomicU32,
    wall_clock_seconds: AtomicU32,
}

impl Default for ScriptLimits {
    fn default() -> Self {
        Self::new(DEFAULT_INSTRUCTION_LIMIT, DEFAULT_WALL_CLOCK_SECONDS)
    }
}

impl ScriptLimits {
    pub fn new(instruction_limit: u32, wall_clock_seconds: u32) -> Self {
        Self {
            instruction_limit: AtomicU32::new(instruction_limit),
            wall_clock_seconds: AtomicU32::new(wall_clock_seconds),
        }
    }

    /// The budgets as the database and environment currently state them.
    pub async fn load(pool: &AnyPool, backend: DatabaseBackend) -> Self {
        let limits = Self::default();
        limits.refresh(pool, backend).await;
        limits
    }

    pub fn instruction_limit(&self) -> u32 {
        self.instruction_limit.load(Ordering::Relaxed)
    }

    pub fn wall_clock(&self) -> Duration {
        Duration::from_secs(u64::from(self.wall_clock_seconds.load(Ordering::Relaxed)))
    }

    pub fn set_instruction_limit(&self, limit: u32) {
        self.instruction_limit.store(limit, Ordering::Relaxed);
    }

    pub fn set_wall_clock_seconds(&self, seconds: u32) {
        self.wall_clock_seconds.store(seconds, Ordering::Relaxed);
    }

    /// Re-read both budgets. A stored value is validated on the way in, so an
    /// unacceptable one can only come from the environment or a hand-edited
    /// row; it is reported and the default stands. A database that cannot be
    /// read says nothing about what it holds, so the current values stand
    /// until a read succeeds; the runners must not swing to the default and
    /// back on a hiccup nobody can see.
    pub async fn refresh(&self, pool: &AnyPool, backend: DatabaseBackend) {
        let read = async {
            let limit = resolve(
                pool,
                backend,
                INSTRUCTION_LIMIT_KEY,
                DEFAULT_INSTRUCTION_LIMIT,
            )
            .await?;
            let seconds = resolve(
                pool,
                backend,
                WALL_CLOCK_SECONDS_KEY,
                DEFAULT_WALL_CLOCK_SECONDS,
            )
            .await?;
            Ok::<_, sqlx::Error>((limit, seconds))
        };
        match read.await {
            Ok((limit, seconds)) => {
                self.set_instruction_limit(limit);
                self.set_wall_clock_seconds(seconds);
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "could not read the script budgets; keeping the current values"
                );
            }
        }
    }
}

/// Whether `key` is one of the two budgets.
pub fn is_limit_key(key: &str) -> bool {
    key == INSTRUCTION_LIMIT_KEY || key == WALL_CLOCK_SECONDS_KEY
}

fn range_for(key: &str) -> Option<RangeInclusive<u32>> {
    match key {
        INSTRUCTION_LIMIT_KEY => Some(INSTRUCTION_LIMIT_RANGE),
        WALL_CLOCK_SECONDS_KEY => Some(WALL_CLOCK_SECONDS_RANGE),
        _ => None,
    }
}

/// The rule a stored budget must satisfy; anything else could disable a limit.
pub fn validate(key: &str, value: &str) -> Result<(), String> {
    let Some(range) = range_for(key) else {
        return Ok(());
    };
    if parse_within(value, &range).is_none() {
        return Err(format!(
            "{key} must be an integer from {} to {}",
            range.start(),
            range.end()
        ));
    }
    Ok(())
}

/// The budget `raw` states, if it is an integer inside `range`.
pub fn parse_within(raw: &str, range: &RangeInclusive<u32>) -> Option<u32> {
    raw.trim().parse::<u32>().ok().filter(|n| range.contains(n))
}

async fn resolve(
    pool: &AnyPool,
    backend: DatabaseBackend,
    key: &str,
    default: u32,
) -> Result<u32, sqlx::Error> {
    let range = range_for(key).expect("resolve is called with a budget key");
    Ok(match try_get_setting(pool, key, backend).await? {
        Some(raw) => parse_within(&raw, &range).unwrap_or_else(|| {
            tracing::warn!(
                key,
                value = %raw,
                "not an integer from {} to {}; using {default}",
                range.start(),
                range.end()
            );
            default
        }),
        None => default,
    })
}

/// Keep `limits` converged with the database for the life of the process.
pub async fn run_refresh_loop(
    limits: std::sync::Arc<ScriptLimits>,
    pool: AnyPool,
    backend: DatabaseBackend,
) {
    refresh_every(REFRESH_INTERVAL, limits, pool, backend).await
}

async fn refresh_every(
    interval: Duration,
    limits: std::sync::Arc<ScriptLimits>,
    pool: AnyPool,
    backend: DatabaseBackend,
) {
    loop {
        tokio::time::sleep(interval).await;
        limits.refresh(&pool, backend).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lua::sandbox::create_sandbox_with_limit;
    use crate::test_support::migrated_memory_pool;
    use serial_test::serial;
    use std::sync::Arc;

    async fn store(pool: &AnyPool, key: &str, value: &str) {
        crate::db::query(
            "INSERT INTO happyview_instance_settings (key, value, updated_at) VALUES (?, ?, ?) \
             ON CONFLICT (key) DO UPDATE SET value = ?, updated_at = ?",
        )
        .bind(key)
        .bind(value)
        .bind("2026-01-01T00:00:00+00:00")
        .bind(value)
        .bind("2026-01-01T00:00:00+00:00")
        .execute(pool)
        .await
        .unwrap();
    }

    /// Clears the named variables when dropped, so a failing assertion in one
    /// test of the serial group cannot leave them set for the next.
    struct EnvGuard(&'static [&'static str]);

    impl EnvGuard {
        fn set(&self, name: &'static str, value: &str) {
            assert!(self.0.contains(&name));
            unsafe { std::env::set_var(name, value) };
        }

        fn remove(&self, name: &'static str) {
            assert!(self.0.contains(&name));
            unsafe { std::env::remove_var(name) };
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for name in self.0 {
                unsafe { std::env::remove_var(name) };
            }
        }
    }

    const BUDGET_ENV: &[&str] = &["SCRIPT_INSTRUCTION_LIMIT", "SCRIPT_WALL_CLOCK_SECONDS"];

    #[test]
    fn a_budget_is_an_integer_inside_its_range() {
        let range = 10..=100;
        assert_eq!(parse_within("10", &range), Some(10));
        assert_eq!(parse_within(" 42 ", &range), Some(42));
        assert_eq!(parse_within("100", &range), Some(100));
        assert_eq!(parse_within("9", &range), None);
        assert_eq!(parse_within("101", &range), None);
        assert_eq!(parse_within("0", &range), None);
        assert_eq!(parse_within("-1", &range), None);
        assert_eq!(parse_within("1e6", &range), None);
        assert_eq!(parse_within("", &range), None);
        assert_eq!(parse_within("ten", &range), None);
    }

    #[test]
    fn validation_covers_only_the_two_budgets() {
        assert_eq!(validate(INSTRUCTION_LIMIT_KEY, "250000"), Ok(()));
        assert_eq!(validate(INSTRUCTION_LIMIT_KEY, "1000"), Ok(()));
        assert_eq!(validate(INSTRUCTION_LIMIT_KEY, "1000000000"), Ok(()));
        assert_eq!(validate(WALL_CLOCK_SECONDS_KEY, "30"), Ok(()));
        assert_eq!(validate(WALL_CLOCK_SECONDS_KEY, "1"), Ok(()));
        assert_eq!(validate(WALL_CLOCK_SECONDS_KEY, "300"), Ok(()));
        for bad in ["0", "-5", "abc", "", "2.5", "999", "1000000001"] {
            let err = validate(INSTRUCTION_LIMIT_KEY, bad).unwrap_err();
            assert_eq!(
                err, "script_instruction_limit must be an integer from 1000 to 1000000000",
                "{bad:?}"
            );
        }
        for bad in ["0", "-5", "abc", "", "2.5", "301", "600000"] {
            let err = validate(WALL_CLOCK_SECONDS_KEY, bad).unwrap_err();
            assert_eq!(
                err, "script_wall_clock_seconds must be an integer from 1 to 300",
                "{bad:?}"
            );
        }
        assert_eq!(validate("app_name", "0"), Ok(()));
    }

    #[test]
    fn the_defaults_are_the_documented_ones() {
        let limits = ScriptLimits::default();
        assert_eq!(limits.instruction_limit(), 1_000_000);
        assert_eq!(limits.wall_clock(), Duration::from_secs(10));
        assert!(INSTRUCTION_LIMIT_RANGE.contains(&DEFAULT_INSTRUCTION_LIMIT));
        assert!(WALL_CLOCK_SECONDS_RANGE.contains(&DEFAULT_WALL_CLOCK_SECONDS));
    }

    /// The floor is what it takes to build a sandbox: at it the VM comes up,
    /// and below it the guards alone spend the budget.
    #[test]
    fn the_instruction_floor_is_enough_to_build_a_sandbox() {
        let floor = *INSTRUCTION_LIMIT_RANGE.start();
        create_sandbox_with_limit(floor).expect("a sandbox builds at the floor");
        let err = create_sandbox_with_limit(5).expect_err("the guards outrun a budget of five");
        assert!(err.to_string().contains("execution limit"), "{err}");
    }

    /// The dashboard validates against the same numbers before it sends a
    /// value, so its copy has to move with these constants.
    #[test]
    fn the_dashboard_mirrors_the_defaults_and_the_ranges() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/web/src/types/settings.ts");
        let source = std::fs::read_to_string(path).unwrap();
        for expected in [
            format!("script_instruction_limit: \"{DEFAULT_INSTRUCTION_LIMIT}\""),
            format!("script_wall_clock_seconds: \"{DEFAULT_WALL_CLOCK_SECONDS}\""),
            format!(
                "script_instruction_limit: {{ min: {}, max: {} }}",
                INSTRUCTION_LIMIT_RANGE.start(),
                INSTRUCTION_LIMIT_RANGE.end()
            ),
            format!(
                "script_wall_clock_seconds: {{ min: {}, max: {} }}",
                WALL_CLOCK_SECONDS_RANGE.start(),
                WALL_CLOCK_SECONDS_RANGE.end()
            ),
        ] {
            assert!(source.contains(&expected), "{path} lacks `{expected}`");
        }
    }

    /// The database row wins over the environment, which wins over the
    /// default. Serialised with the tests below because the environment is
    /// process-wide.
    #[tokio::test]
    #[serial(script_limit_env)]
    async fn refresh_resolves_database_then_env_then_default() {
        let env = EnvGuard(BUDGET_ENV);
        let pool = migrated_memory_pool().await;
        let limits = ScriptLimits::new(1, 1);

        limits.refresh(&pool, DatabaseBackend::Sqlite).await;
        assert_eq!(limits.instruction_limit(), DEFAULT_INSTRUCTION_LIMIT);
        assert_eq!(
            limits.wall_clock(),
            Duration::from_secs(u64::from(DEFAULT_WALL_CLOCK_SECONDS))
        );

        env.set("SCRIPT_INSTRUCTION_LIMIT", "7000");
        env.set("SCRIPT_WALL_CLOCK_SECONDS", "4");
        limits.refresh(&pool, DatabaseBackend::Sqlite).await;
        assert_eq!(limits.instruction_limit(), 7000);
        assert_eq!(limits.wall_clock(), Duration::from_secs(4));

        store(&pool, INSTRUCTION_LIMIT_KEY, "2500").await;
        store(&pool, WALL_CLOCK_SECONDS_KEY, "3").await;
        limits.refresh(&pool, DatabaseBackend::Sqlite).await;
        assert_eq!(limits.instruction_limit(), 2500);
        assert_eq!(limits.wall_clock(), Duration::from_secs(3));

        crate::db::query("DELETE FROM happyview_instance_settings WHERE key = ?")
            .bind(INSTRUCTION_LIMIT_KEY)
            .execute(&pool)
            .await
            .unwrap();
        limits.refresh(&pool, DatabaseBackend::Sqlite).await;
        assert_eq!(limits.instruction_limit(), 7000);
        assert_eq!(limits.wall_clock(), Duration::from_secs(3));

        env.remove("SCRIPT_INSTRUCTION_LIMIT");
        env.remove("SCRIPT_WALL_CLOCK_SECONDS");
        limits.refresh(&pool, DatabaseBackend::Sqlite).await;
        assert_eq!(limits.instruction_limit(), DEFAULT_INSTRUCTION_LIMIT);
    }

    #[tokio::test]
    #[serial(script_limit_env)]
    async fn a_hand_edited_row_that_is_not_a_budget_leaves_the_default() {
        let pool = migrated_memory_pool().await;
        store(&pool, INSTRUCTION_LIMIT_KEY, "0").await;
        store(&pool, WALL_CLOCK_SECONDS_KEY, "soon").await;
        let limits = ScriptLimits::load(&pool, DatabaseBackend::Sqlite).await;
        assert_eq!(limits.instruction_limit(), DEFAULT_INSTRUCTION_LIMIT);
        assert_eq!(
            limits.wall_clock(),
            Duration::from_secs(u64::from(DEFAULT_WALL_CLOCK_SECONDS))
        );
    }

    #[tokio::test]
    #[serial(script_limit_env)]
    async fn an_environment_value_outside_the_range_leaves_the_default() {
        let env = EnvGuard(BUDGET_ENV);
        let pool = migrated_memory_pool().await;
        env.set("SCRIPT_INSTRUCTION_LIMIT", "10");
        env.set("SCRIPT_WALL_CLOCK_SECONDS", "600000");
        let limits = ScriptLimits::load(&pool, DatabaseBackend::Sqlite).await;
        assert_eq!(limits.instruction_limit(), DEFAULT_INSTRUCTION_LIMIT);
        assert_eq!(
            limits.wall_clock(),
            Duration::from_secs(u64::from(DEFAULT_WALL_CLOCK_SECONDS))
        );
    }

    #[tokio::test]
    #[serial(script_limit_env)]
    async fn a_database_that_cannot_be_read_keeps_the_current_values() {
        let pool = migrated_memory_pool().await;
        store(&pool, INSTRUCTION_LIMIT_KEY, "20000000").await;
        store(&pool, WALL_CLOCK_SECONDS_KEY, "60").await;
        let limits = ScriptLimits::load(&pool, DatabaseBackend::Sqlite).await;
        assert_eq!(limits.instruction_limit(), 20_000_000);

        pool.close().await;
        limits.refresh(&pool, DatabaseBackend::Sqlite).await;
        assert_eq!(limits.instruction_limit(), 20_000_000);
        assert_eq!(limits.wall_clock(), Duration::from_secs(60));
    }

    #[tokio::test]
    #[serial(script_limit_env)]
    async fn the_refresh_loop_picks_up_another_writers_change() {
        let pool = migrated_memory_pool().await;
        let limits = Arc::new(ScriptLimits::load(&pool, DatabaseBackend::Sqlite).await);
        let interval = Duration::from_millis(20);
        let task = tokio::spawn(refresh_every(
            interval,
            Arc::clone(&limits),
            pool.clone(),
            DatabaseBackend::Sqlite,
        ));

        store(&pool, INSTRUCTION_LIMIT_KEY, "4321").await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while limits.instruction_limit() != 4321 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the loop did not pick up the stored value"
            );
            tokio::time::sleep(interval).await;
        }
        task.abort();
    }

    #[test]
    fn the_env_fallback_table_names_both_variables() {
        assert_eq!(
            crate::admin::settings::env_var_for(INSTRUCTION_LIMIT_KEY),
            Some("SCRIPT_INSTRUCTION_LIMIT")
        );
        assert_eq!(
            crate::admin::settings::env_var_for(WALL_CLOCK_SECONDS_KEY),
            Some("SCRIPT_WALL_CLOCK_SECONDS")
        );
    }
}

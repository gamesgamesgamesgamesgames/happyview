//! Periodic SQLite upkeep.
//!
//! A WAL only rewinds when a checkpoint has copied every frame back *and* no
//! reader is still using the log. With dozens of pooled connections some
//! reader is nearly always active, so SQLite's own PASSIVE autocheckpoints
//! never get that moment and the log grows until shutdown. A TRUNCATE
//! checkpoint waits for readers to move off the log and then empties it. This
//! task runs one on a timer, and a daily `PRAGMA optimize` keeps planner
//! statistics fresh.

use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use sqlx::AnyPool;

use crate::db::DatabaseBackend;

/// Seconds between TRUNCATE checkpoints unless
/// `SQLITE_CHECKPOINT_INTERVAL_SECS` says otherwise.
pub const DEFAULT_CHECKPOINT_INTERVAL_SECS: u64 = 60;

/// Consecutive busy checkpoints after which the task warns, once per streak.
pub const BUSY_WARN_AFTER: u32 = 10;

/// How long a checkpoint waits for readers before reporting busy. A pending
/// TRUNCATE holds off new writers, so this stays well below the pool's 5 s
/// busy timeout: a busy checkpoint costs nothing, a stalled writer costs
/// ingest.
pub const CHECKPOINT_BUSY_TIMEOUT_MS: u64 = 1000;

/// Quick retries for a checkpoint that came back busy without waiting.
/// SQLite never waits on the checkpoint lock, so a TRUNCATE that collides
/// with a writer's own PASSIVE autocheckpoint reports busy at once. That
/// collision clears in milliseconds, and without a retry it costs a whole
/// interval of WAL growth.
pub const LOCK_COLLISION_RETRIES: u32 = 5;

/// Pause between those retries.
pub const LOCK_COLLISION_RETRY_DELAY: Duration = Duration::from_millis(20);

/// How often `PRAGMA optimize` runs after the one at startup.
pub const OPTIMIZE_INTERVAL: Duration = Duration::from_secs(86_400);

/// What one TRUNCATE checkpoint reported.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CheckpointOutcome {
    pub at: String,
    /// The checkpoint could not finish because a reader or writer held on.
    pub busy: bool,
    /// Frames in the WAL when the checkpoint ran.
    pub wal_frames: i64,
    /// Frames copied back into the database file.
    pub checkpointed_frames: i64,
}

/// The latest outcome, for the admin database page. Process-wide because the
/// process has exactly one SQLite database.
static LAST_CHECKPOINT: Mutex<Option<CheckpointOutcome>> = Mutex::new(None);

pub fn last_checkpoint() -> Option<CheckpointOutcome> {
    LAST_CHECKPOINT.lock().ok().and_then(|last| last.clone())
}

/// Parse `SQLITE_CHECKPOINT_INTERVAL_SECS`. `0` disables the checkpoint task;
/// anything unparseable falls back to the default.
pub fn parse_checkpoint_interval_secs(raw: Option<&str>) -> u64 {
    raw.and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_CHECKPOINT_INTERVAL_SECS)
}

pub fn checkpoint_interval_secs() -> u64 {
    parse_checkpoint_interval_secs(
        std::env::var("SQLITE_CHECKPOINT_INTERVAL_SECS")
            .ok()
            .as_deref(),
    )
}

/// Run one `PRAGMA wal_checkpoint(TRUNCATE)` on a pooled connection.
///
/// The connection's busy timeout is shortened for the checkpoint and put back
/// before it returns to the pool. If putting it back fails, the connection is
/// closed instead: one left at the short timeout would fail ordinary writes
/// under contention. The same holds if the future is dropped mid-checkpoint.
pub async fn checkpoint_truncate(pool: &AnyPool) -> Result<CheckpointOutcome, sqlx::Error> {
    let mut guard = ShortTimeoutGuard {
        conn: pool.acquire().await?,
        armed: true,
    };
    let conn = &mut guard.conn;
    crate::db::query(&pragma_sql(&format!(
        "PRAGMA busy_timeout = {CHECKPOINT_BUSY_TIMEOUT_MS}"
    )))
    .execute(&mut **conn)
    .await?;
    let checkpoint =
        crate::db::query_as::<(i64, i64, i64)>(&pragma_sql("PRAGMA wal_checkpoint(TRUNCATE)"))
            .fetch_one(&mut **conn)
            .await;
    let restored = crate::db::query(&pragma_sql(&format!(
        "PRAGMA busy_timeout = {}",
        crate::db::SQLITE_BUSY_TIMEOUT_MS
    )))
    .execute(&mut **conn)
    .await;
    if restored.is_ok() {
        guard.armed = false;
    }
    let (busy, wal_frames, checkpointed_frames) = checkpoint?;
    restored?;
    let outcome = CheckpointOutcome {
        at: crate::db::now_rfc3339(),
        busy: busy != 0,
        wal_frames,
        checkpointed_frames,
    };
    if let Ok(mut last) = LAST_CHECKPOINT.lock() {
        *last = Some(outcome.clone());
    }
    Ok(outcome)
}

/// Closes the connection on drop while it may still carry the short busy
/// timeout, so a cancelled checkpoint never returns it to the pool that way.
struct ShortTimeoutGuard {
    conn: sqlx::pool::PoolConnection<sqlx::Any>,
    armed: bool,
}

impl Drop for ShortTimeoutGuard {
    fn drop(&mut self) {
        if self.armed {
            self.conn.close_on_drop();
        }
    }
}

fn pragma_sql(sql: &str) -> String {
    crate::db::adapt_sql(sql, DatabaseBackend::Sqlite)
}

/// Counts consecutive busy checkpoints so the task warns once per streak
/// rather than every minute.
#[derive(Debug, Default)]
pub struct BusyTracker {
    consecutive: u32,
    warned: bool,
}

impl BusyTracker {
    /// Record one outcome. Returns true exactly when the warning is due.
    pub fn observe(&mut self, busy: bool) -> bool {
        if !busy {
            self.consecutive = 0;
            self.warned = false;
            return false;
        }
        self.consecutive = self.consecutive.saturating_add(1);
        if self.consecutive >= BUSY_WARN_AFTER && !self.warned {
            self.warned = true;
            return true;
        }
        false
    }
}

/// Run `attempt`, retrying while it comes back busy faster than the busy
/// timeout could have elapsed, which means it lost the checkpoint lock rather
/// than waited on readers. A busy result after waiting on readers is not
/// retried: another wait would hold off writers again for nothing.
async fn retry_lock_collisions<F, Fut>(mut attempt: F) -> Result<CheckpointOutcome, sqlx::Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<CheckpointOutcome, sqlx::Error>>,
{
    let waited_on_readers = Duration::from_millis(CHECKPOINT_BUSY_TIMEOUT_MS / 2);
    let mut retries = 0;
    loop {
        let started = tokio::time::Instant::now();
        let outcome = attempt().await?;
        if !outcome.busy
            || started.elapsed() >= waited_on_readers
            || retries == LOCK_COLLISION_RETRIES
        {
            return Ok(outcome);
        }
        retries += 1;
        tokio::time::sleep(LOCK_COLLISION_RETRY_DELAY).await;
    }
}

async fn run_checkpoint(pool: &AnyPool, busy: &mut BusyTracker) {
    match retry_lock_collisions(|| checkpoint_truncate(pool)).await {
        Ok(outcome) => {
            if outcome.busy {
                tracing::debug!(
                    wal_frames = outcome.wal_frames,
                    checkpointed = outcome.checkpointed_frames,
                    "WAL checkpoint was busy; will retry"
                );
            }
            if busy.observe(outcome.busy) {
                tracing::warn!(
                    "the WAL checkpoint has been busy {BUSY_WARN_AFTER} times in a row; \
                     long-running readers are keeping the write-ahead log from shrinking"
                );
            }
        }
        Err(e) => tracing::warn!(error = %e, "WAL checkpoint failed"),
    }
}

/// Resolves on the interval's next tick, or never when there is no interval.
async fn next_tick(tick: Option<&mut tokio::time::Interval>) {
    match tick {
        Some(tick) => {
            tick.tick().await;
        }
        None => std::future::pending::<()>().await,
    }
}

/// The maintenance loop. A no-op on Postgres. `checkpoint_every` of `None`
/// disables checkpoints but keeps the daily optimize.
pub async fn run(pool: AnyPool, backend: DatabaseBackend, checkpoint_every: Option<Duration>) {
    if backend != DatabaseBackend::Sqlite {
        return;
    }
    let mut busy = BusyTracker::default();
    let mut checkpoint_tick = checkpoint_every.map(|every| {
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick
    });
    // Startup already ran `optimize` (see `db::connect`), so the first one
    // here is a day out.
    let mut optimize_tick = tokio::time::interval_at(
        tokio::time::Instant::now() + OPTIMIZE_INTERVAL,
        OPTIMIZE_INTERVAL,
    );
    optimize_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = next_tick(checkpoint_tick.as_mut()) => run_checkpoint(&pool, &mut busy).await,
            _ = optimize_tick.tick() => {
                if let Err(e) = crate::db::sqlite_optimize(&pool).await {
                    tracing::warn!(error = %e, "daily PRAGMA optimize failed");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use serial_test::serial;

    use super::*;

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn checkpoint_interval_defaults_to_sixty_seconds() {
        assert_eq!(parse_checkpoint_interval_secs(None), 60);
        assert_eq!(parse_checkpoint_interval_secs(Some("banana")), 60);
        assert_eq!(parse_checkpoint_interval_secs(Some("15")), 15);
        assert_eq!(parse_checkpoint_interval_secs(Some("0")), 0, "0 disables");
    }

    #[test]
    fn a_busy_streak_warns_once_and_a_success_resets_it() {
        let mut busy = BusyTracker::default();
        let warnings: Vec<bool> = (0..BUSY_WARN_AFTER + 3)
            .map(|_| busy.observe(true))
            .collect();
        assert_eq!(warnings.iter().filter(|w| **w).count(), 1);
        assert!(warnings[(BUSY_WARN_AFTER - 1) as usize]);

        assert!(!busy.observe(false));
        let again: Vec<bool> = (0..BUSY_WARN_AFTER).map(|_| busy.observe(true)).collect();
        assert_eq!(
            again.iter().filter(|w| **w).count(),
            1,
            "a new streak warns again"
        );
    }

    fn outcome(busy: bool) -> CheckpointOutcome {
        CheckpointOutcome {
            at: String::new(),
            busy,
            wal_frames: if busy { -1 } else { 0 },
            checkpointed_frames: if busy { -1 } else { 0 },
        }
    }

    /// Runs `retry_lock_collisions` over scripted attempts, each taking
    /// `took`, and returns the final outcome and how many
    /// attempts ran.
    async fn retry_script(script: Vec<bool>, took: Duration) -> (CheckpointOutcome, usize) {
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&attempts);
        let result = retry_lock_collisions(|| {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            let busy = script.get(n).copied().unwrap_or(true);
            async move {
                tokio::time::sleep(took).await;
                Ok::<_, sqlx::Error>(outcome(busy))
            }
        })
        .await
        .expect("checkpoint");
        (result, attempts.load(Ordering::SeqCst))
    }

    #[tokio::test]
    async fn a_checkpoint_that_lost_the_lock_is_retried_until_it_runs() {
        let (result, attempts) = retry_script(vec![true, true, false], Duration::ZERO).await;
        assert!(!result.busy);
        assert_eq!(attempts, 3);
    }

    #[tokio::test]
    async fn lock_collision_retries_are_bounded() {
        let (result, attempts) = retry_script(vec![], Duration::ZERO).await;
        assert!(result.busy);
        assert_eq!(attempts, 1 + LOCK_COLLISION_RETRIES as usize);
    }

    #[tokio::test]
    async fn a_checkpoint_busy_after_waiting_on_readers_is_not_retried() {
        let waited = Duration::from_millis(CHECKPOINT_BUSY_TIMEOUT_MS);
        let (result, attempts) = retry_script(vec![true, false], waited).await;
        assert!(result.busy);
        assert_eq!(attempts, 1);
    }

    #[tokio::test]
    async fn a_checkpoint_returns_its_connection_with_the_normal_busy_timeout() {
        sqlx::any::install_default_drivers();
        let path = std::env::temp_dir().join(format!("hv-ckpt-{}.db", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}?mode=rwc", path.display());
        // One connection, so the one the checkpoint borrowed is the one read back.
        let pool = sqlx::pool::PoolOptions::<sqlx::Any>::new()
            .max_connections(1)
            .connect(&url)
            .await
            .expect("connect");
        crate::db::query("PRAGMA journal_mode = WAL")
            .execute(&pool)
            .await
            .expect("enable WAL");
        crate::db::query("CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .execute(&pool)
            .await
            .expect("create table");
        crate::db::query("INSERT INTO t (id) VALUES (1)")
            .execute(&pool)
            .await
            .expect("insert row");

        let outcome = checkpoint_truncate(&pool).await.expect("checkpoint");
        assert!(!outcome.busy, "{outcome:?}");

        let (timeout,): (i64,) = crate::db::query_as("PRAGMA busy_timeout")
            .fetch_one(&pool)
            .await
            .expect("read busy_timeout");
        assert_eq!(timeout, crate::db::SQLITE_BUSY_TIMEOUT_MS as i64);

        drop(pool);
        remove_db_files(&path);
    }

    /// Writes 6000 rows of 4 KiB into a file-backed database while three
    /// staggered readers keep a read transaction open at every instant, and
    /// returns the largest the `-wal` file got and how many times it was seen
    /// to shrink sharply (a truncate).
    ///
    /// Overlapping readers are what stopped the WAL on the managed tenant
    /// from ever rewinding: SQLite's own PASSIVE checkpoints can copy frames
    /// back but cannot restart the log while a reader holds a snapshot in it.
    async fn wal_peak_under_load(checkpoint_every: Option<Duration>) -> (u64, u32) {
        let path = std::env::temp_dir().join(format!("hv-wal-{}.db", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = crate::db::connect(&url, DatabaseBackend::Sqlite).await;
        crate::db::query("CREATE TABLE wal_load (id INTEGER PRIMARY KEY, body BLOB NOT NULL)")
            .execute(&pool)
            .await
            .expect("create table");

        let maintenance = checkpoint_every
            .map(|every| tokio::spawn(run(pool.clone(), DatabaseBackend::Sqlite, Some(every))));

        let stop = Arc::new(AtomicBool::new(false));
        let mut readers = Vec::new();
        for i in 0..3u64 {
            let pool = pool.clone();
            let stop = Arc::clone(&stop);
            readers.push(tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(i * 10)).await;
                while !stop.load(Ordering::Relaxed) {
                    let mut tx = pool.begin().await.expect("begin read");
                    let _: (i64,) = crate::db::query_as("SELECT COUNT(*) FROM wal_load")
                        .fetch_one(&mut *tx)
                        .await
                        .expect("read");
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    tx.commit().await.expect("end read");
                }
            }));
        }

        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let body = vec![7u8; 4096];
        let mut peak = 0u64;
        let mut last = 0u64;
        let mut truncations = 0u32;
        for i in 0..6000i64 {
            crate::db::query("INSERT INTO wal_load (id, body) VALUES (?, ?)")
                .bind(i)
                .bind(body.clone())
                .execute(&pool)
                .await
                .expect("insert");
            if i % 10 == 0 {
                let size = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
                peak = peak.max(size);
                if last > MIB && size < last / 2 {
                    truncations += 1;
                }
                last = size;
            }
        }

        stop.store(true, Ordering::Relaxed);
        for reader in readers {
            reader.await.expect("reader task");
        }
        if let Some(handle) = maintenance {
            handle.abort();
        }
        drop(pool);
        remove_db_files(&path);
        (peak, truncations)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[serial]
    async fn the_checkpoint_task_bounds_the_wal_under_concurrent_readers() {
        let (unchecked, unchecked_truncations) = wal_peak_under_load(None).await;
        let (checked, checked_truncations) =
            wal_peak_under_load(Some(Duration::from_millis(50))).await;
        assert!(
            unchecked > 16 * MIB,
            "without the task the WAL should grow with the writes (peak {unchecked} bytes)"
        );
        assert_eq!(
            unchecked_truncations, 0,
            "overlapping readers should keep the WAL from ever rewinding on its own"
        );
        assert!(
            checked_truncations >= 1,
            "with the task the WAL should be truncated at least once (peak {checked} bytes)"
        );
        assert!(
            checked < unchecked / 2,
            "with the task the WAL should stay well under the unchecked peak \
             (checked {checked}, unchecked {unchecked} bytes)"
        );
    }

    #[tokio::test]
    async fn a_busy_checkpoint_still_restores_the_normal_busy_timeout() {
        sqlx::any::install_default_drivers();
        let path = std::env::temp_dir().join(format!("hv-busy-{}.db", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = sqlx::pool::PoolOptions::<sqlx::Any>::new()
            .max_connections(3)
            .connect(&url)
            .await
            .expect("connect");
        crate::db::query("PRAGMA journal_mode = WAL")
            .execute(&pool)
            .await
            .expect("enable WAL");
        crate::db::query("CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .execute(&pool)
            .await
            .expect("create table");

        // A reader parked mid-snapshot, then a write behind it, leaves frames
        // the TRUNCATE cannot finish until the reader leaves.
        let mut reader = pool.begin().await.expect("begin read");
        let _: (i64,) = crate::db::query_as("SELECT COUNT(*) FROM t")
            .fetch_one(&mut *reader)
            .await
            .expect("read");
        crate::db::query("INSERT INTO t (id) VALUES (1)")
            .execute(&pool)
            .await
            .expect("insert row");

        let outcome = checkpoint_truncate(&pool).await.expect("checkpoint");
        assert!(outcome.busy, "{outcome:?}");

        reader.commit().await.expect("end read");
        let mut a = pool.acquire().await.expect("acquire a");
        let mut b = pool.acquire().await.expect("acquire b");
        let mut c = pool.acquire().await.expect("acquire c");
        for conn in [&mut a, &mut b, &mut c] {
            let (timeout,): (i64,) = crate::db::query_as("PRAGMA busy_timeout")
                .fetch_one(&mut **conn)
                .await
                .expect("read busy_timeout");
            assert_eq!(timeout, crate::db::SQLITE_BUSY_TIMEOUT_MS as i64);
        }

        drop((a, b, c));
        drop(pool);
        remove_db_files(&path);
    }

    fn remove_db_files(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }
}

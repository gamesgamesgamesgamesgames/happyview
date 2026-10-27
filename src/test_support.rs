//! Shared fixtures for `cargo test --lib`.
//!
//! Building an `AppState` requires a fully-constructed OAuth client, which is
//! ~100 lines of boilerplate that several test modules had each copied. This
//! module exists so new tests can take a pool and get a usable state back.

use crate::AppState;
use crate::config::Config;

/// An in-memory SQLite pool. `max_connections(1)` is required: every
/// connection to `sqlite::memory:` gets its own private database, so a larger
/// pool would hand later queries an empty schema.
pub async fn memory_pool() -> sqlx::AnyPool {
    crate::db::install_drivers();
    sqlx::pool::PoolOptions::<sqlx::Any>::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("connect to in-memory sqlite")
}

/// A [`memory_pool`] with every SQLite migration applied and the record
/// store upgraded, as `db::connect` leaves a database. A hand-written
/// `CREATE TABLE` in a test drifts silently from the real schema — a
/// migration can add a column the copy never gets, and a test against the
/// stale copy keeps passing while the code it exercises breaks against a real
/// database. The path is anchored to the crate manifest so the helper works
/// from any working directory.
pub async fn migrated_memory_pool() -> sqlx::AnyPool {
    let pool = pre_upgrade_memory_pool().await;
    crate::record_store::upgrade::run(&pool, crate::db::DatabaseBackend::Sqlite)
        .await
        .expect("upgrade the record store");
    pool
}

/// Every SQLite migration applied, the record store upgrade not run: the
/// legacy record tables are still tables and the v2 tables are empty.
pub async fn pre_upgrade_memory_pool() -> sqlx::AnyPool {
    let pool = memory_pool().await;
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations/sqlite");
    sqlx::migrate::Migrator::new(dir)
        .await
        .expect("load sqlite migrations")
        .run(&pool)
        .await
        .expect("run migrations");
    pool
}

/// A pre-upgrade database (see [`pre_upgrade_memory_pool`]). The Postgres
/// one is a scratch database on the `TEST_DATABASE_URL` server, so tests
/// that copy or swap tables never touch the shared test schema.
pub struct PreUpgradePool {
    pub pool: sqlx::AnyPool,
    pub backend: crate::db::DatabaseBackend,
    scratch: Option<(sqlx::AnyPool, String)>,
}

impl PreUpgradePool {
    /// Close the pool and drop the scratch database, if any.
    pub async fn finish(self) {
        self.pool.close().await;
        if let Some((admin, name)) = self.scratch {
            let _ = crate::db::query(&format!("DROP DATABASE {name} WITH (FORCE)"))
                .execute(&admin)
                .await;
            admin.close().await;
        }
    }
}

/// A pre-upgrade SQLite database, plus a Postgres one when
/// `TEST_DATABASE_URL` names a Postgres server.
pub async fn pre_upgrade_pools() -> Vec<PreUpgradePool> {
    use crate::db::DatabaseBackend;
    let mut out = vec![PreUpgradePool {
        pool: pre_upgrade_memory_pool().await,
        backend: DatabaseBackend::Sqlite,
        scratch: None,
    }];
    if let Ok(url) = std::env::var("TEST_DATABASE_URL")
        && DatabaseBackend::from_url(&url) == DatabaseBackend::Postgres
    {
        crate::db::install_drivers();
        let admin = sqlx::AnyPool::connect(&url)
            .await
            .expect("connect to the test server");
        let name = format!("hv_pre_{}", uuid::Uuid::new_v4().simple());
        crate::db::query(&format!("CREATE DATABASE {name}"))
            .execute(&admin)
            .await
            .expect("create a scratch database");
        let (server, _) = url
            .rsplit_once('/')
            .expect("TEST_DATABASE_URL names a database");
        let pool = sqlx::AnyPool::connect(&format!("{server}/{name}"))
            .await
            .expect("connect to the scratch database");
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations/postgres");
        sqlx::migrate::Migrator::new(dir)
            .await
            .expect("load postgres migrations")
            .run(&pool)
            .await
            .expect("run migrations");
        out.push(PreUpgradePool {
            pool,
            backend: DatabaseBackend::Postgres,
            scratch: Some((admin, name)),
        });
    }
    out
}

/// Insert one record the way raw SQL would, with a URI that agrees with its
/// identity (`at://{did}/{collection}/{rkey}`), and return the URI. Before the
/// record store's cutover this writes the legacy table; afterwards the same
/// statement goes through the compatibility view's trigger, which refuses a
/// URI that does not match.
#[allow(clippy::too_many_arguments)]
pub async fn seed_record(
    pool: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
    did: &str,
    collection: &str,
    rkey: &str,
    record: &str,
    cid: &str,
    indexed_at: Option<&str>,
) -> String {
    let uri = format!("at://{did}/{collection}/{rkey}");
    let sql = crate::db::adapt_sql(
        "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        backend,
    );
    crate::db::query(&sql)
        .bind(&uri)
        .bind(did)
        .bind(collection)
        .bind(rkey)
        .bind(record)
        .bind(cid)
        .bind(indexed_at)
        .bind(crate::db::now_rfc3339())
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("seed {uri}: {e}"));
    uri
}

/// The encryption key every test uses.
///
/// One value across the whole suite: DB-backed tests share a database, and a
/// `#atproto_space` key written under one encryption key cannot be decrypted
/// under another. Differing keys surface later as an opaque "failed to decrypt
/// verification key" on the first space write.
pub const TEST_ENCRYPTION_KEY: [u8; 32] = [0x42u8; 32];

/// Provision the `#atproto_space` signing key for a test.
///
/// Idempotent, and safe to race: every test uses [`TEST_ENCRYPTION_KEY`], so an
/// existing row is always one this state can decrypt.
pub async fn provision_space_signing_key(state: &AppState) {
    let Some(key) = state.config.token_encryption_key.as_ref() else {
        return;
    };

    // A row written under a different encryption key cannot be decrypted, and
    // `ensure_atproto_space_method` returns an existing row as is. Replace it
    // so space writes do not fail with a decryption error.
    let usable = crate::verification_methods::get_private_key_bytes(
        &state.db,
        state.db_backend,
        "#atproto_space",
        key,
    )
    .await
    .ok()
    .flatten()
    .is_some();

    if !usable {
        let sql = crate::db::adapt_sql(
            "DELETE FROM happyview_verification_methods WHERE fragment_id = ?",
            state.db_backend,
        );
        let _ = crate::db::query(&sql)
            .bind("#atproto_space")
            .execute(&state.db)
            .await;
    }

    crate::verification_methods::ensure_atproto_space_method(&state.db, state.db_backend, key)
        .await
        .expect("provision #atproto_space key");
}

/// A state on the database `TEST_DATABASE_URL` names, or `None` when it names
/// none — which the caller reports as a skip, since a silent pass looks
/// exactly like a test that ran.
///
/// The migrated in-memory pool covers SQLite everywhere; this is how the same
/// assertions reach Postgres, whose dialect is the one `adapt_sql` has to
/// build and the one nothing under `src/plugin/` had ever run against.
pub async fn test_state_from_env() -> Option<AppState> {
    // One connect at a time, because connecting migrates: two tests migrating
    // a fresh SQLite file at once collide on `_sqlx_migrations`, and Postgres
    // takes the same path so the order is the same on both.
    static CONNECTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    let url = std::env::var("TEST_DATABASE_URL").ok()?;
    let backend = crate::db::DatabaseBackend::from_url(&url);
    let pool = {
        let _connecting = CONNECTING.lock().await;
        crate::db::connect(&url, backend).await
    };
    Some(test_state_with_pool_on(pool, backend))
}

/// Build an `AppState` backed by `pool`, wired for SQLite with no network
/// dependencies reachable (PLC and OAuth point at unroutable local ports).
pub fn test_state_with_pool(pool: sqlx::AnyPool) -> AppState {
    test_state_with_pool_on(pool, crate::db::DatabaseBackend::Sqlite)
}

/// The configuration every in-memory test state carries.
pub fn test_config(backend: crate::db::DatabaseBackend) -> Config {
    Config {
        host: "127.0.0.1".into(),
        port: 3000,
        database_url: String::new(),
        database_backend: backend,
        sqlite_journal_size_limit: crate::db::DEFAULT_JOURNAL_SIZE_LIMIT,
        public_url: String::new(),
        user_agent: String::new(),
        session_secret: "test-secret".into(),
        jetstream_url: String::new(),
        relay_url: String::new(),
        plc_url: String::new(),
        static_dir: None,
        base_path: None,
        event_log_retention_days: 30,
        app_name: None,
        logo_uri: None,
        tos_uri: None,
        policy_uri: None,
        // Spaces sign every commit with the `#atproto_space` key, which is
        // stored encrypted, so space writes need this set.
        token_encryption_key: Some(TEST_ENCRYPTION_KEY),
        default_rate_limit_capacity: 100,
        default_rate_limit_refill_rate: 2.0,
        telemetry_collector_url: String::new(),
        plugin_cache_dir: None,
        platform_api_key_hash: None,
        job_worker_concurrency: 1,
        superadmin_did: None,
        project_root: None,
        project: None,
        plugin_dir: std::path::PathBuf::from("./plugins"),
        auto_backfill: true,
        script_env_file: None,
        default_plugins: Vec::new(),
    }
}

/// A [`test_state_with_pool`] told which backend `pool` actually is.
///
/// Every statement the host imports issue goes through `adapt_sql`, and its
/// SQLite arm is a no-op, so a state that names the wrong backend exercises a
/// dialect the pool does not speak — or, run against SQLite alone, never
/// exercises Postgres's at all.
pub fn test_state_with_pool_on(
    pool: sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
) -> AppState {
    crate::state::state_with_pool_on(test_config(backend), pool, backend)
}

/// A migrated SQLite pool, plus the `TEST_DATABASE_URL` Postgres database
/// when one is set, for assertions that must hold on both backends.
pub async fn test_pools() -> Vec<(sqlx::AnyPool, crate::db::DatabaseBackend)> {
    use crate::db::DatabaseBackend;
    let mut pools = vec![(migrated_memory_pool().await, DatabaseBackend::Sqlite)];
    if let Ok(url) = std::env::var("TEST_DATABASE_URL")
        && DatabaseBackend::from_url(&url) == DatabaseBackend::Postgres
    {
        pools.push((
            crate::db::connect(&url, DatabaseBackend::Postgres).await,
            DatabaseBackend::Postgres,
        ));
    }
    pools
}

/// `sql` with each `?` replaced, in order, by the matching literal, for
/// handing a test's query to `EXPLAIN` as plain SQL on either backend.
pub fn inline_binds(sql: &str, literals: &[&str]) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut values = literals.iter();
    for ch in sql.chars() {
        if ch == '?' {
            out.push_str(values.next().expect("a literal for every placeholder"));
        } else {
            out.push(ch);
        }
    }
    assert!(values.next().is_none(), "more literals than placeholders");
    out
}

/// [`inline_binds`] for SQL already adapted to `backend`: on Postgres the
/// literals replace `$1`, `$2`, … by number.
pub fn inline_binds_on(
    sql: &str,
    backend: crate::db::DatabaseBackend,
    literals: &[&str],
) -> String {
    match backend {
        crate::db::DatabaseBackend::Sqlite => inline_binds(sql, literals),
        crate::db::DatabaseBackend::Postgres => {
            let mut out = sql.to_string();
            // Highest first, so `$1` never eats the front of `$12`.
            for (i, literal) in literals.iter().enumerate().rev() {
                let placeholder = format!("${}", i + 1);
                assert!(out.contains(&placeholder), "no {placeholder} in {sql}");
                out = out.replace(&placeholder, literal);
            }
            out
        }
    }
}

/// The planner's plan for `sql`, written in SQLite's dialect, one line per
/// step.
///
/// On Postgres, sequential scans and sorts are disabled for the statement.
/// On a near-empty test table both are cheapest, so leaving them on would
/// say nothing; with them off, the plan shows whether an index *can* serve
/// the query in order.
pub async fn query_plan(
    pool: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
    sql: &str,
) -> Vec<String> {
    let explain = |prefix: &str| crate::db::adapt_sql(&format!("{prefix} {sql}"), backend);
    match backend {
        crate::db::DatabaseBackend::Sqlite => {
            crate::db::query_as::<(i64, i64, i64, String)>(&explain("EXPLAIN QUERY PLAN"))
                .fetch_all(pool)
                .await
                .expect("explain query plan")
                .into_iter()
                .map(|(_, _, _, detail)| detail)
                .collect()
        }
        crate::db::DatabaseBackend::Postgres => {
            let mut tx = pool.begin().await.expect("begin");
            crate::db::query("SET LOCAL enable_seqscan = off")
                .execute(&mut *tx)
                .await
                .expect("disable seqscan");
            crate::db::query("SET LOCAL enable_sort = off")
                .execute(&mut *tx)
                .await
                .expect("disable sort");
            let rows: Vec<(String,)> = crate::db::query_as(&explain("EXPLAIN"))
                .fetch_all(&mut *tx)
                .await
                .expect("explain");
            tx.rollback().await.expect("rollback");
            rows.into_iter().map(|(line,)| line).collect()
        }
    }
}

/// Assert `plan` reads `index` and never sorts.
pub fn assert_index_without_sort(
    plan: &[String],
    index: &str,
    backend: crate::db::DatabaseBackend,
) {
    assert!(
        plan.iter().any(|line| line.contains(index)),
        "{backend:?} plan does not use {index}: {plan:#?}"
    );
    let sorts = match backend {
        crate::db::DatabaseBackend::Sqlite => plan.iter().any(|line| line.contains("TEMP B-TREE")),
        crate::db::DatabaseBackend::Postgres => plan.iter().any(|line| line.contains("Sort  (")),
    };
    assert!(!sorts, "{backend:?} plan sorts: {plan:#?}");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test's hand-written record tables drift from the real schema and,
    /// after the record store's cutover, from what the store writes at all.
    /// Tests use `migrated_memory_pool` instead.
    #[test]
    fn no_unit_test_hand_writes_the_record_tables() {
        // Built from parts so this file does not match its own needles.
        let needles = [
            format!("CREATE {} happyview_records", "TABLE"),
            format!("CREATE {} happyview_record_refs", "TABLE"),
        ];
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut stack = vec![src];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read src") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).expect("read file");
                    for needle in &needles {
                        if text.contains(needle.as_str()) {
                            offenders.push(format!("{}: {needle}", path.display()));
                        }
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "hand-written record tables:\n{}",
            offenders.join("\n")
        );
    }

    #[tokio::test]
    async fn seed_record_writes_a_canonical_uri() {
        let pool = migrated_memory_pool().await;
        let uri = seed_record(
            &pool,
            crate::db::DatabaseBackend::Sqlite,
            "did:plc:a",
            "c",
            "1",
            "{}",
            "cid",
            None,
        )
        .await;
        assert_eq!(uri, "at://did:plc:a/c/1");
        let row: (String,) = crate::db::query_as("SELECT did FROM happyview_records WHERE uri = ?")
            .bind(&uri)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.0, "did:plc:a");
    }
}

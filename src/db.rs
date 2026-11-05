use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use chrono::{DateTime, Utc};
use sqlx::migrate::Migrator;
use sqlx::pool::PoolOptions;
use sqlx::{Any, AnyPool};

pub use happyview_provider::db::{
    BUSY_RETRY_ATTEMPTS, DatabaseBackend, MAX_FIELD_PATH_LEN, adapt_sql, is_invalid_data,
    is_retryable_write_error, is_unavailable, is_valid_json_field_path, now_rfc3339,
    postgres_json_chain, query, query_as, query_scalar, retry_on_busy, rev_compare,
};

/// Escape the `LIKE` wildcard metacharacters (`%`, `_`) and the escape character
/// (`\`) in a value that will be embedded as a *literal* inside a `LIKE` pattern.
/// Must be paired with an explicit `ESCAPE '\'` clause — SQLite has no default
/// `LIKE` escape character, so without it the backslashes would be literal.
pub fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Begin a transaction that reads and then writes on what it read, holding
/// the write lock from the start.
///
/// SQLite's default `BEGIN` is deferred: it takes the write lock only at the
/// first write, and if another writer committed since this transaction's
/// first read, that upgrade fails with `SQLITE_BUSY` at once instead of
/// waiting out the busy timeout. `BEGIN IMMEDIATE` takes the write lock up
/// front, so a check (a tombstone lookup, a stored rev) cannot go stale
/// before the write it guards. Postgres gets a plain `BEGIN`; serialize its
/// writers on the rows concerned with `tombstones::lock_uris` and
/// `tombstones::lock_did`.
///
/// Run the whole transaction inside `retry_on_busy`, so a write lock held
/// past the busy timeout retries the transaction rather than failing it.
///
/// Inside [`with_change_reason`] on Postgres, the transaction starts by
/// setting `happyview.change_reason` (transaction-local) for triggers to read.
pub async fn begin_write(
    pool: &AnyPool,
    backend: DatabaseBackend,
) -> Result<sqlx::Transaction<'static, Any>, sqlx::Error> {
    match backend {
        DatabaseBackend::Sqlite => pool.begin_with("BEGIN IMMEDIATE").await,
        DatabaseBackend::Postgres => {
            let mut tx = pool.begin().await?;
            if let Ok(reason) = CHANGE_REASON.try_with(Clone::clone) {
                query("SELECT set_config('happyview.change_reason', $1, true)")
                    .bind(reason)
                    .execute(&mut *tx)
                    .await?;
            }
            Ok(tx)
        }
    }
}

tokio::task_local! {
    static CHANGE_REASON: String;
}

/// Run `f` with every [`begin_write`] transaction it opens on Postgres tagged
/// with `reason` in the transaction-local setting `happyview.change_reason`
/// (`purge:<status>`, `backfill`). HappyView never reads it; a trigger on the
/// records table can, to label its change feed. `None` runs `f` untagged.
pub async fn with_change_reason<F: std::future::Future>(reason: Option<String>, f: F) -> F::Output {
    match reason {
        Some(reason) => CHANGE_REASON.scope(reason, f).await,
        None => f.await,
    }
}

/// Parse a database timestamp string to DateTime<Utc>.
/// Handles RFC 3339 (our app writes), Postgres timestamptz format, and SQLite datetime() format.
pub fn parse_dt(s: &str) -> DateTime<Utc> {
    // Try RFC 3339 first (most common - what our app writes)
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return dt.with_timezone(&Utc);
    }
    // Try SQLite datetime() format: "2025-03-16 12:34:56"
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return naive.and_utc();
    }
    // Try Postgres-style with timezone offset: "2025-03-16 12:34:56.123456+00"
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f") {
        return naive.and_utc();
    }
    // Fallback
    tracing::warn!("Failed to parse datetime string: {s}");
    DateTime::UNIX_EPOCH
}

/// Encode a pagination cursor from a timestamp and URI.
pub fn encode_cursor(timestamp: &str, uri: &str) -> String {
    BASE64.encode(format!("{timestamp}|{uri}"))
}

/// Decode a pagination cursor into (timestamp, uri). Returns None if invalid.
pub fn decode_cursor(cursor: &str) -> Option<(String, String)> {
    let decoded = BASE64.decode(cursor).ok()?;
    let s = String::from_utf8(decoded).ok()?;
    let (ts, uri) = s.split_once('|')?;
    Some((ts.to_string(), uri.to_string()))
}

/// Default `journal_size_limit`: 64 MiB. Caps how large the `-wal` file is left
/// after a checkpoint. Without it the WAL stays at its high-water mark forever,
/// so a single large delete permanently inflates disk usage.
pub const DEFAULT_JOURNAL_SIZE_LIMIT: u64 = 67_108_864;

/// Milliseconds a SQLite connection waits on a lock before `SQLITE_BUSY`.
/// sqlx sets the same 5 s on every connection it opens; this names it so a
/// caller that changes it temporarily can put it back.
pub const SQLITE_BUSY_TIMEOUT_MS: u64 = 5000;

/// Parse `SQLITE_JOURNAL_SIZE_LIMIT`. `-1` means "no limit" and is represented
/// as `u64::MAX`; anything unparseable falls back to the default rather than
/// failing boot over a maintenance knob.
pub fn parse_journal_size_limit(raw: Option<&str>) -> u64 {
    match raw {
        None => DEFAULT_JOURNAL_SIZE_LIMIT,
        Some("-1") => u64::MAX,
        Some(s) => s
            .trim()
            .parse::<u64>()
            .unwrap_or(DEFAULT_JOURNAL_SIZE_LIMIT),
    }
}

/// Resolve the configured journal size limit from the environment.
pub fn journal_size_limit_bytes() -> u64 {
    parse_journal_size_limit(crate::config::vars::var("SQLITE_JOURNAL_SIZE_LIMIT").as_deref())
}

/// Parse `SQLITE_SYNCHRONOUS`. `NORMAL` is the default: in WAL mode a crash
/// can lose the last transactions but cannot corrupt the database, and it
/// drops the fsync SQLite's compiled-in `FULL` issues on every commit. `FULL`
/// is there for operators who want the old durability. Anything else falls
/// back to `NORMAL` rather than failing boot over a tuning knob.
pub fn parse_sqlite_synchronous(raw: Option<&str>) -> &'static str {
    match raw.map(str::trim) {
        None | Some("") => "NORMAL",
        Some(v) if v.eq_ignore_ascii_case("NORMAL") => "NORMAL",
        Some(v) if v.eq_ignore_ascii_case("FULL") => "FULL",
        Some(v) => {
            tracing::warn!(
                value = v,
                "SQLITE_SYNCHRONOUS must be NORMAL or FULL; using NORMAL"
            );
            "NORMAL"
        }
    }
}

/// Resolve the configured `synchronous` level from the environment.
pub fn sqlite_synchronous() -> &'static str {
    parse_sqlite_synchronous(crate::config::vars::var("SQLITE_SYNCHRONOUS").as_deref())
}

/// Rows `ANALYZE` samples per index when `PRAGMA optimize` decides to run it.
/// Without a limit the first optimize after an upgrade reads every index of a
/// multi-gigabyte database in full.
pub const SQLITE_ANALYSIS_LIMIT: u32 = 1000;

/// Refresh planner statistics on the tables SQLite judges to need them.
///
/// `0x10002` is `0x02` (run ANALYZE where useful) plus `0x10000` (consider
/// every table, not only those this connection has queried). The startup
/// connection has queried nothing, so without the high bit this is a no-op.
/// Both pragmas must share one connection: `analysis_limit` is per connection.
pub async fn sqlite_optimize(pool: &AnyPool) -> Result<(), sqlx::Error> {
    let mut conn = pool.acquire().await?;
    let limit_sql = adapt_sql(
        &format!("PRAGMA analysis_limit = {SQLITE_ANALYSIS_LIMIT}"),
        DatabaseBackend::Sqlite,
    );
    crate::db::query(&limit_sql).execute(&mut *conn).await?;
    let optimize_sql = adapt_sql("PRAGMA optimize = 0x10002", DatabaseBackend::Sqlite);
    crate::db::query(&optimize_sql).execute(&mut *conn).await?;
    Ok(())
}

/// Apply per-connection SQLite pragmas to every pooled connection.
///
/// `journal_size_limit`, `synchronous` and `foreign_keys` are per-connection
/// and not persisted in the database file, so unlike `journal_mode` they
/// cannot be set once against the pool — that would reach only whichever
/// single connection served the statement.
fn with_sqlite_pragmas(
    opts: PoolOptions<sqlx::Any>,
    backend: DatabaseBackend,
) -> PoolOptions<sqlx::Any> {
    if backend != DatabaseBackend::Sqlite {
        return opts;
    }
    let limit = journal_size_limit_bytes();
    let synchronous = sqlite_synchronous();
    opts.after_connect(move |conn, _meta| {
        Box::pin(async move {
            let sql = if limit == u64::MAX {
                "PRAGMA journal_size_limit = -1".to_string()
            } else {
                format!("PRAGMA journal_size_limit = {limit}")
            };
            let sql = adapt_sql(&sql, DatabaseBackend::Sqlite);
            crate::db::query(&sql).execute(&mut *conn).await?;
            let sync_sql = adapt_sql(
                &format!("PRAGMA synchronous = {synchronous}"),
                DatabaseBackend::Sqlite,
            );
            crate::db::query(&sync_sql).execute(&mut *conn).await?;
            // sqlx's SQLite driver already turns foreign keys on for every
            // connection it opens; set here as well, so `ON DELETE CASCADE`
            // from a record to its refs never rests on that default alone.
            crate::db::query("PRAGMA foreign_keys = ON")
                .execute(&mut *conn)
                .await?;
            Ok(())
        })
    })
}

/// Compiled in rather than read from `./migrations` at boot, so the binary
/// migrates the same wherever it is run from. `build.rs` makes cargo rebuild
/// when a migration file is added.
pub(crate) static SQLITE_MIGRATIONS: Migrator = sqlx::migrate!("./migrations/sqlite");
pub(crate) static POSTGRES_MIGRATIONS: Migrator = sqlx::migrate!("./migrations/postgres");

/// Migrations a stable branch applied under a version alpha had already given
/// to another migration, as `(stable version, alpha version, description)`.
/// See `migrations/renumbered.tsv`.
pub(crate) fn renumbered_migrations() -> Vec<(i64, i64, &'static str)> {
    include_str!("../migrations/renumbered.tsv")
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut fields = line.split('\t');
            let mut version = || {
                fields
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("migrations/renumbered.tsv: a version")
            };
            let (from, to) = (version(), version());
            let description = fields
                .next()
                .expect("migrations/renumbered.tsv: a description");
            (from, to, description)
        })
        .collect()
}

/// Move a database's record of each renumbered migration to the version alpha
/// carries it under, so the migrator finds the file it ran and runs the one
/// alpha keeps at the old version.
///
/// A row is moved only when its description and checksum are those of the file
/// alpha carries at the new version, which is byte for byte the one the stable
/// branch ran, and nothing holds the new version yet. Anything else is left for
/// the migrator to report. The move is one statement, so a boot that stops
/// after it, or any later boot, finds nothing left to move. On Postgres it runs
/// under the migration lock, so two instances booting together can't both act.
async fn renumber_stable_migrations(
    conn: &mut sqlx::AnyConnection,
    backend: DatabaseBackend,
    migrator: &Migrator,
) -> Result<(), sqlx::Error> {
    let exists = match backend {
        DatabaseBackend::Sqlite => {
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'"
        }
        DatabaseBackend::Postgres => {
            "SELECT 1 FROM information_schema.tables \
             WHERE table_schema = current_schema() AND table_name = '_sqlx_migrations'"
        }
    };
    let found: Option<(i32,)> = crate::db::query_as(exists)
        .fetch_optional(&mut *conn)
        .await?;
    if found.is_none() {
        return Ok(());
    }
    for (from, to, description) in renumbered_migrations() {
        let Some(carried) = migrator.iter().find(|m| m.version == to) else {
            continue;
        };
        let sql = adapt_sql(
            "UPDATE _sqlx_migrations SET version = ? \
             WHERE version = ? AND description = ? AND checksum = ? \
             AND NOT EXISTS (SELECT 1 FROM _sqlx_migrations WHERE version = ?)",
            backend,
        );
        let moved = crate::db::query(&sql)
            .bind(to)
            .bind(from)
            .bind(description)
            .bind(carried.checksum.to_vec())
            .bind(to)
            .execute(&mut *conn)
            .await?
            .rows_affected();
        if moved > 0 {
            tracing::info!(
                from,
                to,
                description,
                "moved a stable migration to its alpha version"
            );
        }
    }
    Ok(())
}

/// Install sqlx's `Any` drivers and register the record store's SQLite
/// functions (`record_store::sqlite_functions`), which must happen before
/// the first SQLite connection opens. Call this, not
/// `sqlx::any::install_default_drivers`, before building any pool.
pub fn install_drivers() {
    sqlx::any::install_default_drivers();
    crate::record_store::sqlite_functions::register();
}

/// Why startup could not bring the schema up to date.
#[derive(Debug, thiserror::Error)]
pub enum MigrateFailure {
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error(transparent)]
    Upgrade(#[from] crate::record_store::upgrade::UpgradeError),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// The pool size from `DATABASE_MAX_CONNECTIONS`, else the backend's default.
fn max_connections_from_env(backend: DatabaseBackend) -> u32 {
    crate::config::vars::var("DATABASE_MAX_CONNECTIONS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(match backend {
            DatabaseBackend::Sqlite => 16,
            DatabaseBackend::Postgres => 32,
        })
}

/// Connect to the configured database, run migrations, and move the record
/// tables into the compact format if they are not yet
/// (`record_store::upgrade::run`).
pub async fn connect(url: &str, backend: DatabaseBackend) -> AnyPool {
    connect_with(url, backend, max_connections_from_env(backend)).await
}

/// [`connect`] with the pool size given rather than read from
/// `DATABASE_MAX_CONNECTIONS`.
pub(crate) async fn connect_with(
    url: &str,
    backend: DatabaseBackend,
    max_connections: u32,
) -> AnyPool {
    let pool = open_with(url, backend, max_connections).await;
    migrate(&pool, backend).await;
    pool
}

/// Connect and apply the per-connection settings without migrating, for a
/// caller whose database is a copy of one that already is.
pub async fn open(url: &str, backend: DatabaseBackend) -> AnyPool {
    open_with(url, backend, max_connections_from_env(backend)).await
}

async fn open_with(url: &str, backend: DatabaseBackend, max_connections: u32) -> AnyPool {
    install_drivers();

    // For SQLite, ensure the parent directory exists
    if backend == DatabaseBackend::Sqlite
        && let Some(path) = url.strip_prefix("sqlite://")
    {
        let path = path.split('?').next().unwrap_or(path);
        if let Some(parent) = std::path::Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).unwrap_or_else(|e| {
                panic!("Failed to create data directory {}: {e}", parent.display())
            });
        }
    }

    let pool = with_sqlite_pragmas(
        PoolOptions::<sqlx::Any>::new()
            .max_connections(max_connections)
            .acquire_timeout(std::time::Duration::from_secs(10))
            .idle_timeout(std::time::Duration::from_secs(300)),
        backend,
    )
    .connect(url)
    .await
    .expect("Failed to connect to database");

    // Incremental vacuum, WAL mode and the busy timeout for SQLite
    if backend == DatabaseBackend::Sqlite {
        // Must run before any table exists: on a populated database this is a
        // silent no-op until a full VACUUM rebuilds the file. New instances get
        // incremental vacuum from birth; existing ones need the one-time repair
        // in `maintenance::vacuum`.
        crate::db::query("PRAGMA auto_vacuum = INCREMENTAL")
            .execute(&pool)
            .await
            .expect("Failed to set auto_vacuum");

        crate::db::query("PRAGMA journal_mode = WAL")
            .execute(&pool)
            .await
            .expect("Failed to enable WAL mode");

        crate::db::query(&format!("PRAGMA busy_timeout = {SQLITE_BUSY_TIMEOUT_MS}"))
            .execute(&pool)
            .await
            .expect("Failed to set busy timeout");
    }

    pool
}

/// Apply every migration this build carries, and for SQLite move the record
/// tables into the compact format.
pub async fn migrate(pool: &AnyPool, backend: DatabaseBackend) {
    let migrator = match backend {
        DatabaseBackend::Sqlite => &SQLITE_MIGRATIONS,
        DatabaseBackend::Postgres => &POSTGRES_MIGRATIONS,
    };

    let migrated: Result<(), MigrateFailure> = match backend {
        DatabaseBackend::Sqlite => {
            async {
                renumber_stable_migrations(&mut *pool.acquire().await?, backend, migrator).await?;
                migrator.run(pool).await?;
                crate::record_store::upgrade::run(pool, backend).await?;
                Ok(())
            }
            .await
        }
        DatabaseBackend::Postgres => migrate_postgres(pool, migrator).await,
    };
    migrated.unwrap_or_else(|e| panic!("Failed to run migrations: {e}"));

    if backend == DatabaseBackend::Sqlite
        && let Err(e) = sqlite_optimize(pool).await
    {
        tracing::warn!(error = %e, "PRAGMA optimize after migrations failed");
    }
}

/// The advisory lock HappyView holds over its Postgres migrations.
///
/// sqlx's migrator takes its own lock with `pg_advisory_lock`, which blocks
/// inside a statement, and a blocked statement is a transaction that
/// `CREATE INDEX CONCURRENTLY` waits out. So a second instance booting while
/// the first builds an index waits on the builder's lock while the builder
/// waits on it: Postgres reports a deadlock and one of them fails. Taking this
/// lock first with `pg_try_advisory_lock`, between short sleeps, means a
/// waiting instance never holds a statement open, and by the time it reaches
/// sqlx's lock nobody else holds it.
const PG_MIGRATION_LOCK_KEY: i64 = 0x6876_6d69_6772_6174;

/// How long a booting instance sleeps between tries at the migration lock.
const PG_MIGRATION_LOCK_RETRY: std::time::Duration = std::time::Duration::from_millis(250);

/// How often a waiting instance says it is still waiting. An index build on
/// a large table can hold the lock for a long time, and silence would look
/// like a hang.
const PG_MIGRATION_LOCK_REPORT: std::time::Duration = std::time::Duration::from_secs(60);

/// Indexes a `-- no-transaction` migration builds `CONCURRENTLY`.
///
/// A concurrent build that fails partway leaves an index of that name marked
/// invalid: never read, still written. sqlx records nothing for the failed
/// migration, so it runs again on the next boot, but its `IF NOT EXISTS` would
/// find the invalid index and skip the build. [`migrate_postgres`] drops each
/// of these that is invalid before migrating, so the retry builds it afresh.
const CONCURRENTLY_BUILT_INDEXES: &[&str] = &["idx_records_collection_created_at_uri"];

/// Run `migrator` against Postgres under [`PG_MIGRATION_LOCK_KEY`], after
/// dropping any invalid leftover of [`CONCURRENTLY_BUILT_INDEXES`], then the
/// record store upgrade under the same lock: a second instance booting waits
/// for the copy as it waits for a migration.
async fn migrate_postgres(pool: &AnyPool, migrator: &Migrator) -> Result<(), MigrateFailure> {
    let mut conn = pool.acquire().await?;

    let started = std::time::Instant::now();
    let mut last_report: Option<std::time::Instant> = None;
    loop {
        let sql = adapt_sql("SELECT pg_try_advisory_lock(?)", DatabaseBackend::Postgres);
        let (locked,): (bool,) = crate::db::query_as(&sql)
            .bind(PG_MIGRATION_LOCK_KEY)
            .fetch_one(&mut *conn)
            .await?;
        if locked {
            break;
        }
        if last_report.is_none_or(|at| at.elapsed() >= PG_MIGRATION_LOCK_REPORT) {
            tracing::info!(
                waited_secs = started.elapsed().as_secs(),
                "another instance is running migrations; waiting for it to finish"
            );
            last_report = Some(std::time::Instant::now());
        }
        tokio::time::sleep(PG_MIGRATION_LOCK_RETRY).await;
    }

    let outcome = async {
        for index in CONCURRENTLY_BUILT_INDEXES {
            let sql = adapt_sql(
                "SELECT 1 FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid \
                 WHERE c.relname = ? AND pg_table_is_visible(c.oid) AND NOT i.indisvalid",
                DatabaseBackend::Postgres,
            );
            let invalid: Option<(i32,)> = crate::db::query_as(&sql)
                .bind(*index)
                .fetch_optional(&mut *conn)
                .await?;
            if invalid.is_some() {
                tracing::warn!(
                    index,
                    "dropping an index a failed concurrent build left invalid"
                );
                let sql = adapt_sql(
                    &format!("DROP INDEX CONCURRENTLY IF EXISTS {index}"),
                    DatabaseBackend::Postgres,
                );
                crate::db::query(&sql).execute(&mut *conn).await?;
            }
        }
        // `run` would do, but its `Acquire` bound is not general enough for
        // the compiler to prove `connect`'s future `Send`. `run_direct` is
        // what `run` calls once it has the connection.
        renumber_stable_migrations(&mut conn, DatabaseBackend::Postgres, migrator).await?;
        migrator.run_direct(None, &mut *conn, false).await?;
        // On the lock's own connection: it stays busy (no idle session the
        // server could drop mid-copy), and a one-connection pool suffices.
        crate::record_store::upgrade::run_on(&mut conn, DatabaseBackend::Postgres).await?;
        Ok::<(), MigrateFailure>(())
    }
    .await;

    let sql = adapt_sql("SELECT pg_advisory_unlock(?)", DatabaseBackend::Postgres);
    let unlocked = crate::db::query(&sql)
        .bind(PG_MIGRATION_LOCK_KEY)
        .execute(&mut *conn)
        .await;
    outcome?;
    unlocked?;
    Ok(())
}

/// Extract the filesystem path from a `sqlite://` URL, dropping query params.
/// Returns `None` for non-SQLite URLs and for in-memory databases.
pub fn sqlite_path_from_url(url: &str) -> Option<std::path::PathBuf> {
    let rest = url
        .strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))?;
    let path = rest.split('?').next().unwrap_or(rest);
    if path.is_empty() || path == ":memory:" {
        return None;
    }
    Some(std::path::PathBuf::from(path))
}

/// Upper bound on the computed backfill pool size. SQLite allows one writer,
/// and every open connection is another reader that can keep the WAL from
/// rewinding, so its pool stays small; `BACKFILL_DATABASE_MAX_CONNECTIONS`
/// still overrides it.
pub fn backfill_pool_ceiling(backend: DatabaseBackend) -> u32 {
    match backend {
        DatabaseBackend::Sqlite => 16,
        DatabaseBackend::Postgres => 256,
    }
}

pub fn needed_backfill_connections(pds: u32, dids_per_pds: u32, resolution: u32) -> u32 {
    (pds * dids_per_pds) + resolution + 4
}

pub fn compute_backfill_pool_size(
    backend: DatabaseBackend,
    pds: u32,
    dids_per_pds: u32,
    resolution: u32,
) -> u32 {
    crate::config::vars::var("BACKFILL_DATABASE_MAX_CONNECTIONS")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or_else(|| {
            needed_backfill_connections(pds, dids_per_pds, resolution)
                .min(backfill_pool_ceiling(backend))
        })
        .max(1)
}

/// The backfill pool, sized for the crawl concurrency a job will run with:
/// [`crate::admin::settings::backfill_concurrency`] read from `settings`, the
/// migrated main pool, so a setting saved from the dashboard counts.
pub async fn connect_backfill_pool(
    url: &str,
    backend: DatabaseBackend,
    settings: &AnyPool,
) -> AnyPool {
    install_drivers();

    let (pds, dids, resolution) =
        crate::admin::settings::backfill_concurrency(settings, backend).await;
    let max_connections = compute_backfill_pool_size(backend, pds, dids, resolution);

    tracing::info!(max_connections, "backfill pool sized");

    let pool = with_sqlite_pragmas(
        PoolOptions::<sqlx::Any>::new()
            .max_connections(max_connections)
            .acquire_timeout(std::time::Duration::from_secs(30))
            .idle_timeout(std::time::Duration::from_secs(300)),
        backend,
    )
    .connect(url)
    .await
    .expect("Failed to connect backfill database pool");

    if backend == DatabaseBackend::Sqlite {
        crate::db::query("PRAGMA journal_mode = WAL")
            .execute(&pool)
            .await
            .expect("Failed to enable WAL mode on backfill pool");

        crate::db::query(&format!("PRAGMA busy_timeout = {SQLITE_BUSY_TIMEOUT_MS}"))
            .execute(&pool)
            .await
            .expect("Failed to set busy timeout on backfill pool");
    }

    pool
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;
    use serial_test::serial;

    #[tokio::test]
    async fn open_skips_migrations_and_migrate_applies_them() {
        let dir = std::env::temp_dir().join(format!("hv-db-open-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!("sqlite://{}?mode=rwc", dir.join("a.db").display());
        let jobs_table =
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'happyview_jobs'";

        let pool = open(&url, DatabaseBackend::Sqlite).await;
        let before: Vec<(String,)> = query_as(jobs_table).fetch_all(&pool).await.unwrap();
        assert!(before.is_empty(), "open must not migrate");

        migrate(&pool, DatabaseBackend::Sqlite).await;
        // A second run is what every boot after the first does.
        migrate(&pool, DatabaseBackend::Sqlite).await;
        let after: Vec<(String,)> = query_as(jobs_table).fetch_all(&pool).await.unwrap();
        assert_eq!(after.len(), 1);

        let records: (String,) =
            query_as("SELECT type FROM sqlite_master WHERE name = 'happyview_records'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            records.0, "view",
            "migrate must run the record store upgrade"
        );

        pool.close().await;
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Every migration file in the tree is compiled into the binary, so a
    /// binary run outside its checkout migrates exactly as one run inside it.
    #[test]
    fn embedded_migrations_cover_every_file() {
        for (dir, migrator) in [
            ("migrations/sqlite", &super::SQLITE_MIGRATIONS),
            ("migrations/postgres", &super::POSTGRES_MIGRATIONS),
        ] {
            let on_disk =
                std::fs::read_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(dir))
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter(|e| e.path().extension().is_some_and(|x| x == "sql"))
                    .count();
            assert!(on_disk > 0, "{dir} holds no migrations");
            assert_eq!(migrator.iter().count(), on_disk, "{dir}");
        }
    }

    const CAST_ON_READ: &[&str] = &[
        "service_identity.setup_complete",
        "happyview_jobs.inherit_auth",
    ];

    const UNMAPPABLE_DECLTYPES: &[&str] =
        &["boolean", "bool", "date", "time", "datetime", "timestamp"];

    /// Every index a Postgres migration builds `CONCURRENTLY` is one
    /// `migrate_postgres` knows to repair. A failed build leaves the index
    /// invalid, and one missing from [`CONCURRENTLY_BUILT_INDEXES`] would stay
    /// that way, skipped by its own `IF NOT EXISTS` on every retry.
    #[test]
    fn concurrently_built_indexes_are_all_repairable() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations/postgres");
        let mut found = 0;
        for entry in std::fs::read_dir(&dir).expect("migrations dir") {
            let path = entry.expect("dir entry").path();
            if path.extension().is_none_or(|e| e != "sql") {
                continue;
            }
            let sql = std::fs::read_to_string(&path).expect("read migration");
            let code = sql
                .lines()
                .map(|line| line.split("--").next().unwrap_or(""))
                .collect::<Vec<_>>()
                .join(" ")
                .to_ascii_lowercase();
            let words: Vec<&str> = code.split_whitespace().collect();
            for (i, pair) in words.windows(2).enumerate() {
                if pair != ["index", "concurrently"] || i == 0 {
                    continue;
                }
                if !matches!(words[i - 1], "create" | "unique") {
                    continue;
                }
                let mut rest = words[i + 2..].iter();
                let mut name = rest.next().copied().unwrap_or("");
                if name == "if" {
                    name = rest.nth(2).copied().unwrap_or("");
                }
                let name = name.split('(').next().unwrap_or("");
                assert!(
                    CONCURRENTLY_BUILT_INDEXES.contains(&name),
                    "{} builds {name:?} concurrently; list it in CONCURRENTLY_BUILT_INDEXES",
                    path.display()
                );
                found += 1;
            }
        }
        assert!(found > 0, "found no concurrent index builds to check");
    }

    /// A `-- no-transaction` migration exists to run `CREATE INDEX
    /// CONCURRENTLY`, which Postgres refuses inside a transaction block — and
    /// Postgres runs a string of several statements as one implicit
    /// transaction. So each such file holds exactly one statement.
    #[test]
    fn no_transaction_migrations_hold_exactly_one_statement() {
        for dir in ["migrations/sqlite", "migrations/postgres"] {
            let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
            for entry in std::fs::read_dir(&dir).expect("migrations dir") {
                let path = entry.expect("dir entry").path();
                if path.extension().is_none_or(|e| e != "sql") {
                    continue;
                }
                let sql = std::fs::read_to_string(&path).expect("read migration");
                if !sql.starts_with("-- no-transaction") {
                    continue;
                }
                let code: String = sql
                    .lines()
                    .map(|line| line.split("--").next().unwrap_or(""))
                    .collect::<Vec<_>>()
                    .join("\n");
                let statements = code.split(';').filter(|s| !s.trim().is_empty()).count();
                assert_eq!(
                    statements,
                    1,
                    "{} must hold exactly one statement",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn sqlite_migrations_declare_no_unmappable_column_types() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations/sqlite");
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .expect("migrations/sqlite must exist")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "sql"))
            .collect();
        files.sort();
        assert!(!files.is_empty(), "no SQLite migrations found in {dir:?}");

        let mut offenders = Vec::new();

        for path in &files {
            let sql = std::fs::read_to_string(path).expect("failed to read migration");
            let file = path.file_name().unwrap().to_string_lossy().to_string();
            let mut current_table = String::from("<unknown>");

            for line in sql.lines() {
                let line = line.split("--").next().unwrap_or("").trim();
                if line.is_empty() {
                    continue;
                }
                let lower = line.to_ascii_lowercase();

                // Track the enclosing CREATE TABLE so offenders can be named.
                if let Some(rest) = lower.strip_prefix("create table") {
                    let rest = rest.trim_start_matches(" if not exists").trim();
                    current_table = rest
                        .split(['(', ' '])
                        .find(|s| !s.is_empty())
                        .unwrap_or("<unknown>")
                        .to_string();
                }

                // `ALTER TABLE <t> ADD COLUMN <col> <type>` names its own table.
                let (table, decl) = if let Some(idx) = lower.find("add column") {
                    let table = lower
                        .strip_prefix("alter table")
                        .and_then(|r| r.split_whitespace().next())
                        .unwrap_or("<unknown>")
                        .to_string();
                    (table, line[idx + "add column".len()..].trim())
                } else {
                    (current_table.clone(), line)
                };

                // A column declaration is `<name> <type> ...`; anything whose
                // first token is a keyword (CREATE, PRIMARY, FOREIGN, …) is not.
                let mut tokens = decl.split_whitespace();
                let (Some(name), Some(ty)) = (tokens.next(), tokens.next()) else {
                    continue;
                };
                let ty = ty.trim_end_matches(',').to_ascii_lowercase();
                if !UNMAPPABLE_DECLTYPES.contains(&ty.as_str()) {
                    continue;
                }

                let name = name.trim_matches('"').to_ascii_lowercase();
                let qualified = format!("{table}.{name}");
                if !CAST_ON_READ.contains(&qualified.as_str()) {
                    offenders.push(format!("{file}: {qualified} declared {ty}"));
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "SQLite columns declared with a type sqlx's `Any` driver cannot decode:\n  {}\n\n\
             Any query selecting one of these fails to decode the whole row on SQLite \
             (Postgres is unaffected, so CI will not catch it). Either declare the column \
             INTEGER, or read it as `CAST({{col}} AS INTEGER)` in every query and add it to \
             CAST_ON_READ above.",
            offenders.join("\n  ")
        );
    }

    #[test]
    fn escape_like_escapes_metacharacters() {
        assert_eq!(escape_like("bafyrealcid"), "bafyrealcid"); // unchanged
        assert_eq!(escape_like("%"), "\\%");
        assert_eq!(escape_like("a_b"), "a\\_b");
        assert_eq!(escape_like("a%b_c"), "a\\%b\\_c");
        // Backslash is escaped first so it can't form a spurious escape sequence.
        assert_eq!(escape_like("a\\%"), "a\\\\\\%");
    }

    // -----------------------------------------------------------------------
    // retry_on_busy
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn a_busy_write_is_retried_until_the_lock_clears() {
        install_drivers();
        let path = std::env::temp_dir().join(format!("hv-busy-{}.db", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let holder_pool = PoolOptions::<sqlx::Any>::new()
            .max_connections(1)
            .connect(&url)
            .await
            .expect("connect holder");
        crate::db::query("PRAGMA journal_mode = WAL")
            .execute(&holder_pool)
            .await
            .expect("enable WAL");
        crate::db::query("CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .execute(&holder_pool)
            .await
            .expect("create table");
        // A second pool that never waits on a lock, so a held write lock
        // surfaces as SQLITE_BUSY at once.
        let writer = PoolOptions::<sqlx::Any>::new()
            .max_connections(1)
            .after_connect(|conn, _| {
                Box::pin(async move {
                    crate::db::query("PRAGMA busy_timeout = 0")
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            })
            .connect(&url)
            .await
            .expect("connect writer");

        let mut holder = holder_pool.acquire().await.expect("acquire holder");
        crate::db::query("BEGIN IMMEDIATE")
            .execute(&mut *holder)
            .await
            .expect("take the write lock");

        let err = crate::db::query("INSERT INTO t (id) VALUES (1)")
            .execute(&writer)
            .await
            .expect_err("the write lock is held");
        assert!(is_retryable_write_error(&err), "{err}");
        assert!(
            is_unavailable(&err),
            "a lock still held is unavailable: {err}"
        );

        let release = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            crate::db::query("COMMIT")
                .execute(&mut *holder)
                .await
                .expect("release the write lock");
        });

        retry_on_busy(|| crate::db::query("INSERT INTO t (id) VALUES (1)").execute(&writer))
            .await
            .expect("the retried write lands once the lock clears");
        release.await.expect("release task");

        let dup = crate::db::query("INSERT INTO t (id) VALUES (1)")
            .execute(&writer)
            .await
            .expect_err("duplicate key");
        assert!(
            !is_retryable_write_error(&dup),
            "a constraint failure is final: {dup}"
        );
        assert!(!is_unavailable(&dup), "{dup}");

        drop(writer);
        drop(holder_pool);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    #[tokio::test]
    async fn postgres_serialization_and_deadlock_codes_are_retryable() {
        let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
            crate::test_support::unavailable(crate::test_support::NO_DATABASE);
            return;
        };
        if DatabaseBackend::from_url(&url) != DatabaseBackend::Postgres {
            return;
        }
        let pool = connect(&url, DatabaseBackend::Postgres).await;
        for code in ["40001", "40P01"] {
            let sql =
                format!("DO $$ BEGIN RAISE EXCEPTION 'forced' USING ERRCODE = '{code}'; END $$");
            let err = query(&sql)
                .execute(&pool)
                .await
                .expect_err("the block raises");
            assert!(is_retryable_write_error(&err), "{code}: {err}");
        }
        let err = query("SELECT * FROM happyview_no_such_table_t3")
            .execute(&pool)
            .await
            .expect_err("missing table");
        assert!(!is_retryable_write_error(&err), "{err}");
    }

    #[tokio::test]
    async fn a_read_only_sqlite_database_is_unavailable() {
        install_drivers();
        let path = std::env::temp_dir().join(format!("hv-readonly-{}.db", uuid::Uuid::new_v4()));
        let rw = PoolOptions::<sqlx::Any>::new()
            .max_connections(1)
            .connect(&format!("sqlite://{}?mode=rwc", path.display()))
            .await
            .expect("create");
        query("CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .execute(&rw)
            .await
            .expect("create table");
        rw.close().await;
        let ro = PoolOptions::<sqlx::Any>::new()
            .max_connections(1)
            .connect(&format!("sqlite://{}?mode=ro", path.display()))
            .await
            .expect("open read-only");
        let err = query("INSERT INTO t (id) VALUES (1)")
            .execute(&ro)
            .await
            .expect_err("the database is read-only");
        assert!(is_unavailable(&err), "{err}");
        ro.close().await;
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn a_failing_sqlite_statement_is_not_unavailable() {
        let pool = crate::test_support::memory_pool().await;
        let err = query("SELECT * FROM happyview_no_such_table")
            .execute(&pool)
            .await
            .expect_err("missing table");
        assert!(!is_unavailable(&err), "{err}");
    }

    #[tokio::test]
    async fn postgres_connection_and_shutdown_codes_are_unavailable() {
        let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
            crate::test_support::unavailable(crate::test_support::NO_DATABASE);
            return;
        };
        if DatabaseBackend::from_url(&url) != DatabaseBackend::Postgres {
            return;
        }
        let pool = connect(&url, DatabaseBackend::Postgres).await;
        let raise = |code: &str| {
            format!("DO $$ BEGIN RAISE EXCEPTION 'forced' USING ERRCODE = '{code}'; END $$")
        };
        for code in [
            "08000", "08001", "08003", "08004", "08006", "57P01", "57P03", "53300", "25006",
            "58030", "28P01", "3D000", "40001", "40P01",
        ] {
            let err = query(&raise(code))
                .execute(&pool)
                .await
                .expect_err("the block raises");
            assert!(is_unavailable(&err), "{code}: {err}");
        }
        for code in ["23505", "22P02", "42P01", "57014", "08P01", "22021"] {
            let err = query(&raise(code))
                .execute(&pool)
                .await
                .expect_err("the block raises");
            assert!(!is_unavailable(&err), "{code}: {err}");
        }
        for (code, invalid) in [("22021", true), ("54000", true), ("23505", false)] {
            let err = query(&raise(code))
                .execute(&pool)
                .await
                .expect_err("the block raises");
            assert_eq!(is_invalid_data(&err), invalid, "{code}: {err}");
        }

        // A backend terminated under a live connection, as a restart does.
        let mut conn = pool.acquire().await.expect("acquire");
        let err = query("SELECT pg_terminate_backend(pg_backend_pid())")
            .execute(&mut *conn)
            .await
            .expect_err("the backend is terminated");
        assert!(is_unavailable(&err), "{err:?}");
        conn.detach();
    }

    // -----------------------------------------------------------------------
    // begin_write and the rev-guarded upsert
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn a_sqlite_write_transaction_takes_the_write_lock_when_it_begins() {
        install_drivers();
        let path = std::env::temp_dir().join(format!("hv-begin-write-{}.db", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}?mode=rwc", path.display());
        // Never wait on a lock, so a held write lock surfaces as SQLITE_BUSY.
        let pool = || {
            PoolOptions::<sqlx::Any>::new()
                .max_connections(1)
                .after_connect(|conn, _| {
                    Box::pin(async move {
                        crate::db::query("PRAGMA busy_timeout = 0")
                            .execute(&mut *conn)
                            .await?;
                        Ok(())
                    })
                })
                .connect(&url)
        };
        let first = pool().await.expect("connect first");
        crate::db::query("PRAGMA journal_mode = WAL")
            .execute(&first)
            .await
            .expect("enable WAL");
        let second = pool().await.expect("connect second");

        let held = begin_write(&first, DatabaseBackend::Sqlite)
            .await
            .expect("begin the first write transaction");

        let deferred = second
            .begin()
            .await
            .expect("a deferred BEGIN takes no lock");
        deferred.rollback().await.expect("roll back");
        let err = begin_write(&second, DatabaseBackend::Sqlite)
            .await
            .expect_err("the write lock is already held");
        assert!(is_retryable_write_error(&err), "{err}");

        held.commit().await.expect("release the write lock");
        begin_write(&second, DatabaseBackend::Sqlite)
            .await
            .expect("the lock is free again")
            .commit()
            .await
            .expect("commit");

        drop(first);
        drop(second);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    /// `DATABASE_MAX_CONNECTIONS=1` on Postgres: the record store upgrade
    /// runs on the connection holding the migration lock, so a boot that
    /// has to copy records needs no second one.
    #[tokio::test]
    async fn a_one_connection_postgres_boot_upgrades_the_record_store() {
        let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
            crate::test_support::unavailable(crate::test_support::NO_DATABASE);
            return;
        };
        if DatabaseBackend::from_url(&url) != DatabaseBackend::Postgres {
            return;
        }
        install_drivers();
        let admin = AnyPool::connect(&url)
            .await
            .expect("connect to the test server");
        let name = format!("hv_onecon_{}", uuid::Uuid::new_v4().simple());
        crate::db::query(&format!("CREATE DATABASE {name}"))
            .execute(&admin)
            .await
            .expect("create a scratch database");
        let (server, _) = url.rsplit_once('/').expect("a database path");
        let scratch = format!("{server}/{name}");

        let pool = AnyPool::connect(&scratch).await.expect("connect");
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations/postgres");
        Migrator::new(dir)
            .await
            .expect("load")
            .run(&pool)
            .await
            .expect("migrate to before the upgrade");
        let uri = crate::test_support::seed_record(
            &pool,
            DatabaseBackend::Postgres,
            "did:plc:onecon",
            "app.test.post",
            "r1",
            r#"{"n":1}"#,
            "c1",
            None,
        )
        .await;
        pool.close().await;

        let booted = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            connect_with(&scratch, DatabaseBackend::Postgres, 1),
        )
        .await
        .expect("the boot finishes");
        let got = crate::record_store::RecordStore::new(booted.clone(), DatabaseBackend::Postgres)
            .get(&uri)
            .await
            .expect("get")
            .expect("the record survived the upgrade");
        assert_eq!(got.record, r#"{"n":1}"#);
        booted.close().await;
        let _ = crate::db::query(&format!("DROP DATABASE {name} WITH (FORCE)"))
            .execute(&admin)
            .await;
        admin.close().await;
    }

    #[tokio::test]
    async fn a_write_transaction_begins_and_commits_on_either_backend() {
        for (pool, backend) in crate::test_support::test_pools().await {
            let mut tx = begin_write(&pool, backend).await.expect("begin");
            crate::db::query("SELECT 1")
                .execute(&mut *tx)
                .await
                .expect("query inside the transaction");
            tx.commit().await.expect("commit");
        }
    }

    // -----------------------------------------------------------------------
    // backfill pool sizing
    // -----------------------------------------------------------------------

    #[test]
    fn needed_backfill_connections_formula() {
        assert_eq!(needed_backfill_connections(10, 3, 100), 134);
        assert_eq!(needed_backfill_connections(1, 1, 1), 6);
        assert_eq!(needed_backfill_connections(0, 0, 0), 4);
    }

    #[test]
    fn backfill_pool_ceiling_values() {
        assert_eq!(backfill_pool_ceiling(DatabaseBackend::Sqlite), 16);
        assert_eq!(backfill_pool_ceiling(DatabaseBackend::Postgres), 256);
    }

    #[test]
    fn needed_connections_capped_by_sqlite_ceiling() {
        let needed = needed_backfill_connections(10, 3, 100);
        let capped = needed.min(backfill_pool_ceiling(DatabaseBackend::Sqlite));
        assert_eq!(needed, 134);
        assert_eq!(capped, 16);
    }

    #[test]
    fn needed_connections_capped_by_postgres_ceiling() {
        let needed = needed_backfill_connections(50, 10, 200);
        let capped = needed.min(backfill_pool_ceiling(DatabaseBackend::Postgres));
        assert_eq!(needed, 704);
        assert_eq!(capped, 256);
    }

    #[test]
    fn needed_connections_below_ceiling_unchanged() {
        let needed = needed_backfill_connections(2, 2, 10);
        let capped = needed.min(backfill_pool_ceiling(DatabaseBackend::Postgres));
        assert_eq!(needed, 18);
        assert_eq!(capped, 18);
    }

    #[test]
    fn needed_connections_minimum_is_overhead() {
        let needed = needed_backfill_connections(0, 0, 0);
        assert_eq!(needed, 4);
    }

    // -----------------------------------------------------------------------
    // parse_dt
    // -----------------------------------------------------------------------

    #[test]
    fn parse_dt_rfc3339() {
        let dt = parse_dt("2025-03-16T12:34:56Z");
        assert_eq!(dt.year(), 2025);
        assert_eq!(dt.month(), 3);
    }

    #[test]
    fn parse_dt_sqlite_format() {
        let dt = parse_dt("2025-03-16 12:34:56");
        assert_eq!(dt.year(), 2025);
        assert_eq!(dt.month(), 3);
    }

    // -----------------------------------------------------------------------
    // journal_size_limit
    // -----------------------------------------------------------------------

    #[test]
    fn journal_size_limit_defaults_to_64_mib() {
        assert_eq!(parse_journal_size_limit(None), 67_108_864);
    }

    #[test]
    fn journal_size_limit_parses_env_value() {
        assert_eq!(parse_journal_size_limit(Some("1048576")), 1_048_576);
    }

    #[test]
    fn journal_size_limit_rejects_garbage_and_uses_default() {
        assert_eq!(parse_journal_size_limit(Some("banana")), 67_108_864);
        assert_eq!(parse_journal_size_limit(Some("")), 67_108_864);
    }

    #[test]
    fn journal_size_limit_allows_unlimited_sentinel() {
        // -1 disables the limit; it is the one negative value SQLite accepts.
        assert_eq!(parse_journal_size_limit(Some("-1")), u64::MAX);
    }

    // -----------------------------------------------------------------------
    // sqlite_path_from_url
    // -----------------------------------------------------------------------

    #[test]
    fn sqlite_path_from_url_strips_scheme_and_params() {
        assert_eq!(
            sqlite_path_from_url("sqlite://data/happyview.db?mode=rwc"),
            Some(std::path::PathBuf::from("data/happyview.db"))
        );
        assert_eq!(
            sqlite_path_from_url("sqlite:/var/lib/hv.db"),
            Some(std::path::PathBuf::from("/var/lib/hv.db"))
        );
        assert_eq!(sqlite_path_from_url("postgres://localhost/hv"), None);
        assert_eq!(sqlite_path_from_url("sqlite://:memory:"), None);
    }

    // -----------------------------------------------------------------------
    // with_sqlite_pragmas: journal_size_limit actually reaches pooled
    // connections, not just the one `connect()` happens to touch first.
    // -----------------------------------------------------------------------

    #[tokio::test]
    #[serial]
    async fn journal_size_limit_pragma_reaches_pooled_connections() {
        let path =
            std::env::temp_dir().join(format!("hv-journal-pragma-{}.db", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}?mode=rwc", path.display());

        // SAFETY: this test is `#[serial]`; no other test reads or mutates
        // this variable concurrently.
        unsafe {
            std::env::set_var("SQLITE_JOURNAL_SIZE_LIMIT", "1048576");
        }
        let pool = connect(&url, DatabaseBackend::Sqlite).await;
        unsafe {
            std::env::remove_var("SQLITE_JOURNAL_SIZE_LIMIT");
        }

        // Hold several connections open simultaneously so the pool has to
        // actually establish more than one, proving `after_connect` runs on
        // each rather than on a single lucky connection reused by every
        // acquire.
        let mut conns = Vec::new();
        for _ in 0..4 {
            conns.push(pool.acquire().await.expect("failed to acquire connection"));
        }

        for mut conn in conns {
            let (limit,): (i64,) = crate::db::query_as("PRAGMA journal_size_limit")
                .fetch_one(&mut *conn)
                .await
                .expect("failed to read journal_size_limit");
            assert_eq!(
                limit, 1_048_576,
                "journal_size_limit pragma did not reach a pooled connection"
            );
        }

        drop(pool);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    // -----------------------------------------------------------------------
    // synchronous
    // -----------------------------------------------------------------------

    #[test]
    fn sqlite_synchronous_defaults_to_normal() {
        assert_eq!(parse_sqlite_synchronous(None), "NORMAL");
        assert_eq!(parse_sqlite_synchronous(Some("")), "NORMAL");
        assert_eq!(parse_sqlite_synchronous(Some("normal")), "NORMAL");
    }

    #[test]
    fn sqlite_synchronous_allows_full() {
        assert_eq!(parse_sqlite_synchronous(Some("FULL")), "FULL");
        assert_eq!(parse_sqlite_synchronous(Some(" full ")), "FULL");
    }

    #[test]
    fn sqlite_synchronous_rejects_other_levels() {
        // OFF can corrupt the database on power loss and EXTRA buys nothing
        // over FULL in WAL mode, so neither is offered.
        assert_eq!(parse_sqlite_synchronous(Some("OFF")), "NORMAL");
        assert_eq!(parse_sqlite_synchronous(Some("banana")), "NORMAL");
    }

    async fn synchronous_on_every_connection(env_value: Option<&str>) -> Vec<i64> {
        let path = std::env::temp_dir().join(format!("hv-sync-{}.db", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}?mode=rwc", path.display());

        // SAFETY: callers are `#[serial]`; nothing else reads or writes this
        // variable concurrently.
        unsafe {
            match env_value {
                Some(v) => std::env::set_var("SQLITE_SYNCHRONOUS", v),
                None => std::env::remove_var("SQLITE_SYNCHRONOUS"),
            }
        }
        let pool = connect(&url, DatabaseBackend::Sqlite).await;
        unsafe {
            std::env::remove_var("SQLITE_SYNCHRONOUS");
        }

        let mut conns = Vec::new();
        for _ in 0..4 {
            conns.push(pool.acquire().await.expect("failed to acquire connection"));
        }
        let mut levels = Vec::new();
        for mut conn in conns {
            let (level,): (i64,) = crate::db::query_as("PRAGMA synchronous")
                .fetch_one(&mut *conn)
                .await
                .expect("failed to read synchronous");
            levels.push(level);
        }

        drop(pool);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
        levels
    }

    /// `PRAGMA synchronous` reads back 1 for NORMAL and 2 for FULL.
    #[tokio::test]
    #[serial]
    async fn synchronous_normal_reaches_pooled_connections() {
        assert_eq!(
            synchronous_on_every_connection(None).await,
            vec![1, 1, 1, 1]
        );
    }

    #[tokio::test]
    #[serial]
    async fn synchronous_full_reaches_pooled_connections() {
        assert_eq!(
            synchronous_on_every_connection(Some("FULL")).await,
            vec![2, 2, 2, 2]
        );
    }

    /// The pool is sized for the concurrency a crawl will read, which is the
    /// instance setting when there is one: an operator who raised it from
    /// the dashboard gets a pool to match at the next start.
    #[tokio::test]
    #[serial]
    async fn the_backfill_pool_is_sized_from_the_instance_settings() {
        let path = std::env::temp_dir().join(format!("hv-bf-size-{}.db", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let main = connect(&url, DatabaseBackend::Sqlite).await;
        let size = |pool: &AnyPool| pool.options().get_max_connections();

        let defaults = connect_backfill_pool(&url, DatabaseBackend::Sqlite, &main).await;
        assert_eq!(
            size(&defaults),
            16,
            "10 × 3 + 100 + 4, held to SQLite's ceiling"
        );

        for (key, value) in [
            ("backfill_concurrent_pds", "2"),
            ("backfill_concurrent_dids_per_pds", "2"),
            ("backfill_concurrent_resolution", "3"),
        ] {
            crate::admin::settings::put_setting(&main, key, value, DatabaseBackend::Sqlite)
                .await
                .unwrap();
        }
        let sized = connect_backfill_pool(&url, DatabaseBackend::Sqlite, &main).await;
        assert_eq!(size(&sized), 11, "2 × 2 + 3 + 4, from the stored settings");

        defaults.close().await;
        sized.close().await;
        main.close().await;
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    #[serial]
    async fn synchronous_normal_reaches_backfill_pool_connections() {
        let path = std::env::temp_dir().join(format!("hv-bf-sync-{}.db", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}?mode=rwc", path.display());

        // SAFETY: callers are `#[serial]`; nothing else reads or writes this
        // variable concurrently.
        unsafe {
            std::env::remove_var("SQLITE_SYNCHRONOUS");
        }
        let settings = crate::test_support::migrated_memory_pool().await;
        let pool = connect_backfill_pool(&url, DatabaseBackend::Sqlite, &settings).await;

        let mut conns = Vec::new();
        for _ in 0..4 {
            conns.push(pool.acquire().await.expect("failed to acquire connection"));
        }
        for mut conn in conns {
            let (level,): (i64,) = crate::db::query_as("PRAGMA synchronous")
                .fetch_one(&mut *conn)
                .await
                .expect("failed to read synchronous");
            assert_eq!(level, 1);
        }

        drop(pool);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    /// `PRAGMA optimize` with the "every table" bit analyzes a table this
    /// connection never queried, which is the startup case.
    #[tokio::test]
    #[serial]
    async fn optimize_writes_planner_statistics() {
        let path = std::env::temp_dir().join(format!("hv-optimize-{}.db", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = connect(&url, DatabaseBackend::Sqlite).await;

        for i in 0..200 {
            crate::db::query(
                "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at) \
                 VALUES (?, 'did:plc:opt', 'app.test.post', ?, '{}', 'bafyreitestcid', '2026-01-01T00:00:00+00:00', '2026-01-01T00:00:00+00:00')",
            )
            .bind(format!("at://did:plc:opt/app.test.post/{i}"))
            .bind(i.to_string())
            .execute(&pool)
            .await
            .expect("seed record");
        }

        sqlite_optimize(&pool).await.expect("optimize");

        let (rows,): (i64,) = crate::db::query_as(
            "SELECT COUNT(*) FROM sqlite_stat1 WHERE tbl = 'happyview_records_v2'",
        )
        .fetch_one(&pool)
        .await
        .expect("sqlite_stat1 should exist after optimize");
        assert!(rows > 0, "optimize did not analyze happyview_records_v2");

        drop(pool);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    // -----------------------------------------------------------------------
    // pool idle count
    // -----------------------------------------------------------------------

    // sqlx-core 0.9.0 made a released connection acquirable before counting it
    // idle, so a concurrent acquire could take it first and wrap `num_idle` to
    // `usize::MAX`. The pool's maintenance task loops over `0..num_idle()`
    // without yielding, which pinned a worker thread at 100% CPU. On 0.9.0
    // this sees the wrapped count within a millisecond.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn the_pool_idle_count_never_wraps() {
        const MAX: usize = 3;
        install_drivers();
        let pool = PoolOptions::<sqlx::Any>::new()
            .max_connections(MAX as u32)
            .connect("sqlite::memory:")
            .await
            .expect("connect");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let tasks: Vec<_> = (0..24)
            .map(|_| {
                let pool = pool.clone();
                tokio::spawn(async move {
                    let mut most = 0;
                    while most <= MAX && std::time::Instant::now() < deadline {
                        match pool.try_acquire() {
                            Some(conn) => drop(conn),
                            None => tokio::task::yield_now().await,
                        }
                        most = most.max(pool.num_idle());
                    }
                    most
                })
            })
            .collect();
        for task in tasks {
            let most = task.await.expect("task");
            assert!(
                most <= MAX,
                "num_idle reached {most} with {MAX} connections"
            );
        }
    }
}

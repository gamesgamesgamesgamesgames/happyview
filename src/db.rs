use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use chrono::{DateTime, Utc};
use regex::Regex;
use serde::Deserialize;
use sqlx::AnyPool;
use sqlx::any::{AnyArguments, AnyRow};
use sqlx::migrate::Migrator;
use sqlx::pool::PoolOptions;
use sqlx::query::{Query, QueryAs, QueryScalar};
use sqlx::{Any, AssertSqlSafe, FromRow};
use sqlx::{query as sqlx_query, query_as as sqlx_query_as, query_scalar as sqlx_query_scalar};
use std::path::Path;
use std::sync::LazyLock;

/// Bind value for `happyview_records.indexed_at` on **local** write paths.
///
/// `indexed_at` records when a record arrived *from the network*, so only the
/// Jetstream consumer (`record_handler`) and backfill may write it. Everything
/// else — `Record:save()`, `save_local()`, the built-in XRPC procedure
/// handlers, linked-repo writes — binds this on insert and omits the column
/// from its `ON CONFLICT` branch, so a real arrival time survives an
/// AppView-side edit.
///
/// It has to be an explicit NULL rather than an omitted column: SQLite still
/// declares `indexed_at TEXT DEFAULT (datetime('now'))` while Postgres dropped
/// its default in `20260213000000_add_created_at.sql`, so leaving the column out
/// would fabricate an arrival time on one backend and not the other.
pub const NO_INDEXED_AT: Option<&str> = None;

// ---------------------------------------------------------------------------
// SQL query helpers (sqlx 0.9 `SqlSafeStr`)
// ---------------------------------------------------------------------------
//
// sqlx 0.9 requires the SQL passed to `query*` to implement `SqlSafeStr`, which
// only `&'static str` satisfies directly — runtime strings must be wrapped in
// `AssertSqlSafe`. Almost every query here is a static SQLite template run
// through `adapt_sql`, where no user-supplied *value* is concatenated, since
// values vary only as bound `?` placeholders. Two kinds of exception make the
// assertion a claim about validation rather than about the absence of
// interpolation:
//
//   * Identifiers and JSON field paths are interpolated, because `adapt_sql`
//     rewrites SQL text and cannot reach a bound path. Each passes a
//     validating helper first — `plugin::host::records::is_valid_identifier`
//     and this module's `is_valid_json_field_path`, which admit only letters,
//     digits, underscore, dot and a bracketed numeric index — and
//     `records.rs` is the only module that interpolates caller-controlled
//     text at all.
//   * `plugin::host::db`'s `run_query` and `run_execute` pass a plugin's
//     entire statement through untranslated. There is no template there: what
//     stands in for one is `raw_sql_guard::check_raw_sql_tables` plus the
//     `database:read`/`database:write` capability.
//
// Routing all dynamic queries through these three helpers keeps that assertion
// in one audited place instead of at ~460 call sites. Backend is always `Any`.

/// `sqlx::query` for a runtime-built (adapted) SQL string.
pub fn query<'q>(sql: &str) -> Query<'q, Any, AnyArguments> {
    sqlx_query(AssertSqlSafe(sql.to_owned()))
}

/// `sqlx::query_as` for a runtime-built (adapted) SQL string.
pub fn query_as<'q, O>(sql: &str) -> QueryAs<'q, Any, O, AnyArguments>
where
    O: for<'r> FromRow<'r, AnyRow>,
{
    sqlx_query_as(AssertSqlSafe(sql.to_owned()))
}

/// `sqlx::query_scalar` for a runtime-built (adapted) SQL string.
pub fn query_scalar<'q, O>(sql: &str) -> QueryScalar<'q, Any, O, AnyArguments>
where
    (O,): for<'r> FromRow<'r, AnyRow>,
{
    sqlx_query_scalar(AssertSqlSafe(sql.to_owned()))
}

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

/// Attempts `retry_on_busy` makes before giving the error back.
pub const BUSY_RETRY_ATTEMPTS: u32 = 3;

/// Whether a write failed only because another writer held the lock, which a
/// short wait can fix: SQLite `BUSY`/`LOCKED` under any extended code once the
/// busy timeout has run out, and Postgres serialization failures and
/// deadlocks. Everything else is final.
pub fn is_retryable_write_error(e: &sqlx::Error) -> bool {
    let Some(db) = e.as_database_error() else {
        return false;
    };
    let Some(code) = db.code() else {
        return false;
    };
    if db.try_downcast_ref::<sqlx::sqlite::SqliteError>().is_some() {
        // Extended result codes keep the primary code in the low byte.
        return code.parse::<i32>().is_ok_and(|c| matches!(c & 0xff, 5 | 6));
    }
    matches!(code.as_ref(), "40001" | "40P01")
}

/// Run a write, retrying up to `BUSY_RETRY_ATTEMPTS` times with backoff while
/// it fails retryably. The final error is returned for the caller to log;
/// nothing is swallowed here.
pub async fn retry_on_busy<T, F, Fut>(mut op: F) -> Result<T, sqlx::Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, sqlx::Error>>,
{
    let mut attempt: u32 = 1;
    loop {
        match op().await {
            Err(e) if attempt < BUSY_RETRY_ATTEMPTS && is_retryable_write_error(&e) => {
                tokio::time::sleep(std::time::Duration::from_millis(50 << attempt)).await;
                attempt += 1;
            }
            other => return other,
        }
    }
}

/// The `WHERE` of `ON CONFLICT (uri) DO UPDATE`, so an upsert leaves a row
/// alone when neither its CID nor its body changed. That avoids rewriting the
/// row and its indexes, and keeps its original `indexed_at`. A stored row with
/// no `indexed_at` (one written locally, before the network echoed it) always
/// updates, so the echo stamps it.
pub fn record_changed_clause(backend: DatabaseBackend) -> &'static str {
    match backend {
        DatabaseBackend::Sqlite => {
            "happyview_records.cid IS NOT excluded.cid OR happyview_records.record IS NOT excluded.record OR happyview_records.indexed_at IS NULL"
        }
        DatabaseBackend::Postgres => {
            "happyview_records.cid IS DISTINCT FROM excluded.cid OR happyview_records.record IS DISTINCT FROM excluded.record OR happyview_records.indexed_at IS NULL"
        }
    }
}

/// Database backend type, auto-detected from DATABASE_URL or set via DATABASE_BACKEND.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DatabaseBackend {
    Sqlite,
    Postgres,
}

impl DatabaseBackend {
    /// Detect backend from DATABASE_URL prefix.
    pub fn from_url(url: &str) -> Self {
        if url.starts_with("sqlite://") || url.starts_with("sqlite:") {
            DatabaseBackend::Sqlite
        } else {
            DatabaseBackend::Postgres
        }
    }

    /// Parse from string (e.g., from DATABASE_BACKEND env var).
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "sqlite" => Some(DatabaseBackend::Sqlite),
            "postgres" | "postgresql" => Some(DatabaseBackend::Postgres),
            _ => None,
        }
    }
}

/// Regex matching `json_extract(col, '$.path.to.leaf')`
/// Captures: (1) column name, (2) the JSON path after `$.`
static JSON_EXTRACT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"json_extract\((\w+(?:\.\w+)*),\s*'\$\.([^']+)'\)").unwrap());

/// Regex matching `datetime('now', '±N unit')`
/// Captures: (1) sign (+/-), (2) the interval value e.g. "7 days"
static DATETIME_INTERVAL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"datetime\('now',\s*'([+-])(\d+\s+[^']+)'\)").unwrap());

/// Regex matching bare `datetime('now')`
static DATETIME_NOW_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"datetime\('now'\)").unwrap());

/// Convert SQL written in SQLite dialect to work on the target backend.
///
/// Source SQL uses SQLite syntax:
/// - `?` placeholders
/// - `json_extract(col, '$.path')` for JSON access
/// - `datetime('now')` / `datetime('now', '±N unit')` for timestamps
/// - `LIKE` for case-insensitive matching
/// - `0`/`1` for booleans
///
/// For Postgres, converts to:
/// - `$1, $2, $3...` numbered placeholders
/// - `col::jsonb->'seg1'->'seg2'->>'leaf'` JSON chains
/// - `NOW()` / `NOW() ± INTERVAL 'N unit'`
/// - `LIKE` stays as-is (works on both)
/// - `0`/`1` stays as-is (works on both)
pub fn adapt_sql(sql: &str, backend: DatabaseBackend) -> String {
    match backend {
        DatabaseBackend::Sqlite => {
            // Source is already SQLite — no-op
            sql.to_string()
        }
        DatabaseBackend::Postgres => {
            let mut result = sql.to_string();

            // 1. json_extract → Postgres JSON chain
            result = adapt_json_extract_to_postgres(&result);

            // 2. datetime('now', '±N unit') → NOW() ± INTERVAL 'N unit'
            //    Must run before bare datetime('now') replacement.
            result = DATETIME_INTERVAL_RE
                .replace_all(&result, |caps: &regex::Captures| {
                    let sign = &caps[1];
                    let interval = &caps[2];
                    format!("NOW() {sign} INTERVAL '{interval}'")
                })
                .to_string();

            // 3. datetime('now') → NOW()
            result = DATETIME_NOW_RE.replace_all(&result, "NOW()").to_string();

            // 4. ? → $1, $2, $3... (quote-aware)
            result = adapt_placeholders_to_postgres(&result);

            result
        }
    }
}

/// The longest JSON field path [`is_valid_json_field_path`] accepts. A path
/// is interpolated, and one caller repeats it five times in a statement while
/// the Postgres rewrite expands every segment, so an unbounded path lets a
/// caller turn its own input into orders of magnitude more SQL.
pub const MAX_FIELD_PATH_LEN: usize = 512;

/// A JSON field path as used in a record filter, sort or search:
/// dot-separated identifier segments with optional numeric array indices
/// (`author.handle`, `tags[0]`).
///
/// This is the guard that makes interpolating a path sound, and it lives
/// beside [`postgres_json_chain`], the thing that interpolates: a validator
/// far from its interpolation is a validator someone removes.
pub fn is_valid_json_field_path(path: &str) -> bool {
    if path.is_empty() || path.len() > MAX_FIELD_PATH_LEN {
        return false;
    }
    for segment in path.split('.') {
        if segment.is_empty() {
            return false;
        }
        let bracket_start = segment.find('[').unwrap_or(segment.len());
        let ident = &segment[..bracket_start];
        if ident.is_empty() || !ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return false;
        }
        let mut rest = &segment[bracket_start..];
        while !rest.is_empty() {
            if !rest.starts_with('[') {
                return false;
            }
            let close = match rest.find(']') {
                Some(i) => i,
                None => return false,
            };
            let idx = &rest[1..close];
            if idx.is_empty() || !idx.chars().all(|c| c.is_ascii_digit()) {
                return false;
            }
            // Postgres's `->` takes an `int4` subscript and refuses a wider
            // one, where SQLite reads an out-of-range index as no match. An
            // index neither backend can address is not a path either answers.
            if idx.parse::<i32>().is_err() {
                return false;
            }
            rest = &rest[close + 1..];
        }
    }
    true
}

/// The Postgres access chain for a JSON path inside `col`: one arrow per path
/// segment, with `leaf` as the last. `->>` yields the leaf as text, `->` as
/// `jsonb`, whose comparison and ordering follow the JSON type rather than
/// the text.
///
/// The only place such a chain is built. Two constructions of it drifted
/// apart while both were correct for every input they saw — one broke out of
/// an unterminated bracket, the other never checked for one — and a
/// difference no caller can reach is a difference no reader will notice.
///
/// Total, not validating: `adapt_sql` runs over statements this module did
/// not build, so the walk has to terminate on any input. Callers that
/// interpolate a caller-controlled path guard it with
/// [`is_valid_json_field_path`] first.
pub fn postgres_json_chain(col: &str, path: &str, leaf: &str) -> String {
    let mut parts: Vec<(String, bool)> = Vec::new();
    for segment in path.split('.') {
        let bracket_start = segment.find('[').unwrap_or(segment.len());
        let field_name = &segment[..bracket_start];
        if !field_name.is_empty() {
            parts.push((field_name.to_string(), false));
        }
        let mut rest = &segment[bracket_start..];
        while rest.starts_with('[') {
            if let Some(close) = rest.find(']') {
                parts.push((rest[1..close].to_string(), true));
                rest = &rest[close + 1..];
            } else {
                break;
            }
        }
    }

    let mut chain = format!("{col}::jsonb");
    let last = parts.len().saturating_sub(1);
    for (i, (text, is_index)) in parts.iter().enumerate() {
        let arrow = if i == last { leaf } else { "->" };
        if *is_index {
            chain.push_str(&format!("{arrow}{text}"));
        } else {
            chain.push_str(&format!("{arrow}'{text}'"));
        }
    }
    chain
}

/// Convert `json_extract(col, '$.seg1.seg2.leaf')` to Postgres `col::jsonb->'seg1'->'seg2'->>'leaf'`.
/// Handles array indices: `seg[0].leaf` becomes `->seg->0->>'leaf'`.
///
/// Terminating in `->>` is what a rewritten `json_extract` means: its readers
/// want the value as text. A caller wanting the `jsonb` value calls
/// [`postgres_json_chain`] with `->` instead.
fn adapt_json_extract_to_postgres(sql: &str) -> String {
    JSON_EXTRACT_RE
        .replace_all(sql, |caps: &regex::Captures| {
            postgres_json_chain(&caps[1], &caps[2], "->>")
        })
        .to_string()
}

/// Convert `?` placeholders to `$1, $2, $3...` for Postgres, skipping `?` inside single-quoted strings.
fn adapt_placeholders_to_postgres(sql: &str) -> String {
    let mut result = String::with_capacity(sql.len());
    let mut counter = 0u32;
    let mut in_string = false;

    let chars: Vec<char> = sql.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            if in_string {
                // Check for escaped quote ''
                if i + 1 < chars.len() && chars[i + 1] == '\'' {
                    result.push('\'');
                    result.push('\'');
                    i += 2;
                    continue;
                }
                in_string = false;
            } else {
                in_string = true;
            }
            result.push(c);
        } else if c == '?' && !in_string {
            counter += 1;
            result.push('$');
            result.push_str(&counter.to_string());
        } else {
            result.push(c);
        }
        i += 1;
    }

    result
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

/// Get current UTC time as RFC 3339 string for database binding.
pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
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
    parse_journal_size_limit(std::env::var("SQLITE_JOURNAL_SIZE_LIMIT").ok().as_deref())
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
    parse_sqlite_synchronous(std::env::var("SQLITE_SYNCHRONOUS").ok().as_deref())
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
/// `journal_size_limit` and `synchronous` are per-connection and not persisted
/// in the database file, so unlike `journal_mode` they cannot be set once
/// against the pool — that would reach only whichever single connection served
/// the statement.
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
            Ok(())
        })
    })
}

/// Connect to the configured database and run migrations.
pub async fn connect(url: &str, backend: DatabaseBackend) -> AnyPool {
    sqlx::any::install_default_drivers();

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

    let max_connections = std::env::var("DATABASE_MAX_CONNECTIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(match backend {
            DatabaseBackend::Sqlite => 16,
            DatabaseBackend::Postgres => 32,
        });

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

    // Enable foreign keys and WAL mode for SQLite
    if backend == DatabaseBackend::Sqlite {
        // Must run before any table exists: on a populated database this is a
        // silent no-op until a full VACUUM rebuilds the file. New instances get
        // incremental vacuum from birth; existing ones need the one-time repair
        // in `maintenance::vacuum`.
        crate::db::query("PRAGMA auto_vacuum = INCREMENTAL")
            .execute(&pool)
            .await
            .expect("Failed to set auto_vacuum");

        crate::db::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await
            .expect("Failed to enable foreign keys");

        crate::db::query("PRAGMA journal_mode = WAL")
            .execute(&pool)
            .await
            .expect("Failed to enable WAL mode");

        crate::db::query(&format!("PRAGMA busy_timeout = {SQLITE_BUSY_TIMEOUT_MS}"))
            .execute(&pool)
            .await
            .expect("Failed to set busy timeout");
    }

    // Run migrations from the appropriate directory
    let migration_dir = match backend {
        DatabaseBackend::Sqlite => "./migrations/sqlite",
        DatabaseBackend::Postgres => "./migrations/postgres",
    };

    let migrator = Migrator::new(Path::new(migration_dir))
        .await
        .unwrap_or_else(|e| panic!("Failed to load migrations from {migration_dir}: {e}"));

    migrator.run(&pool).await.expect("Failed to run migrations");

    if backend == DatabaseBackend::Sqlite
        && let Err(e) = sqlite_optimize(&pool).await
    {
        tracing::warn!(error = %e, "PRAGMA optimize after migrations failed");
    }

    pool
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
    std::env::var("BACKFILL_DATABASE_MAX_CONNECTIONS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or_else(|| {
            needed_backfill_connections(pds, dids_per_pds, resolution)
                .min(backfill_pool_ceiling(backend))
        })
        .max(1)
}

pub async fn connect_backfill_pool(url: &str, backend: DatabaseBackend) -> AnyPool {
    let pds: u32 = std::env::var("BACKFILL_CONCURRENT_PDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let dids: u32 = std::env::var("BACKFILL_CONCURRENT_DIDS_PER_PDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let resolution: u32 = std::env::var("BACKFILL_CONCURRENT_RESOLUTION")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
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
        crate::db::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await
            .expect("Failed to enable foreign keys on backfill pool");

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

    #[test]
    fn json_field_paths() {
        assert!(is_valid_json_field_path("name"));
        assert!(is_valid_json_field_path("author_name"));
        assert!(is_valid_json_field_path("author.handle"));
        assert!(is_valid_json_field_path("tags[0]"));
        assert!(is_valid_json_field_path("data[0][1]"));
        assert!(is_valid_json_field_path("author.websites[0].url"));
        assert!(is_valid_json_field_path("a.b.c.d.e"));
        assert!(!is_valid_json_field_path(""));
        assert!(!is_valid_json_field_path(".name"));
        assert!(!is_valid_json_field_path("name."));
        assert!(!is_valid_json_field_path("name..foo"));
        assert!(!is_valid_json_field_path("a..b"));
        assert!(!is_valid_json_field_path("[0]"));
        assert!(!is_valid_json_field_path("name[]"));
        assert!(!is_valid_json_field_path("name[abc]"));
        assert!(!is_valid_json_field_path("tags[x]"));
        assert!(!is_valid_json_field_path("name; DROP TABLE"));
        assert!(!is_valid_json_field_path("name'OR 1=1"));
        assert!(!is_valid_json_field_path("a'b"));
        assert!(!is_valid_json_field_path("na-me"));
        // The bounds that keep a path addressable on both backends.
        assert!(!is_valid_json_field_path("tags[3000000000]"));
        assert!(is_valid_json_field_path("tags[2147483647]"));
        assert!(!is_valid_json_field_path(
            &"a".repeat(MAX_FIELD_PATH_LEN + 1)
        ));
        assert!(is_valid_json_field_path(&"a".repeat(MAX_FIELD_PATH_LEN)));
    }

    /// The one walk, at both leaves. A path's segments and indices render the
    /// same either way; only the final arrow differs, which is the whole
    /// reason two callers need it.
    #[test]
    fn a_json_path_renders_one_chain_with_either_leaf() {
        assert_eq!(
            postgres_json_chain("record", "author.handle", "->>"),
            "record::jsonb->'author'->>'handle'"
        );
        assert_eq!(
            postgres_json_chain("record", "author.handle", "->"),
            "record::jsonb->'author'->'handle'"
        );
        assert_eq!(
            postgres_json_chain("record", "tags[0]", "->>"),
            "record::jsonb->'tags'->>0"
        );
        assert_eq!(postgres_json_chain("r", "n", "->"), "r::jsonb->'n'");
    }

    /// The walk terminates on input no validator passed, because `adapt_sql`
    /// runs over statements this module did not build.
    #[test]
    fn the_chain_walk_terminates_on_an_unterminated_bracket() {
        assert_eq!(
            postgres_json_chain("record", "tags[0", "->>"),
            "record::jsonb->>'tags'"
        );
    }

    const CAST_ON_READ: &[&str] = &[
        "service_identity.setup_complete",
        "happyview_jobs.inherit_auth",
    ];

    const UNMAPPABLE_DECLTYPES: &[&str] =
        &["boolean", "bool", "date", "time", "datetime", "timestamp"];

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
        sqlx::any::install_default_drivers();
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

        drop(writer);
        drop(holder_pool);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    // -----------------------------------------------------------------------
    // DatabaseBackend detection
    // -----------------------------------------------------------------------

    #[test]
    fn backend_from_url_detects_sqlite() {
        assert_eq!(
            DatabaseBackend::from_url("sqlite://data/happyview.db"),
            DatabaseBackend::Sqlite
        );
        assert_eq!(
            DatabaseBackend::from_url("sqlite:data/happyview.db?mode=rwc"),
            DatabaseBackend::Sqlite
        );
    }

    #[test]
    fn backend_from_url_detects_postgres() {
        assert_eq!(
            DatabaseBackend::from_url("postgres://localhost/happyview"),
            DatabaseBackend::Postgres
        );
        assert_eq!(
            DatabaseBackend::from_url("postgresql://user:pass@host/db"),
            DatabaseBackend::Postgres
        );
    }

    #[test]
    fn backend_from_str_parses() {
        assert_eq!(
            DatabaseBackend::from_str("sqlite"),
            Some(DatabaseBackend::Sqlite)
        );
        assert_eq!(
            DatabaseBackend::from_str("POSTGRES"),
            Some(DatabaseBackend::Postgres)
        );
        assert_eq!(
            DatabaseBackend::from_str("postgresql"),
            Some(DatabaseBackend::Postgres)
        );
        assert_eq!(DatabaseBackend::from_str("invalid"), None);
    }

    // -----------------------------------------------------------------------
    // adapt_sql: placeholder conversion
    // -----------------------------------------------------------------------

    #[test]
    fn adapt_sql_sqlite_keeps_placeholders() {
        let sql = "SELECT * FROM foo WHERE id = ? AND name = ?";
        assert_eq!(
            adapt_sql(sql, DatabaseBackend::Sqlite),
            "SELECT * FROM foo WHERE id = ? AND name = ?"
        );
    }

    #[test]
    fn adapt_sql_postgres_converts_placeholders() {
        let sql = "SELECT * FROM foo WHERE id = ? AND name = ?";
        assert_eq!(
            adapt_sql(sql, DatabaseBackend::Postgres),
            "SELECT * FROM foo WHERE id = $1 AND name = $2"
        );
    }

    #[test]
    fn adapt_sql_postgres_handles_many_placeholders() {
        let sql = "INSERT INTO t VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";
        let result = adapt_sql(sql, DatabaseBackend::Postgres);
        assert_eq!(
            result,
            "INSERT INTO t VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"
        );
    }

    #[test]
    fn adapt_sql_postgres_skips_question_marks_in_strings() {
        let sql = "SELECT * FROM foo WHERE name = ? AND note LIKE '??%'";
        assert_eq!(
            adapt_sql(sql, DatabaseBackend::Postgres),
            "SELECT * FROM foo WHERE name = $1 AND note LIKE '??%'"
        );
    }

    // -----------------------------------------------------------------------
    // adapt_sql: JSON operator conversion
    // -----------------------------------------------------------------------

    #[test]
    fn adapt_sql_sqlite_keeps_json_extract() {
        let sql = "SELECT json_extract(record, '$.title') FROM records";
        assert_eq!(
            adapt_sql(sql, DatabaseBackend::Sqlite),
            "SELECT json_extract(record, '$.title') FROM records"
        );
    }

    #[test]
    fn adapt_sql_postgres_converts_simple_json_extract() {
        let sql = "SELECT json_extract(record, '$.title') FROM records WHERE collection = ?";
        assert_eq!(
            adapt_sql(sql, DatabaseBackend::Postgres),
            "SELECT record::jsonb->>'title' FROM records WHERE collection = $1"
        );
    }

    #[test]
    fn adapt_sql_postgres_converts_chained_json_extract() {
        let sql = "WHERE json_extract(lexicon_json, '$.defs.main.type') = 'record'";
        assert_eq!(
            adapt_sql(sql, DatabaseBackend::Postgres),
            "WHERE lexicon_json::jsonb->'defs'->'main'->>'type' = 'record'"
        );
    }

    #[test]
    fn adapt_sql_postgres_converts_array_index_json_extract() {
        let sql = "WHERE json_extract(record, '$.value.websites[0].url') = ?";
        assert_eq!(
            adapt_sql(sql, DatabaseBackend::Postgres),
            "WHERE record::jsonb->'value'->'websites'->0->>'url' = $1"
        );
    }

    #[test]
    fn adapt_sql_sqlite_keeps_array_index_json_extract() {
        let sql = "WHERE json_extract(record, '$.value.tags[0]') = ?";
        assert_eq!(
            adapt_sql(sql, DatabaseBackend::Sqlite),
            "WHERE json_extract(record, '$.value.tags[0]') = ?"
        );
    }

    #[test]
    fn adapt_sql_multiple_json_expressions() {
        let sql =
            "SELECT json_extract(record, '$.title'), json_extract(record, '$.year') FROM records";
        assert_eq!(
            adapt_sql(sql, DatabaseBackend::Postgres),
            "SELECT record::jsonb->>'title', record::jsonb->>'year' FROM records"
        );
        assert_eq!(
            adapt_sql(sql, DatabaseBackend::Sqlite),
            "SELECT json_extract(record, '$.title'), json_extract(record, '$.year') FROM records"
        );
    }

    // -----------------------------------------------------------------------
    // adapt_sql: LIKE stays as-is
    // -----------------------------------------------------------------------

    #[test]
    fn adapt_sql_postgres_keeps_like() {
        let sql = "WHERE name LIKE ?";
        assert_eq!(
            adapt_sql(sql, DatabaseBackend::Postgres),
            "WHERE name LIKE $1"
        );
    }

    #[test]
    fn adapt_sql_sqlite_keeps_like() {
        let sql = "WHERE name LIKE ?";
        assert_eq!(adapt_sql(sql, DatabaseBackend::Sqlite), "WHERE name LIKE ?");
    }

    // -----------------------------------------------------------------------
    // adapt_sql: datetime('now') conversion
    // -----------------------------------------------------------------------

    #[test]
    fn adapt_sql_sqlite_keeps_datetime_now() {
        let sql = "INSERT INTO t (created_at) VALUES (datetime('now'))";
        assert_eq!(
            adapt_sql(sql, DatabaseBackend::Sqlite),
            "INSERT INTO t (created_at) VALUES (datetime('now'))"
        );
    }

    #[test]
    fn adapt_sql_postgres_converts_datetime_now() {
        let sql = "INSERT INTO t (created_at) VALUES (datetime('now'))";
        assert_eq!(
            adapt_sql(sql, DatabaseBackend::Postgres),
            "INSERT INTO t (created_at) VALUES (NOW())"
        );
    }

    // -----------------------------------------------------------------------
    // adapt_sql: datetime('now', '±N unit') conversion
    // -----------------------------------------------------------------------

    #[test]
    fn adapt_sql_sqlite_keeps_datetime_interval() {
        let sql = "WHERE indexed_at > datetime('now', '-7 days')";
        assert_eq!(
            adapt_sql(sql, DatabaseBackend::Sqlite),
            "WHERE indexed_at > datetime('now', '-7 days')"
        );
    }

    #[test]
    fn adapt_sql_postgres_converts_datetime_minus_interval() {
        let sql = "WHERE indexed_at > datetime('now', '-7 days')";
        assert_eq!(
            adapt_sql(sql, DatabaseBackend::Postgres),
            "WHERE indexed_at > NOW() - INTERVAL '7 days'"
        );
    }

    #[test]
    fn adapt_sql_postgres_converts_datetime_plus_interval() {
        let sql = "WHERE expires_at < datetime('now', '+30 minutes')";
        assert_eq!(
            adapt_sql(sql, DatabaseBackend::Postgres),
            "WHERE expires_at < NOW() + INTERVAL '30 minutes'"
        );
    }

    #[test]
    fn adapt_sql_postgres_interval_with_json_and_placeholders() {
        let sql = "SELECT json_extract(record, '$.subject') FROM records WHERE collection = ? AND indexed_at > datetime('now', '-7 days') LIMIT ?";
        assert_eq!(
            adapt_sql(sql, DatabaseBackend::Postgres),
            "SELECT record::jsonb->>'subject' FROM records WHERE collection = $1 AND indexed_at > NOW() - INTERVAL '7 days' LIMIT $2"
        );
    }

    // -----------------------------------------------------------------------
    // adapt_sql: boolean literals stay as 0/1
    // -----------------------------------------------------------------------

    #[test]
    fn adapt_sql_keeps_integer_booleans() {
        let sql = "UPDATE t SET active = 1 WHERE id = ?";
        assert_eq!(
            adapt_sql(sql, DatabaseBackend::Postgres),
            "UPDATE t SET active = 1 WHERE id = $1"
        );
        assert_eq!(
            adapt_sql(sql, DatabaseBackend::Sqlite),
            "UPDATE t SET active = 1 WHERE id = ?"
        );
    }

    // -----------------------------------------------------------------------
    // adapt_sql: combined conversions
    // -----------------------------------------------------------------------

    #[test]
    fn adapt_sql_combined_json_like_placeholders() {
        let sql = "SELECT * FROM records WHERE json_extract(record, '$.title') LIKE ? LIMIT ?";
        assert_eq!(
            adapt_sql(sql, DatabaseBackend::Postgres),
            "SELECT * FROM records WHERE record::jsonb->>'title' LIKE $1 LIMIT $2"
        );
        assert_eq!(
            adapt_sql(sql, DatabaseBackend::Sqlite),
            "SELECT * FROM records WHERE json_extract(record, '$.title') LIKE ? LIMIT ?"
        );
    }

    #[test]
    fn adapt_sql_no_json_operators_unchanged() {
        let sql = "SELECT COUNT(*) FROM records WHERE collection = ?";
        assert_eq!(
            adapt_sql(sql, DatabaseBackend::Postgres),
            "SELECT COUNT(*) FROM records WHERE collection = $1"
        );
    }

    // -----------------------------------------------------------------------
    // parse_dt
    // -----------------------------------------------------------------------

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
        let pool = connect_backfill_pool(&url, DatabaseBackend::Sqlite).await;

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
            "SELECT COUNT(*) FROM sqlite_stat1 WHERE tbl = 'happyview_records'",
        )
        .fetch_one(&pool)
        .await
        .expect("sqlite_stat1 should exist after optimize");
        assert!(rows > 0, "optimize did not analyze happyview_records");

        drop(pool);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }
}

//! Database helpers shared by HappyView and its providers.

use std::sync::LazyLock;

use regex::Regex;
use serde::Deserialize;

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

/// A database error's code, split by backend: SQLite's numeric result code,
/// or Postgres's SQLSTATE.
enum DbCode<'a> {
    Sqlite(i32),
    Postgres(std::borrow::Cow<'a, str>),
}

fn db_code(e: &sqlx::Error) -> Option<DbCode<'_>> {
    let db = e.as_database_error()?;
    let code = db.code()?;
    if db.try_downcast_ref::<sqlx::sqlite::SqliteError>().is_some() {
        return code.parse().ok().map(DbCode::Sqlite);
    }
    Some(DbCode::Postgres(code))
}

/// Whether a query failed because the database could not be reached or used
/// at all, rather than because of anything in the query. The same query is
/// expected to succeed once the database is back, so a caller waits and
/// retries it instead of giving it up. Everything else is a failure of that
/// query.
///
/// That is: the pool timed out or closed, the connection or its TLS broke,
/// the SQLite worker died, a transaction could not begin; a Postgres
/// connection failure (`08000`, `08001`, `08003`, `08004`, `08006`, but not
/// the per-message `08P01`), shutdown (`57P`), resource exhaustion (`53`), a
/// read-only transaction (`25006`, a standby), a system error (`58`), an
/// authorization failure (`28`) or a missing database (`3D000`); a SQLite
/// file that cannot be written, read or opened (`sqlite_code_is_unavailable`);
/// or a lock still held after `retry_on_busy` (`is_retryable_write_error`).
pub fn is_unavailable(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::WorkerCrashed
        | sqlx::Error::BeginFailed => true,
        sqlx::Error::Database(_) => match db_code(e) {
            Some(DbCode::Sqlite(code)) => sqlite_code_is_unavailable(code),
            Some(DbCode::Postgres(code)) => postgres_code_is_unavailable(&code),
            None => false,
        },
        _ => false,
    }
}

/// SQLite `BUSY`, `LOCKED`, `NOMEM`, `READONLY`, `IOERR`, `FULL` and
/// `CANTOPEN`, under any extended code (the primary code is the low byte).
fn sqlite_code_is_unavailable(code: i32) -> bool {
    matches!(code & 0xff, 5 | 6 | 7 | 8 | 10 | 13 | 14)
}

fn postgres_code_is_unavailable(code: &str) -> bool {
    matches!(
        code,
        "08000" | "08001" | "08003" | "08004" | "08006" | "25006" | "3D000" | "40001" | "40P01"
    ) || ["57P", "53", "58", "28"]
        .iter()
        .any(|class| code.starts_with(class))
}

/// Whether a write failed because of the data it carried, so retrying the
/// same data can never succeed: a value the driver could not encode, a
/// Postgres data exception (class `22`, e.g. `22021`, a NUL byte in text) or
/// a statement too complex (`54000`), or SQLite `TOOBIG`.
pub fn is_invalid_data(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::Encode(_) => true,
        sqlx::Error::Database(_) => match db_code(e) {
            Some(DbCode::Sqlite(code)) => sqlite_code_is_invalid_data(code),
            Some(DbCode::Postgres(code)) => code.starts_with("22") || code == "54000",
            None => false,
        },
        _ => false,
    }
}

fn sqlite_code_is_invalid_data(code: i32) -> bool {
    code & 0xff == 18
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

/// `lhs op rhs` over two rev (TID) expressions. TIDs sort by time in byte
/// order; Postgres would otherwise compare text in the database's collation,
/// so it gets `COLLATE "C"` on both sides.
pub fn rev_compare(backend: DatabaseBackend, lhs: &str, op: &str, rhs: &str) -> String {
    match backend {
        DatabaseBackend::Sqlite => format!("{lhs} {op} {rhs}"),
        DatabaseBackend::Postgres => format!("{lhs} COLLATE \"C\" {op} {rhs} COLLATE \"C\""),
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

#[cfg(test)]
mod tests {
    use super::*;

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

    // -----------------------------------------------------------------------
    // is_unavailable
    // -----------------------------------------------------------------------

    #[test]
    fn pool_and_io_errors_are_unavailable() {
        use std::io::{Error as IoError, ErrorKind};
        for err in [
            sqlx::Error::PoolTimedOut,
            sqlx::Error::PoolClosed,
            sqlx::Error::WorkerCrashed,
            sqlx::Error::BeginFailed,
            sqlx::Error::Tls("handshake failed".into()),
            sqlx::Error::Io(IoError::from(ErrorKind::ConnectionRefused)),
            sqlx::Error::Io(IoError::from(ErrorKind::ConnectionReset)),
            sqlx::Error::Io(IoError::from(ErrorKind::UnexpectedEof)),
        ] {
            assert!(is_unavailable(&err), "{err}");
        }
        for err in [
            sqlx::Error::RowNotFound,
            sqlx::Error::Protocol("bad frame".into()),
            sqlx::Error::ColumnNotFound("rev".into()),
            sqlx::Error::Decode("not a number".into()),
        ] {
            assert!(!is_unavailable(&err), "{err}");
        }
    }

    /// A database error with a chosen code, standing in for a Postgres one
    /// that is impractical to provoke.
    #[derive(Debug)]
    struct CodedError(&'static str);

    impl std::fmt::Display for CodedError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "error {}", self.0)
        }
    }

    impl std::error::Error for CodedError {}

    impl sqlx::error::DatabaseError for CodedError {
        fn message(&self) -> &str {
            "coded"
        }
        fn code(&self) -> Option<std::borrow::Cow<'_, str>> {
            Some(self.0.into())
        }
        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }
        fn kind(&self) -> sqlx::error::ErrorKind {
            sqlx::error::ErrorKind::Other
        }
    }

    fn coded(code: &'static str) -> sqlx::Error {
        sqlx::Error::Database(Box::new(CodedError(code)))
    }

    #[test]
    fn postgres_codes_for_an_unusable_database_are_unavailable() {
        for code in [
            "08000", "08001", "08003", "08004", "08006", "57P01", "57P02", "57P03", "57P04",
            "53000", "53100", "53200", "53300", "25006", "58000", "58030", "28000", "28P01",
            "3D000", "40001", "40P01",
        ] {
            assert!(is_unavailable(&coded(code)), "{code}");
        }
        for code in [
            "08P01", "08007", "22021", "22001", "22P02", "23505", "42P01", "57014", "54000",
        ] {
            assert!(!is_unavailable(&coded(code)), "{code}");
        }
    }

    #[test]
    fn sqlite_codes_for_an_unusable_database_are_unavailable() {
        // Primary codes, then extended ones: BUSY_SNAPSHOT, IOERR_READ,
        // READONLY_ROLLBACK, CANTOPEN_ISDIR.
        for code in [5, 6, 7, 8, 10, 13, 14, 517, 266, 776, 526] {
            assert!(sqlite_code_is_unavailable(code), "{code}");
        }
        // ERROR, TOOBIG, CONSTRAINT, CONSTRAINT_UNIQUE, MISMATCH.
        for code in [1, 18, 19, 2067, 20] {
            assert!(!sqlite_code_is_unavailable(code), "{code}");
        }
    }

    #[test]
    fn errors_in_the_data_itself_are_invalid() {
        for err in [
            coded("22021"),
            coded("22001"),
            coded("22P02"),
            coded("54000"),
            sqlx::Error::Encode("not encodable".into()),
        ] {
            assert!(is_invalid_data(&err), "{err}");
        }
        for err in [
            coded("23505"),
            coded("08006"),
            coded("54001"),
            sqlx::Error::PoolClosed,
            sqlx::Error::RowNotFound,
        ] {
            assert!(!is_invalid_data(&err), "{err}");
        }
        assert!(sqlite_code_is_invalid_data(18), "TOOBIG");
        assert!(!sqlite_code_is_invalid_data(19), "CONSTRAINT");
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
}

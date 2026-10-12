//! Bare-table helpers for tests elsewhere in the crate that need a lexicon
//! table without the rest of the schema.

use sqlx::AnyPool;

/// The columns the telemetry probes read, and nothing else, so a probe test
/// can also leave every other table missing.
pub(crate) async fn create_bare_lexicons_table(pool: &AnyPool) {
    crate::db::query(&format!(
        "CREATE TABLE {} (
            id TEXT PRIMARY KEY,
            revision INTEGER NOT NULL DEFAULT 1,
            lexicon_json TEXT NOT NULL,
            backfill INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        super::lexicon_rows::TABLE
    ))
    .execute(pool)
    .await
    .expect("create lexicons");
}

pub(crate) async fn insert_bare_lexicon(pool: &AnyPool, nsid: &str, created_at: &str) {
    let sql = crate::db::adapt_sql(
        &format!(
            "INSERT INTO {} (id, lexicon_json, backfill, created_at) VALUES (?, '{{}}', 0, ?)",
            super::lexicon_rows::TABLE
        ),
        crate::db::DatabaseBackend::Sqlite,
    );
    crate::db::query(&sql)
        .bind(nsid)
        .bind(created_at)
        .execute(pool)
        .await
        .unwrap();
}

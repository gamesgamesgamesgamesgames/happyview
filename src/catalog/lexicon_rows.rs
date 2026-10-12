//! Every statement that names the lexicons table. Nothing outside
//! `src/catalog/` may name it; `catalog::tests` enforces that.

use serde_json::Value;
use sqlx::AnyPool;

use super::lexicons::LexiconWrite;
use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};

pub const TABLE: &str = "happyview_lexicons";

/// The columns the catalog keeps in memory for a stored row.
pub(crate) struct LoadedRow {
    pub id: String,
    pub lexicon_json: String,
    pub revision: i32,
    pub target_collection: Option<String>,
    pub action: Option<String>,
    pub token_cost: Option<i32>,
    pub source: String,
    pub authority_did: Option<String>,
}

#[allow(clippy::type_complexity)]
pub(crate) async fn load_all(db: &AnyPool) -> Result<Vec<LoadedRow>, sqlx::Error> {
    let rows: Vec<(
        String,
        String,
        i32,
        Option<String>,
        Option<String>,
        Option<i32>,
        String,
        Option<String>,
    )> = crate::db::query_as(
        "SELECT id, lexicon_json, revision, target_collection, action, token_cost, source, authority_did FROM happyview_lexicons",
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                lexicon_json,
                revision,
                target_collection,
                action,
                token_cost,
                source,
                authority_did,
            )| {
                LoadedRow {
                    id,
                    lexicon_json,
                    revision,
                    target_collection,
                    action,
                    token_cost,
                    source,
                    authority_did,
                }
            },
        )
        .collect())
}

pub const COUNT_ALL_SQL: &str = "SELECT COUNT(*) FROM happyview_lexicons";
pub const COUNT_CREATED_SINCE_SQL: &str =
    "SELECT COUNT(*) FROM happyview_lexicons WHERE created_at >= ?";

/// Every column, as the admin API lists a row.
pub(crate) struct FullRow {
    pub id: String,
    pub revision: i32,
    pub lexicon_json: String,
    pub backfill: bool,
    pub action: Option<String>,
    pub target_collection: Option<String>,
    pub source: String,
    pub authority_did: Option<String>,
    pub last_fetched_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub token_cost: Option<i32>,
}

const FULL_COLUMNS: &str = "id, revision, lexicon_json, backfill, action, target_collection, source, authority_did, last_fetched_at, created_at, updated_at, token_cost";

type FullColumns = (
    String,
    i32,
    String,
    i32,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
    Option<i32>,
);

fn full(c: FullColumns) -> FullRow {
    FullRow {
        id: c.0,
        revision: c.1,
        lexicon_json: c.2,
        backfill: c.3 != 0,
        action: c.4,
        target_collection: c.5,
        source: c.6,
        authority_did: c.7,
        last_fetched_at: c.8,
        created_at: c.9,
        updated_at: c.10,
        token_cost: c.11,
    }
}

pub(crate) async fn list_full(
    db: &AnyPool,
    backend: DatabaseBackend,
) -> Result<Vec<FullRow>, sqlx::Error> {
    let sql = adapt_sql(
        &format!("SELECT {FULL_COLUMNS} FROM happyview_lexicons ORDER BY id"),
        backend,
    );
    let rows: Vec<FullColumns> = crate::db::query_as(&sql).fetch_all(db).await?;
    Ok(rows.into_iter().map(full).collect())
}

pub(crate) async fn get_full(
    db: &AnyPool,
    backend: DatabaseBackend,
    id: &str,
) -> Result<Option<FullRow>, sqlx::Error> {
    let sql = adapt_sql(
        &format!("SELECT {FULL_COLUMNS} FROM happyview_lexicons WHERE id = ?"),
        backend,
    );
    let row: Option<FullColumns> = crate::db::query_as(&sql)
        .bind(id)
        .fetch_optional(db)
        .await?;
    Ok(row.map(full))
}

pub(crate) async fn upsert_manual(
    db: &AnyPool,
    backend: DatabaseBackend,
    id: &str,
    lexicon: &LexiconWrite,
) -> Result<i32, sqlx::Error> {
    let sql = adapt_sql(
        r#"
        INSERT INTO happyview_lexicons (id, lexicon_json, backfill, target_collection, action, token_cost, source, created_at)
        VALUES (?, ?, ?, ?, ?, ?, 'manual', ?)
        ON CONFLICT (id) DO UPDATE SET
            lexicon_json = EXCLUDED.lexicon_json,
            backfill = EXCLUDED.backfill,
            target_collection = EXCLUDED.target_collection,
            action = EXCLUDED.action,
            token_cost = EXCLUDED.token_cost,
            source = 'manual',
            revision = happyview_lexicons.revision + 1,
            updated_at = ?
        RETURNING revision
        "#,
        backend,
    );
    let now = now_rfc3339();
    let row: (i32,) = crate::db::query_as(&sql)
        .bind(id)
        .bind(serde_json::to_string(&lexicon.lexicon_json).unwrap_or_default())
        .bind(if lexicon.backfill { 1_i32 } else { 0_i32 })
        .bind(&lexicon.target_collection)
        .bind(lexicon.action.to_optional_str())
        .bind(lexicon.token_cost)
        .bind(&now)
        .bind(&now)
        .fetch_one(db)
        .await?;
    Ok(row.0)
}

pub(crate) async fn upsert_network(
    db: &AnyPool,
    backend: DatabaseBackend,
    id: &str,
    lexicon: &LexiconWrite,
    authority_did: &str,
) -> Result<i32, sqlx::Error> {
    let sql = adapt_sql(
        r#"
        INSERT INTO happyview_lexicons (id, lexicon_json, backfill, target_collection, source, authority_did, last_fetched_at, created_at)
        VALUES (?, ?, ?, ?, 'network', ?, ?, ?)
        ON CONFLICT (id) DO UPDATE SET
            lexicon_json = EXCLUDED.lexicon_json,
            backfill = EXCLUDED.backfill,
            target_collection = EXCLUDED.target_collection,
            source = 'network',
            authority_did = EXCLUDED.authority_did,
            last_fetched_at = ?,
            revision = happyview_lexicons.revision + 1,
            updated_at = ?
        RETURNING revision
        "#,
        backend,
    );
    let now = now_rfc3339();
    let row: (i32,) = crate::db::query_as(&sql)
        .bind(id)
        .bind(serde_json::to_string(&lexicon.lexicon_json).unwrap_or_default())
        .bind(if lexicon.backfill { 1_i32 } else { 0_i32 })
        .bind(&lexicon.target_collection)
        .bind(authority_did)
        .bind(&now)
        .bind(&now)
        .bind(&now)
        .bind(&now)
        .fetch_one(db)
        .await?;
    Ok(row.0)
}

/// `None` when no network row has this id.
pub(crate) async fn refresh_network(
    db: &AnyPool,
    backend: DatabaseBackend,
    id: &str,
    lexicon_json: &Value,
) -> Result<Option<i32>, sqlx::Error> {
    let sql = adapt_sql(
        "UPDATE happyview_lexicons SET lexicon_json = ?, last_fetched_at = ?, revision = revision + 1, updated_at = ? WHERE id = ? AND source = 'network' RETURNING revision",
        backend,
    );
    let now = now_rfc3339();
    let row: Option<(i32,)> = crate::db::query_as(&sql)
        .bind(serde_json::to_string(lexicon_json).unwrap_or_default())
        .bind(&now)
        .bind(&now)
        .bind(id)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|r| r.0))
}

pub(crate) async fn delete(
    db: &AnyPool,
    backend: DatabaseBackend,
    id: &str,
) -> Result<u64, sqlx::Error> {
    let sql = adapt_sql("DELETE FROM happyview_lexicons WHERE id = ?", backend);
    Ok(crate::db::query(&sql)
        .bind(id)
        .execute(db)
        .await?
        .rows_affected())
}

pub(crate) async fn delete_network(
    db: &AnyPool,
    backend: DatabaseBackend,
    id: &str,
) -> Result<u64, sqlx::Error> {
    let sql = adapt_sql(
        "DELETE FROM happyview_lexicons WHERE id = ? AND source = 'network'",
        backend,
    );
    Ok(crate::db::query(&sql)
        .bind(id)
        .execute(db)
        .await?
        .rows_affected())
}

/// Every stored id, for the boot-time sweep that runs before the catalog loads.
pub async fn ids(db: &AnyPool, backend: DatabaseBackend) -> Result<Vec<String>, sqlx::Error> {
    let sql = adapt_sql("SELECT id FROM happyview_lexicons", backend);
    let rows: Vec<(String,)> = crate::db::query_as(&sql).fetch_all(db).await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Deletes a row whose id is blank. The `trim(id) = ''` bound is in SQL as
/// well as in the caller so a change to the caller's classifier can never
/// widen what this statement removes.
pub async fn delete_blank(
    db: &AnyPool,
    backend: DatabaseBackend,
    id: &str,
) -> Result<u64, sqlx::Error> {
    let sql = adapt_sql(
        "DELETE FROM happyview_lexicons WHERE id = ? AND trim(id) = ''",
        backend,
    );
    Ok(crate::db::query(&sql)
        .bind(id)
        .execute(db)
        .await?
        .rows_affected())
}

/// A row with whatever text `lexicon_json` is given, for tests of rows the
/// admin API could never have written.
#[cfg(test)]
pub(crate) async fn insert_raw_for_test(
    db: &AnyPool,
    backend: DatabaseBackend,
    id: &str,
    lexicon_json: &str,
) {
    let sql = adapt_sql(
        "INSERT INTO happyview_lexicons (id, lexicon_json, backfill, source, created_at, updated_at) VALUES (?, ?, 0, 'manual', ?, ?)",
        backend,
    );
    let now = now_rfc3339();
    crate::db::query(&sql)
        .bind(id)
        .bind(lexicon_json)
        .bind(&now)
        .bind(&now)
        .execute(db)
        .await
        .expect("insert a raw lexicon row");
}

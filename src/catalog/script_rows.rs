//! Every statement that names the scripts table. Nothing outside
//! `src/catalog/` may name it; `catalog::tests` enforces that.

use sqlx::AnyPool;

use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};

pub const TABLE: &str = "happyview_scripts";
pub const COUNT_ALL_SQL: &str = "SELECT COUNT(*) FROM happyview_scripts";
pub const COUNT_UPDATED_SINCE_SQL: &str =
    "SELECT COUNT(*) FROM happyview_scripts WHERE updated_at >= ?";

/// `(id, script_type, body, description, outbound_xrpcs, created_at, updated_at)`.
pub(crate) type ScriptColumns = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
);

const COLUMNS: &str = "id, script_type, body, description, outbound_xrpcs, created_at, updated_at";

pub(crate) async fn get(
    db: &AnyPool,
    backend: DatabaseBackend,
    id: &str,
) -> Result<Option<ScriptColumns>, sqlx::Error> {
    let sql = adapt_sql(
        &format!("SELECT {COLUMNS} FROM happyview_scripts WHERE id = ?"),
        backend,
    );
    crate::db::query_as(&sql).bind(id).fetch_optional(db).await
}

/// `suffix` keeps rows whose id ends with `:<suffix>`.
pub(crate) async fn list(
    db: &AnyPool,
    backend: DatabaseBackend,
    suffix: Option<&str>,
) -> Result<Vec<ScriptColumns>, sqlx::Error> {
    let mut sql = format!("SELECT {COLUMNS} FROM happyview_scripts");
    if suffix.is_some() {
        sql.push_str(" WHERE id LIKE ?");
    }
    sql.push_str(" ORDER BY id");
    let sql = adapt_sql(&sql, backend);
    let mut q = crate::db::query_as::<ScriptColumns>(&sql);
    if let Some(suffix) = suffix {
        q = q.bind(format!("%:{suffix}"));
    }
    q.fetch_all(db).await
}

pub(crate) async fn exists(
    db: &AnyPool,
    backend: DatabaseBackend,
    id: &str,
) -> Result<bool, sqlx::Error> {
    let sql = adapt_sql("SELECT id FROM happyview_scripts WHERE id = ?", backend);
    let row: Option<(String,)> = crate::db::query_as(&sql)
        .bind(id)
        .fetch_optional(db)
        .await?;
    Ok(row.is_some())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn upsert(
    db: &AnyPool,
    backend: DatabaseBackend,
    id: &str,
    script_type: &str,
    body: &str,
    description: Option<&str>,
    outbound_json: Option<&str>,
) -> Result<(), sqlx::Error> {
    let sql = adapt_sql(
        r#"
        INSERT INTO happyview_scripts (id, script_type, body, description, outbound_xrpcs, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (id) DO UPDATE SET
            script_type    = EXCLUDED.script_type,
            body           = EXCLUDED.body,
            description    = EXCLUDED.description,
            outbound_xrpcs = EXCLUDED.outbound_xrpcs,
            updated_at     = EXCLUDED.updated_at
        "#,
        backend,
    );
    let now = now_rfc3339();
    crate::db::query(&sql)
        .bind(id)
        .bind(script_type)
        .bind(body)
        .bind(description)
        .bind(outbound_json)
        .bind(&now)
        .bind(&now)
        .execute(db)
        .await?;
    Ok(())
}

pub(crate) async fn delete(
    db: &AnyPool,
    backend: DatabaseBackend,
    id: &str,
) -> Result<u64, sqlx::Error> {
    let sql = adapt_sql("DELETE FROM happyview_scripts WHERE id = ?", backend);
    Ok(crate::db::query(&sql)
        .bind(id)
        .execute(db)
        .await?
        .rows_affected())
}

/// Every stored trigger id, for the boot-time NSID audit.
pub async fn ids(db: &AnyPool, backend: DatabaseBackend) -> Result<Vec<String>, sqlx::Error> {
    let sql = adapt_sql("SELECT id FROM happyview_scripts", backend);
    let rows: Vec<(String,)> = crate::db::query_as(&sql).fetch_all(db).await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

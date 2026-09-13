//! Raw SQL for plugins. Same boundary as Lua's `db.raw`: the table guard in
//! `raw_sql_guard` protects HappyView's internal tables; the capability decides
//! whether writes are allowed at all.

use serde_json::{Map, Value};
use sqlx::{Column, Row};

use crate::raw_sql_guard::check_raw_sql_tables;

fn bind_params<'q>(
    mut query: sqlx::query::Query<'q, sqlx::Any, sqlx::any::AnyArguments>,
    params: &'q [Value],
) -> Result<sqlx::query::Query<'q, sqlx::Any, sqlx::any::AnyArguments>, String> {
    for value in params {
        query = match value {
            Value::String(s) => query.bind(s.as_str()),
            Value::Number(n) if n.is_i64() => query.bind(n.as_i64().unwrap()),
            Value::Number(n) => query.bind(n.as_f64().unwrap_or(0.0)),
            Value::Bool(b) => query.bind(if *b { 1_i32 } else { 0_i32 }),
            Value::Null => query.bind(Option::<String>::None),
            other => return Err(format!("unsupported parameter type: {other}")),
        };
    }
    Ok(query)
}

pub(super) fn row_to_json(row: &sqlx::any::AnyRow) -> Map<String, Value> {
    let mut out = Map::new();
    for col in row.columns() {
        let name = col.name();
        let v = if let Ok(s) = row.try_get::<String, _>(name) {
            Value::String(s)
        } else if let Ok(n) = row.try_get::<i64, _>(name) {
            Value::from(n)
        } else if let Ok(n) = row.try_get::<i32, _>(name) {
            Value::from(n)
        } else if let Ok(n) = row.try_get::<f64, _>(name) {
            Value::from(n)
        } else if let Ok(b) = row.try_get::<bool, _>(name) {
            Value::Bool(b)
        } else {
            Value::Null
        };
        out.insert(name.to_string(), v);
    }
    out
}

/// SQL is passed through untranslated; placeholders are backend-native
/// (`?` on SQLite, `$1`… on Postgres), as for Lua's `db.raw`.
pub async fn run_query(
    db: &sqlx::AnyPool,
    sql: &str,
    params: &[Value],
) -> Result<Vec<Map<String, Value>>, String> {
    check_raw_sql_tables(sql)?;
    let rows = bind_params(crate::db::query(sql), params)?
        .fetch_all(db)
        .await
        .map_err(|e| format!("query failed: {e}"))?;
    Ok(rows.iter().map(row_to_json).collect())
}

/// SQL is passed through untranslated; placeholders are backend-native
/// (`?` on SQLite, `$1`… on Postgres), as for Lua's `db.raw`.
pub async fn run_execute(db: &sqlx::AnyPool, sql: &str, params: &[Value]) -> Result<u64, String> {
    check_raw_sql_tables(sql)?;
    let result = bind_params(crate::db::query(sql), params)?
        .execute(db)
        .await
        .map_err(|e| format!("execute failed: {e}"))?;
    Ok(result.rows_affected())
}

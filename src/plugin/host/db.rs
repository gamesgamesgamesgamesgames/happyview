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
            // A boolean binds as one: Postgres will not compare its own
            // `boolean` against an integer, and SQLite stores the bind as the
            // 0/1 an integer column already holds.
            Value::Bool(b) => query.bind(*b),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DatabaseBackend;
    use serde_json::json;
    use serial_test::serial;

    /// Every backend the suite can reach; the Postgres half needs a database
    /// to be pointed at and says so when there is none.
    async fn backends() -> Vec<(sqlx::AnyPool, DatabaseBackend)> {
        let mut out = vec![(
            crate::test_support::migrated_memory_pool().await,
            DatabaseBackend::Sqlite,
        )];
        match std::env::var("TEST_DATABASE_URL") {
            Ok(url) => {
                let backend = DatabaseBackend::from_url(&url);
                out.push((crate::db::connect(&url, backend).await, backend));
            }
            Err(_) => eprintln!("TEST_DATABASE_URL unset: the Postgres half is skipped"),
        }
        out
    }

    /// Raw SQL is passed through untranslated, so a test writes each
    /// backend's own placeholder exactly as a plugin would.
    fn placeholder(backend: DatabaseBackend, position: usize) -> String {
        match backend {
            DatabaseBackend::Sqlite => "?".to_string(),
            DatabaseBackend::Postgres => format!("${position}"),
        }
    }

    /// A boolean parameter against a boolean column. Binding it as an integer
    /// is well-typed on SQLite, which has no boolean of its own, and leaves
    /// Postgres comparing `boolean = integer`, which it refuses.
    #[tokio::test]
    #[serial]
    async fn a_boolean_parameter_compares_against_a_boolean_column() {
        for (db, backend) in backends().await {
            crate::db::query(
                "CREATE TABLE IF NOT EXISTS host_db_flags (name TEXT, active BOOLEAN)",
            )
            .execute(&db)
            .await
            .expect("create the table");
            crate::db::query("DELETE FROM host_db_flags")
                .execute(&db)
                .await
                .expect("clear the table");

            let inserted = run_execute(
                &db,
                &format!(
                    "INSERT INTO host_db_flags (name, active) VALUES ({}, {}), ({}, {})",
                    placeholder(backend, 1),
                    placeholder(backend, 2),
                    placeholder(backend, 3),
                    placeholder(backend, 4),
                ),
                &[json!("on"), json!(true), json!("off"), json!(false)],
            )
            .await
            .expect("the insert should run");
            assert_eq!(inserted, 2);

            let rows = run_query(
                &db,
                &format!(
                    "SELECT name FROM host_db_flags WHERE active = {}",
                    placeholder(backend, 1)
                ),
                &[json!(true)],
            )
            .await
            .unwrap_or_else(|e| panic!("the query should run on {backend:?}: {e}"));
            assert_eq!(rows.len(), 1, "on {backend:?}");
            assert_eq!(rows[0]["name"], json!("on"));
        }
    }
}

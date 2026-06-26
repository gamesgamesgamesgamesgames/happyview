use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use mlua::{Lua, LuaSerdeExt, Result as LuaResult};
use serde_json::{Value, json};
use sqlx::{Column, Row};
use std::sync::Arc;

use crate::AppState;
use crate::db::{DatabaseBackend, adapt_sql, decode_cursor, encode_cursor};

const MAX_FILTER_DEPTH: u8 = 5;
const ALLOWED_OPS: &[&str] = &["=", "!=", "<", ">", "<=", ">=", "LIKE", "NOT LIKE"];

fn is_valid_json_field_path(path: &str) -> bool {
    if path.is_empty() {
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
            rest = &rest[close + 1..];
        }
    }
    true
}

#[derive(Debug)]
enum FilterNode {
    Condition {
        field: String,
        op: String,
        value: String,
    },
    Group {
        combine: String,
        children: Vec<FilterNode>,
    },
}

fn parse_filter_node(table: &mlua::Table, depth: u8) -> LuaResult<FilterNode> {
    if depth >= MAX_FILTER_DEPTH {
        return Err(mlua::Error::runtime(format!(
            "filter nesting too deep (max {MAX_FILTER_DEPTH} levels)",
        )));
    }

    if let Ok(field) = table.get::<String>("field") {
        if !is_valid_json_field_path(&field) {
            return Err(mlua::Error::runtime(format!(
                "invalid filter field '{field}': use alphanumeric names with optional dot notation and array indices (e.g. 'name', 'author.handle', 'tags[0]')",
            )));
        }

        let op: String = table
            .get::<String>("op")
            .unwrap_or_else(|_| "=".to_string());
        let op_upper = op.to_uppercase();
        if !ALLOWED_OPS.contains(&op_upper.as_str()) {
            return Err(mlua::Error::runtime(format!(
                "invalid filter op '{op}': must be one of {ALLOWED_OPS:?}",
            )));
        }

        let val: mlua::Value = table.get("value")?;
        let value = match val {
            mlua::Value::String(s) => s.to_str()?.to_string(),
            mlua::Value::Integer(n) => n.to_string(),
            mlua::Value::Number(n) => n.to_string(),
            mlua::Value::Boolean(b) => (if b { "true" } else { "false" }).to_string(),
            other => {
                return Err(mlua::Error::runtime(format!(
                    "unsupported filter value type for '{field}': {}",
                    other.type_name()
                )));
            }
        };

        return Ok(FilterNode::Condition {
            field,
            op: op_upper,
            value,
        });
    }

    let combine: String = table
        .get::<String>("combine")
        .unwrap_or_else(|_| "AND".to_string())
        .to_uppercase();
    if combine != "AND" && combine != "OR" {
        return Err(mlua::Error::runtime(format!(
            "invalid filter combine '{combine}': must be 'AND' or 'OR'",
        )));
    }

    let mut children = Vec::new();
    for child in table.sequence_values::<mlua::Table>() {
        children.push(parse_filter_node(&child?, depth + 1)?);
    }

    if children.is_empty() {
        return Err(mlua::Error::runtime("filter group has no conditions"));
    }

    Ok(FilterNode::Group { combine, children })
}

fn build_filter_sql(node: &FilterNode, binds: &mut Vec<String>) -> String {
    match node {
        FilterNode::Condition { field, op, value } => {
            binds.push(value.clone());
            format!("json_extract(record, '$.{field}') {op} ?")
        }
        FilterNode::Group { combine, children } => {
            let parts: Vec<String> = children
                .iter()
                .map(|c| build_filter_sql(c, binds))
                .collect();
            if parts.len() == 1 {
                parts.into_iter().next().unwrap()
            } else {
                format!("({})", parts.join(&format!(" {combine} ")))
            }
        }
    }
}

/// Register the `db` table with database query functions.
pub fn register_db_api(lua: &Lua, state: Arc<AppState>) -> LuaResult<()> {
    let db_table = lua.create_table()?;

    // db.query({ collection, did?, limit?, offset?, cursor?, sort?, sortDirection?, filter? }) -> { records, cursor? }
    let state_query = state.clone();
    let query_fn = lua.create_async_function(move |lua, opts: mlua::Table| {
        let state = state_query.clone();
        async move {
            let backend = state.db_backend;
            let collection: String = opts.get("collection")?;
            let did: Option<String> = opts.get("did").ok();
            let limit: i64 = opts.get::<i64>("limit").unwrap_or(20).min(100);
            let sort: Option<String> = opts.get("sort").ok();
            let sort_direction: Option<String> = opts.get("sortDirection").ok();
            let cursor_str: Option<String> = opts.get("cursor").ok();

            if let Some(ref field) = sort
                && !is_valid_json_field_path(field)
            {
                return Err(mlua::Error::runtime(
                    "invalid sort field: use alphanumeric names with optional dot notation and array indices (e.g. 'name', 'author.handle', 'tags[0]')",
                ));
            }

            let direction = match sort_direction.as_deref() {
                Some("asc") => "ASC",
                Some("desc") => "DESC",
                None => "DESC",
                Some(other) => {
                    return Err(mlua::Error::runtime(format!(
                        "invalid sortDirection '{other}': must be 'asc' or 'desc'"
                    )));
                }
            };

            let filter_table: Option<mlua::Table> = opts.get("filter").ok();
            let mut filter_binds: Vec<String> = Vec::new();
            let filter_clause = if let Some(ref tbl) = filter_table {
                let node = parse_filter_node(tbl, 0)?;
                let sql = build_filter_sql(&node, &mut filter_binds);
                format!(" AND {sql}")
            } else {
                String::new()
            };

            let result_table = lua.create_table()?;

            if let Some(ref sort_field) = sort {
                // Custom sort: use OFFSET/LIMIT with base64-encoded offset cursor
                let offset: i64 = if let Some(ref cursor) = cursor_str {
                    BASE64.decode(cursor).ok()
                        .and_then(|b| String::from_utf8(b).ok())
                        .and_then(|s| s.parse::<i64>().ok())
                        .unwrap_or(0)
                } else {
                    opts.get::<i64>("offset").unwrap_or(0)
                };

                let top_level_columns = ["indexed_at", "did", "uri"];
                let order_expr = if top_level_columns.contains(&sort_field.as_str()) {
                    format!("{sort_field} {direction}")
                } else {
                    format!("json_extract(record, '$.{sort_field}') {direction}")
                };

                let did_clause = if did.is_some() { " AND did = ?" } else { "" };
                let sql = adapt_sql(
                    &format!("SELECT uri, did, record FROM happyview_records WHERE collection = ?{did_clause}{filter_clause} ORDER BY {order_expr} LIMIT ? OFFSET ?"),
                    backend,
                );
                let mut q = sqlx::query_as(&sql).bind(&collection);
                if let Some(ref did) = did { q = q.bind(did); }
                for val in &filter_binds { q = q.bind(val); }
                let rows: Vec<(String, String, String)> = q
                    .bind(limit)
                    .bind(offset)
                    .fetch_all(&state.db)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("DB query failed: {e}")))?;

                let has_next = rows.len() as i64 == limit;

                if has_next {
                    let next_offset = offset + limit;
                    result_table.set("cursor", BASE64.encode(next_offset.to_string()))?;
                }

                let records: Vec<Value> = rows
                    .into_iter()
                    .map(|(uri, _did, record_str)| {
                        let mut record: Value = serde_json::from_str(&record_str).unwrap_or(json!({}));
                        if let Some(obj) = record.as_object_mut() {
                            obj.insert("uri".to_string(), json!(uri));
                        }
                        record
                    })
                    .collect();

                let record_values: Vec<mlua::Value> = records
                    .iter()
                    .map(|r| lua.to_value(r))
                    .collect::<LuaResult<_>>()?;
                let records_table = lua.create_sequence_from(record_values)?;
                records_table.set_metatable(Some(lua.array_metatable()))?;
                result_table.set("records", records_table)?;
            } else {
                // Cursor-based pagination on (created_at, uri)
                let cursor_parts = cursor_str.as_ref().and_then(|c| decode_cursor(c));

                type RowType = (String, String, String, String);

                let did_clause = if did.is_some() { " AND did = ?" } else { "" };
                let cursor_clause = if cursor_parts.is_some() {
                    " AND (created_at < ? OR (created_at = ? AND uri < ?))"
                } else {
                    ""
                };
                let sql = adapt_sql(
                    &format!(
                        "SELECT uri, did, record, created_at FROM happyview_records \
                         WHERE collection = ?{did_clause}{cursor_clause}{filter_clause} \
                         ORDER BY created_at DESC, uri DESC \
                         LIMIT ?"
                    ),
                    backend,
                );
                let mut q = sqlx::query_as::<_, RowType>(&sql).bind(&collection);
                if let Some(ref did) = did { q = q.bind(did); }
                if let Some((cursor_ts, cursor_uri)) = &cursor_parts {
                    q = q.bind(cursor_ts).bind(cursor_ts).bind(cursor_uri);
                }
                for val in &filter_binds { q = q.bind(val); }
                let rows_raw: Vec<RowType> = q
                    .bind(limit)
                    .fetch_all(&state.db)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("DB query failed: {e}")))?;

                let has_next = rows_raw.len() as i64 == limit;

                if has_next
                    && let Some((last_uri, _, _, last_created_at)) = rows_raw.last()
                {
                    let cursor = encode_cursor(last_created_at, last_uri);
                    result_table.set("cursor", cursor)?;
                }

                let records: Vec<Value> = rows_raw
                    .into_iter()
                    .map(|(uri, _did, record_str, _created_at)| {
                        let mut record: Value = serde_json::from_str(&record_str).unwrap_or(json!({}));
                        if let Some(obj) = record.as_object_mut() {
                            obj.insert("uri".to_string(), json!(uri));
                        }
                        record
                    })
                    .collect();

                let record_values: Vec<mlua::Value> = records
                    .iter()
                    .map(|r| lua.to_value(r))
                    .collect::<LuaResult<_>>()?;
                let records_table = lua.create_sequence_from(record_values)?;
                records_table.set_metatable(Some(lua.array_metatable()))?;
                result_table.set("records", records_table)?;
            }

            Ok(mlua::Value::Table(result_table))
        }
    })?;
    db_table.set("query", query_fn)?;

    // db.get(uri) -> record table or nil
    let state_get = state.clone();
    let get_fn = lua.create_async_function(move |lua, uri: String| {
        let state = state_get.clone();
        async move {
            let backend = state.db_backend;
            let sql = adapt_sql(
                "SELECT record FROM happyview_records WHERE uri = ?",
                backend,
            );
            let row: Option<(String,)> = sqlx::query_as(&sql)
                .bind(&uri)
                .fetch_optional(&state.db)
                .await
                .map_err(|e| mlua::Error::runtime(format!("DB query failed: {e}")))?;

            match row {
                Some((record_str,)) => {
                    let mut record: Value = serde_json::from_str(&record_str).unwrap_or(json!({}));
                    if let Some(obj) = record.as_object_mut() {
                        obj.insert("uri".to_string(), json!(uri));
                    }
                    lua.to_value(&record)
                }
                None => Ok(mlua::Value::Nil),
            }
        }
    })?;
    db_table.set("get", get_fn)?;

    // db.search({ collection, field, query, limit? }) -> { records }
    let state_search = state.clone();
    let search_fn = lua.create_async_function(move |lua, opts: mlua::Table| {
        let state = state_search.clone();
        async move {
            let backend = state.db_backend;
            let collection: String = opts.get("collection")?;
            let field: String = opts.get("field")?;
            let query: String = opts.get("query")?;
            let limit: i64 = opts.get::<i64>("limit").unwrap_or(10).min(100);

            if !is_valid_json_field_path(&field) {
                return Err(mlua::Error::runtime(
                    "invalid search field: use alphanumeric names with optional dot notation and array indices (e.g. 'name', 'author.handle', 'tags[0]')",
                ));
            }

            let like_pattern = format!("%{query}%");

            // Cannot use adapt_sql: Postgres reuses $3 for two bind positions,
            // while SQLite needs separate ? for each. Different bind counts.
            let rows: Vec<(String, String, String)> = match backend {
                DatabaseBackend::Sqlite => {
                    let sql = format!(
                        "SELECT uri, did, record FROM happyview_records \
                         WHERE collection = ? \
                           AND json_extract(record, '$.{field}') LIKE ? COLLATE NOCASE \
                         ORDER BY \
                           CASE \
                             WHEN LOWER(json_extract(record, '$.{field}')) = LOWER(?) THEN 0 \
                             WHEN LOWER(json_extract(record, '$.{field}')) LIKE LOWER(?) || '%' THEN 1 \
                             ELSE 2 \
                           END, \
                           json_extract(record, '$.{field}') \
                         LIMIT ?"
                    );
                    sqlx::query_as(&sql)
                    .bind(&collection)
                    .bind(&like_pattern)
                    .bind(&query)
                    .bind(&query)
                    .bind(limit)
                    .fetch_all(&state.db)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("DB search failed: {e}")))?
                }
                DatabaseBackend::Postgres => {
                    let sql = format!(
                        "SELECT uri, did, record FROM happyview_records \
                         WHERE collection = $1 \
                           AND record::jsonb->>'{field}' ILIKE $2 \
                         ORDER BY \
                           CASE \
                             WHEN LOWER(record::jsonb->>'{field}') = LOWER($3) THEN 0 \
                             WHEN LOWER(record::jsonb->>'{field}') LIKE LOWER($3) || '%' THEN 1 \
                             ELSE 2 \
                           END, \
                           record::jsonb->>'{field}' \
                         LIMIT $4"
                    );
                    sqlx::query_as(&sql)
                    .bind(&collection)
                    .bind(&like_pattern)
                    .bind(&query)
                    .bind(limit)
                    .fetch_all(&state.db)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("DB search failed: {e}")))?
                }
            };

            let records: Vec<Value> = rows
                .into_iter()
                .map(|(uri, _did, record_str)| {
                    let mut record: Value = serde_json::from_str(&record_str).unwrap_or(json!({}));
                    if let Some(obj) = record.as_object_mut() {
                        obj.insert("uri".to_string(), json!(uri));
                    }
                    record
                })
                .collect();

            let record_values: Vec<mlua::Value> = records
                .iter()
                .map(|r| lua.to_value(r))
                .collect::<LuaResult<_>>()?;
            let records_table = lua.create_sequence_from(record_values)?;
            records_table.set_metatable(Some(lua.array_metatable()))?;

            let result_table = lua.create_table()?;
            result_table.set("records", records_table)?;

            Ok(mlua::Value::Table(result_table))
        }
    })?;
    db_table.set("search", search_fn)?;

    // db.count(collection, did?) -> integer
    let state_count = state.clone();
    let count_fn =
        lua.create_async_function(move |_, (collection, did): (String, Option<String>)| {
            let state = state_count.clone();
            async move {
                let backend = state.db_backend;
                let count: (i64,) = if let Some(ref did) = did {
                    let sql = adapt_sql(
                        "SELECT COUNT(*) FROM happyview_records WHERE collection = ? AND did = ?",
                        backend,
                    );
                    sqlx::query_as(&sql)
                        .bind(&collection)
                        .bind(did)
                        .fetch_one(&state.db)
                        .await
                        .map_err(|e| mlua::Error::runtime(format!("DB count failed: {e}")))?
                } else {
                    let sql = adapt_sql(
                        "SELECT COUNT(*) FROM happyview_records WHERE collection = ?",
                        backend,
                    );
                    sqlx::query_as(&sql)
                        .bind(&collection)
                        .fetch_one(&state.db)
                        .await
                        .map_err(|e| mlua::Error::runtime(format!("DB count failed: {e}")))?
                };
                Ok(count.0)
            }
        })?;
    db_table.set("count", count_fn)?;

    // db.backlinks({ collection, uri, did?, limit?, cursor? }) -> { records, cursor? }
    // Find records in `collection` that reference the given AT URI via record_refs.
    let state_backlinks = state.clone();
    let backlinks_fn = lua.create_async_function(move |lua, opts: mlua::Table| {
        let state = state_backlinks.clone();
        async move {
            let backend = state.db_backend;
            let collection: String = opts.get("collection")?;
            let uri: String = opts.get("uri")?;
            let did: Option<String> = opts.get("did").ok();
            let limit: i64 = opts.get::<i64>("limit").unwrap_or(20).min(100);
            let cursor_str: Option<String> = opts.get("cursor").ok();

            let cursor_parts = cursor_str.as_ref().and_then(|c| decode_cursor(c));

            type RowType = (String, String, String, String);

            let rows_raw: Vec<RowType> = match (&did, &cursor_parts) {
                (Some(did), Some((cursor_ts, cursor_uri))) => {
                    let sql = adapt_sql(
                        "SELECT r.uri, r.did, r.record, r.created_at FROM happyview_records r \
                         INNER JOIN happyview_record_refs ref ON ref.source_uri = r.uri \
                         WHERE ref.target_uri = ? AND ref.collection = ? AND r.did = ? \
                         AND (r.created_at < ? OR (r.created_at = ? AND r.uri < ?)) \
                         ORDER BY r.created_at DESC, r.uri DESC \
                         LIMIT ?",
                        backend,
                    );
                    sqlx::query_as(&sql)
                        .bind(&uri)
                        .bind(&collection)
                        .bind(did)
                        .bind(cursor_ts)
                        .bind(cursor_ts)
                        .bind(cursor_uri)
                        .bind(limit)
                        .fetch_all(&state.db)
                        .await
                        .map_err(|e| mlua::Error::runtime(format!("DB backlinks failed: {e}")))?
                }
                (Some(did), None) => {
                    let sql = adapt_sql(
                        "SELECT r.uri, r.did, r.record, r.created_at FROM happyview_records r \
                         INNER JOIN happyview_record_refs ref ON ref.source_uri = r.uri \
                         WHERE ref.target_uri = ? AND ref.collection = ? AND r.did = ? \
                         ORDER BY r.created_at DESC, r.uri DESC \
                         LIMIT ?",
                        backend,
                    );
                    sqlx::query_as(&sql)
                        .bind(&uri)
                        .bind(&collection)
                        .bind(did)
                        .bind(limit)
                        .fetch_all(&state.db)
                        .await
                        .map_err(|e| mlua::Error::runtime(format!("DB backlinks failed: {e}")))?
                }
                (None, Some((cursor_ts, cursor_uri))) => {
                    let sql = adapt_sql(
                        "SELECT r.uri, r.did, r.record, r.created_at FROM happyview_records r \
                         INNER JOIN happyview_record_refs ref ON ref.source_uri = r.uri \
                         WHERE ref.target_uri = ? AND ref.collection = ? \
                         AND (r.created_at < ? OR (r.created_at = ? AND r.uri < ?)) \
                         ORDER BY r.created_at DESC, r.uri DESC \
                         LIMIT ?",
                        backend,
                    );
                    sqlx::query_as(&sql)
                        .bind(&uri)
                        .bind(&collection)
                        .bind(cursor_ts)
                        .bind(cursor_ts)
                        .bind(cursor_uri)
                        .bind(limit)
                        .fetch_all(&state.db)
                        .await
                        .map_err(|e| mlua::Error::runtime(format!("DB backlinks failed: {e}")))?
                }
                (None, None) => {
                    let sql = adapt_sql(
                        "SELECT r.uri, r.did, r.record, r.created_at FROM happyview_records r \
                         INNER JOIN happyview_record_refs ref ON ref.source_uri = r.uri \
                         WHERE ref.target_uri = ? AND ref.collection = ? \
                         ORDER BY r.created_at DESC, r.uri DESC \
                         LIMIT ?",
                        backend,
                    );
                    sqlx::query_as(&sql)
                        .bind(&uri)
                        .bind(&collection)
                        .bind(limit)
                        .fetch_all(&state.db)
                        .await
                        .map_err(|e| mlua::Error::runtime(format!("DB backlinks failed: {e}")))?
                }
            };

            let has_next = rows_raw.len() as i64 == limit;

            let result_table = lua.create_table()?;

            if has_next && let Some((last_uri, _, _, last_created_at)) = rows_raw.last() {
                let cursor = encode_cursor(last_created_at, last_uri);
                result_table.set("cursor", cursor)?;
            }

            let records: Vec<Value> = rows_raw
                .into_iter()
                .map(|(uri, _did, record_str, _created_at)| {
                    let mut record: Value = serde_json::from_str(&record_str).unwrap_or(json!({}));
                    if let Some(obj) = record.as_object_mut() {
                        obj.insert("uri".to_string(), json!(uri));
                    }
                    record
                })
                .collect();

            let record_values: Vec<mlua::Value> = records
                .iter()
                .map(|r| lua.to_value(r))
                .collect::<LuaResult<_>>()?;
            let records_table = lua.create_sequence_from(record_values)?;
            records_table.set_metatable(Some(lua.array_metatable()))?;
            result_table.set("records", records_table)?;

            Ok(mlua::Value::Table(result_table))
        }
    })?;
    db_table.set("backlinks", backlinks_fn)?;

    // db.raw(sql, params?) -> rows[]
    let state_raw = state.clone();
    let raw_fn =
        lua.create_async_function(move |lua, (sql, params): (String, Option<mlua::Table>)| {
            let state = state_raw.clone();
            async move {
                let mut query = sqlx::query(&sql);
                if let Some(ref params_table) = params {
                    for value in params_table.sequence_values::<mlua::Value>() {
                        let value = value?;
                        query = match value {
                            mlua::Value::String(s) => query.bind(s.to_str()?.to_string()),
                            mlua::Value::Integer(n) => query.bind(n),
                            mlua::Value::Number(n) => query.bind(n),
                            mlua::Value::Boolean(b) => query.bind(if b { 1_i32 } else { 0_i32 }),
                            mlua::Value::Nil => query.bind(Option::<String>::None),
                            other => {
                                return Err(mlua::Error::runtime(format!(
                                    "unsupported parameter type: {}",
                                    other.type_name()
                                )));
                            }
                        };
                    }
                }

                let rows = query
                    .fetch_all(&state.db)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("db.raw query failed: {e}")))?;

                // Convert rows to Lua tables
                let mut lua_rows: Vec<mlua::Value> = Vec::with_capacity(rows.len());
                for row in &rows {
                    let row_table = lua.create_table()?;
                    for col in row.columns() {
                        let name = col.name();
                        let lua_val: mlua::Value = match row.try_get::<String, _>(name) {
                            Ok(s) => mlua::Value::String(lua.create_string(&s)?),
                            Err(_) => match row.try_get::<i64, _>(name) {
                                Ok(n) => mlua::Value::Integer(n),
                                Err(_) => match row.try_get::<i32, _>(name) {
                                    Ok(n) => mlua::Value::Integer(n as i64),
                                    Err(_) => match row.try_get::<f64, _>(name) {
                                        Ok(n) => mlua::Value::Number(n),
                                        Err(_) => match row.try_get::<bool, _>(name) {
                                            Ok(b) => mlua::Value::Boolean(b),
                                            Err(_) => mlua::Value::Nil,
                                        },
                                    },
                                },
                            },
                        };
                        row_table.set(name, lua_val)?;
                    }
                    lua_rows.push(mlua::Value::Table(row_table));
                }

                let result = lua.create_sequence_from(lua_rows)?;
                result.set_metatable(Some(lua.array_metatable()))?;
                Ok(result)
            }
        })?;
    db_table.set("raw", raw_fn)?;

    // db.backend() -> "sqlite" | "postgres"
    let backend = state.db_backend;
    let backend_fn = lua.create_function(move |_, ()| {
        Ok(match backend {
            DatabaseBackend::Sqlite => "sqlite",
            DatabaseBackend::Postgres => "postgres",
        })
    })?;
    db_table.set("backend", backend_fn)?;

    lua.globals().set("db", db_table)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db::DatabaseBackend;
    use crate::lexicon::LexiconRegistry;
    use tokio::sync::watch;

    fn test_state() -> AppState {
        let config = Config {
            host: "127.0.0.1".into(),
            port: 3000,
            database_url: String::new(),
            database_backend: crate::db::DatabaseBackend::Sqlite,
            public_url: String::new(),
            session_secret: "test-secret".into(),
            jetstream_url: String::new(),
            relay_url: String::new(),
            plc_url: String::new(),
            static_dir: String::new(),
            base_path: None,
            event_log_retention_days: 30,
            app_name: None,
            logo_uri: None,
            tos_uri: None,
            policy_uri: None,
            token_encryption_key: None,
            default_rate_limit_capacity: 100,
            default_rate_limit_refill_rate: 2.0,
        };
        let (tx, _) = watch::channel(vec![]);
        let (labeler_tx, _) = watch::channel(());
        sqlx::any::install_default_drivers();
        let test_db = sqlx::AnyPool::connect_lazy("sqlite::memory:").unwrap();
        let atrium_http = std::sync::Arc::new(atrium_oauth::DefaultHttpClient::default());
        let did_resolver = atrium_identity::did::CommonDidResolver::new(
            atrium_identity::did::CommonDidResolverConfig {
                plc_directory_url: "https://plc.directory".into(),
                http_client: std::sync::Arc::clone(&atrium_http),
            },
        );
        let handle_resolver = atrium_identity::handle::AtprotoHandleResolver::new(
            atrium_identity::handle::AtprotoHandleResolverConfig {
                dns_txt_resolver: crate::dns::NativeDnsResolver::new(),
                http_client: atrium_http,
            },
        );
        let oauth = atrium_oauth::OAuthClient::new(atrium_oauth::OAuthClientConfig {
            client_metadata: atrium_oauth::AtprotoLocalhostClientMetadata {
                redirect_uris: Some(vec!["http://127.0.0.1:0/auth/callback".into()]),
                scopes: Some(vec![atrium_oauth::Scope::Known(
                    atrium_oauth::KnownScope::Atproto,
                )]),
            },
            keys: None,
            state_store: crate::auth::oauth_store::DbStateStore::new(
                test_db.clone(),
                crate::db::DatabaseBackend::Sqlite,
            ),
            session_store: crate::auth::oauth_store::DbSessionStore::new(
                test_db.clone(),
                crate::db::DatabaseBackend::Sqlite,
            ),
            resolver: atrium_oauth::OAuthResolverConfig {
                did_resolver,
                handle_resolver,
                authorization_server_metadata: Default::default(),
                protected_resource_metadata: Default::default(),
            },
        })
        .expect("Failed to create test OAuth client");
        AppState {
            config,
            http: reqwest::Client::new(),
            db: test_db.clone(),
            backfill_db: test_db.clone(),
            db_backend: DatabaseBackend::Sqlite,
            domain_cache: crate::domain::DomainCache::new(),
            lexicons: LexiconRegistry::new(),
            collections_tx: tx,
            labeler_subscriptions_tx: labeler_tx,
            rate_limiter: crate::rate_limit::RateLimiter::new(
                crate::rate_limit::RateLimitDefaults {
                    query_cost: 1,
                    procedure_cost: 1,
                    proxy_cost: 1,
                },
            ),
            oauth: std::sync::Arc::new(crate::auth::OAuthClientRegistry::new(std::sync::Arc::new(
                oauth,
            ))),
            oauth_state_store: crate::auth::oauth_store::DbStateStore::new(
                test_db.clone(),
                crate::db::DatabaseBackend::Sqlite,
            ),
            cookie_key: axum_extra::extract::cookie::Key::derive_from(
                b"test-secret-for-tests-only-not-production",
            ),
            plugin_registry: std::sync::Arc::new(crate::plugin::PluginRegistry::new()),
            wasm_runtime: std::sync::Arc::new(
                crate::plugin::WasmRuntime::new().expect("wasm runtime"),
            ),
            attestation_signer: None,
            official_registry: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::plugin::official_registry::OfficialRegistryState::default(),
            )),
            official_registry_config: crate::plugin::official_registry::RegistryConfig::production(
            ),
            proxy_config: std::sync::Arc::new(arc_swap::ArcSwap::new(std::sync::Arc::new(
                crate::proxy_config::ProxyConfig::default(),
            ))),
            backfill_events_tx: tokio::sync::broadcast::channel(16).0,
            verbose_event_logging: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    fn setup(state: &AppState) -> Lua {
        let lua = Lua::new();
        register_db_api(&lua, Arc::new(state.clone())).unwrap();
        lua
    }

    #[tokio::test]
    async fn raw_allows_non_select() {
        let state = test_state();
        let lua = setup(&state);
        let result: Result<mlua::Value, _> = lua
            .load(r#"return db.raw("DELETE FROM happyview_records")"#)
            .eval_async()
            .await;
        // Should fail with a DB connection error, NOT a validation error
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            !err.contains("only supports SELECT"),
            "should have passed validation but got: {err}"
        );
    }

    #[tokio::test]
    async fn raw_allows_select() {
        let state = test_state();
        let lua = setup(&state);
        let result: Result<mlua::Value, _> =
            lua.load(r#"return db.raw("SELECT 1")"#).eval_async().await;
        // Should either succeed (SQLite in-memory) or fail with a DB connection error,
        // but NOT a validation error.
        if let Err(e) = &result {
            let err = e.to_string();
            assert!(
                !err.contains("only supports SELECT"),
                "should have passed validation but got: {err}"
            );
        }
    }

    #[test]
    fn valid_json_field_paths() {
        assert!(super::is_valid_json_field_path("name"));
        assert!(super::is_valid_json_field_path("author_name"));
        assert!(super::is_valid_json_field_path("author.handle"));
        assert!(super::is_valid_json_field_path("tags[0]"));
        assert!(super::is_valid_json_field_path("data[0][1]"));
        assert!(super::is_valid_json_field_path("author.websites[0].url"));
        assert!(super::is_valid_json_field_path("a.b.c.d.e"));
    }

    #[test]
    fn invalid_json_field_paths() {
        assert!(!super::is_valid_json_field_path(""));
        assert!(!super::is_valid_json_field_path(".name"));
        assert!(!super::is_valid_json_field_path("name."));
        assert!(!super::is_valid_json_field_path("name..foo"));
        assert!(!super::is_valid_json_field_path("[0]"));
        assert!(!super::is_valid_json_field_path("name[]"));
        assert!(!super::is_valid_json_field_path("name[abc]"));
        assert!(!super::is_valid_json_field_path("name; DROP TABLE"));
        assert!(!super::is_valid_json_field_path("name'OR 1=1"));
        assert!(!super::is_valid_json_field_path("na-me"));
    }

    #[tokio::test]
    async fn query_accepts_nested_sort_field() {
        let state = test_state();
        let lua = setup(&state);
        let result: Result<mlua::Value, _> = lua
            .load(r#"return db.query({ collection = "test", sort = "author.handle" })"#)
            .eval_async()
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            !err.contains("invalid sort field"),
            "nested sort field should be accepted, got: {err}"
        );
    }

    #[tokio::test]
    async fn query_accepts_array_index_sort_field() {
        let state = test_state();
        let lua = setup(&state);
        let result: Result<mlua::Value, _> = lua
            .load(r#"return db.query({ collection = "test", sort = "tags[0]" })"#)
            .eval_async()
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            !err.contains("invalid sort field"),
            "array index sort field should be accepted, got: {err}"
        );
    }

    #[tokio::test]
    async fn query_rejects_invalid_sort_field() {
        let state = test_state();
        let lua = setup(&state);
        let result: Result<mlua::Value, _> = lua
            .load(r#"return db.query({ collection = "test", sort = "name; DROP TABLE" })"#)
            .eval_async()
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("invalid sort field"),
            "expected sort field error, got: {err}"
        );
    }

    #[tokio::test]
    async fn query_rejects_invalid_sort_direction() {
        let state = test_state();
        let lua = setup(&state);
        let result: Result<mlua::Value, _> = lua
            .load(r#"return db.query({ collection = "test", sortDirection = "sideways" })"#)
            .eval_async()
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("invalid sortDirection"),
            "expected sortDirection error, got: {err}"
        );
    }

    #[test]
    fn cursor_round_trip() {
        let encoded = super::encode_cursor("2026-03-12T10:00:00Z", "at://did:plc:abc/col/rkey");
        let (ts, uri) = super::decode_cursor(&encoded).unwrap();
        assert_eq!(ts, "2026-03-12T10:00:00Z");
        assert_eq!(uri, "at://did:plc:abc/col/rkey");
    }

    #[test]
    fn decode_invalid_cursor_returns_none() {
        assert!(super::decode_cursor("not-valid-base64!!!").is_none());
    }

    #[test]
    fn decode_cursor_missing_pipe_returns_none() {
        use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
        let encoded = BASE64.encode("no-pipe-here");
        assert!(super::decode_cursor(&encoded).is_none());
    }

    // -----------------------------------------------------------------------
    // parse_filter_node / build_filter_sql
    // -----------------------------------------------------------------------

    fn make_condition_table(lua: &Lua, field: &str, op: &str, value: &str) -> mlua::Table {
        let t = lua.create_table().unwrap();
        t.set("field", field).unwrap();
        t.set("op", op).unwrap();
        t.set("value", value).unwrap();
        t
    }

    #[test]
    fn filter_simple_condition() {
        let lua = Lua::new();
        let t = make_condition_table(&lua, "name", "=", "alice");
        let node = parse_filter_node(&t, 0).unwrap();
        let mut binds = Vec::new();
        let sql = build_filter_sql(&node, &mut binds);
        assert_eq!(sql, "json_extract(record, '$.name') = ?");
        assert_eq!(binds, vec!["alice"]);
    }

    #[test]
    fn filter_defaults_op_to_equals() {
        let lua = Lua::new();
        let t = lua.create_table().unwrap();
        t.set("field", "status").unwrap();
        t.set("value", "active").unwrap();
        let node = parse_filter_node(&t, 0).unwrap();
        let mut binds = Vec::new();
        let sql = build_filter_sql(&node, &mut binds);
        assert_eq!(sql, "json_extract(record, '$.status') = ?");
    }

    #[test]
    fn filter_rejects_invalid_op() {
        let lua = Lua::new();
        let t = make_condition_table(&lua, "name", "DROP", "x");
        let err = parse_filter_node(&t, 0).unwrap_err();
        assert!(err.to_string().contains("invalid filter op"));
    }

    #[test]
    fn filter_rejects_invalid_field() {
        let lua = Lua::new();
        let t = make_condition_table(&lua, "name; DROP TABLE", "=", "x");
        let err = parse_filter_node(&t, 0).unwrap_err();
        assert!(err.to_string().contains("invalid filter field"));
    }

    #[test]
    fn filter_and_group() {
        let lua = Lua::new();
        let group = lua.create_table().unwrap();
        group.set("combine", "AND").unwrap();
        let c1 = make_condition_table(&lua, "status", "=", "active");
        let c2 = make_condition_table(&lua, "age", ">", "18");
        group.set(1, c1).unwrap();
        group.set(2, c2).unwrap();
        let node = parse_filter_node(&group, 0).unwrap();
        let mut binds = Vec::new();
        let sql = build_filter_sql(&node, &mut binds);
        assert_eq!(
            sql,
            "(json_extract(record, '$.status') = ? AND json_extract(record, '$.age') > ?)"
        );
        assert_eq!(binds, vec!["active", "18"]);
    }

    #[test]
    fn filter_or_group() {
        let lua = Lua::new();
        let group = lua.create_table().unwrap();
        group.set("combine", "OR").unwrap();
        let c1 = make_condition_table(&lua, "role", "=", "admin");
        let c2 = make_condition_table(&lua, "role", "=", "mod");
        group.set(1, c1).unwrap();
        group.set(2, c2).unwrap();
        let node = parse_filter_node(&group, 0).unwrap();
        let mut binds = Vec::new();
        let sql = build_filter_sql(&node, &mut binds);
        assert_eq!(
            sql,
            "(json_extract(record, '$.role') = ? OR json_extract(record, '$.role') = ?)"
        );
        assert_eq!(binds, vec!["admin", "mod"]);
    }

    #[test]
    fn filter_single_child_group_unwraps() {
        let lua = Lua::new();
        let group = lua.create_table().unwrap();
        group.set("combine", "AND").unwrap();
        let c1 = make_condition_table(&lua, "x", "=", "1");
        group.set(1, c1).unwrap();
        let node = parse_filter_node(&group, 0).unwrap();
        let mut binds = Vec::new();
        let sql = build_filter_sql(&node, &mut binds);
        assert_eq!(sql, "json_extract(record, '$.x') = ?");
    }

    #[test]
    fn filter_rejects_invalid_combine() {
        let lua = Lua::new();
        let group = lua.create_table().unwrap();
        group.set("combine", "XOR").unwrap();
        let c1 = make_condition_table(&lua, "x", "=", "1");
        group.set(1, c1).unwrap();
        let err = parse_filter_node(&group, 0).unwrap_err();
        assert!(err.to_string().contains("invalid filter combine"));
    }

    #[test]
    fn filter_rejects_empty_group() {
        let lua = Lua::new();
        let group = lua.create_table().unwrap();
        group.set("combine", "AND").unwrap();
        let err = parse_filter_node(&group, 0).unwrap_err();
        assert!(err.to_string().contains("filter group has no conditions"));
    }

    #[test]
    fn filter_rejects_excessive_depth() {
        let lua = Lua::new();
        let c = make_condition_table(&lua, "x", "=", "1");
        let err = parse_filter_node(&c, MAX_FILTER_DEPTH).unwrap_err();
        assert!(err.to_string().contains("filter nesting too deep"));
    }

    #[test]
    fn filter_accepts_all_ops() {
        let lua = Lua::new();
        for op in ALLOWED_OPS {
            let t = make_condition_table(&lua, "field", op, "val");
            assert!(
                parse_filter_node(&t, 0).is_ok(),
                "op '{op}' should be accepted"
            );
        }
    }

    #[test]
    fn filter_op_case_insensitive() {
        let lua = Lua::new();
        let t = make_condition_table(&lua, "name", "like", "alice%");
        let node = parse_filter_node(&t, 0).unwrap();
        let mut binds = Vec::new();
        let sql = build_filter_sql(&node, &mut binds);
        assert_eq!(sql, "json_extract(record, '$.name') LIKE ?");
    }

    #[test]
    fn filter_integer_value() {
        let lua = Lua::new();
        let t = lua.create_table().unwrap();
        t.set("field", "count").unwrap();
        t.set("op", ">").unwrap();
        t.set("value", 42).unwrap();
        let node = parse_filter_node(&t, 0).unwrap();
        let mut binds = Vec::new();
        build_filter_sql(&node, &mut binds);
        assert_eq!(binds, vec!["42"]);
    }

    #[test]
    fn filter_boolean_value() {
        let lua = Lua::new();
        let t = lua.create_table().unwrap();
        t.set("field", "active").unwrap();
        t.set("value", true).unwrap();
        let node = parse_filter_node(&t, 0).unwrap();
        let mut binds = Vec::new();
        build_filter_sql(&node, &mut binds);
        assert_eq!(binds, vec!["true"]);
    }

    #[test]
    fn filter_nested_field_path() {
        let lua = Lua::new();
        let t = make_condition_table(&lua, "author.websites[0].url", "=", "https://example.com");
        let node = parse_filter_node(&t, 0).unwrap();
        let mut binds = Vec::new();
        let sql = build_filter_sql(&node, &mut binds);
        assert_eq!(sql, "json_extract(record, '$.author.websites[0].url') = ?");
    }

    // -----------------------------------------------------------------------
    // query sort direction
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn query_accepts_valid_sort_direction() {
        let state = test_state();
        let lua = setup(&state);
        let result: Result<mlua::Value, _> = lua
            .load(r#"return db.query({ collection = "test", sortDirection = "asc" })"#)
            .eval_async()
            .await;
        // Should fail with a DB connection error, NOT a validation error
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            !err.contains("invalid sortDirection"),
            "should have passed validation but got: {err}"
        );
    }
}

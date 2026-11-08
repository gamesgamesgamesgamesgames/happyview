use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use mlua::{Lua, LuaSerdeExt, Result as LuaResult};
use serde_json::{Value, json};
use sqlx::{Column, Row};
use std::sync::Arc;

use crate::AppState;
use crate::db::{DatabaseBackend, adapt_sql, decode_cursor, encode_cursor};

const MAX_FILTER_DEPTH: u8 = 5;
const ALLOWED_OPS: &[&str] = &["=", "!=", "<", ">", "<=", ">=", "LIKE", "NOT LIKE"];

/// Table-name prefix reserved for HappyView's own internal tables. `db.raw`
/// blocks these **by default** — so a table added in a future migration is
/// protected until it is deliberately allowed — except for the data tables in
/// [`ALLOWED_INTERNAL_TABLES`].
const PROTECTED_TABLE_PREFIX: &str = "happyview_";

/// Internal tables that don't carry the `happyview_` prefix but are still
/// off-limits (SQLx's migration bookkeeping).
const PROTECTED_EXACT_TABLES: &[&str] = &["_sqlx_migrations"];

/// Internal tables `db.raw` is allowed to read and write despite the reserved
/// prefix: public AppView data and space *data*. Everything else `happyview_*`
/// stays blocked — secrets and tokens (`happyview_dpop_keys`/`_sessions`,
/// `happyview_api_keys`, `happyview_oauth_*`, `happyview_script_variables`),
/// auth/privilege state (`happyview_users`/`_user_permissions`, the delegation
/// tables), trust config (`happyview_domains`, `happyview_instance_settings`),
/// and cryptographic material (`happyview_space_credentials`, and
/// `happyview_space_repo_state` which holds commit-signature key material).
///
/// Space membership/records are exposed because a space defines *access*, not
/// confidentiality — whether to expose otherwise-private space data through the
/// AppView is left to the admin.
const ALLOWED_INTERNAL_TABLES: &[&str] = &[
    // Public AppView data.
    "happyview_records",
    "happyview_record_refs",
    "happyview_labels",
    "happyview_lexicons",
    // Background jobs.
    "happyview_jobs",
    // Space data (not the credential/key-material tables).
    "happyview_spaces",
    "happyview_space_members",
    "happyview_space_records",
    "happyview_space_record_oplog",
    "happyview_space_notify_registrations",
    "happyview_space_dids",
];

/// Reject a `db.raw` SQL string that references a protected internal table.
///
/// Tokenizes the SQL (so string literals and comments containing the prefix are
/// ignored, and quoted / schema-qualified identifiers are still caught) and
/// blocks any `happyview_*` (or `_sqlx_migrations`) identifier that is not in
/// [`ALLOWED_INTERNAL_TABLES`]. Unicode-escaped identifiers (`U&"…"`) are refused
/// outright as an evasion vector, and SQL that cannot be tokenized fails closed.
fn check_raw_sql_tables(sql: &str) -> Result<(), String> {
    use sqlparser::dialect::GenericDialect;
    use sqlparser::tokenizer::{Token, Tokenizer};

    // `U&'…'` / `U&"…"` unicode-escaped literals could smuggle a protected
    // identifier past tokenization (the escapes decode to letters); there is no
    // legitimate need for them in `db.raw`, so refuse them outright.
    let lowered = sql.to_ascii_lowercase();
    if lowered.contains("u&\"") || lowered.contains("u&'") {
        return Err("db.raw does not allow unicode-escaped identifiers".into());
    }

    // Tokenizing (rather than substring matching) means the prefix inside string
    // literals or comments is ignored, while quoted and schema-qualified
    // identifiers are still seen. SQL we cannot tokenize fails closed.
    let tokens = Tokenizer::new(&GenericDialect {}, sql)
        .tokenize()
        .map_err(|e| format!("db.raw could not parse SQL: {e}"))?;

    for token in tokens {
        if let Token::Word(word) = token {
            let name = word.value.to_ascii_lowercase();
            let is_internal = name.starts_with(PROTECTED_TABLE_PREFIX)
                || PROTECTED_EXACT_TABLES.contains(&name.as_str());
            if is_internal && !ALLOWED_INTERNAL_TABLES.contains(&name.as_str()) {
                return Err(format!(
                    "db.raw cannot reference the protected internal HappyView table '{}'",
                    word.value
                ));
            }
        }
    }

    Ok(())
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
        if !crate::db::is_valid_json_field_path(&field) {
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
                && !crate::db::is_valid_json_field_path(field)
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
                let mut q = crate::db::query_as(&sql).bind(&collection);
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
                let mut q = crate::db::query_as::<RowType>(&sql).bind(&collection);
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
            let row: Option<(String,)> = crate::db::query_as(&sql)
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

            if !crate::db::is_valid_json_field_path(&field) {
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
                    crate::db::query_as(&sql)
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
                    crate::db::query_as(&sql)
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
                    crate::db::query_as(&sql)
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
                    crate::db::query_as(&sql)
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
                    crate::db::query_as(&sql)
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
                    crate::db::query_as(&sql)
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
                    crate::db::query_as(&sql)
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
                    crate::db::query_as(&sql)
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
                // Refused: PLUGIN_SQL_DATABASE_URL was set but the login pool is
                // unusable, or the plugin schema holds code from a login-pool
                // session. Refuse db.raw rather than run it on the privileged
                // connection.
                if matches!(state.plugin_sql, crate::PluginSqlAccess::Refused) {
                    return Err(mlua::Error::runtime(
                        "db.raw is unavailable on this instance",
                    ));
                }
                // Protect HappyView's internal tables (secrets, auth, config,
                // AppView bookkeeping) from raw access; own tables are fine.
                check_raw_sql_tables(&sql).map_err(mlua::Error::runtime)?;

                let mut query = crate::db::query(&sql);
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

                let rows =
                    if let crate::PluginSqlAccess::LoginPool(login_pool) = &state.plugin_sql {
                        // The dedicated login pool: its connections are logged in as
                        // the unprivileged `happyview_plugin` role, so `session_user`
                        // is never the privileged HappyView role and there is no
                        // privileged role for a `RESET ROLE` to return to. `DISCARD
                        // ALL` on release keeps nothing on the pooled connection. The
                        // transaction pins `search_path` to the plugin schema first.
                        let mut tx = login_pool.begin().await.map_err(|e| {
                            mlua::Error::runtime(format!("db.raw query failed: {e}"))
                        })?;
                        crate::db::query(&format!(
                            "SET LOCAL search_path = {PLUGIN_SCHEMA}, public"
                        ))
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| mlua::Error::runtime(format!("db.raw query failed: {e}")))?;
                        let rows = query.fetch_all(&mut *tx).await.map_err(|e| {
                            mlua::Error::runtime(format!("db.raw query failed: {e}"))
                        })?;
                        tx.commit().await.map_err(|e| {
                            mlua::Error::runtime(format!("db.raw query failed: {e}"))
                        })?;
                        rows
                    } else {
                        // No login pool configured: run on the main connection behind
                        // the tokenizer guard above, as before.
                        query.fetch_all(&state.db).await.map_err(|e| {
                            mlua::Error::runtime(format!("db.raw query failed: {e}"))
                        })?
                    };

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

// -- Least-privileged `db.raw` (Postgres) -----------------------------------
//
// On Postgres the guard in `check_raw_sql_tables` works on identifiers only, so
// it is not the real boundary (SQL hidden in a string a built-in executes never
// appears as an identifier, and nothing stops a `RESET ROLE`). `db.raw` therefore
// runs on a dedicated pool that logs in **as** the unprivileged `happyview_plugin`
// role: its session user has no access to HappyView's tables and there is no
// privileged role to switch back to, so anything off the allowlisted tables is
// denied by the database however the statement is written. The role's own tables
// live in a dedicated `happyview_plugin` schema, first on its `search_path` and
// never on HappyView's, so it cannot define an object HappyView would resolve.
// When no login pool is configured (`PLUGIN_SQL_DATABASE_URL` unset), `db.raw`
// keeps running on the main connection behind the existing guard. SQLite is
// unchanged.

/// The unprivileged role the login pool connects as.
pub const PLUGIN_ROLE: &str = "happyview_plugin";
/// The schema that role creates its own tables in.
pub const PLUGIN_SCHEMA: &str = "happyview_plugin";

/// Ensure the plugin schema exists and the `happyview_plugin` login role is
/// granted exactly the allowlisted internal tables (and the plugin schema it
/// creates its own objects in). Postgres only, idempotent, and fails soft: the
/// login role is provisioned out of band (it needs a password), so when it is
/// absent the grants are swallowed and `db.raw` falls back to the main-pool
/// guard. The schema is owned by the connecting HappyView role, which grants the
/// login role CREATE on it, so no role membership is required.
pub async fn ensure_plugin_schema(pool: &sqlx::AnyPool, backend: crate::db::DatabaseBackend) {
    if backend != crate::db::DatabaseBackend::Postgres {
        return;
    }
    for stmt in plugin_schema_setup_statements() {
        if let Err(e) = crate::db::query(&stmt).execute(pool).await {
            tracing::debug!(error = %e, "db.raw plugin schema setup statement did not apply");
        }
    }
}

/// Open the dedicated login pool for `db.raw` from `url` (a connection string for
/// the `happyview_plugin` login role). Each released connection is reset with
/// `DISCARD ALL`, so nothing a statement left on the session outlives it.
///
/// **The role is verified, not trusted.** A URL that merely connects is not
/// enough: if it named a privileged role, `db.raw` would run on a privileged
/// session user — the condition the pool exists to rule out. After connecting,
/// the session user is checked against `main_role` and refused unless it is
/// unprivileged, not the main role or a member of it, has no access to the
/// protected tables and no `CREATE` on `public`. Returns `None` (logged) when it
/// cannot connect or fails the check.
pub async fn open_plugin_login_pool(url: &str, main_role: &str) -> Option<sqlx::AnyPool> {
    sqlx::any::install_default_drivers();
    let pool = sqlx::any::AnyPoolOptions::new()
        .max_connections(8)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .idle_timeout(std::time::Duration::from_secs(300))
        .after_release(|conn, _meta| {
            Box::pin(async move {
                crate::db::query("DISCARD ALL").execute(&mut *conn).await?;
                Ok(true)
            })
        })
        .connect(url)
        .await
        .map_err(|e| tracing::warn!(error = %e, "could not open the db.raw login pool"))
        .ok()?;
    if !login_role_is_unprivileged(&pool, main_role).await {
        return None;
    }
    Some(pool)
}

/// Verify the pool's session user is a safe least-privileged role. `to_regclass`
/// guards against a protected table being absent (a missing table reads as "no
/// privilege", never an error). Any failure refuses the mode.
async fn login_role_is_unprivileged(pool: &sqlx::AnyPool, main_role: &str) -> bool {
    use sqlx::Row;
    let sql = "SELECT (\
        NOT r.rolsuper AND NOT r.rolcreaterole AND NOT r.rolcreatedb \
        AND NOT r.rolreplication AND NOT r.rolbypassrls \
        AND session_user <> $1 \
        AND NOT pg_has_role(session_user, $1, 'MEMBER') \
        AND NOT COALESCE(has_table_privilege(session_user, to_regclass('happyview_users'), 'SELECT'), false) \
        AND NOT COALESCE(has_table_privilege(session_user, to_regclass('happyview_api_keys'), 'SELECT'), false) \
        AND NOT COALESCE(has_table_privilege(session_user, to_regclass('happyview_dpop_keys'), 'SELECT'), false) \
        AND NOT COALESCE(has_table_privilege(session_user, to_regclass('happyview_oauth_sessions'), 'SELECT'), false) \
        AND NOT has_schema_privilege(session_user, 'public', 'CREATE') \
    ) AS ok FROM pg_roles r WHERE r.rolname = session_user";
    let ok = match crate::db::query(sql)
        .bind(main_role)
        .fetch_optional(pool)
        .await
    {
        Ok(Some(row)) => row.try_get::<bool, _>("ok").unwrap_or(false),
        _ => false,
    };
    if !ok {
        tracing::error!(
            "PLUGIN_SQL_DATABASE_URL names a role that is not a safe least-privileged plugin \
             role; refusing the db.raw login pool. Point it at the unprivileged \
             'happyview_plugin' role."
        );
    }
    ok
}

/// Whether the plugin schema holds any function, procedure or (non-internal)
/// trigger left from a login-pool session. Postgres only; a query error reads as
/// "present" (fail closed).
pub async fn plugin_schema_has_code(
    pool: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
) -> bool {
    use sqlx::Row;
    if backend != crate::db::DatabaseBackend::Postgres {
        return false;
    }
    let sql = "SELECT EXISTS (\
        SELECT 1 FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
          WHERE n.nspname = 'happyview_plugin' \
        UNION ALL \
        SELECT 1 FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid \
          JOIN pg_namespace n ON n.oid = c.relnamespace \
          WHERE n.nspname = 'happyview_plugin' AND NOT t.tgisinternal \
    ) AS present";
    match crate::db::query(sql).fetch_optional(pool).await {
        Ok(Some(row)) => row.try_get::<bool, _>("present").unwrap_or(true),
        Ok(None) => false,
        Err(e) => {
            tracing::error!(error = %e, "could not check the plugin schema for code; failing closed");
            true
        }
    }
}

fn plugin_schema_setup_statements() -> Vec<String> {
    let mut stmts = vec![
        // The plugin's own objects live here, first on its search_path and never
        // on HappyView's — so nothing it defines can shadow an object HappyView
        // resolves (the CVE-2018-1058 class). The schema is owned by the
        // HappyView role; the login role is granted CREATE on it.
        format!("CREATE SCHEMA IF NOT EXISTS {PLUGIN_SCHEMA}"),
        format!("GRANT USAGE, CREATE ON SCHEMA {PLUGIN_SCHEMA} TO {PLUGIN_ROLE}"),
        format!("GRANT USAGE ON SCHEMA public TO {PLUGIN_ROLE}"),
        format!("ALTER ROLE {PLUGIN_ROLE} SET search_path = {PLUGIN_SCHEMA}, public"),
        format!("GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO {PLUGIN_ROLE}"),
        // Caller-owned tables already in public are owned by the HappyView role;
        // grant the login role access so they stay reachable. New caller tables
        // are created in the dedicated schema instead.
        format!(
            "DO $$ DECLARE r record; BEGIN \
             FOR r IN SELECT c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = 'public' AND c.relkind IN ('r','p','v') \
             AND c.relname NOT LIKE 'happyview\\_%' AND c.relname <> '_sqlx_migrations' LOOP \
             EXECUTE format('GRANT SELECT, INSERT, UPDATE, DELETE ON public.%I TO {PLUGIN_ROLE}', r.relname); \
             END LOOP; END $$"
        ),
    ];
    for rel in ALLOWED_INTERNAL_TABLES {
        stmts.push(format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON {rel} TO {PLUGIN_ROLE}"
        ));
    }
    stmts
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
            sqlite_journal_size_limit: crate::db::DEFAULT_JOURNAL_SIZE_LIMIT,
            public_url: String::new(),
            user_agent: String::new(),
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
            telemetry_collector_url: String::new(),
            platform_api_key_hash: None,
            job_worker_concurrency: 1,
        };
        let (tx, _) = watch::channel(vec![]);
        let (labeler_tx, _) = watch::channel(());
        sqlx::any::install_default_drivers();
        let test_db = sqlx::AnyPool::connect_lazy("sqlite::memory:").unwrap();
        let atrium_http = std::sync::Arc::new(crate::http_retry::HappyViewHttpClient::default());
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
                None,
            ),
            session_store: crate::auth::oauth_store::DbSessionStore::new(
                test_db.clone(),
                crate::db::DatabaseBackend::Sqlite,
                None,
            ),
            resolver: atrium_oauth::OAuthResolverConfig {
                did_resolver,
                handle_resolver,
                authorization_server_metadata: Default::default(),
                protected_resource_metadata: Default::default(),
            },
            http_client: crate::http_retry::HappyViewHttpClient::default(),
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
            oauth: std::sync::Arc::new(crate::auth::OAuthClientRegistry::new(
                std::sync::Arc::new(oauth),
                None,
            )),
            oauth_state_store: crate::auth::oauth_store::DbStateStore::new(
                test_db.clone(),
                crate::db::DatabaseBackend::Sqlite,
                None,
            ),
            linked_repos_client: std::sync::Arc::new(
                crate::linked_repos::client::build(
                    "https://plc.directory",
                    "http://127.0.0.1:0/oauth-client-metadata.json",
                    "http://127.0.0.1:0",
                    "http://127.0.0.1:0/auth/callback".into(),
                    true,
                    vec![atrium_oauth::Scope::Known(
                        atrium_oauth::KnownScope::Atproto,
                    )],
                    crate::auth::oauth_store::DbStateStore::new(
                        test_db.clone(),
                        crate::db::DatabaseBackend::Sqlite,
                        None,
                    ),
                    test_db.clone(),
                    crate::db::DatabaseBackend::Sqlite,
                    None,
                    None,
                )
                .expect("Failed to create test linked-repo OAuth client"),
            ),
            linked_repos_client_kid: None,
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
            client_jwks: Vec::new(),
            telemetry_counters: std::sync::Arc::new(crate::telemetry::counters::Counters::new()),
            plugin_sql: crate::PluginSqlAccess::Guard,
        }
    }

    fn setup(state: &AppState) -> Lua {
        let lua = Lua::new();
        register_db_api(&lua, Arc::new(state.clone())).unwrap();
        lua
    }

    #[tokio::test]
    async fn raw_allows_non_select_on_own_tables() {
        let state = test_state();
        let lua = setup(&state);
        // Non-SELECT statements are allowed against non-internal tables. Passes
        // table validation; may then fail on the (empty in-memory) DB.
        let result: Result<mlua::Value, _> = lua
            .load(r#"return db.raw("DELETE FROM my_table")"#)
            .eval_async()
            .await;
        if let Err(e) = &result {
            let err = e.to_string();
            assert!(
                !err.contains("internal HappyView table"),
                "should have passed table validation but got: {err}"
            );
        }
    }

    #[tokio::test]
    async fn raw_blocks_internal_tables() {
        let state = test_state();
        let lua = setup(&state);
        let result: Result<mlua::Value, _> = lua
            .load(r#"return db.raw("SELECT * FROM happyview_dpop_keys")"#)
            .eval_async()
            .await;
        let err = result.expect_err("querying an internal table must be rejected");
        assert!(
            err.to_string().contains("internal HappyView table"),
            "expected an internal-table error, got: {err}"
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
    fn raw_sql_allows_non_protected_tables() {
        // Admins can get wild with their own tables.
        assert!(super::check_raw_sql_tables("SELECT * FROM my_table").is_ok());
        assert!(super::check_raw_sql_tables("CREATE TABLE analytics (id INT)").is_ok());
        assert!(super::check_raw_sql_tables("INSERT INTO analytics VALUES (1)").is_ok());
        assert!(super::check_raw_sql_tables("UPDATE analytics SET id = 2").is_ok());
        assert!(super::check_raw_sql_tables("DELETE FROM analytics WHERE id = 1").is_ok());
        assert!(super::check_raw_sql_tables("DROP TABLE analytics").is_ok());
        // A table that merely *contains* the prefix mid-name is fine.
        assert!(super::check_raw_sql_tables("SELECT * FROM myhappyview_data").is_ok());
        // The prefix appearing inside a string literal is not a table reference.
        assert!(
            super::check_raw_sql_tables("INSERT INTO logs (msg) VALUES ('happyview_started')")
                .is_ok()
        );
    }

    #[test]
    fn raw_sql_allows_allowlisted_internal_tables() {
        // Public AppView data and background jobs are readable/writable.
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_records").is_ok());
        assert!(
            super::check_raw_sql_tables("DELETE FROM happyview_records WHERE uri = $1").is_ok()
        );
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_record_refs").is_ok());
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_labels").is_ok());
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_lexicons").is_ok());
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_jobs").is_ok());
        // Space data (access, not confidentiality).
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_space_records").is_ok());
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_space_members").is_ok());
    }

    #[test]
    fn raw_sql_blocks_protected_tables() {
        // Secrets / tokens / keys.
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_dpop_keys").is_err());
        assert!(super::check_raw_sql_tables("DROP TABLE happyview_api_keys").is_err());
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_script_variables").is_err());
        // Auth / privilege / trust config.
        assert!(super::check_raw_sql_tables("UPDATE happyview_users SET is_super = true").is_err());
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_domains").is_err());
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_instance_settings").is_err());
        // Space credential / key-material tables stay blocked even though other
        // space tables are allowed.
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_space_credentials").is_err());
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_space_repo_state").is_err());
        // The migration bookkeeping table is off-limits too.
        assert!(super::check_raw_sql_tables("SELECT * FROM _sqlx_migrations").is_err());
    }

    #[test]
    fn raw_sql_blocks_protected_tables_evasion() {
        // Case-insensitive.
        assert!(super::check_raw_sql_tables("SELECT * FROM HAPPYVIEW_USERS").is_err());
        // Double-quoted identifier.
        assert!(super::check_raw_sql_tables(r#"SELECT * FROM "happyview_api_keys""#).is_err());
        // Schema-qualified.
        assert!(
            super::check_raw_sql_tables("SELECT * FROM public.happyview_dpop_sessions").is_err()
        );
        // Second statement in a batch.
        assert!(super::check_raw_sql_tables("SELECT 1; SELECT * FROM happyview_users").is_err());
        // JOIN / subquery position.
        assert!(
            super::check_raw_sql_tables(
                "SELECT * FROM my_table JOIN happyview_api_clients USING (id)"
            )
            .is_err()
        );
        // Unicode-escaped identifier evasion is refused outright.
        assert!(super::check_raw_sql_tables(r#"SELECT * FROM U&"happyview_dpop_keys""#).is_err());
    }

    #[test]
    fn valid_json_field_paths() {
        assert!(crate::db::is_valid_json_field_path("name"));
        assert!(crate::db::is_valid_json_field_path("author_name"));
        assert!(crate::db::is_valid_json_field_path("author.handle"));
        assert!(crate::db::is_valid_json_field_path("tags[0]"));
        assert!(crate::db::is_valid_json_field_path("data[0][1]"));
        assert!(crate::db::is_valid_json_field_path(
            "author.websites[0].url"
        ));
        assert!(crate::db::is_valid_json_field_path("a.b.c.d.e"));
    }

    #[test]
    fn invalid_json_field_paths() {
        assert!(!crate::db::is_valid_json_field_path(""));
        assert!(!crate::db::is_valid_json_field_path(".name"));
        assert!(!crate::db::is_valid_json_field_path("name."));
        assert!(!crate::db::is_valid_json_field_path("name..foo"));
        assert!(!crate::db::is_valid_json_field_path("[0]"));
        assert!(!crate::db::is_valid_json_field_path("name[]"));
        assert!(!crate::db::is_valid_json_field_path("name[abc]"));
        assert!(!crate::db::is_valid_json_field_path("name; DROP TABLE"));
        assert!(!crate::db::is_valid_json_field_path("name'OR 1=1"));
        assert!(!crate::db::is_valid_json_field_path("na-me"));
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

    /// The restricted role gets its own schema, placed first on its search_path
    /// and never on HappyView's, so an object it creates cannot be resolved by
    /// HappyView's own queries. It is granted the allowlisted internal tables and
    /// no others.
    #[test]
    fn plugin_role_setup_uses_a_dedicated_schema_first_on_the_path() {
        let stmts = super::plugin_schema_setup_statements();
        let joined = stmts.join("\n");
        assert!(
            stmts.iter().any(|s| s.contains(&format!(
                "CREATE SCHEMA IF NOT EXISTS {}",
                super::PLUGIN_SCHEMA
            ))),
            "creates the dedicated schema:\n{joined}"
        );
        assert!(
            stmts.iter().any(|s| s.contains(&format!(
                "ALTER ROLE {} SET search_path = {}, public",
                super::PLUGIN_ROLE,
                super::PLUGIN_SCHEMA
            ))),
            "puts the dedicated schema first on the role's search_path:\n{joined}"
        );
        // Granted an allowlisted internal table, and not a protected one.
        assert!(stmts.iter().any(|s| s.contains("happyview_records")));
        assert!(
            !joined.contains("happyview_users") && !joined.contains("happyview_oauth"),
            "must not grant protected tables:\n{joined}"
        );
    }
}

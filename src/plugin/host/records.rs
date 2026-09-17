//! Record and table queries for plugins and the Lua `db` global. Every piece
//! of SQL that touches `happyview_records` on a guest's behalf is generated
//! here, so a plugin never writes SQL for records and the schema can change
//! in one place.

use serde_json::{Value, json};

use super::db::row_to_json;
use crate::db::{DatabaseBackend, adapt_sql, decode_cursor, encode_cursor};
use crate::raw_sql_guard::check_raw_sql_tables;
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use happyview_plugin_sdk::wire::{
    BacklinksQuery, Filter, IndexPut, RecordEnvelope, RecordRef, RecordsCount, RecordsPage,
    RecordsQuery, RecordsSearch, Sort, TableQuery,
};

pub const MAX_FILTER_DEPTH: u8 = 5;
pub const DEFAULT_LIMIT: u32 = 20;
pub const MAX_LIMIT: u32 = 100;

/// Columns `records_query`'s sort accepts bare, as opposed to a JSON path
/// evaluated inside `record`.
const TOP_LEVEL_COLUMNS: [&str; 3] = ["indexed_at", "did", "uri"];

#[derive(Debug, thiserror::Error)]
pub enum RecordsError {
    #[error("{0}")]
    InvalidSpec(String),
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
}

fn invalid(msg: impl Into<String>) -> RecordsError {
    RecordsError::InvalidSpec(msg.into())
}

/// A JSON field path as used in a record filter or sort: dot-separated
/// identifier segments with optional numeric array indices (`author.handle`,
/// `tags[0]`). Guards against SQL injection since the path is interpolated
/// directly into a `json_extract`/`->` expression rather than bound.
pub fn is_valid_json_field_path(path: &str) -> bool {
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

/// A bare SQL identifier: a table or column name used unquoted, as opposed to
/// a JSON path evaluated inside a `record` column.
pub fn is_valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `ilike` is Postgres-only; SQLite's `LIKE` is already case-insensitive for
/// ASCII, so it lowers to plain `LIKE` there.
pub fn normalize_op(op: &str, backend: DatabaseBackend) -> Result<&'static str, RecordsError> {
    Ok(match op.to_ascii_uppercase().as_str() {
        "=" => "=",
        "!=" => "!=",
        "<" => "<",
        ">" => ">",
        "<=" => "<=",
        ">=" => ">=",
        "LIKE" => "LIKE",
        "NOT LIKE" => "NOT LIKE",
        "ILIKE" => match backend {
            DatabaseBackend::Sqlite => "LIKE",
            DatabaseBackend::Postgres => "ILIKE",
        },
        other => {
            return Err(invalid(format!(
                "invalid filter op '{other}': must be one of =, !=, <, >, <=, >=, like, not like, ilike"
            )));
        }
    })
}

/// Render a filter tree to SQL, pushing bind values in placeholder order.
/// `field_sql` turns a field name into the column or JSON-path expression
/// for whatever table the caller is querying, and rejects names it does not
/// accept.
pub fn filter_sql(
    filter: &Filter,
    backend: DatabaseBackend,
    field_sql: &dyn Fn(&str) -> Result<String, RecordsError>,
    depth: u8,
    binds: &mut Vec<Value>,
) -> Result<String, RecordsError> {
    if depth >= MAX_FILTER_DEPTH {
        return Err(invalid(format!(
            "filter nesting too deep (max {MAX_FILTER_DEPTH} levels)"
        )));
    }
    match filter {
        Filter::Condition(c) => {
            let column = field_sql(&c.field)?;
            let op = normalize_op(&c.op, backend)?;
            binds.push(c.value.clone());
            Ok(format!("{column} {op} ?"))
        }
        Filter::Group {
            combine,
            conditions,
        } => {
            let combine = match combine.to_ascii_uppercase().as_str() {
                "AND" => "AND",
                "OR" => "OR",
                other => {
                    return Err(invalid(format!(
                        "invalid filter combine '{other}': must be 'and' or 'or'"
                    )));
                }
            };
            if conditions.is_empty() {
                return Err(invalid("filter group has no conditions"));
            }
            let parts = conditions
                .iter()
                .map(|c| filter_sql(c, backend, field_sql, depth + 1, binds))
                .collect::<Result<Vec<_>, _>>()?;
            if parts.len() == 1 {
                Ok(parts.into_iter().next().expect("one part"))
            } else {
                Ok(format!("({})", parts.join(&format!(" {combine} "))))
            }
        }
    }
}

fn record_field_sql(path: &str) -> Result<String, RecordsError> {
    if !is_valid_json_field_path(path) {
        return Err(invalid(format!(
            "invalid field '{path}': use alphanumeric names with optional dot notation and array indices (e.g. 'name', 'author.handle', 'tags[0]')"
        )));
    }
    Ok(format!("json_extract(record, '$.{path}')"))
}

fn column_sql(name: &str) -> Result<String, RecordsError> {
    if !is_valid_identifier(name) {
        return Err(invalid(format!("invalid column name '{name}'")));
    }
    Ok(name.to_string())
}

fn direction_sql(direction: &str) -> Result<&'static str, RecordsError> {
    match direction.to_ascii_lowercase().as_str() {
        "asc" => Ok("ASC"),
        "desc" => Ok("DESC"),
        other => Err(invalid(format!(
            "invalid sort direction '{other}': must be 'asc' or 'desc'"
        ))),
    }
}

fn clamp_limit(limit: Option<u32>) -> i64 {
    i64::from(limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT))
}

/// Record filters bind as text; the rationale is on the SDK's `Condition`.
fn bind_as_text(value: Value) -> String {
    match value {
        Value::String(s) => s,
        other => other.to_string(),
    }
}

type AnyQuery<'q> = sqlx::query::Query<'q, sqlx::Any, sqlx::any::AnyArguments>;
type AnyCountQuery<'q> = sqlx::query::QueryAs<'q, sqlx::Any, (i64,), sqlx::any::AnyArguments>;

/// Table filters bind by JSON type: a number as `i64`, or `f64` when it does
/// not fit; a boolean as `bool`; a string as text. The rationale is on the
/// SDK's `Condition`.
fn bind_value<'q>(query: AnyQuery<'q>, value: &Value) -> AnyQuery<'q> {
    match value {
        Value::Number(n) if n.is_i64() => query.bind(n.as_i64().expect("checked is_i64")),
        Value::Number(n) => query.bind(n.as_f64().unwrap_or_default()),
        Value::Bool(b) => query.bind(*b),
        Value::String(s) => query.bind(s.clone()),
        other => query.bind(other.to_string()),
    }
}

/// As [`bind_value`], for the `COUNT(*)` query shape.
fn bind_count_value<'q>(query: AnyCountQuery<'q>, value: &Value) -> AnyCountQuery<'q> {
    match value {
        Value::Number(n) if n.is_i64() => query.bind(n.as_i64().expect("checked is_i64")),
        Value::Number(n) => query.bind(n.as_f64().unwrap_or_default()),
        Value::Bool(b) => query.bind(*b),
        Value::String(s) => query.bind(s.clone()),
        other => query.bind(other.to_string()),
    }
}

/// The columns every record read selects, in the order [`RecordRow`] decodes
/// them. `created_at` is there for the keyset cursor, not the envelope.
fn record_columns(qualifier: &str) -> String {
    [
        "uri",
        "did",
        "collection",
        "rkey",
        "record",
        "cid",
        "indexed_at",
        "created_at",
    ]
    .map(|column| format!("{qualifier}{column}"))
    .join(", ")
}

type RecordRow = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
);

fn created_at(row: &RecordRow) -> &str {
    &row.7
}

/// The one place a read turns a row into the shape a guest sees. The
/// rationale for the shape lives on [`RecordEnvelope`].
fn envelope(row: RecordRow) -> Value {
    let (uri, did, collection, rkey, record, cid, indexed_at, _created_at) = row;
    serde_json::to_value(RecordEnvelope {
        uri,
        did,
        collection,
        rkey,
        cid: cid.filter(|cid| !cid.is_empty()),
        indexed_at,
        record: serde_json::from_str(&record).unwrap_or(json!({})),
    })
    .expect("an envelope of strings and JSON serializes")
}

/// Builds the two SQL shapes `records_query` needs: a custom sort uses
/// `OFFSET`/`LIMIT`; the default (no sort) uses a keyset cursor on
/// `(created_at, uri)`. Bind values are returned in placeholder order; the
/// `LIMIT`/`OFFSET` integers themselves are appended by `records_query`.
pub fn records_query_sql(
    spec: &RecordsQuery,
    backend: DatabaseBackend,
) -> Result<(String, Vec<String>), RecordsError> {
    let mut binds = vec![Value::String(spec.collection.clone())];
    let mut where_clause = String::from("WHERE collection = ?");
    if let Some(did) = &spec.did {
        where_clause.push_str(" AND did = ?");
        binds.push(Value::String(did.clone()));
    }

    let sql = if let Some(Sort { field, direction }) = &spec.sort {
        let column = if TOP_LEVEL_COLUMNS.contains(&field.as_str()) {
            column_sql(field)?
        } else {
            record_field_sql(field)?
        };
        let direction = direction_sql(direction)?;
        if let Some(filter) = &spec.filter {
            let clause = filter_sql(filter, backend, &record_field_sql, 0, &mut binds)?;
            where_clause.push_str(" AND ");
            where_clause.push_str(&clause);
        }
        format!(
            "SELECT {} FROM happyview_records {where_clause} ORDER BY {column} {direction} LIMIT ? OFFSET ?",
            record_columns("")
        )
    } else {
        let cursor_parts = spec.cursor.as_ref().and_then(|c| decode_cursor(c));
        if let Some((cursor_ts, cursor_uri)) = &cursor_parts {
            where_clause.push_str(" AND (created_at < ? OR (created_at = ? AND uri < ?))");
            binds.push(Value::String(cursor_ts.clone()));
            binds.push(Value::String(cursor_ts.clone()));
            binds.push(Value::String(cursor_uri.clone()));
        }
        if let Some(filter) = &spec.filter {
            let clause = filter_sql(filter, backend, &record_field_sql, 0, &mut binds)?;
            where_clause.push_str(" AND ");
            where_clause.push_str(&clause);
        }
        format!(
            "SELECT {} FROM happyview_records {where_clause} ORDER BY created_at DESC, uri DESC LIMIT ?",
            record_columns("")
        )
    };

    let binds = binds.into_iter().map(bind_as_text).collect();
    Ok((adapt_sql(&sql, backend), binds))
}

/// Paginated record listing: a custom sort walks pages with a base64-encoded
/// decimal offset; the default sort walks a keyset cursor on
/// `(created_at, uri)` so pages stay stable under concurrent inserts.
pub async fn records_query(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    spec: RecordsQuery,
) -> Result<RecordsPage, RecordsError> {
    let limit = clamp_limit(spec.limit);
    let custom_sort = spec.sort.is_some();
    let (sql, binds) = records_query_sql(&spec, backend)?;

    let mut q = crate::db::query_as::<RecordRow>(&sql);
    for bind in &binds {
        q = q.bind(bind);
    }

    if custom_sort {
        let offset: i64 = spec
            .cursor
            .as_ref()
            .and_then(|c| BASE64.decode(c).ok())
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        let rows = q.bind(limit).bind(offset).fetch_all(db).await?;
        let has_next = rows.len() as i64 == limit;
        let cursor = has_next.then(|| BASE64.encode((offset + limit).to_string()));
        let records = rows.into_iter().map(envelope).collect();
        Ok(RecordsPage { records, cursor })
    } else {
        let rows = q.bind(limit).fetch_all(db).await?;
        Ok(keyset_page(rows, limit))
    }
}

/// A page walked by the `(created_at, uri)` keyset: the cursor is the last
/// row's position, present only when the page came back full.
fn keyset_page(rows: Vec<RecordRow>, limit: i64) -> RecordsPage {
    let has_next = rows.len() as i64 == limit;
    let cursor = has_next
        .then(|| rows.last())
        .flatten()
        .map(|row| encode_cursor(created_at(row), &row.0));
    let records = rows.into_iter().map(envelope).collect();
    RecordsPage { records, cursor }
}

pub async fn records_count(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    spec: RecordsCount,
) -> Result<i64, RecordsError> {
    let mut binds = vec![Value::String(spec.collection.clone())];
    let mut sql = String::from("SELECT COUNT(*) FROM happyview_records WHERE collection = ?");
    if let Some(did) = &spec.did {
        sql.push_str(" AND did = ?");
        binds.push(Value::String(did.clone()));
    }
    if let Some(filter) = &spec.filter {
        let clause = filter_sql(filter, backend, &record_field_sql, 0, &mut binds)?;
        sql.push_str(" AND ");
        sql.push_str(&clause);
    }
    let sql = adapt_sql(&sql, backend);
    let binds: Vec<String> = binds.into_iter().map(bind_as_text).collect();
    let mut q = crate::db::query_as::<(i64,)>(&sql);
    for bind in &binds {
        q = q.bind(bind);
    }
    let (count,) = q.fetch_one(db).await?;
    Ok(count)
}

pub async fn records_get(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    uri: &str,
) -> Result<Option<Value>, RecordsError> {
    let sql = adapt_sql(
        &format!(
            "SELECT {} FROM happyview_records WHERE uri = ?",
            record_columns("")
        ),
        backend,
    );
    let row: Option<RecordRow> = crate::db::query_as(&sql)
        .bind(uri)
        .fetch_optional(db)
        .await?;
    Ok(row.map(envelope))
}

pub async fn records_search(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    spec: RecordsSearch,
) -> Result<Vec<Value>, RecordsError> {
    if !is_valid_json_field_path(&spec.field) {
        return Err(invalid(
            "invalid search field: use alphanumeric names with optional dot notation and array indices (e.g. 'name', 'author.handle', 'tags[0]')",
        ));
    }
    let limit = i64::from(spec.limit.unwrap_or(10).clamp(1, MAX_LIMIT));
    let like_pattern = format!("%{}%", spec.query);
    let field = &spec.field;
    let columns = record_columns("");

    // Cannot use adapt_sql: Postgres reuses $3 for two bind positions, while
    // SQLite needs separate ? for each. Different bind counts.
    let rows: Vec<RecordRow> = match backend {
        DatabaseBackend::Sqlite => {
            let sql = format!(
                "SELECT {columns} FROM happyview_records \
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
                .bind(&spec.collection)
                .bind(&like_pattern)
                .bind(&spec.query)
                .bind(&spec.query)
                .bind(limit)
                .fetch_all(db)
                .await?
        }
        DatabaseBackend::Postgres => {
            let sql = format!(
                "SELECT {columns} FROM happyview_records \
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
                .bind(&spec.collection)
                .bind(&like_pattern)
                .bind(&spec.query)
                .bind(limit)
                .fetch_all(db)
                .await?
        }
    };

    Ok(rows.into_iter().map(envelope).collect())
}

/// Records in `spec.collection` that reference `spec.uri` via
/// `happyview_record_refs`, keyset-paginated the same way as `records_query`.
pub async fn backlinks_query(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    spec: BacklinksQuery,
) -> Result<RecordsPage, RecordsError> {
    let limit = clamp_limit(spec.limit);
    let mut binds = vec![spec.uri.clone(), spec.collection.clone()];
    let mut where_clause = String::from("WHERE ref.target_uri = ? AND ref.collection = ?");
    if let Some(did) = &spec.did {
        where_clause.push_str(" AND r.did = ?");
        binds.push(did.clone());
    }
    if let Some((cursor_ts, cursor_uri)) = spec.cursor.as_ref().and_then(|c| decode_cursor(c)) {
        where_clause.push_str(" AND (r.created_at < ? OR (r.created_at = ? AND r.uri < ?))");
        binds.push(cursor_ts.clone());
        binds.push(cursor_ts);
        binds.push(cursor_uri);
    }
    let sql = adapt_sql(
        &format!(
            "SELECT {} FROM happyview_records r \
             INNER JOIN happyview_record_refs ref ON ref.source_uri = r.uri \
             {where_clause} \
             ORDER BY r.created_at DESC, r.uri DESC \
             LIMIT ?",
            record_columns("r.")
        ),
        backend,
    );

    let mut q = crate::db::query_as::<RecordRow>(&sql);
    for bind in &binds {
        q = q.bind(bind);
    }
    let rows = q.bind(limit).fetch_all(db).await?;
    Ok(keyset_page(rows, limit))
}

/// Builds the SQL for `table_query`: a plain `SELECT * FROM <table>` (or
/// `COUNT(*)`) over an arbitrary table, guarded by the same protected-table
/// list as `db.raw` since a validated identifier can still name an internal
/// table.
pub fn table_query_sql(
    spec: &TableQuery,
    backend: DatabaseBackend,
) -> Result<(String, Vec<Value>), RecordsError> {
    if !is_valid_identifier(&spec.table) {
        return Err(invalid(format!("invalid table name '{}'", spec.table)));
    }
    let mut binds: Vec<Value> = Vec::new();
    let mut sql = if spec.count {
        format!("SELECT COUNT(*) FROM {}", spec.table)
    } else {
        format!("SELECT * FROM {}", spec.table)
    };
    if let Some(filter) = &spec.filter {
        let clause = filter_sql(filter, backend, &column_sql, 0, &mut binds)?;
        sql.push_str(" WHERE ");
        sql.push_str(&clause);
    }
    if !spec.count {
        if let Some(Sort { field, direction }) = &spec.sort {
            sql.push_str(&format!(
                " ORDER BY {} {}",
                column_sql(field)?,
                direction_sql(direction)?
            ));
        }
        sql.push_str(" LIMIT ?");
    }
    check_raw_sql_tables(&sql).map_err(invalid)?;
    Ok((adapt_sql(&sql, backend), binds))
}

pub async fn table_query(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    spec: TableQuery,
) -> Result<Value, RecordsError> {
    let count = spec.count;
    let (sql, binds) = table_query_sql(&spec, backend)?;

    if count {
        let mut q = crate::db::query_as::<(i64,)>(&sql);
        for bind in &binds {
            q = bind_count_value(q, bind);
        }
        let (n,) = q.fetch_one(db).await?;
        Ok(json!(n))
    } else {
        let mut q = crate::db::query(&sql);
        for bind in &binds {
            q = bind_value(q, bind);
        }
        let rows = q.bind(clamp_limit(spec.limit)).fetch_all(db).await?;
        Ok(json!(rows.iter().map(row_to_json).collect::<Vec<_>>()))
    }
}

/// Upsert one record into the local index, bypassing the network.
///
/// `indexed_at` and `cid` describe what arrived from the network, so an update
/// leaves both alone: the stored CID describes the version the PDS holds and is
/// what strongRefs point at, and a local edit does not change either. An insert
/// may record a CID the caller was given by the network, and otherwise stores
/// an empty one rather than NULL — the column is NOT NULL on both backends, and
/// `cid_verify` already reads an empty CID as "nothing to check".
pub async fn index_put(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    spec: IndexPut,
) -> Result<RecordRef, RecordsError> {
    let IndexPut {
        collection,
        rkey,
        did,
        record,
        cid,
    } = spec;
    let did = did.ok_or_else(|| invalid("index_put needs a did: pass the record's author"))?;
    let uri = format!("at://{did}/{collection}/{rkey}");
    let record_str = serde_json::to_string(&record).unwrap_or_default();
    let now = crate::db::now_rfc3339();

    let upsert_sql = adapt_sql(
        r#"INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?)
           ON CONFLICT (uri) DO UPDATE
               SET record = EXCLUDED.record"#,
        backend,
    );
    crate::db::query(&upsert_sql)
        .bind(&uri)
        .bind(&did)
        .bind(&collection)
        .bind(&rkey)
        .bind(&record_str)
        .bind(cid.unwrap_or_default())
        .bind(crate::db::NO_INDEXED_AT)
        .bind(&now)
        .execute(db)
        .await?;

    let _ = crate::record_refs::sync_refs(db, &uri, &collection, &record, backend).await;

    // Read the CID back rather than assume one: an update leaves whatever the
    // network last stored, which is the value a caller building a strongRef
    // needs.
    let cid_sql = adapt_sql("SELECT cid FROM happyview_records WHERE uri = ?", backend);
    let row: Option<(Option<String>,)> = crate::db::query_as(&cid_sql)
        .bind(&uri)
        .fetch_optional(db)
        .await?;
    let cid = row.and_then(|(cid,)| cid).unwrap_or_default();

    Ok(RecordRef { uri, cid })
}

/// A record the caller has just written to their PDS, as the PDS answered it.
#[derive(Debug, Clone, Copy)]
pub struct NetworkWrite<'a> {
    pub uri: &'a str,
    pub did: &'a str,
    pub collection: &'a str,
    pub rkey: &'a str,
    pub record: &'a Value,
    pub cid: &'a str,
}

/// Which step of a mirror failed. The row and its refs are separate
/// statements, so the index can hold a correct row whose backlinks are empty
/// until Jetstream re-syncs them; an operator reading the log needs to know
/// whether the record is missing or merely unlinked.
#[derive(Debug, thiserror::Error)]
pub enum MirrorError {
    #[error("row not written: {0}")]
    Row(sqlx::Error),
    #[error("row written but its refs not synced: {0}")]
    Refs(sqlx::Error),
}

/// Reflect a write the caller just made to their PDS into the local index, so
/// a script that writes and then reads its own record sees it before Jetstream
/// echoes it back.
///
/// The PDS's `cid` lands on insert *and* update, where `index_put` records a
/// caller-supplied one on insert only: the PDS has just stated which version
/// it holds, and the stored CID exists to describe exactly that, so the
/// statement replaces whatever the row had. `indexed_at` is still the network
/// echo's to set: bound NULL on insert, untouched on update.
pub async fn mirror_network_write(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    write: NetworkWrite<'_>,
) -> Result<(), MirrorError> {
    let record_str = serde_json::to_string(write.record).unwrap_or_default();
    let upsert_sql = adapt_sql(
        r#"INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?)
           ON CONFLICT (uri) DO UPDATE
               SET record = EXCLUDED.record,
                   cid = EXCLUDED.cid"#,
        backend,
    );
    crate::db::query(&upsert_sql)
        .bind(write.uri)
        .bind(write.did)
        .bind(write.collection)
        .bind(write.rkey)
        .bind(&record_str)
        .bind(write.cid)
        .bind(crate::db::NO_INDEXED_AT)
        .bind(crate::db::now_rfc3339())
        .execute(db)
        .await
        .map_err(MirrorError::Row)?;
    crate::record_refs::sync_refs(db, write.uri, write.collection, write.record, backend)
        .await
        .map_err(MirrorError::Refs)
}

/// Drop a record the caller just deleted from their PDS, and the references
/// it made, so a read before the Jetstream echo does not resurrect it.
pub async fn mirror_network_delete(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    uri: &str,
) -> Result<(), RecordsError> {
    let refs_sql = adapt_sql(
        "DELETE FROM happyview_record_refs WHERE source_uri = ?",
        backend,
    );
    crate::db::query(&refs_sql).bind(uri).execute(db).await?;
    index_delete(db, backend, uri).await?;
    Ok(())
}

/// Drop one record from the local index. Idempotent; the bool says whether a
/// row was actually there.
pub async fn index_delete(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    uri: &str,
) -> Result<bool, RecordsError> {
    let sql = adapt_sql("DELETE FROM happyview_records WHERE uri = ?", backend);
    let result = crate::db::query(&sql).bind(uri).execute(db).await?;
    Ok(result.rows_affected() > 0)
}

/// The raw JSON of an uploaded lexicon, or `None` when this instance holds no
/// lexicon under that NSID.
pub async fn lexicon_get(lexicons: &crate::lexicon::LexiconRegistry, nsid: &str) -> Option<Value> {
    lexicons.get(nsid).await.map(|lexicon| lexicon.raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DatabaseBackend::{Postgres, Sqlite};
    use happyview_plugin_sdk::wire::{
        BacklinksQuery, Condition, Filter, RecordsCount, RecordsQuery, RecordsSearch, Sort,
        TableQuery,
    };

    fn cond(field: &str, op: &str, value: Value) -> Filter {
        Filter::Condition(Condition {
            field: field.into(),
            op: op.into(),
            value,
        })
    }

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
    }

    #[test]
    fn identifiers() {
        assert!(is_valid_identifier("score"));
        assert!(is_valid_identifier("_t1"));
        assert!(!is_valid_identifier("1abc"));
        assert!(!is_valid_identifier("a-b"));
        assert!(!is_valid_identifier("a b"));
        assert!(!is_valid_identifier(""));
    }

    #[test]
    fn operators_normalise_and_ilike_lowers_per_backend() {
        assert_eq!(normalize_op("like", Sqlite).unwrap(), "LIKE");
        assert_eq!(normalize_op("Not Like", Sqlite).unwrap(), "NOT LIKE");
        assert_eq!(normalize_op("ilike", Sqlite).unwrap(), "LIKE");
        assert_eq!(normalize_op("ilike", Postgres).unwrap(), "ILIKE");
        assert!(matches!(
            normalize_op("~", Sqlite),
            Err(RecordsError::InvalidSpec(_))
        ));
    }

    #[test]
    fn records_sql_default_sort_uses_keyset_cursor() {
        let spec = RecordsQuery {
            collection: "c".into(),
            did: Some("did:plc:a".into()),
            filter: Some(cond("status", "=", json!("active"))),
            sort: None,
            limit: Some(10),
            cursor: Some(crate::db::encode_cursor("2026-01-01T00:00:00Z", "at://x")),
        };
        let (sql, binds) = records_query_sql(&spec, Sqlite).unwrap();
        assert!(sql.contains("WHERE collection = ? AND did = ?"), "{sql}");
        assert!(
            sql.contains("(created_at < ? OR (created_at = ? AND uri < ?))"),
            "{sql}"
        );
        assert!(
            sql.contains("json_extract(record, '$.status') = ?"),
            "{sql}"
        );
        assert!(
            sql.ends_with("ORDER BY created_at DESC, uri DESC LIMIT ?"),
            "{sql}"
        );
        assert_eq!(
            binds,
            vec![
                "c",
                "did:plc:a",
                "2026-01-01T00:00:00Z",
                "2026-01-01T00:00:00Z",
                "at://x",
                "active"
            ]
        );
    }

    #[test]
    fn records_sql_custom_sort_uses_offset() {
        let spec = RecordsQuery {
            collection: "c".into(),
            did: None,
            filter: None,
            sort: Some(Sort {
                field: "createdAt".into(),
                direction: "asc".into(),
            }),
            limit: None,
            cursor: None,
        };
        let (sql, binds) = records_query_sql(&spec, Sqlite).unwrap();
        assert!(
            sql.ends_with("ORDER BY json_extract(record, '$.createdAt') ASC LIMIT ? OFFSET ?"),
            "{sql}"
        );
        assert_eq!(binds, vec!["c"]);
    }

    #[test]
    fn records_sql_top_level_sort_column_is_bare() {
        let spec = RecordsQuery {
            collection: "c".into(),
            did: None,
            filter: None,
            sort: Some(Sort {
                field: "indexed_at".into(),
                direction: "desc".into(),
            }),
            limit: None,
            cursor: None,
        };
        let (sql, _) = records_query_sql(&spec, Sqlite).unwrap();
        assert!(sql.contains("ORDER BY indexed_at DESC"), "{sql}");
    }

    #[test]
    fn records_sql_rejects_bad_sort_field_and_direction() {
        let bad_field = RecordsQuery {
            collection: "c".into(),
            did: None,
            filter: None,
            sort: Some(Sort {
                field: "a'b".into(),
                direction: "asc".into(),
            }),
            limit: None,
            cursor: None,
        };
        assert!(matches!(
            records_query_sql(&bad_field, Sqlite),
            Err(RecordsError::InvalidSpec(_))
        ));
        let bad_dir = RecordsQuery {
            collection: "c".into(),
            did: None,
            filter: None,
            sort: Some(Sort {
                field: "a".into(),
                direction: "sideways".into(),
            }),
            limit: None,
            cursor: None,
        };
        assert!(matches!(
            records_query_sql(&bad_dir, Sqlite),
            Err(RecordsError::InvalidSpec(_))
        ));
    }

    #[test]
    fn filter_groups_nest_and_cap_depth() {
        let mut binds = Vec::new();
        let f = Filter::Group {
            combine: "or".into(),
            conditions: vec![
                cond("a", "=", json!("1")),
                Filter::Group {
                    combine: "and".into(),
                    conditions: vec![cond("b", ">", json!("2")), cond("c", "like", json!("%x%"))],
                },
            ],
        };
        let sql = filter_sql(
            &f,
            Sqlite,
            &|p| Ok(format!("json_extract(record, '$.{p}')")),
            0,
            &mut binds,
        )
        .unwrap();
        assert_eq!(
            sql,
            "(json_extract(record, '$.a') = ? OR (json_extract(record, '$.b') > ? AND json_extract(record, '$.c') LIKE ?))"
        );
        assert_eq!(binds, vec![json!("1"), json!("2"), json!("%x%")]);

        let mut deep = cond("a", "=", json!("1"));
        for _ in 0..MAX_FILTER_DEPTH {
            deep = Filter::Group {
                combine: "and".into(),
                conditions: vec![deep],
            };
        }
        let err =
            filter_sql(&deep, Sqlite, &|p| Ok(p.to_string()), 0, &mut Vec::new()).unwrap_err();
        assert!(err.to_string().contains("nesting"), "{err}");
    }

    #[test]
    fn filter_rejects_bad_combine_and_empty_group() {
        let bad = Filter::Group {
            combine: "xor".into(),
            conditions: vec![cond("a", "=", json!("1"))],
        };
        assert!(filter_sql(&bad, Sqlite, &|p| Ok(p.to_string()), 0, &mut Vec::new()).is_err());
        let empty = Filter::Group {
            combine: "and".into(),
            conditions: vec![],
        };
        assert!(filter_sql(&empty, Sqlite, &|p| Ok(p.to_string()), 0, &mut Vec::new()).is_err());
    }

    #[test]
    fn table_sql_quotes_nothing_and_validates_identifiers() {
        let spec = TableQuery {
            table: "leaderboard".into(),
            filter: Some(cond("score", ">", json!("100"))),
            sort: Some(Sort {
                field: "score".into(),
                direction: "desc".into(),
            }),
            limit: Some(5),
            count: false,
        };
        let (sql, binds) = table_query_sql(&spec, Sqlite).unwrap();
        assert_eq!(
            sql,
            "SELECT * FROM leaderboard WHERE score > ? ORDER BY score DESC LIMIT ?"
        );
        assert_eq!(binds, vec![json!("100")]);

        let count = TableQuery {
            table: "leaderboard".into(),
            filter: None,
            sort: None,
            limit: None,
            count: true,
        };
        let (sql, _) = table_query_sql(&count, Sqlite).unwrap();
        assert_eq!(sql, "SELECT COUNT(*) FROM leaderboard");

        let bad = TableQuery {
            table: "lead;board".into(),
            filter: None,
            sort: None,
            limit: None,
            count: false,
        };
        assert!(matches!(
            table_query_sql(&bad, Sqlite),
            Err(RecordsError::InvalidSpec(_))
        ));
        let bad_col = TableQuery {
            table: "t".into(),
            filter: Some(cond("a b", "=", json!("1"))),
            sort: None,
            limit: None,
            count: false,
        };
        assert!(matches!(
            table_query_sql(&bad_col, Sqlite),
            Err(RecordsError::InvalidSpec(_))
        ));
    }

    #[test]
    fn table_sql_refuses_protected_tables() {
        let spec = TableQuery {
            table: "happyview_plugin_secrets".into(),
            filter: None,
            sort: None,
            limit: None,
            count: false,
        };
        let err = table_query_sql(&spec, Sqlite).unwrap_err();
        assert!(matches!(err, RecordsError::InvalidSpec(_)), "{err}");
    }

    async fn seeded_pool() -> sqlx::AnyPool {
        let pool = crate::test_support::memory_pool().await;
        for sql in [
            "CREATE TABLE happyview_records (uri TEXT PRIMARY KEY, did TEXT NOT NULL, collection TEXT NOT NULL, rkey TEXT, record TEXT NOT NULL, cid TEXT, indexed_at TEXT, created_at TEXT)",
            "CREATE TABLE happyview_record_refs (source_uri TEXT NOT NULL, target_uri TEXT NOT NULL, collection TEXT NOT NULL)",
            "INSERT INTO happyview_records VALUES ('at://a/c/1', 'did:plc:a', 'c', '1', '{\"n\":\"one\",\"score\":1}', 'cid1', NULL, '2026-01-01T00:00:01Z')",
            "INSERT INTO happyview_records VALUES ('at://a/c/2', 'did:plc:a', 'c', '2', '{\"n\":\"two\",\"score\":2}', 'cid2', NULL, '2026-01-01T00:00:02Z')",
            "INSERT INTO happyview_records VALUES ('at://b/c/3', 'did:plc:b', 'c', '3', '{\"n\":\"three\",\"score\":3}', 'cid3', NULL, '2026-01-01T00:00:03Z')",
            "INSERT INTO happyview_record_refs VALUES ('at://a/c/2', 'at://a/c/1', 'c')",
            "CREATE TABLE leaderboard (name TEXT, score INTEGER)",
            "INSERT INTO leaderboard VALUES ('x', 50), ('y', 150)",
            "CREATE TABLE flags (name TEXT, active INTEGER)",
            "INSERT INTO flags VALUES ('a', 1), ('b', 0)",
        ] {
            crate::db::query(sql).execute(&pool).await.unwrap();
        }
        pool
    }

    #[tokio::test]
    async fn records_query_paginates_with_keyset_cursor() {
        let pool = seeded_pool().await;
        let page = records_query(
            &pool,
            Sqlite,
            RecordsQuery {
                collection: "c".into(),
                did: None,
                filter: None,
                sort: None,
                limit: Some(2),
                cursor: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(page.records.len(), 2);
        assert_eq!(page.records[0]["uri"], "at://b/c/3");
        let cursor = page.cursor.expect("more pages");
        let next = records_query(
            &pool,
            Sqlite,
            RecordsQuery {
                collection: "c".into(),
                did: None,
                filter: None,
                sort: None,
                limit: Some(2),
                cursor: Some(cursor),
            },
        )
        .await
        .unwrap();
        assert_eq!(next.records.len(), 1);
        assert_eq!(next.records[0]["uri"], "at://a/c/1");
        assert!(next.cursor.is_none());
    }

    #[tokio::test]
    async fn records_query_filters_and_sorts() {
        let pool = seeded_pool().await;
        let page = records_query(
            &pool,
            Sqlite,
            RecordsQuery {
                collection: "c".into(),
                did: Some("did:plc:a".into()),
                filter: Some(cond("n", "ilike", json!("T%"))),
                sort: Some(Sort {
                    field: "score".into(),
                    direction: "asc".into(),
                }),
                limit: None,
                cursor: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0]["record"]["n"], "two");
    }

    /// A stored body that carries its own `uri` field keeps it: the index's
    /// URI lives beside the body, not inside it.
    #[tokio::test]
    async fn every_read_returns_an_envelope_around_the_stored_body() {
        let pool = seeded_pool().await;
        for sql in [
            "INSERT INTO happyview_records VALUES ('at://a/c/4', 'did:plc:a', 'c', '4', '{\"uri\":\"at://spoof\",\"n\":\"four\"}', '', '2026-02-02T00:00:00Z', '2026-01-01T00:00:04Z')",
            "INSERT INTO happyview_record_refs VALUES ('at://a/c/4', 'at://b/c/3', 'c')",
        ] {
            crate::db::query(sql).execute(&pool).await.unwrap();
        }
        let expected = json!({
            "uri": "at://a/c/4",
            "did": "did:plc:a",
            "collection": "c",
            "rkey": "4",
            "cid": null,
            "indexed_at": "2026-02-02T00:00:00Z",
            "record": {"uri": "at://spoof", "n": "four"},
        });

        let got = records_get(&pool, Sqlite, "at://a/c/4")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got, expected);

        let page = records_query(
            &pool,
            Sqlite,
            RecordsQuery {
                collection: "c".into(),
                did: None,
                filter: Some(cond("n", "=", json!("four"))),
                sort: None,
                limit: None,
                cursor: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(page.records, vec![expected.clone()]);

        let found = records_search(
            &pool,
            Sqlite,
            RecordsSearch {
                collection: "c".into(),
                field: "n".into(),
                query: "fou".into(),
                limit: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(found, vec![expected.clone()]);

        let back = backlinks_query(
            &pool,
            Sqlite,
            BacklinksQuery {
                uri: "at://b/c/3".into(),
                collection: "c".into(),
                did: None,
                limit: None,
                cursor: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(back.records, vec![expected]);
    }

    /// The seeded rows hold a real CID and no `indexed_at`, the state of a
    /// record written locally with a CID the PDS handed back.
    #[tokio::test]
    async fn envelope_reports_a_cid_and_a_null_indexed_at() {
        let pool = seeded_pool().await;
        let got = records_get(&pool, Sqlite, "at://a/c/1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got["cid"], "cid1");
        assert!(got["indexed_at"].is_null(), "{got}");
        assert_eq!(got["record"], json!({"n": "one", "score": 1}));
    }

    #[tokio::test]
    async fn records_count_get_search_and_backlinks() {
        let pool = seeded_pool().await;
        let n = records_count(
            &pool,
            Sqlite,
            RecordsCount {
                collection: "c".into(),
                did: Some("did:plc:a".into()),
                filter: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(n, 2);
        let got = records_get(&pool, Sqlite, "at://a/c/1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got["record"]["n"], "one");
        assert_eq!(got["uri"], "at://a/c/1");
        assert!(
            records_get(&pool, Sqlite, "at://nope")
                .await
                .unwrap()
                .is_none()
        );
        let found = records_search(
            &pool,
            Sqlite,
            RecordsSearch {
                collection: "c".into(),
                field: "n".into(),
                query: "tw".into(),
                limit: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(found.len(), 1);
        let back = backlinks_query(
            &pool,
            Sqlite,
            BacklinksQuery {
                uri: "at://a/c/1".into(),
                collection: "c".into(),
                did: None,
                limit: None,
                cursor: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(back.records.len(), 1);
        assert_eq!(back.records[0]["uri"], "at://a/c/2");
    }

    #[tokio::test]
    async fn table_query_returns_rows_or_count() {
        let pool = seeded_pool().await;
        let rows = table_query(
            &pool,
            Sqlite,
            TableQuery {
                table: "leaderboard".into(),
                filter: Some(cond("score", ">", json!("100"))),
                sort: None,
                limit: None,
                count: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 1);
        assert_eq!(rows[0]["name"], "y");
        let n = table_query(
            &pool,
            Sqlite,
            TableQuery {
                table: "leaderboard".into(),
                filter: None,
                sort: None,
                limit: None,
                count: true,
            },
        )
        .await
        .unwrap();
        assert_eq!(n, serde_json::json!(2));
    }

    #[tokio::test]
    async fn table_query_binds_a_json_number_typed() {
        let pool = seeded_pool().await;
        let rows = table_query(
            &pool,
            Sqlite,
            TableQuery {
                table: "leaderboard".into(),
                filter: Some(cond("score", ">", json!(100))),
                sort: None,
                limit: None,
                count: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 1);
        assert_eq!(rows[0]["name"], "y");
        assert_eq!(rows[0]["score"], 150);
    }

    #[tokio::test]
    async fn table_query_binds_a_json_boolean_typed() {
        let pool = seeded_pool().await;
        let rows = table_query(
            &pool,
            Sqlite,
            TableQuery {
                table: "flags".into(),
                filter: Some(cond("active", "=", json!(true))),
                sort: None,
                limit: None,
                count: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 1);
        assert_eq!(rows[0]["name"], "a");
    }

    fn index_put_spec(did: &str, rkey: &str, record: Value) -> IndexPut {
        IndexPut {
            collection: "c".into(),
            rkey: rkey.into(),
            did: Some(did.into()),
            record,
            cid: None,
        }
    }

    #[tokio::test]
    async fn index_put_inserts_then_updates() {
        let pool = seeded_pool().await;
        let inserted = index_put(
            &pool,
            Sqlite,
            index_put_spec("did:plc:a", "new", json!({"n": "first"})),
        )
        .await
        .unwrap();
        assert_eq!(inserted.uri, "at://did:plc:a/c/new");
        assert_eq!(inserted.cid, "");

        let updated = index_put(
            &pool,
            Sqlite,
            index_put_spec("did:plc:a", "new", json!({"n": "second"})),
        )
        .await
        .unwrap();
        assert_eq!(updated.uri, inserted.uri);

        let stored = records_get(&pool, Sqlite, "at://did:plc:a/c/new")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored["record"]["n"], "second");

        let rows: Vec<(i64,)> =
            crate::db::query_as("SELECT COUNT(*) FROM happyview_records WHERE uri = ?")
                .bind("at://did:plc:a/c/new")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(rows[0].0, 1, "an upsert, not a second row");
    }

    /// A caller who just wrote the record to a PDS knows its CID, and a brand
    /// new row is the one moment recording it invents nothing.
    #[tokio::test]
    async fn index_put_records_a_supplied_cid_on_insert() {
        let pool = seeded_pool().await;
        let inserted = index_put(
            &pool,
            Sqlite,
            IndexPut {
                collection: "c".into(),
                rkey: "fresh".into(),
                did: Some("did:plc:a".into()),
                record: json!({"n": "first"}),
                cid: Some("bafyfromthepds".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(inserted.cid, "bafyfromthepds");
    }

    /// The row's CID describes the version a PDS holds. A later index write
    /// carrying a different one is describing something it cannot know about
    /// the network, so the stored value wins.
    #[tokio::test]
    async fn index_put_never_overwrites_an_existing_cid() {
        let pool = seeded_pool().await;
        let updated = index_put(
            &pool,
            Sqlite,
            IndexPut {
                collection: "c".into(),
                rkey: "1".into(),
                did: Some("a".into()),
                record: json!({"n": "edited"}),
                cid: Some("bafysomethingelse".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(updated.cid, "cid1");

        let row: (Option<String>,) =
            crate::db::query_as("SELECT cid FROM happyview_records WHERE uri = ?")
                .bind("at://a/c/1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row.0.as_deref(), Some("cid1"));
    }

    /// A local edit says nothing about what the network holds, so neither
    /// column may be disturbed by one.
    #[tokio::test]
    async fn index_put_leaves_network_provenance_alone() {
        let pool = seeded_pool().await;
        crate::db::query(
            "UPDATE happyview_records SET indexed_at = ?, cid = ? WHERE uri = 'at://a/c/1'",
        )
        .bind("2026-01-01T00:00:00Z")
        .bind("bafyoriginal")
        .execute(&pool)
        .await
        .unwrap();

        let put = index_put(
            &pool,
            Sqlite,
            IndexPut {
                collection: "c".into(),
                rkey: "1".into(),
                did: Some("a".into()),
                record: json!({"n": "edited"}),
                cid: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(put.cid, "bafyoriginal");

        let row: (Option<String>, Option<String>) =
            crate::db::query_as("SELECT indexed_at, cid FROM happyview_records WHERE uri = ?")
                .bind("at://a/c/1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row.0.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(row.1.as_deref(), Some("bafyoriginal"));
    }

    #[tokio::test]
    async fn index_put_without_a_did_says_so() {
        let pool = seeded_pool().await;
        let err = index_put(
            &pool,
            Sqlite,
            IndexPut {
                collection: "c".into(),
                rkey: "1".into(),
                did: None,
                record: json!({}),
                cid: None,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, RecordsError::InvalidSpec(_)), "{err}");
        assert!(err.to_string().contains("did"), "{err}");
    }

    async fn provenance(pool: &sqlx::AnyPool, uri: &str) -> (Option<String>, Option<String>) {
        crate::db::query_as("SELECT cid, indexed_at FROM happyview_records WHERE uri = ?")
            .bind(uri)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn ref_targets(pool: &sqlx::AnyPool, source: &str) -> Vec<String> {
        let rows: Vec<(String,)> = crate::db::query_as(
            "SELECT target_uri FROM happyview_record_refs WHERE source_uri = ? ORDER BY target_uri",
        )
        .bind(source)
        .fetch_all(pool)
        .await
        .unwrap();
        rows.into_iter().map(|(t,)| t).collect()
    }

    #[tokio::test]
    async fn mirrored_create_lands_with_the_pds_cid_and_no_indexed_at() {
        let pool = seeded_pool().await;
        let record = json!({"n": "mine", "subject": "at://a/c/1"});
        mirror_network_write(
            &pool,
            Sqlite,
            NetworkWrite {
                uri: "at://did:plc:a/c/fresh",
                did: "did:plc:a",
                collection: "c",
                rkey: "fresh",
                record: &record,
                cid: "bafyfromthepds",
            },
        )
        .await
        .unwrap();

        let got = records_get(&pool, Sqlite, "at://did:plc:a/c/fresh")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got["cid"], "bafyfromthepds");
        assert!(got["indexed_at"].is_null(), "{got}");
        assert_eq!(got["rkey"], "fresh");
        assert_eq!(got["record"], record);
        assert_eq!(
            ref_targets(&pool, "at://did:plc:a/c/fresh").await,
            vec!["at://a/c/1"]
        );
    }

    #[tokio::test]
    async fn mirrored_put_replaces_the_cid_and_leaves_indexed_at() {
        let pool = seeded_pool().await;
        crate::db::query("UPDATE happyview_records SET indexed_at = ? WHERE uri = 'at://a/c/2'")
            .bind("2026-01-01T00:00:00Z")
            .execute(&pool)
            .await
            .unwrap();
        let record = json!({"n": "edited", "subject": "at://b/c/3"});
        mirror_network_write(
            &pool,
            Sqlite,
            NetworkWrite {
                uri: "at://a/c/2",
                did: "did:plc:a",
                collection: "c",
                rkey: "2",
                record: &record,
                cid: "bafyv2",
            },
        )
        .await
        .unwrap();

        let (cid, indexed_at) = provenance(&pool, "at://a/c/2").await;
        assert_eq!(cid.as_deref(), Some("bafyv2"));
        assert_eq!(indexed_at.as_deref(), Some("2026-01-01T00:00:00Z"));
        let got = records_get(&pool, Sqlite, "at://a/c/2")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got["record"], record);
        assert_eq!(ref_targets(&pool, "at://a/c/2").await, vec!["at://b/c/3"]);
    }

    #[tokio::test]
    async fn mirrored_delete_removes_the_row_and_its_refs() {
        let pool = seeded_pool().await;
        assert_eq!(ref_targets(&pool, "at://a/c/2").await, vec!["at://a/c/1"]);
        mirror_network_delete(&pool, Sqlite, "at://a/c/2")
            .await
            .unwrap();
        assert!(
            records_get(&pool, Sqlite, "at://a/c/2")
                .await
                .unwrap()
                .is_none()
        );
        assert!(ref_targets(&pool, "at://a/c/2").await.is_empty());
        mirror_network_delete(&pool, Sqlite, "at://a/c/2")
            .await
            .expect("a second delete is a no-op");
    }

    /// The caller decides what a failed mirror means; the function itself
    /// has to surface it, and say which step failed, rather than swallow it.
    #[tokio::test]
    async fn a_mirror_that_cannot_write_names_the_failed_step() {
        let pool = crate::test_support::memory_pool().await;
        let record = json!({"subject": "at://a/c/1"});
        let write = NetworkWrite {
            uri: "at://a/c/1",
            did: "did:plc:a",
            collection: "c",
            rkey: "1",
            record: &record,
            cid: "bafy",
        };
        let err = mirror_network_write(&pool, Sqlite, write)
            .await
            .unwrap_err();
        assert!(matches!(err, MirrorError::Row(_)), "{err}");
        assert!(err.to_string().starts_with("row not written"), "{err}");

        crate::db::query(
            "CREATE TABLE happyview_records (uri TEXT PRIMARY KEY, did TEXT NOT NULL, collection TEXT NOT NULL, rkey TEXT, record TEXT NOT NULL, cid TEXT, indexed_at TEXT, created_at TEXT)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let err = mirror_network_write(&pool, Sqlite, write)
            .await
            .unwrap_err();
        assert!(matches!(err, MirrorError::Refs(_)), "{err}");
        assert!(err.to_string().starts_with("row written"), "{err}");
        assert!(
            records_get(&pool, Sqlite, "at://a/c/1")
                .await
                .unwrap()
                .is_some(),
            "the row landed before the refs step failed"
        );
    }

    #[tokio::test]
    async fn a_mirror_delete_that_cannot_write_reports_it() {
        let pool = crate::test_support::memory_pool().await;
        let err = mirror_network_delete(&pool, Sqlite, "at://a/c/1")
            .await
            .unwrap_err();
        assert!(matches!(err, RecordsError::Database(_)), "{err}");
    }

    #[tokio::test]
    async fn index_delete_reports_whether_a_row_went() {
        let pool = seeded_pool().await;
        assert!(index_delete(&pool, Sqlite, "at://a/c/1").await.unwrap());
        assert!(!index_delete(&pool, Sqlite, "at://a/c/1").await.unwrap());
        assert!(
            records_get(&pool, Sqlite, "at://a/c/1")
                .await
                .unwrap()
                .is_none()
        );
    }
}

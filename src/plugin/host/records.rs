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
    BacklinksQuery, Filter, RecordsCount, RecordsPage, RecordsQuery, RecordsSearch, Sort,
    TableQuery,
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
    binds: &mut Vec<String>,
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

fn record_with_uri(uri: String, record_str: &str) -> Value {
    let mut record: Value = serde_json::from_str(record_str).unwrap_or(json!({}));
    if let Some(obj) = record.as_object_mut() {
        obj.insert("uri".to_string(), json!(uri));
    }
    record
}

/// Builds the two SQL shapes `records_query` needs: a custom sort uses
/// `OFFSET`/`LIMIT`; the default (no sort) uses a keyset cursor on
/// `(created_at, uri)`. Bind values are returned in placeholder order; the
/// `LIMIT`/`OFFSET` integers themselves are appended by `records_query`.
pub fn records_query_sql(
    spec: &RecordsQuery,
    backend: DatabaseBackend,
) -> Result<(String, Vec<String>), RecordsError> {
    let mut binds = vec![spec.collection.clone()];
    let mut where_clause = String::from("WHERE collection = ?");
    if let Some(did) = &spec.did {
        where_clause.push_str(" AND did = ?");
        binds.push(did.clone());
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
            "SELECT uri, did, record, created_at FROM happyview_records {where_clause} ORDER BY {column} {direction} LIMIT ? OFFSET ?"
        )
    } else {
        let cursor_parts = spec.cursor.as_ref().and_then(|c| decode_cursor(c));
        if let Some((cursor_ts, cursor_uri)) = &cursor_parts {
            where_clause.push_str(" AND (created_at < ? OR (created_at = ? AND uri < ?))");
            binds.push(cursor_ts.clone());
            binds.push(cursor_ts.clone());
            binds.push(cursor_uri.clone());
        }
        if let Some(filter) = &spec.filter {
            let clause = filter_sql(filter, backend, &record_field_sql, 0, &mut binds)?;
            where_clause.push_str(" AND ");
            where_clause.push_str(&clause);
        }
        format!(
            "SELECT uri, did, record, created_at FROM happyview_records {where_clause} ORDER BY created_at DESC, uri DESC LIMIT ?"
        )
    };

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

    type Row = (String, String, String, String);
    let mut q = crate::db::query_as::<Row>(&sql);
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
        let records = rows
            .into_iter()
            .map(|(uri, _did, record, _created_at)| record_with_uri(uri, &record))
            .collect();
        Ok(RecordsPage { records, cursor })
    } else {
        let rows = q.bind(limit).fetch_all(db).await?;
        let has_next = rows.len() as i64 == limit;
        let cursor = if has_next {
            rows.last()
                .map(|(uri, _did, _record, created_at)| encode_cursor(created_at, uri))
        } else {
            None
        };
        let records = rows
            .into_iter()
            .map(|(uri, _did, record, _created_at)| record_with_uri(uri, &record))
            .collect();
        Ok(RecordsPage { records, cursor })
    }
}

pub async fn records_count(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    spec: RecordsCount,
) -> Result<i64, RecordsError> {
    let mut binds = vec![spec.collection.clone()];
    let mut sql = String::from("SELECT COUNT(*) FROM happyview_records WHERE collection = ?");
    if let Some(did) = &spec.did {
        sql.push_str(" AND did = ?");
        binds.push(did.clone());
    }
    if let Some(filter) = &spec.filter {
        let clause = filter_sql(filter, backend, &record_field_sql, 0, &mut binds)?;
        sql.push_str(" AND ");
        sql.push_str(&clause);
    }
    let sql = adapt_sql(&sql, backend);
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
        "SELECT record FROM happyview_records WHERE uri = ?",
        backend,
    );
    let row: Option<(String,)> = crate::db::query_as(&sql)
        .bind(uri)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|(record,)| record_with_uri(uri.to_string(), &record)))
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

    // Cannot use adapt_sql: Postgres reuses $3 for two bind positions, while
    // SQLite needs separate ? for each. Different bind counts.
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
                .bind(&spec.collection)
                .bind(&like_pattern)
                .bind(&spec.query)
                .bind(limit)
                .fetch_all(db)
                .await?
        }
    };

    Ok(rows
        .into_iter()
        .map(|(uri, _did, record)| record_with_uri(uri, &record))
        .collect())
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
            "SELECT r.uri, r.did, r.record, r.created_at FROM happyview_records r \
             INNER JOIN happyview_record_refs ref ON ref.source_uri = r.uri \
             {where_clause} \
             ORDER BY r.created_at DESC, r.uri DESC \
             LIMIT ?"
        ),
        backend,
    );

    type Row = (String, String, String, String);
    let mut q = crate::db::query_as::<Row>(&sql);
    for bind in &binds {
        q = q.bind(bind);
    }
    let rows = q.bind(limit).fetch_all(db).await?;
    let has_next = rows.len() as i64 == limit;
    let cursor = if has_next {
        rows.last()
            .map(|(uri, _did, _record, created_at)| encode_cursor(created_at, uri))
    } else {
        None
    };
    let records = rows
        .into_iter()
        .map(|(uri, _did, record, _created_at)| record_with_uri(uri, &record))
        .collect();
    Ok(RecordsPage { records, cursor })
}

/// Builds the SQL for `table_query`: a plain `SELECT * FROM <table>` (or
/// `COUNT(*)`) over an arbitrary table, guarded by the same protected-table
/// list as `db.raw` since a validated identifier can still name an internal
/// table.
pub fn table_query_sql(
    spec: &TableQuery,
    backend: DatabaseBackend,
) -> Result<(String, Vec<String>), RecordsError> {
    if !is_valid_identifier(&spec.table) {
        return Err(invalid(format!("invalid table name '{}'", spec.table)));
    }
    let mut binds = Vec::new();
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
            q = q.bind(bind);
        }
        let (n,) = q.fetch_one(db).await?;
        Ok(json!(n))
    } else {
        let mut q = crate::db::query(&sql);
        for bind in &binds {
            q = q.bind(bind);
        }
        let rows = q.bind(clamp_limit(spec.limit)).fetch_all(db).await?;
        Ok(json!(rows.iter().map(row_to_json).collect::<Vec<_>>()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DatabaseBackend::{Postgres, Sqlite};
    use happyview_plugin_sdk::wire::{
        BacklinksQuery, Condition, Filter, RecordsCount, RecordsQuery, RecordsSearch, Sort,
        TableQuery,
    };

    fn cond(field: &str, op: &str, value: &str) -> Filter {
        Filter::Condition(Condition {
            field: field.into(),
            op: op.into(),
            value: value.into(),
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
            filter: Some(cond("status", "=", "active")),
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
                cond("a", "=", "1"),
                Filter::Group {
                    combine: "and".into(),
                    conditions: vec![cond("b", ">", "2"), cond("c", "like", "%x%")],
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
        assert_eq!(binds, vec!["1", "2", "%x%"]);

        let mut deep = cond("a", "=", "1");
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
            conditions: vec![cond("a", "=", "1")],
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
            filter: Some(cond("score", ">", "100")),
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
        assert_eq!(binds, vec!["100"]);

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
            filter: Some(cond("a b", "=", "1")),
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
                filter: Some(cond("n", "ilike", "T%")),
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
        assert_eq!(page.records[0]["n"], "two");
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
        assert_eq!(got["n"], "one");
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
                filter: Some(cond("score", ">", "100")),
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
}

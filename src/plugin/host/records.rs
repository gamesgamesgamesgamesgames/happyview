//! Record and table queries for plugins and the Lua `db` global. Every piece
//! of SQL that touches `happyview_records` on a guest's behalf is generated
//! here, so a plugin never writes SQL for records and the schema can change
//! in one place.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::db::row_to_json;
use crate::db::{
    DatabaseBackend, MAX_FIELD_PATH_LEN, adapt_sql, decode_cursor, encode_cursor,
    is_valid_json_field_path, postgres_json_chain,
};
use crate::raw_sql_guard::check_raw_sql_tables;
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use happyview_plugin_sdk::wire::{
    BacklinksQuery, Filter, IndexPut, RecordEnvelope, RecordRef, RecordsCount, RecordsPage,
    RecordsQuery, RecordsSearch, Sort, TableQuery,
};
use sqlx::any::AnyTypeInfoKind;
use sqlx::{AssertSqlSafe, Executor, SqlSafeStr};

pub const MAX_FILTER_DEPTH: u8 = 5;
pub const DEFAULT_LIMIT: u32 = 20;
pub const MAX_LIMIT: u32 = 100;

/// Columns `records_query`'s sort accepts bare, as opposed to a JSON path
/// evaluated inside `record`.
const TOP_LEVEL_COLUMNS: [&str; 3] = ["indexed_at", "did", "uri"];

/// Those of [`TOP_LEVEL_COLUMNS`] the schema lets be null, and so the only
/// ones whose sort needs a null placement. `uri` and `did` are `NOT NULL` on
/// both backends.
const NULLABLE_TOP_LEVEL_COLUMNS: [&str; 1] = ["indexed_at"];

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

/// Renders one condition: the whole predicate, with a single `?` where the
/// value binds, and the value to bind there.
///
/// The callback owns the entire predicate rather than just the left side,
/// because the operator and the field together decide both operands — a JSON
/// path and a real column need different expressions, the bound value's type
/// has to suit whichever the field names, and one backend needs a cast around
/// the placeholder itself.
type FieldBind<'a> = dyn Fn(&str, &str, Value) -> Result<(String, Value), RecordsError> + 'a;

/// Render a filter tree to SQL, pushing bind values in placeholder order.
pub fn filter_sql(
    filter: &Filter,
    backend: DatabaseBackend,
    field_bind: &FieldBind<'_>,
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
            let op = normalize_op(&c.op, backend)?;
            let (predicate, value) = field_bind(&c.field, op, c.value.clone())?;
            binds.push(value);
            Ok(predicate)
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
                .map(|c| filter_sql(c, backend, field_bind, depth + 1, binds))
                .collect::<Result<Vec<_>, _>>()?;
            if parts.len() == 1 {
                Ok(parts.into_iter().next().expect("one part"))
            } else {
                Ok(format!("({})", parts.join(&format!(" {combine} "))))
            }
        }
    }
}

fn invalid_field(path: &str) -> RecordsError {
    invalid(format!(
        "invalid field '{path}': use alphanumeric names with optional dot notation and array indices (e.g. 'name', 'author.handle', 'tags[0]'), at most {MAX_FIELD_PATH_LEN} characters"
    ))
}

/// The JSON value at `path` inside the `record` column, in whatever type the
/// stored JSON gives it.
fn record_field_sql(path: &str) -> Result<String, RecordsError> {
    if !is_valid_json_field_path(path) {
        return Err(invalid_field(path));
    }
    Ok(format!("json_extract(record, '$.{path}')"))
}

/// The JSON value at `path` as text, which is what a pattern match compares.
///
/// The cast names on both backends the type only one of them would pick:
/// SQLite's `json_extract` hands back the value's own JSON type, while
/// Postgres's `->>` is already text.
pub fn record_text_field_sql(path: &str) -> Result<String, RecordsError> {
    Ok(format!("CAST({} AS TEXT)", record_field_sql(path)?))
}

/// A record filter's expression and bind.
///
/// The filter value's own JSON type is the comparison type: a number compares
/// as a number and a string as a string, so `score > 100` answers the
/// question it asks. Comparing as text instead would answer a different one —
/// `'50' > '100'` is true — and a record body carries no schema to overrule
/// the value with.
///
/// Each backend needs its own form to express that. SQLite's `json_extract`
/// already yields the stored value's type, so a typed bind compares against
/// it directly. Postgres's `->>` yields text, so the comparison is against
/// the `jsonb` value from `->`, whose ordering is numeric within numbers; the
/// bound value is its own JSON encoding, cast back to `jsonb`. The encoding
/// comes from `serde_json`, so it is always valid JSON — a malformed literal
/// would fail the cast. A `::numeric` cast on the column is the alternative
/// and is unusable: it evaluates per row, so one row holding a string fails
/// the whole query.
///
/// The two agree exactly for a field holding one JSON type per row. They
/// disagree about a number against a string, which is the same ill-defined
/// shape [`record_sort_sql`] documents for ordering.
///
/// A pattern match is textual by definition, so there both sides go to text.
fn record_bind_on(
    backend: DatabaseBackend,
) -> impl Fn(&str, &str, Value) -> Result<(String, Value), RecordsError> {
    move |path, op, value| record_field_bind(path, op, value, backend)
}

fn record_field_bind(
    path: &str,
    op: &str,
    value: Value,
    backend: DatabaseBackend,
) -> Result<(String, Value), RecordsError> {
    if value.is_null() {
        return Err(invalid(format!(
            "filter on '{path}' has a null value: compare against a string, number or boolean"
        )));
    }
    if is_text_op(op) {
        return Ok((
            format!("{} {op} ?", record_text_field_sql(path)?),
            Value::String(bind_as_text(value)),
        ));
    }
    Ok(match backend {
        DatabaseBackend::Sqlite => (format!("{} {op} ?", record_field_sql(path)?), value),
        DatabaseBackend::Postgres => (
            format!("{} {op} CAST(? AS jsonb)", postgres_jsonb_chain(path)?),
            Value::String(value.to_string()),
        ),
    })
}

fn column_sql(name: &str) -> Result<String, RecordsError> {
    if !is_valid_identifier(name) {
        return Err(invalid(format!("invalid column name '{name}'")));
    }
    Ok(name.to_string())
}

/// The Postgres `jsonb` chain for a record field, validated here because
/// this is where a caller's path becomes part of a statement.
fn postgres_jsonb_chain(path: &str) -> Result<String, RecordsError> {
    if !is_valid_json_field_path(path) {
        return Err(invalid_field(path));
    }
    Ok(postgres_json_chain("record", path, "->"))
}

/// The sort key for a record field: the JSON value in the form each backend
/// orders by the value's own type rather than as text.
///
/// Text ordering puts 100 before 2, so a numeric field has to sort as a
/// number. SQLite's `json_extract` already yields one; Postgres's `->>`
/// yields text, and `jsonb` from `->` is the form whose ordering is numeric
/// within numbers. A `(… ->> …)::numeric` cast would also order numerically
/// and is the wrong tool: it evaluates for every row, so a single row whose
/// value is not a number fails the whole query.
///
/// The two orderings agree for a field that is numeric in every row. They do
/// not agree when one field holds several JSON types across rows, which is a
/// sort with no correct answer; both are at least deterministic.
fn record_sort_sql(path: &str, backend: DatabaseBackend) -> Result<String, RecordsError> {
    match backend {
        DatabaseBackend::Sqlite => record_field_sql(path),
        DatabaseBackend::Postgres => postgres_jsonb_chain(path),
    }
}

/// An `ORDER BY` term, with a null placement only where the expression can
/// actually be null.
///
/// The placement restates SQLite's own defaults so that Postgres, whose
/// defaults are the opposite, puts an absent value in the same place. Stating
/// it on an expression that is never null changes no answer and costs the
/// index: Postgres will not use a `NOT NULL` constraint to prove the two
/// orderings equivalent, so `ORDER BY uri ASC NULLS FIRST` sorts the whole
/// collection where `ORDER BY uri ASC` walks twenty rows of the primary key.
///
/// Where it is stated the cost is real and accepted, not absent: every query
/// here carries a collection predicate, which lets the
/// `(collection, indexed_at DESC)` index serve an `indexed_at` ordering, and
/// the placement defeats that in both directions. It buys agreement between
/// the backends about where a row with no arrival time sorts, which is worth
/// more than the plan.
fn order_by(expression: &str, direction: &str, nullable: bool) -> String {
    if !nullable {
        return format!("{expression} {direction}");
    }
    let nulls = match direction {
        "ASC" => "NULLS FIRST",
        _ => "NULLS LAST",
    };
    format!("{expression} {direction} {nulls}")
}

/// Whether an operator compares its operands as text rather than by their own
/// type.
fn is_text_op(op: &str) -> bool {
    matches!(op, "LIKE" | "NOT LIKE" | "ILIKE")
}

/// A table filter's expression and bind, given the types `column_types`
/// reports for the table's columns.
///
/// A table filter names a real column whose SQL type the guest never states,
/// and the two backends disagree about what to do when the JSON value's type
/// is not that column's: SQLite applies the column's affinity and compares
/// anyway, Postgres refuses to compare the two types at all. Binding from the
/// JSON type alone is therefore right on one backend only, so the value is
/// brought to the column's own type first. A pattern match is textual by
/// definition, so there the column goes to text instead.
///
/// The lookup is case-folded because both backends report a column's
/// *declared* name whatever case the query asked for, while identifiers
/// themselves are case-insensitive. Matching exactly would miss the type for
/// a filter on `Score` against a column named `score` and leave Postgres
/// raising the very type error this coercion exists to prevent.
fn table_field_bind(
    column_types: &BTreeMap<String, AnyTypeInfoKind>,
    name: &str,
    op: &str,
    value: Value,
) -> Result<(String, Value), RecordsError> {
    let column = column_sql(name)?;
    if is_text_op(op) {
        return Ok((
            format!("CAST({column} AS TEXT) {op} ?"),
            Value::String(bind_as_text(value)),
        ));
    }
    if value.is_null() {
        return Err(invalid(format!(
            "filter on '{column}' has a null value: compare against a string, number or boolean"
        )));
    }
    let value = match column_types.get(&column.to_ascii_lowercase()) {
        Some(kind) => coerce_to_column(&column, *kind, value)?,
        None => value,
    };
    Ok((format!("{column} {op} ?"), value))
}

/// Re-type a filter value as `kind`, or refuse it as unsuitable for the
/// column. Refusing is the one answer both backends can give: SQLite would
/// coerce a mismatch to something arbitrary and Postgres would fail deep in
/// the driver with a type error naming neither the column nor the filter.
fn coerce_to_column(
    column: &str,
    kind: AnyTypeInfoKind,
    value: Value,
) -> Result<Value, RecordsError> {
    let integral = |f: f64| (f.fract() == 0.0 && f.is_finite()).then_some(f as i64);
    let coerced = match kind {
        AnyTypeInfoKind::Bool => match &value {
            Value::Bool(_) => Some(value.clone()),
            Value::Number(n) => match n.as_i64() {
                Some(0) => Some(json!(false)),
                Some(1) => Some(json!(true)),
                _ => None,
            },
            Value::String(s) => match s.as_str() {
                "true" | "t" | "1" => Some(json!(true)),
                "false" | "f" | "0" => Some(json!(false)),
                _ => None,
            },
            _ => None,
        },
        AnyTypeInfoKind::SmallInt | AnyTypeInfoKind::Integer | AnyTypeInfoKind::BigInt => {
            match &value {
                Value::Number(n) if n.is_i64() => Some(value.clone()),
                Value::Number(n) => n.as_f64().and_then(integral).map(|i| json!(i)),
                Value::Bool(b) => Some(json!(i64::from(*b))),
                Value::String(s) => s.trim().parse::<i64>().ok().map(|i| json!(i)),
                _ => None,
            }
        }
        AnyTypeInfoKind::Real | AnyTypeInfoKind::Double => match &value {
            Value::Number(n) => n.as_f64().filter(|f| f.is_finite()).map(|f| json!(f)),
            Value::String(s) => s
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|f| f.is_finite())
                .map(|f| json!(f)),
            _ => None,
        },
        AnyTypeInfoKind::Text => Some(Value::String(bind_as_text(value.clone()))),
        // A blob or an untyped expression has no JSON counterpart to convert
        // towards, so the value's own type is the best available guess.
        AnyTypeInfoKind::Blob | AnyTypeInfoKind::Null => Some(value.clone()),
    };
    coerced.ok_or_else(|| {
        invalid(format!(
            "filter value {value} does not fit column '{column}', which holds {}",
            kind_name(kind)
        ))
    })
}

fn kind_name(kind: AnyTypeInfoKind) -> &'static str {
    match kind {
        AnyTypeInfoKind::Bool => "a boolean",
        AnyTypeInfoKind::SmallInt | AnyTypeInfoKind::Integer | AnyTypeInfoKind::BigInt => {
            "an integer"
        }
        AnyTypeInfoKind::Real | AnyTypeInfoKind::Double => "a number",
        AnyTypeInfoKind::Text => "text",
        AnyTypeInfoKind::Blob => "a blob",
        AnyTypeInfoKind::Null => "no type",
    }
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

/// A value as the text a pattern match compares against. A JSON string is
/// its own contents; anything else is its JSON encoding.
fn bind_as_text(value: Value) -> String {
    match value {
        Value::String(s) => s,
        other => other.to_string(),
    }
}

type AnyQuery<'q> = sqlx::query::Query<'q, sqlx::Any, sqlx::any::AnyArguments>;
type AnyQueryAs<'q, O> = sqlx::query::QueryAs<'q, sqlx::Any, O, sqlx::any::AnyArguments>;

/// Bind one value as the SQL type its JSON type names: a number as `i64`, or
/// `f64` when it does not fit; a boolean as `bool`; a string as text.
///
/// Where a bound value has to be some *other* type — the column's, or a
/// backend's JSON encoding — the caller has already converted it, so this
/// stays a faithful mapping of JSON onto SQL rather than a second place
/// deciding types. See [`table_field_bind`] and [`record_field_bind`].
fn bind_value<'q>(query: AnyQuery<'q>, value: &Value) -> AnyQuery<'q> {
    match value {
        Value::Number(n) if n.is_i64() => query.bind(n.as_i64().expect("checked is_i64")),
        Value::Number(n) => query.bind(n.as_f64().unwrap_or_default()),
        Value::Bool(b) => query.bind(*b),
        Value::String(s) => query.bind(s.clone()),
        other => query.bind(other.to_string()),
    }
}

/// As [`bind_value`], for the query shapes that decode into a row type.
fn bind_value_as<'q, O>(query: AnyQueryAs<'q, O>, value: &Value) -> AnyQueryAs<'q, O> {
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
) -> Result<(String, Vec<Value>), RecordsError> {
    let mut binds = vec![Value::String(spec.collection.clone())];
    let mut where_clause = String::from("WHERE collection = ?");
    if let Some(did) = &spec.did {
        where_clause.push_str(" AND did = ?");
        binds.push(Value::String(did.clone()));
    }

    let sql = if let Some(Sort { field, direction }) = &spec.sort {
        // A JSON path is null wherever the field is absent, which no row of a
        // schemaless body guarantees; a bare column is null only where the
        // schema lets it be.
        let (column, nullable) = if TOP_LEVEL_COLUMNS.contains(&field.as_str()) {
            (
                column_sql(field)?,
                NULLABLE_TOP_LEVEL_COLUMNS.contains(&field.as_str()),
            )
        } else {
            (record_sort_sql(field, backend)?, true)
        };
        let order = order_by(&column, direction_sql(direction)?, nullable);
        if let Some(filter) = &spec.filter {
            let clause = filter_sql(filter, backend, &record_bind_on(backend), 0, &mut binds)?;
            where_clause.push_str(" AND ");
            where_clause.push_str(&clause);
        }
        format!(
            "SELECT {} FROM happyview_records {where_clause} ORDER BY {order} LIMIT ? OFFSET ?",
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
            let clause = filter_sql(filter, backend, &record_bind_on(backend), 0, &mut binds)?;
            where_clause.push_str(" AND ");
            where_clause.push_str(&clause);
        }
        format!(
            "SELECT {} FROM happyview_records {where_clause} ORDER BY created_at DESC, uri DESC LIMIT ?",
            record_columns("")
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

    let mut q = crate::db::query_as::<RecordRow>(&sql);
    for bind in &binds {
        q = bind_value_as(q, bind);
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
        let clause = filter_sql(filter, backend, &record_bind_on(backend), 0, &mut binds)?;
        sql.push_str(" AND ");
        sql.push_str(&clause);
    }
    let sql = adapt_sql(&sql, backend);
    let mut q = crate::db::query_as::<(i64,)>(&sql);
    for bind in &binds {
        q = bind_value_as(q, bind);
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
    let columns = record_columns("");

    // The field goes through `record_field_sql` so `adapt_sql` can rewrite it:
    // a hand-written `record::jsonb->>'author.handle'` reads a top-level key
    // of that literal name and matches nothing, where the rewriter builds the
    // chain a dotted path means. `ilike` is the one token the two dialects
    // spell differently, and `normalize_op` already knows which.
    let field = record_field_sql(&spec.field)?;
    let ilike = normalize_op("ilike", backend)?;
    let sql = adapt_sql(
        &format!(
            "SELECT {columns} FROM happyview_records \
             WHERE collection = ? \
               AND {field} {ilike} ? \
             ORDER BY \
               CASE \
                 WHEN LOWER({field}) = LOWER(?) THEN 0 \
                 WHEN LOWER({field}) LIKE LOWER(?) || '%' THEN 1 \
                 ELSE 2 \
               END, \
               {field} \
             LIMIT ?"
        ),
        backend,
    );
    let rows: Vec<RecordRow> = crate::db::query_as(&sql)
        .bind(&spec.collection)
        .bind(&like_pattern)
        .bind(&spec.query)
        .bind(&spec.query)
        .bind(limit)
        .fetch_all(db)
        .await?;

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
    column_types: &BTreeMap<String, AnyTypeInfoKind>,
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
        let bind =
            |name: &str, op: &str, value: Value| table_field_bind(column_types, name, op, value);
        let clause = filter_sql(filter, backend, &bind, 0, &mut binds)?;
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

/// Every distinct column a filter names, in the order first mentioned.
fn filter_columns(filter: &Filter, into: &mut Vec<String>) {
    match filter {
        Filter::Condition(c) => {
            if is_valid_identifier(&c.field) && !into.contains(&c.field) {
                into.push(c.field.clone());
            }
        }
        Filter::Group { conditions, .. } => {
            for condition in conditions {
                filter_columns(condition, into);
            }
        }
    }
}

/// The type each of `columns` holds in `table`, as the database reports it
/// for a prepared `SELECT` naming exactly those columns, keyed by the
/// lower-cased column name.
///
/// Only the filtered columns are named: a `SELECT *` would also have to
/// describe siblings, and a column of a type the `Any` driver has no
/// counterpart for fails the whole preparation. An unanswerable one yields no
/// types rather than an error, leaving the filter to bind by JSON type as it
/// would with no database to ask.
///
/// Preparation, not description: `Executor::describe` carries the same column
/// types but is `#[doc(hidden)]` and compiled only under a sqlx feature this
/// crate never asks for, arriving by way of the default `macros` feature it
/// does not use. It also costs more than it gives here — a `pg_attribute`
/// join for nullability on Postgres, an `EXPLAIN` bytecode walk on SQLite —
/// when only the column types are read.
async fn column_types_for(
    db: &sqlx::AnyPool,
    table: &str,
    columns: &[String],
) -> BTreeMap<String, AnyTypeInfoKind> {
    let empty = BTreeMap::new();
    if columns.is_empty() || !is_valid_identifier(table) {
        return empty;
    }
    let sql = format!("SELECT {} FROM {table}", columns.join(", "));
    if check_raw_sql_tables(&sql).is_err() {
        return empty;
    }
    let Ok(prepared) = db
        .prepare_with(AssertSqlSafe(sql).into_sql_str(), &[])
        .await
    else {
        return empty;
    };
    sqlx::Statement::columns(&prepared)
        .iter()
        .map(|column| {
            (
                sqlx::Column::name(column).to_ascii_lowercase(),
                sqlx::Column::type_info(column).kind(),
            )
        })
        .collect()
}

pub async fn table_query(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    spec: TableQuery,
) -> Result<Value, RecordsError> {
    let count = spec.count;
    let column_types = match &spec.filter {
        Some(filter) => {
            let mut columns = Vec::new();
            filter_columns(filter, &mut columns);
            column_types_for(db, &spec.table, &columns).await
        }
        None => BTreeMap::new(),
    };
    let (sql, binds) = table_query_sql(&spec, backend, &column_types)?;

    if count {
        let mut q = crate::db::query_as::<(i64,)>(&sql);
        for bind in &binds {
            q = bind_value_as(q, bind);
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
/// This is index-only: nothing is written to a PDS, so Jetstream never echoes
/// it back. `indexed_at` and `cid` describe what arrived from the network, so
/// an update leaves both alone (unlike writes that will be echoed, which clear
/// `indexed_at` so the echo re-stamps it): the stored CID describes the version
/// the PDS holds and is what strongRefs point at, and a local edit does not
/// change either. An insert may record a CID the caller was given by the
/// network, and otherwise stores an empty one rather than NULL — the column is
/// NOT NULL on both backends, and `cid_verify` already reads an empty CID as
/// "nothing to check".
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
/// echo's to set: bound NULL on insert and cleared on update, so the
/// identical echo re-stamps the row.
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
                   cid = EXCLUDED.cid, indexed_at = NULL"#,
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
    use serial_test::serial;

    /// A field expression that is the field name itself, for the tests that
    /// exercise the tree walk rather than any one table's columns.
    fn bare_field(field: &str, op: &str, value: Value) -> Result<(String, Value), RecordsError> {
        Ok((format!("{field} {op} ?"), value))
    }

    fn cond(field: &str, op: &str, value: Value) -> Filter {
        Filter::Condition(Condition {
            field: field.into(),
            op: op.into(),
            value,
        })
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
                json!("c"),
                json!("did:plc:a"),
                json!("2026-01-01T00:00:00Z"),
                json!("2026-01-01T00:00:00Z"),
                json!("at://x"),
                json!("active")
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
            sql.ends_with(
                "ORDER BY json_extract(record, '$.createdAt') ASC NULLS FIRST LIMIT ? OFFSET ?"
            ),
            "{sql}"
        );
        assert_eq!(binds, vec![json!("c")]);
    }

    /// A null placement only where the expression can be null. On a `NOT
    /// NULL` column it changes no answer, and Postgres will not use the
    /// constraint to prove the two orderings equivalent — `ORDER BY uri ASC
    /// NULLS FIRST` sorts the whole collection where `ORDER BY uri ASC` walks
    /// twenty rows of the primary key.
    #[test]
    fn a_sort_states_null_placement_only_for_a_nullable_expression() {
        let sorted = |field: &str, direction: &str| {
            let spec = RecordsQuery {
                collection: "c".into(),
                did: None,
                filter: None,
                sort: Some(Sort {
                    field: field.into(),
                    direction: direction.into(),
                }),
                limit: None,
                cursor: None,
            };
            records_query_sql(&spec, Sqlite).unwrap().0
        };

        for (field, direction) in [("uri", "asc"), ("did", "asc"), ("uri", "desc")] {
            let sql = sorted(field, direction);
            assert!(!sql.contains("NULLS"), "{field} {direction}: {sql}");
        }

        assert!(
            sorted("indexed_at", "asc").contains("ORDER BY indexed_at ASC NULLS FIRST"),
            "{}",
            sorted("indexed_at", "asc")
        );
        assert!(
            sorted("indexed_at", "desc").contains("ORDER BY indexed_at DESC NULLS LAST"),
            "{}",
            sorted("indexed_at", "desc")
        );
        // A JSON path is null wherever the field is absent, whatever the row.
        assert!(
            sorted("score", "asc").contains("ASC NULLS FIRST"),
            "{}",
            sorted("score", "asc")
        );
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
            &|p: &str, op: &str, v: Value| Ok((format!("json_extract(record, '$.{p}') {op} ?"), v)),
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
        let err = filter_sql(&deep, Sqlite, &bare_field, 0, &mut Vec::new()).unwrap_err();
        assert!(err.to_string().contains("nesting"), "{err}");
    }

    #[test]
    fn filter_rejects_bad_combine_and_empty_group() {
        let bad = Filter::Group {
            combine: "xor".into(),
            conditions: vec![cond("a", "=", json!("1"))],
        };
        assert!(filter_sql(&bad, Sqlite, &bare_field, 0, &mut Vec::new()).is_err());
        let empty = Filter::Group {
            combine: "and".into(),
            conditions: vec![],
        };
        assert!(filter_sql(&empty, Sqlite, &bare_field, 0, &mut Vec::new()).is_err());
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
        let (sql, binds) = table_query_sql(&spec, Sqlite, &BTreeMap::new()).unwrap();
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
        let (sql, _) = table_query_sql(&count, Sqlite, &BTreeMap::new()).unwrap();
        assert_eq!(sql, "SELECT COUNT(*) FROM leaderboard");

        let bad = TableQuery {
            table: "lead;board".into(),
            filter: None,
            sort: None,
            limit: None,
            count: false,
        };
        assert!(matches!(
            table_query_sql(&bad, Sqlite, &BTreeMap::new()),
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
            table_query_sql(&bad_col, Sqlite, &BTreeMap::new()),
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
        let err = table_query_sql(&spec, Sqlite, &BTreeMap::new()).unwrap_err();
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

    /// `index_put` is index-only and never echoed, so a local edit leaves
    /// both columns alone.
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
    async fn mirrored_put_replaces_the_cid_and_clears_indexed_at() {
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
        assert_eq!(indexed_at, None, "the identical echo must re-stamp the row");
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

    /// Every backend the suite can reach: the migrated in-memory SQLite
    /// database always, plus whatever `TEST_DATABASE_URL` names. A read whose
    /// statement `adapt_sql` rewrites is half-tested by either alone, and the
    /// SQLite half is the one that needs no rewriting.
    async fn backends() -> Vec<(sqlx::AnyPool, DatabaseBackend)> {
        let mut out = vec![(crate::test_support::migrated_memory_pool().await, Sqlite)];
        match std::env::var("TEST_DATABASE_URL") {
            Ok(url) => {
                let backend = DatabaseBackend::from_url(&url);
                out.push((crate::db::connect(&url, backend).await, backend));
            }
            Err(_) => eprintln!("TEST_DATABASE_URL unset: the Postgres half is skipped"),
        }
        out
    }

    /// Two records under a collection of this test's own, so a shared
    /// database cannot mix them with another suite's rows.
    async fn seed_records(db: &sqlx::AnyPool, backend: DatabaseBackend, collection: &str) {
        crate::db::query(&adapt_sql(
            "DELETE FROM happyview_records WHERE collection = ?",
            backend,
        ))
        .bind(collection)
        .execute(db)
        .await
        .expect("clear the collection");
        let insert = adapt_sql(
            "INSERT INTO happyview_records \
             (uri, did, collection, rkey, record, cid, indexed_at, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, NULL, ?)",
            backend,
        );
        for (rkey, body) in [
            (
                "1",
                json!({"n": "one", "score": 50, "author": {"handle": "alice"}}),
            ),
            (
                "2",
                json!({"n": "two", "score": 150, "author": {"handle": "bob"}}),
            ),
        ] {
            crate::db::query(&insert)
                .bind(format!("at://did:plc:seed/{collection}/{rkey}"))
                .bind("did:plc:seed")
                .bind(collection)
                .bind(rkey)
                .bind(body.to_string())
                .bind(format!("cid{rkey}"))
                .bind(format!("2026-01-01T00:00:0{rkey}+00:00"))
                .execute(db)
                .await
                .expect("seed a record");
        }
    }

    /// A table with one column of each type a filter has to compare against.
    async fn seed_scores(db: &sqlx::AnyPool, table: &str) {
        crate::db::query(&format!(
            "CREATE TABLE IF NOT EXISTS {table} (name TEXT, score INTEGER)"
        ))
        .execute(db)
        .await
        .expect("create the table");
        crate::db::query(&format!("DELETE FROM {table}"))
            .execute(db)
            .await
            .expect("clear the table");
        for (name, score) in [("x", 50), ("y", 150), ("150", 7)] {
            crate::db::query(&format!(
                "INSERT INTO {table} (name, score) VALUES ('{name}', {score})"
            ))
            .execute(db)
            .await
            .expect("seed a row");
        }
    }

    fn one_filter(field: &str, op: &str, value: Value) -> Option<Filter> {
        Some(cond(field, op, value))
    }

    /// Equality on a numeric record field. The filter value's JSON type is
    /// the comparison type, so the JSON number `150` matches a stored `150`
    /// and the JSON string `"150"` does not — the caller asked about a
    /// string, and the record holds a number. Both readings have to be the
    /// same on either backend, which is the part that was broken: SQLite's
    /// `json_extract` hands back the stored number, so a filter binding text
    /// matched nothing there while Postgres's `->>` is text and matched.
    #[tokio::test]
    #[serial]
    async fn record_equality_follows_the_filter_values_json_type_on_both_backends() {
        let collection = "host.records.numeric";
        for (db, backend) in backends().await {
            seed_records(&db, backend, collection).await;
            let matching = |value: Value| async {
                records_query(
                    &db,
                    backend,
                    RecordsQuery {
                        collection: collection.into(),
                        did: None,
                        filter: one_filter("score", "=", value),
                        sort: None,
                        limit: None,
                        cursor: None,
                    },
                )
                .await
                .expect("the query should run")
                .records
            };

            let number = matching(json!(150)).await;
            assert_eq!(
                number.len(),
                1,
                "score = 150 on {backend:?} should match one record"
            );
            assert_eq!(number[0]["record"]["n"], "two");

            assert!(
                matching(json!("150")).await.is_empty(),
                "score = \"150\" on {backend:?} should match no record, since the stored value is a number"
            );

            // A string field answers the mirror image: the string matches and
            // a number does not.
            let name = matching_field(&db, backend, collection, "n", json!("two")).await;
            assert_eq!(name.len(), 1, "n = \"two\" on {backend:?}");

            let n = records_count(
                &db,
                backend,
                RecordsCount {
                    collection: collection.into(),
                    did: None,
                    filter: one_filter("score", "=", json!(50)),
                },
            )
            .await
            .expect("the count should run");
            assert_eq!(n, 1, "score = 50 on {backend:?} should count one record");
        }
    }

    /// The records an equality filter on `field` matches.
    async fn matching_field(
        db: &sqlx::AnyPool,
        backend: DatabaseBackend,
        collection: &str,
        field: &str,
        value: Value,
    ) -> Vec<Value> {
        records_query(
            db,
            backend,
            RecordsQuery {
                collection: collection.into(),
                did: None,
                filter: one_filter(field, "=", value),
                sort: None,
                limit: None,
                cursor: None,
            },
        )
        .await
        .expect("the query should run")
        .records
    }

    /// A search on a dotted field, which needs the JSON access `adapt_sql`
    /// builds: a Postgres `->>'author.handle'` reads a top-level key of that
    /// literal name and matches nothing. A single-segment field is the one
    /// case where the two forms agree, so it cannot show this.
    #[tokio::test]
    #[serial]
    async fn record_search_matches_a_nested_field_on_both_backends() {
        let collection = "host.records.nested";
        for (db, backend) in backends().await {
            seed_records(&db, backend, collection).await;
            let found = records_search(
                &db,
                backend,
                RecordsSearch {
                    collection: collection.into(),
                    field: "author.handle".into(),
                    query: "ALI".into(),
                    limit: None,
                },
            )
            .await
            .expect("the search should run");
            assert_eq!(
                found.len(),
                1,
                "author.handle on {backend:?} should match one record"
            );
            assert_eq!(found[0]["record"]["n"], "one");
        }
    }

    /// A table filter whose JSON type is not the column's. Postgres refuses
    /// to compare the two at all and SQLite coerces to whatever the column's
    /// affinity says, so the value is brought to the column's own type first
    /// and both backends answer alike.
    #[tokio::test]
    #[serial]
    async fn table_filters_compare_against_the_column_type_on_both_backends() {
        let table = "host_records_scores";
        for (db, backend) in backends().await {
            seed_scores(&db, table).await;
            let rows = |filter: Option<Filter>| TableQuery {
                table: table.into(),
                filter,
                sort: None,
                limit: None,
                count: false,
            };

            // A string against an integer column, and a number against the
            // same column: one shape that only SQLite's affinity accepts,
            // and one that both do.
            for value in [json!("100"), json!(100)] {
                let got = table_query(&db, backend, rows(one_filter("score", ">", value.clone())))
                    .await
                    .expect("the query should run");
                assert_eq!(
                    got.as_array().expect("an array of rows").len(),
                    1,
                    "score > {value} on {backend:?} should match one row"
                );
                assert_eq!(got[0]["name"], "y");
            }

            // A number against a text column, the converse mismatch.
            let got = table_query(&db, backend, rows(one_filter("name", "=", json!(150))))
                .await
                .expect("the query should run");
            assert_eq!(got.as_array().expect("an array of rows").len(), 1);
            assert_eq!(got[0]["score"], 7);

            // A pattern match is textual whatever the column holds.
            let got = table_query(&db, backend, rows(one_filter("score", "like", json!("1%"))))
                .await
                .expect("the query should run");
            assert_eq!(got.as_array().expect("an array of rows").len(), 1);
            assert_eq!(got[0]["name"], "y");
        }
    }

    /// A value no shape of the column can hold. Refusing by name is the one
    /// answer both backends can give: Postgres fails in the driver on a type
    /// it will not compare, and SQLite coerces to whatever the column's
    /// affinity says and returns an empty result that reads as "no matches".
    #[tokio::test]
    #[serial]
    async fn a_table_filter_value_the_column_cannot_hold_is_refused_by_name() {
        let table = "host_records_scores";
        for (db, backend) in backends().await {
            seed_scores(&db, table).await;
            let err = table_query(
                &db,
                backend,
                TableQuery {
                    table: table.into(),
                    filter: one_filter("score", "=", json!("abc")),
                    sort: None,
                    limit: None,
                    count: false,
                },
            )
            .await
            .expect_err("a value that does not fit should be refused");
            assert!(
                matches!(err, RecordsError::InvalidSpec(_)),
                "on {backend:?}: {err}"
            );
            assert!(err.to_string().contains("score"), "{err}");
        }
    }

    /// The four shapes a record field takes across rows, seeded under a
    /// collection per shape so each sort is over that shape alone.
    async fn seed_sort_rows(
        db: &sqlx::AnyPool,
        backend: DatabaseBackend,
        collection: &str,
        rows: &[(&str, &str)],
    ) {
        crate::db::query(&adapt_sql(
            "DELETE FROM happyview_records WHERE collection = ?",
            backend,
        ))
        .bind(collection)
        .execute(db)
        .await
        .expect("clear the collection");
        let insert = adapt_sql(
            "INSERT INTO happyview_records \
             (uri, did, collection, rkey, record, cid, indexed_at, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, NULL, ?)",
            backend,
        );
        for (rkey, body) in rows {
            crate::db::query(&insert)
                .bind(format!("at://did:plc:seed/{collection}/{rkey}"))
                .bind("did:plc:seed")
                .bind(collection)
                .bind(*rkey)
                .bind(*body)
                .bind("cidsort")
                .bind("2026-01-01T00:00:00+00:00")
                .execute(db)
                .await
                .expect("seed a record");
        }
    }

    /// The rkeys a sort on `field` returns, in order.
    async fn sorted_rkeys(
        db: &sqlx::AnyPool,
        backend: DatabaseBackend,
        collection: &str,
        field: &str,
        direction: &str,
    ) -> Vec<String> {
        let page = records_query(
            db,
            backend,
            RecordsQuery {
                collection: collection.into(),
                did: None,
                filter: None,
                sort: Some(Sort {
                    field: field.into(),
                    direction: direction.into(),
                }),
                limit: Some(MAX_LIMIT),
                cursor: None,
            },
        )
        .await
        .expect("the sorted query should run");
        page.records
            .into_iter()
            .map(|r| r["rkey"].as_str().expect("an rkey").to_string())
            .collect()
    }

    /// A field numeric in every row, the case that has a correct answer.
    /// Postgres's `->>` yields text, which puts 100 before 2; the `jsonb`
    /// value from `->` orders numerically, as SQLite's `json_extract`
    /// already does.
    #[tokio::test]
    #[serial]
    async fn a_numeric_field_sorts_numerically_on_both_backends() {
        let collection = "host.records.sort.numeric";
        let rows = [
            ("2", r#"{"f": 2}"#),
            ("10", r#"{"f": 10}"#),
            ("1p5", r#"{"f": 1.5}"#),
            ("100", r#"{"f": 100}"#),
            ("neg3", r#"{"f": -3}"#),
        ];
        for (db, backend) in backends().await {
            seed_sort_rows(&db, backend, collection, &rows).await;
            assert_eq!(
                sorted_rkeys(&db, backend, collection, "f", "asc").await,
                ["neg3", "1p5", "2", "10", "100"],
                "ascending on {backend:?}"
            );
            assert_eq!(
                sorted_rkeys(&db, backend, collection, "f", "desc").await,
                ["100", "10", "2", "1p5", "neg3"],
                "descending on {backend:?}"
            );
        }
    }

    /// A field absent from some rows, and JSON null in others. Both read as
    /// no value, and both sort at the ascending end on either backend:
    /// SQLite puts a SQL NULL first by default and Postgres puts one last,
    /// so the placement is stated rather than inherited.
    #[tokio::test]
    #[serial]
    async fn a_missing_or_null_field_sorts_at_the_ascending_end_on_both_backends() {
        let collection = "host.records.sort.absent";
        let rows = [
            ("absent", r#"{"other": 1}"#),
            ("null", r#"{"f": null}"#),
            ("one", r#"{"f": 1}"#),
            ("two", r#"{"f": 2}"#),
        ];
        for (db, backend) in backends().await {
            seed_sort_rows(&db, backend, collection, &rows).await;

            let asc = sorted_rkeys(&db, backend, collection, "f", "asc").await;
            assert_eq!(
                &asc[2..],
                ["one", "two"],
                "ascending on {backend:?}: {asc:?}"
            );
            assert!(
                asc[..2].contains(&"absent".to_string()) && asc[..2].contains(&"null".to_string()),
                "a missing and a null value lead on {backend:?}: {asc:?}"
            );

            let desc = sorted_rkeys(&db, backend, collection, "f", "desc").await;
            assert_eq!(
                &desc[..2],
                ["two", "one"],
                "descending on {backend:?}: {desc:?}"
            );
        }
    }

    /// A field holding several JSON types across rows. The two orders below
    /// differ, and that is the expectation rather than a fault left standing:
    /// a field that is a number in one row and a string in another has no
    /// correct ordering to converge on, and each backend is at least
    /// deterministic. SQLite compares storage classes, putting every number
    /// before every string; Postgres compares `jsonb`, whose class order runs
    /// null, string, number, boolean. Converging them would cost a second
    /// dialect branch — a `CASE` over `jsonb_typeof` against `json_type` — to
    /// pick one arbitrary answer over another, and a divergent path per
    /// backend is what every fault this file's dual-backend tests cover grew
    /// out of. A field with one type per row is the case that has an answer,
    /// and `a_numeric_field_sorts_numerically_on_both_backends` holds it.
    #[tokio::test]
    #[serial]
    async fn a_mixed_type_field_sorts_deterministically_and_differently_per_backend() {
        let collection = "host.records.sort.mixed";
        let rows = [
            ("num2", r#"{"f": 2}"#),
            ("num10", r#"{"f": 10}"#),
            ("strA", r#"{"f": "Apple"}"#),
            ("strZ", r#"{"f": "Zebra"}"#),
            ("yes", r#"{"f": true}"#),
        ];
        for (db, backend) in backends().await {
            seed_sort_rows(&db, backend, collection, &rows).await;
            let asc = sorted_rkeys(&db, backend, collection, "f", "asc").await;
            match backend {
                // `true` extracts as the integer 1, so it sorts among the
                // numbers rather than as a class of its own.
                DatabaseBackend::Sqlite => {
                    assert_eq!(asc, ["yes", "num2", "num10", "strA", "strZ"], "{asc:?}")
                }
                DatabaseBackend::Postgres => {
                    assert_eq!(asc, ["strA", "strZ", "num2", "num10", "yes"], "{asc:?}")
                }
            }
        }
    }

    /// Ordering comparisons on a numeric record field, the operators nothing
    /// covered. Comparing as text answers a different question — `'50'` is
    /// above `'100'` — so a filter that means "above 100" has to compare as a
    /// number, and has to give the same answer on both backends.
    #[tokio::test]
    #[serial]
    async fn ordering_a_numeric_record_field_compares_numerically_on_both_backends() {
        let collection = "host.records.ordering";
        for (db, backend) in backends().await {
            seed_records(&db, backend, collection).await;
            // Seeded scores are 50 and 150, on records "one" and "two".
            for (op, value, expected) in [
                (">", 100, vec!["two"]),
                (">=", 150, vec!["two"]),
                ("<", 100, vec!["one"]),
                ("<=", 50, vec!["one"]),
                (">", 200, vec![]),
                ("<", 10, vec![]),
            ] {
                let page = records_query(
                    &db,
                    backend,
                    RecordsQuery {
                        collection: collection.into(),
                        did: None,
                        filter: one_filter("score", op, json!(value)),
                        sort: None,
                        limit: None,
                        cursor: None,
                    },
                )
                .await
                .unwrap_or_else(|e| panic!("score {op} {value} on {backend:?}: {e}"));
                let got: Vec<&str> = page
                    .records
                    .iter()
                    .map(|r| r["record"]["n"].as_str().expect("a name"))
                    .collect();
                assert_eq!(got, expected, "score {op} {value} on {backend:?}");
            }

            let n = records_count(
                &db,
                backend,
                RecordsCount {
                    collection: collection.into(),
                    did: None,
                    filter: one_filter("score", ">", json!(100)),
                },
            )
            .await
            .expect("the count should run");
            assert_eq!(n, 1, "counting score > 100 on {backend:?}");
        }
    }

    /// A sort field the validator refuses, pinned at the call site rather
    /// than only in the validator: the Postgres arm interpolates its path, so
    /// the refusal has to survive someone rearranging how the two arms get
    /// their expression.
    #[test]
    fn records_sql_refuses_an_invalid_sort_field_on_every_backend() {
        for backend in [Sqlite, Postgres] {
            for field in [
                "a'||(SELECT 1)||'",
                "a'--",
                "a..b",
                "",
                "tags[x]",
                "tags[3000000000]",
            ] {
                let spec = RecordsQuery {
                    collection: "c".into(),
                    did: None,
                    filter: None,
                    sort: Some(Sort {
                        field: field.into(),
                        direction: "asc".into(),
                    }),
                    limit: None,
                    cursor: None,
                };
                assert!(
                    matches!(
                        records_query_sql(&spec, backend),
                        Err(RecordsError::InvalidSpec(_))
                    ),
                    "sort on {field:?} should be refused on {backend:?}"
                );
            }
        }
    }

    /// A filter naming a column in another case. Identifiers are
    /// case-insensitive on both backends, but each reports a column's
    /// declared name, so an exact-match type lookup misses and leaves
    /// Postgres refusing to compare the column against the JSON type.
    #[tokio::test]
    #[serial]
    async fn a_table_filter_finds_the_column_type_whatever_case_it_names() {
        let table = "host_records_scores";
        for (db, backend) in backends().await {
            seed_scores(&db, table).await;
            let rows = table_query(
                &db,
                backend,
                TableQuery {
                    table: table.into(),
                    filter: one_filter("SCORE", ">", json!("100")),
                    sort: None,
                    limit: None,
                    count: false,
                },
            )
            .await
            .unwrap_or_else(|e| panic!("a filter on SCORE on {backend:?}: {e}"));
            assert_eq!(rows.as_array().expect("an array of rows").len(), 1);
            assert_eq!(rows[0]["name"], "y");
        }
    }

    /// A null filter value, which neither backend can compare the way a
    /// caller would read it: SQLite's `NULL = NULL` is never true, while
    /// Postgres's `jsonb` null is an ordinary value that matches.
    #[tokio::test]
    #[serial]
    async fn a_null_filter_value_is_refused_by_name_on_both_backends() {
        let collection = "host.records.nullvalue";
        for (db, backend) in backends().await {
            seed_records(&db, backend, collection).await;
            let err = records_query(
                &db,
                backend,
                RecordsQuery {
                    collection: collection.into(),
                    did: None,
                    filter: one_filter("score", "=", Value::Null),
                    sort: None,
                    limit: None,
                    cursor: None,
                },
            )
            .await
            .expect_err("a null filter value should be refused");
            assert!(
                matches!(err, RecordsError::InvalidSpec(_)),
                "on {backend:?}: {err}"
            );
            assert!(err.to_string().contains("score"), "{err}");
        }
    }

    /// The default listing (no custom sort) reads
    /// `idx_records_collection_created_at_uri` in order, on both backends.
    #[tokio::test]
    async fn the_default_records_listing_walks_the_collection_created_at_index() {
        let spec = RecordsQuery {
            collection: "app.test.post".into(),
            did: None,
            filter: None,
            sort: None,
            limit: Some(50),
            cursor: Some(crate::db::encode_cursor(
                "2026-01-01T00:00:00+00:00",
                "at://did:plc:a/app.test.post/1",
            )),
        };
        let (sql, binds) = records_query_sql(&spec, Sqlite).unwrap();
        assert_eq!(binds.len(), 4, "the collection and three cursor values");
        let sql = crate::test_support::inline_binds(
            &sql,
            &[
                "'app.test.post'",
                "'2026-01-01T00:00:00+00:00'",
                "'2026-01-01T00:00:00+00:00'",
                "'at://did:plc:a/app.test.post/1'",
                "50",
            ],
        );
        for (pool, backend) in crate::test_support::test_pools().await {
            let plan = crate::test_support::query_plan(&pool, backend, &sql).await;
            crate::test_support::assert_index_without_sort(
                &plan,
                "idx_records_collection_created_at_uri",
                backend,
            );
        }
    }

    #[tokio::test]
    async fn the_superseded_record_indexes_are_gone() {
        for (pool, backend) in crate::test_support::test_pools().await {
            let sql = match backend {
                Sqlite => {
                    "SELECT name FROM sqlite_master WHERE type = 'index' \
                     AND name IN ('idx_records_collection', 'idx_records_created_at_uri')"
                }
                Postgres => {
                    "SELECT indexname FROM pg_indexes \
                     WHERE indexname IN ('idx_records_collection', 'idx_records_created_at_uri')"
                }
            };
            let left: Vec<(String,)> = crate::db::query_as(sql)
                .fetch_all(&pool)
                .await
                .expect("list indexes");
            assert!(left.is_empty(), "{backend:?} still has {left:?}");

            if backend == Postgres {
                // A failed CONCURRENTLY build leaves an index behind marked invalid.
                let (valid,): (bool,) = crate::db::query_as(
                    "SELECT i.indisvalid FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid \
                     WHERE c.relname = 'idx_records_collection_created_at_uri'",
                )
                .fetch_one(&pool)
                .await
                .expect("new index exists");
                assert!(valid);
            }
        }
    }
}

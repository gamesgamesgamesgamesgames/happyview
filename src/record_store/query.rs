//! Record queries for plugins and the Lua db global, moved here from plugin::host::records so the record SQL lives in one module.

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use happyview_plugin_sdk::wire::{
    BacklinksQuery, RecordEnvelope, RecordsCount, RecordsPage, RecordsQuery, RecordsSearch, Sort,
};
use serde_json::{Value, json};

use crate::db::{
    DatabaseBackend, adapt_sql, decode_cursor, encode_cursor, is_valid_json_field_path,
    postgres_json_chain,
};
use crate::plugin::host::records::{
    MAX_LIMIT, RecordsError, bind_as_text, bind_value_as, clamp_limit, column_sql, direction_sql,
    filter_sql, invalid, invalid_field, is_text_op, normalize_op, order_by,
};
use crate::record_store::{COLLECTION_ID, DID_ID, RecordStore, StoredRecord, records_relation};

/// Columns `records_query`'s sort accepts bare, as opposed to a JSON path
/// evaluated inside `record`.
const TOP_LEVEL_COLUMNS: [&str; 3] = ["indexed_at", "did", "uri"];

/// Those of [`TOP_LEVEL_COLUMNS`] the schema lets be null, and so the only
/// ones whose sort needs a null placement. `uri` and `did` are `NOT NULL` on
/// both backends.
const NULLABLE_TOP_LEVEL_COLUMNS: [&str; 1] = ["indexed_at"];

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

/// The columns every record read selects from [`records_relation`], in the
/// order [`RecordRow`] decodes them. `created_at_us` and `id` are there for
/// the keyset cursor, not the envelope.
fn record_columns() -> String {
    [
        "uri",
        "did",
        "collection",
        "rkey",
        "record",
        "cid",
        "indexed_at",
        "created_at_us",
        "id",
    ]
    .map(|column| format!("rec.{column}"))
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
    i64,
    i64,
);

/// What a page cursor that does not parse is refused with. Script runners
/// recognise it and answer the caller 400 `InvalidRequest`
/// (`lua::execute`), since the cursor is the caller's.
pub const INVALID_CURSOR: &str = "invalid cursor: request the first page again";

/// A keyset cursor: `(created_at_us, id)`. Anything else, including a cursor
/// from before the compact format (`created_at|uri`), is refused rather than
/// read as the first page.
fn keyset_cursor(cursor: &str) -> Result<(i64, i64), RecordsError> {
    decode_cursor(cursor)
        .and_then(|(ts, id)| Some((ts.parse().ok()?, id.parse().ok()?)))
        .ok_or_else(|| invalid(INVALID_CURSOR))
}

/// The keyset seek after `cursor`, newest first. A row-value comparison, so
/// both backends seek the `(collection_id, created_at, id)` index to the
/// cursor; the expanded `a < ? OR (a = ? AND b < ?)` is a filter on Postgres,
/// which then reads the collection from its newest record on every page.
const KEYSET_AFTER: &str = " AND (rec.created_at_us, rec.id) < (?, ?)";
const KEYSET_ORDER: &str = "ORDER BY rec.created_at_us DESC, rec.id DESC";

/// The one place a read turns a row into the shape a guest sees. The
/// rationale for the shape lives on [`RecordEnvelope`].
fn envelope(row: RecordRow) -> Value {
    let (uri, did, collection, rkey, record, cid, indexed_at, _, _) = row;
    stored_envelope(uri, did, collection, rkey, record, cid, indexed_at)
}

impl From<StoredRecord> for Value {
    fn from(rec: StoredRecord) -> Self {
        stored_envelope(
            rec.uri,
            rec.did,
            rec.collection,
            rec.rkey,
            rec.record,
            rec.cid,
            rec.indexed_at,
        )
    }
}

fn stored_envelope(
    uri: String,
    did: String,
    collection: String,
    rkey: String,
    record: String,
    cid: Option<String>,
    indexed_at: Option<String>,
) -> Value {
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
/// `(created_at, id)`. Bind values are returned in placeholder order; the
/// `LIMIT`/`OFFSET` integers themselves are appended by `records_query`.
pub fn records_query_sql(
    spec: &RecordsQuery,
    backend: DatabaseBackend,
) -> Result<(String, Vec<Value>), RecordsError> {
    let mut binds = vec![Value::String(spec.collection.clone())];
    let mut where_clause = format!("WHERE rec.collection_id = {COLLECTION_ID}");
    if let Some(did) = &spec.did {
        where_clause.push_str(&format!(" AND rec.did_id = {DID_ID}"));
        binds.push(Value::String(did.clone()));
    }
    let relation = records_relation(backend);

    let sql = if let Some(Sort { field, direction }) = &spec.sort {
        // A JSON path is null wherever the field is absent, which no row of a
        // schemaless body guarantees; a bare column is null only where the
        // schema lets it be.
        let (column, nullable) = if TOP_LEVEL_COLUMNS.contains(&field.as_str()) {
            let column = match column_sql(field)?.as_str() {
                // The stored microseconds order as the rendered text does.
                "indexed_at" => "rec.indexed_at_us".to_string(),
                other => format!("rec.{other}"),
            };
            (column, NULLABLE_TOP_LEVEL_COLUMNS.contains(&field.as_str()))
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
            "SELECT {} FROM {relation} {where_clause} ORDER BY {order} LIMIT ? OFFSET ?",
            record_columns()
        )
    } else {
        if let Some(cursor) = &spec.cursor {
            let (ts, id) = keyset_cursor(cursor)?;
            where_clause.push_str(KEYSET_AFTER);
            binds.extend([json!(ts), json!(id)]);
        }
        if let Some(filter) = &spec.filter {
            let clause = filter_sql(filter, backend, &record_bind_on(backend), 0, &mut binds)?;
            where_clause.push_str(" AND ");
            where_clause.push_str(&clause);
        }
        format!(
            "SELECT {} FROM {relation} {where_clause} {KEYSET_ORDER} LIMIT ?",
            record_columns()
        )
    };

    Ok((adapt_sql(&sql, backend), binds))
}

/// Paginated record listing: a custom sort walks pages with a base64-encoded
/// decimal offset; the default sort walks a keyset cursor on
/// `(created_at, id)` so pages stay stable under concurrent inserts.
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

/// A page walked by the `(created_at, id)` keyset: the cursor is the last
/// row's position, present only when the page came back full.
fn keyset_page(rows: Vec<RecordRow>, limit: i64) -> RecordsPage {
    let has_next = rows.len() as i64 == limit;
    let cursor = has_next
        .then(|| rows.last())
        .flatten()
        .map(|row| encode_cursor(&row.7.to_string(), &row.8.to_string()));
    let records = rows.into_iter().map(envelope).collect();
    RecordsPage { records, cursor }
}

pub async fn records_count(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    spec: RecordsCount,
) -> Result<i64, RecordsError> {
    let mut binds = vec![Value::String(spec.collection.clone())];
    let mut sql = format!(
        "SELECT COUNT(*) FROM {} WHERE rec.collection_id = {COLLECTION_ID}",
        records_relation(backend)
    );
    if let Some(did) = &spec.did {
        sql.push_str(&format!(" AND rec.did_id = {DID_ID}"));
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
    Ok(RecordStore::new(db.clone(), backend)
        .get(uri)
        .await?
        .map(Value::from))
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
    let columns = record_columns();

    // The field goes through `record_field_sql` so `adapt_sql` can rewrite it:
    // a hand-written `record::jsonb->>'author.handle'` reads a top-level key
    // of that literal name and matches nothing, where the rewriter builds the
    // chain a dotted path means. `ilike` is the one token the two dialects
    // spell differently, and `normalize_op` already knows which.
    let field = record_field_sql(&spec.field)?;
    let ilike = normalize_op("ilike", backend)?;
    let sql = adapt_sql(
        &format!(
            "SELECT {columns} FROM {} \
             WHERE rec.collection_id = {COLLECTION_ID} \
               AND {field} {ilike} ? \
             ORDER BY \
               CASE \
                 WHEN LOWER({field}) = LOWER(?) THEN 0 \
                 WHEN LOWER({field}) LIKE LOWER(?) || '%' THEN 1 \
                 ELSE 2 \
               END, \
               {field} \
             LIMIT ?",
            records_relation(backend)
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

/// Records in `spec.collection` that reference `spec.uri`, keyset-paginated
/// the same way as `records_query`. The target is parsed and matched by
/// interned id on the refs target index, never by the derived `target_uri`.
pub(crate) fn backlinks_query_sql(
    spec: &BacklinksQuery,
    backend: DatabaseBackend,
) -> Result<(String, Vec<Value>), RecordsError> {
    let target = crate::record_store::codec::RefTarget::parse(&spec.uri);
    let mut binds: Vec<Value> = vec![json!(target.did_text())];
    let target_collection = match target.collection() {
        Some(nsid) => {
            binds.push(json!(nsid));
            COLLECTION_ID.to_string()
        }
        None => target.kind().to_string(),
    };
    binds.push(json!(target.rkey()));
    binds.push(json!(spec.collection));
    let mut where_clause = format!(
        "WHERE f.target_did_id = {DID_ID} AND f.target_collection_id = {target_collection} \
         AND f.target_rkey = ? AND f.collection_id = {COLLECTION_ID}"
    );
    if let Some(did) = &spec.did {
        where_clause.push_str(&format!(" AND rec.did_id = {DID_ID}"));
        binds.push(json!(did));
    }
    if let Some(cursor) = &spec.cursor {
        let (ts, id) = keyset_cursor(cursor)?;
        where_clause.push_str(KEYSET_AFTER);
        binds.extend([json!(ts), json!(id)]);
    }
    let sql = format!(
        "SELECT {} FROM {} JOIN happyview_record_refs_v2 f ON f.source_id = rec.id \
         {where_clause} {KEYSET_ORDER} LIMIT ?",
        record_columns(),
        records_relation(backend)
    );
    Ok((adapt_sql(&sql, backend), binds))
}

pub async fn backlinks_query(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    spec: BacklinksQuery,
) -> Result<RecordsPage, RecordsError> {
    let limit = clamp_limit(spec.limit);
    let (sql, binds) = backlinks_query_sql(&spec, backend)?;
    let mut q = crate::db::query_as::<RecordRow>(&sql);
    for bind in &binds {
        q = bind_value_as(q, bind);
    }
    let rows = q.bind(limit).fetch_all(db).await?;
    Ok(keyset_page(rows, limit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use happyview_plugin_sdk::wire::RecordsQuery;

    /// The default listing (no custom sort) reads
    /// `idx_records_v2_collection_created_at_id` in order, on both backends.
    #[tokio::test]
    async fn the_default_records_listing_walks_the_collection_created_at_index() {
        for (pool, backend) in crate::test_support::test_pools().await {
            for did in [None, Some("did:plc:a".to_string())] {
                let spec = RecordsQuery {
                    collection: "app.test.post".into(),
                    did: did.clone(),
                    filter: None,
                    sort: None,
                    limit: Some(50),
                    cursor: Some(encode_cursor("1767225600000000", "42")),
                };
                let (sql, binds) = records_query_sql(&spec, backend).unwrap();
                let mut literals = vec!["'app.test.post'"];
                if did.is_some() {
                    literals.push("'did:plc:a'");
                }
                literals.extend(["1767225600000000", "42", "50"]);
                assert_eq!(binds.len() + 1, literals.len(), "{backend:?}");
                let sql = crate::test_support::inline_binds_on(&sql, backend, &literals);
                let plan = crate::test_support::query_plan(&pool, backend, &sql).await;
                crate::test_support::assert_index_without_sort(
                    &plan,
                    "idx_records_v2_collection_created_at_id",
                    backend,
                );
                // The cursor bounds the index scan, not a filter after it.
                assert!(
                    !plan
                        .iter()
                        .any(|line| line.contains("Filter") && line.contains("created_at")),
                    "{backend:?}: {plan:#?}"
                );
            }
        }
    }

    #[test]
    fn a_cursor_from_before_the_compact_format_is_refused() {
        let spec = RecordsQuery {
            collection: "app.test.post".into(),
            did: None,
            filter: None,
            sort: None,
            limit: Some(50),
            cursor: Some(encode_cursor(
                "2026-01-01T00:00:00+00:00",
                "at://did:plc:a/app.test.post/1",
            )),
        };
        assert!(matches!(
            records_query_sql(&spec, DatabaseBackend::Sqlite),
            Err(RecordsError::InvalidSpec(_))
        ));
    }

    /// Backlinks seek the refs target index by interned id; they never
    /// filter the derived `target_uri`. Seeded with many targets and
    /// analyzed, so the planner weighs a real choice.
    #[tokio::test]
    async fn backlinks_seek_the_refs_target_index() {
        for db in crate::test_support::pre_upgrade_pools().await {
            let (pool, backend) = (&db.pool, db.backend);
            crate::record_store::upgrade::run(pool, backend)
                .await
                .expect("upgrade");
            let records: Vec<crate::record_store::PreparedRecord> = (0..2000)
                .map(|i| crate::record_store::PreparedRecord {
                    did: format!("did:plc:src{}", i % 300),
                    collection: "app.test.like".to_string(),
                    rkey: format!("l{i}"),
                    record_json: json!({
                        "subject": format!("at://did:plc:t{}/app.test.post/r{}", i % 500, i % 7),
                        "author": format!("at://did:plc:t{}", i % 500),
                    })
                    .to_string(),
                    cid: "c".to_string(),
                    rev: None,
                })
                .collect();
            RecordStore::new(pool.clone(), backend)
                .write_network(pool, &records)
                .await
                .expect("seed");
            crate::db::query("ANALYZE")
                .execute(pool)
                .await
                .expect("analyze");
            for (target, literals) in [
                (
                    "at://did:plc:t7/app.test.post/r0",
                    vec![
                        "'did:plc:t7'",
                        "'app.test.post'",
                        "'r0'",
                        "'app.test.like'",
                        "50",
                    ],
                ),
                (
                    "at://did:plc:t7",
                    vec!["'did:plc:t7'", "''", "'app.test.like'", "50"],
                ),
            ] {
                let spec = BacklinksQuery {
                    uri: target.to_string(),
                    collection: "app.test.like".into(),
                    did: None,
                    limit: Some(50),
                    cursor: None,
                };
                let (sql, _) = backlinks_query_sql(&spec, backend).unwrap();
                let sql = crate::test_support::inline_binds_on(&sql, backend, &literals);
                let plan = crate::test_support::query_plan(pool, backend, &sql).await;
                assert!(
                    plan.iter()
                        .any(|line| line.contains("idx_record_refs_v2_target")),
                    "{backend:?} {target}: {plan:#?}"
                );
            }
            db.finish().await;
        }
    }
}

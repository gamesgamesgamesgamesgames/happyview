//! The boot-time move from the legacy record tables to the compact v2
//! tables, in one rewrite:
//!
//! 1. copy `happyview_records` in keyset batches on `uri`, each batch
//!    committed with its cursor in `happyview_instance_settings`, so an
//!    interrupted copy resumes; bodies are copied as they are (raw on
//!    SQLite, for the background task to compress);
//! 2. copy `happyview_record_refs` the same way;
//! 3. verify every row and every ref against the legacy tables, swap the
//!    legacy tables for compatibility views of the same names, drop them,
//!    and reclaim the space (`run`, in the same transaction as the
//!    verification).
//!
//! If verification fails nothing is dropped, the copy is reset
//! ([`reset_copy`]), and boot stops naming the mismatch. Rollback is
//! restore-from-backup.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use sqlx::{AnyConnection, AnyPool, Connection};

use super::codec::{self, RefTarget};
use super::compat;
use super::compat::CompatNames;
use super::intern::{DID_CACHE_CAPACITY, InternScope, Interner};
use crate::db::{DatabaseBackend, adapt_sql};
use crate::record_refs::{get_setting, put_setting};

pub const COPY_BATCH: i64 = 10_000;
pub const VERIFY_SAMPLE: usize = 1_000;
pub const RECORDS_CURSOR: &str = "record_store_copy_records_after";
pub const REFS_CURSOR: &str = "record_store_copy_refs_after";

/// Rows per multi-row insert: at most nine binds each.
const INSERT_CHUNK: usize = 500;
/// Rows looked up by identity per query: at most four binds each.
const LOOKUP_CHUNK: usize = 200;
/// Differences a failed verification names.
const MISMATCHES_NAMED: usize = 5;

/// The refs cursor: `[source_uri, target_uri]` as a JSON array, which
/// round-trips any text either holds.
fn encode_refs_cursor(source: &str, target: &str) -> String {
    serde_json::to_string(&(source, target)).expect("two strings serialize")
}

/// A cursor this module did not write starts the walk over, which only
/// re-reads refs the copy already holds (`ON CONFLICT DO NOTHING`).
fn decode_refs_cursor(saved: Option<&str>) -> (String, String) {
    let Some(saved) = saved else {
        return Default::default();
    };
    serde_json::from_str(saved).unwrap_or_else(|e| {
        tracing::warn!(
            cursor = saved,
            error = %e,
            "record store upgrade: unreadable refs cursor; copying refs from the start"
        );
        Default::default()
    })
}

/// Legacy refs after `(source_uri, target_uri)` in key order. The row-value
/// comparison is a range on the primary key on both backends; the expanded
/// `a > ? OR (a = ? AND b > ?)` is not on Postgres, which then filters from
/// the start of the index on every batch.
const REFS_AFTER: &str = "SELECT source_uri, target_uri, CAST(collection AS TEXT) FROM happyview_record_refs \
     WHERE (source_uri, target_uri) > (?, ?) ORDER BY source_uri, target_uri LIMIT ?";

#[derive(Debug, thiserror::Error)]
pub enum UpgradeError {
    #[error("record store upgrade: database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error(
        "record store upgrade: the legacy record {uri:?} has no at://did/collection/rkey uri; \
         fix or delete that row, then restart (nothing was dropped)"
    )]
    BadUri { uri: String },
    #[error(
        "record store upgrade: refs are copied once every record is; \
         the records copy stopped before {next_uri:?}"
    )]
    RecordsPending { next_uri: String },
    #[error("record store upgrade verification failed, nothing was dropped: {0}")]
    Mismatch(String),
    #[error(
        "record store upgrade: these objects depend on happyview_records or \
         happyview_record_refs, which the upgrade replaces with views of the same names: {}; \
         drop them, restart to upgrade, then recreate the views and rules (foreign keys \
         cannot point at a view); nothing was copied or dropped",
        .0.join(", ")
    )]
    DependentObjects(Vec<String>),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CopyProgress {
    pub rows: u64,
    pub batches: u64,
    pub done: bool,
    /// Refs whose source record does not exist.
    pub skipped: u64,
    /// Records whose did/collection/rkey columns disagreed with their uri.
    pub identity_mismatches: u64,
    /// Timestamps that could not be read (see [`legacy_times`]).
    pub unreadable_timestamps: u64,
    /// Refs whose collection column disagreed with their source record's
    /// collection.
    pub ref_collection_mismatches: u64,
    /// The legacy tables were already views: another boot finished the
    /// upgrade while this copy ran, and this one stopped.
    pub superseded: bool,
}

/// What a verification compared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VerifySummary {
    pub records: u64,
    pub refs: u64,
    /// Legacy timestamps that could not be read, which the copy must hold
    /// at their fallback values.
    pub unreadable_timestamps: u64,
    /// Refs whose collection, kept as the legacy row held it, disagrees with
    /// their source record's collection.
    pub ref_collection_mismatches: u64,
}

/// The legacy record columns, read as text whatever a SQLite row holds.
const LEGACY_COLUMNS: &str = "uri, CAST(did AS TEXT), CAST(collection AS TEXT), CAST(rkey AS TEXT), \
     CAST(record AS TEXT), CAST(cid AS TEXT), CAST(indexed_at AS TEXT), CAST(created_at AS TEXT), \
     CAST(rev AS TEXT)";

type LegacyRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// The microseconds a legacy row's timestamps copy as, and the columns that
/// could not be read. An unreadable `indexed_at` becomes `0` (1970-01-01); a
/// NULL one stays NULL, which means "awaiting its echo". An unreadable
/// `created_at` takes the row's `indexed_at`, or `0` when that is NULL or
/// unreadable too. The copy and the verification both call this, so a
/// fallback value verifies and any other difference does not.
fn legacy_times(
    indexed_at: Option<&str>,
    created_at: Option<&str>,
) -> (Option<i64>, i64, Vec<&'static str>) {
    let mut unreadable = Vec::new();
    let indexed = indexed_at.map(|text| {
        codec::parse_micros(text).unwrap_or_else(|| {
            unreadable.push("indexed_at");
            0
        })
    });
    let created = match created_at.and_then(codec::parse_micros) {
        Some(us) => us,
        None => {
            unreadable.push("created_at");
            indexed.unwrap_or(0)
        }
    };
    (indexed, created, unreadable)
}

struct CopiedRow {
    did: String,
    collection: String,
    rkey: String,
    record: String,
    cid: Vec<u8>,
    rev: Option<String>,
    indexed_at: Option<i64>,
    created_at: i64,
}

fn copied(row: &LegacyRow, progress: &mut CopyProgress) -> Result<CopiedRow, UpgradeError> {
    let (uri, did, collection, rkey, record, cid, indexed_at, created_at, rev) = row;
    let Some((uri_did, uri_collection, uri_rkey)) = codec::parse_record_uri(uri) else {
        return Err(UpgradeError::BadUri { uri: uri.clone() });
    };
    if (uri_did, uri_collection, uri_rkey) != (did.as_str(), collection.as_str(), rkey.as_str()) {
        progress.identity_mismatches += 1;
        tracing::warn!(
            uri,
            did,
            collection,
            rkey,
            "record store upgrade: identity columns disagree with the uri; copied by the uri"
        );
    }
    let (indexed_at_us, created_at_us, unreadable) =
        legacy_times(indexed_at.as_deref(), created_at.as_deref());
    for column in unreadable {
        progress.unreadable_timestamps += 1;
        let text = match column {
            "indexed_at" => indexed_at.as_deref(),
            _ => created_at.as_deref(),
        };
        tracing::warn!(
            uri,
            column,
            text,
            "record store upgrade: unreadable timestamp; copied with its fallback"
        );
    }
    Ok(CopiedRow {
        did: uri_did.to_string(),
        collection: uri_collection.to_string(),
        rkey: uri_rkey.to_string(),
        record: record.clone(),
        cid: codec::encode_cid(cid),
        rev: rev.clone(),
        indexed_at: indexed_at_us,
        created_at: created_at_us,
    })
}

/// A legacy record whose `uri` is empty or NULL (SQLite's `uri TEXT PRIMARY
/// KEY` allows NULL) sorts before every keyset cursor, so neither the copy
/// nor the verification would see it. Refuse it by name instead.
async fn refuse_blank_uris<'e, E>(executor: E) -> Result<(), UpgradeError>
where
    E: sqlx::Executor<'e, Database = sqlx::Any>,
{
    // uri, did, collection, rkey, as stored.
    type Blank = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let blank: Option<Blank> = crate::db::query_as(
        "SELECT uri, CAST(did AS TEXT), CAST(collection AS TEXT), CAST(rkey AS TEXT) \
             FROM happyview_records WHERE uri IS NULL OR uri = '' LIMIT 1",
    )
    .fetch_optional(executor)
    .await?;
    match blank {
        None => Ok(()),
        Some((uri, did, collection, rkey)) => Err(UpgradeError::BadUri {
            uri: format!(
                "{} (did {}, collection {}, rkey {})",
                uri.map_or_else(|| "NULL".to_string(), |u| format!("{u:?}")),
                did.as_deref().unwrap_or("NULL"),
                collection.as_deref().unwrap_or("NULL"),
                rkey.as_deref().unwrap_or("NULL"),
            ),
        }),
    }
}

/// Begin a transaction on `conn` that holds the write lock from the start:
/// `BEGIN IMMEDIATE` on SQLite, so a check made inside it (are the legacy
/// tables still tables?) cannot go stale before the write it guards.
async fn begin_write_on(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
) -> Result<sqlx::Transaction<'_, sqlx::Any>, sqlx::Error> {
    match backend {
        DatabaseBackend::Sqlite => conn.begin_with("BEGIN IMMEDIATE").await,
        DatabaseBackend::Postgres => conn.begin().await,
    }
}

/// Wait before retrying a write that found the database busy, or give the
/// error back once `attempt` is the last.
async fn busy_backoff(attempt: &mut u32, e: sqlx::Error) -> Result<(), sqlx::Error> {
    if *attempt < crate::db::BUSY_RETRY_ATTEMPTS && crate::db::is_retryable_write_error(&e) {
        tokio::time::sleep(std::time::Duration::from_millis(50 << *attempt)).await;
        *attempt += 1;
        Ok(())
    } else {
        Err(e)
    }
}

/// [`copy_records_on`] on a connection of its own from `pool`.
pub async fn copy_records(
    pool: &AnyPool,
    backend: DatabaseBackend,
    interner: &Interner,
    batch: i64,
    max_batches: Option<u64>,
) -> Result<CopyProgress, UpgradeError> {
    let mut conn = pool.acquire().await?;
    copy_records_on(&mut conn, backend, interner, batch, max_batches).await
}

/// Copy legacy records after the saved cursor, `batch` at a time, each batch
/// committed with its cursor. Stops, marked `superseded`, as soon as the
/// legacy tables turn out to be views already: every batch checks that in
/// its own write transaction, so a copy racing another boot's swap never
/// writes after it.
pub async fn copy_records_on(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
    interner: &Interner,
    batch: i64,
    max_batches: Option<u64>,
) -> Result<CopyProgress, UpgradeError> {
    let mut progress = CopyProgress::default();
    if !legacy_tables_present(&mut *conn, backend).await? {
        progress.done = true;
        progress.superseded = true;
        return Ok(progress);
    }
    refuse_blank_uris(&mut *conn).await?;
    let mut after = get_setting(&mut *conn, backend, RECORDS_CURSOR)
        .await?
        .unwrap_or_default();
    let select = adapt_sql(
        &format!(
            "SELECT {LEGACY_COLUMNS} FROM happyview_records WHERE uri > ? ORDER BY uri LIMIT ?"
        ),
        backend,
    );
    loop {
        if max_batches.is_some_and(|max| progress.batches >= max) {
            return Ok(progress);
        }
        let rows: Vec<LegacyRow> = crate::db::query_as(&select)
            .bind(&after)
            .bind(batch)
            .fetch_all(&mut *conn)
            .await?;
        let Some(last) = rows.last().map(|row| row.0.clone()) else {
            progress.done = true;
            return Ok(progress);
        };
        let copied_rows = rows
            .iter()
            .map(|row| copied(row, &mut progress))
            .collect::<Result<Vec<_>, _>>()?;
        let mut attempt = 1;
        let wrote = loop {
            match write_records(&mut *conn, backend, interner, &copied_rows, &last).await {
                Err(e) => busy_backoff(&mut attempt, e).await?,
                Ok(wrote) => break wrote,
            }
        };
        if !wrote {
            progress.done = true;
            progress.superseded = true;
            return Ok(progress);
        }
        progress.rows += rows.len() as u64;
        progress.batches += 1;
        after = last;
        tracing::info!(
            copied = progress.rows,
            "record store upgrade: copying records"
        );
    }
}

/// Write one batch and its cursor. `false`, writing nothing, when the
/// legacy tables are already views.
async fn write_records(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
    interner: &Interner,
    rows: &[CopiedRow],
    cursor: &str,
) -> Result<bool, sqlx::Error> {
    let mut tx = begin_write_on(conn, backend).await?;
    if !legacy_tables_present(&mut *tx, backend).await? {
        tx.rollback().await?;
        return Ok(false);
    }
    let mut scope = InternScope::default();
    let dids: Vec<&str> = rows.iter().map(|r| r.did.as_str()).collect();
    let collections: Vec<&str> = rows.iter().map(|r| r.collection.as_str()).collect();
    interner
        .intern(&mut tx, backend, &mut scope, &dids, &collections)
        .await?;
    let (columns, values) = match backend {
        DatabaseBackend::Sqlite => (
            "did_id, collection_id, rkey, cid, body, body_dict, rev, indexed_at, created_at",
            "(?, ?, ?, ?, ?, NULL, ?, ?, ?)",
        ),
        DatabaseBackend::Postgres => (
            "did_id, collection_id, rkey, cid, record, rev, indexed_at, created_at",
            "(?, ?, ?, ?, ?, ?, ?, ?)",
        ),
    };
    for chunk in rows.chunks(INSERT_CHUNK) {
        let sql = adapt_sql(
            &format!(
                "INSERT INTO happyview_records_v2 ({columns}) VALUES {} \
                 ON CONFLICT (did_id, collection_id, rkey) DO NOTHING",
                vec![values; chunk.len()].join(", ")
            ),
            backend,
        );
        let mut insert = crate::db::query(&sql);
        for row in chunk {
            insert = insert
                .bind(scope.dids[&row.did])
                .bind(scope.collections[&row.collection])
                .bind(row.rkey.as_str())
                .bind(row.cid.clone());
            insert = match backend {
                DatabaseBackend::Sqlite => insert.bind(row.record.as_bytes().to_vec()),
                DatabaseBackend::Postgres => insert.bind(row.record.as_str()),
            };
            insert = insert
                .bind(row.rev.as_deref())
                .bind(row.indexed_at)
                .bind(row.created_at);
        }
        insert.execute(&mut *tx).await?;
    }
    put_setting(&mut tx, backend, RECORDS_CURSOR, cursor).await?;
    tx.commit().await?;
    interner.remember(scope);
    Ok(true)
}

/// source_uri, target_uri, collection.
type LegacyRef = (String, String, String);

#[derive(Default)]
struct RefsWritten {
    skipped: u64,
    collection_mismatches: u64,
}

/// [`copy_refs_on`] on a connection of its own from `pool`.
pub async fn copy_refs(
    pool: &AnyPool,
    backend: DatabaseBackend,
    interner: &Interner,
    batch: i64,
    max_batches: Option<u64>,
) -> Result<CopyProgress, UpgradeError> {
    let mut conn = pool.acquire().await?;
    copy_refs_on(&mut conn, backend, interner, batch, max_batches).await
}

/// Copy legacy refs after the saved cursor once every record is copied,
/// stopping like [`copy_records_on`] when the legacy tables are views.
pub async fn copy_refs_on(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
    interner: &Interner,
    batch: i64,
    max_batches: Option<u64>,
) -> Result<CopyProgress, UpgradeError> {
    let mut progress = CopyProgress::default();
    if !legacy_tables_present(&mut *conn, backend).await? {
        progress.done = true;
        progress.superseded = true;
        return Ok(progress);
    }
    // A ref whose source is not copied yet would be skipped for good once
    // the cursor passed it.
    let records_after = get_setting(&mut *conn, backend, RECORDS_CURSOR)
        .await?
        .unwrap_or_default();
    let pending: Option<(String,)> = crate::db::query_as(&adapt_sql(
        "SELECT uri FROM happyview_records WHERE uri > ? ORDER BY uri LIMIT 1",
        backend,
    ))
    .bind(&records_after)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some((next_uri,)) = pending {
        return Err(UpgradeError::RecordsPending { next_uri });
    }

    let saved = get_setting(&mut *conn, backend, REFS_CURSOR).await?;
    let (mut after_source, mut after_target) = decode_refs_cursor(saved.as_deref());
    let select = adapt_sql(REFS_AFTER, backend);
    loop {
        if max_batches.is_some_and(|max| progress.batches >= max) {
            return Ok(progress);
        }
        let rows: Vec<LegacyRef> = crate::db::query_as(&select)
            .bind(&after_source)
            .bind(&after_target)
            .bind(batch)
            .fetch_all(&mut *conn)
            .await?;
        let Some((last_source, last_target, _)) = rows.last().cloned() else {
            progress.done = true;
            return Ok(progress);
        };
        let cursor = encode_refs_cursor(&last_source, &last_target);
        let mut attempt = 1;
        let written = loop {
            match write_refs(&mut *conn, backend, interner, &rows, &cursor).await {
                Err(e) => busy_backoff(&mut attempt, e).await?,
                Ok(written) => break written,
            }
        };
        let Some(written) = written else {
            progress.done = true;
            progress.superseded = true;
            return Ok(progress);
        };
        progress.rows += rows.len() as u64;
        progress.skipped += written.skipped;
        progress.ref_collection_mismatches += written.collection_mismatches;
        progress.batches += 1;
        (after_source, after_target) = (last_source, last_target);
        tracing::info!(copied = progress.rows, "record store upgrade: copying refs");
    }
}

/// `(a = ? AND b = ?) OR …` over `columns`, for `n` rows.
fn rows_matching(columns: &[&str], n: usize) -> String {
    let one = format!(
        "({})",
        columns
            .iter()
            .map(|column| format!("{column} = ?"))
            .collect::<Vec<_>>()
            .join(" AND ")
    );
    vec![one; n].join(" OR ")
}

/// The v2 `(id, collection_id)` of each record identity found, read through
/// the identity index.
async fn record_ids(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
    keys: &[(i64, i64, &str)],
) -> Result<HashMap<(i64, i64, String), i64>, sqlx::Error> {
    let mut ids = HashMap::with_capacity(keys.len());
    for chunk in keys.chunks(LOOKUP_CHUNK) {
        let sql = adapt_sql(
            &format!(
                "SELECT id, did_id, collection_id, rkey FROM happyview_records_v2 WHERE {}",
                rows_matching(&["did_id", "collection_id", "rkey"], chunk.len())
            ),
            backend,
        );
        let mut lookup = crate::db::query_as::<(i64, i64, i64, String)>(&sql);
        for (did_id, collection_id, rkey) in chunk {
            lookup = lookup.bind(*did_id).bind(*collection_id).bind(*rkey);
        }
        for (id, did_id, collection_id, rkey) in lookup.fetch_all(&mut *conn).await? {
            ids.insert((did_id, collection_id, rkey), id);
        }
    }
    Ok(ids)
}

/// Write one batch of refs and its cursor; `None`, writing nothing, when the
/// legacy tables are already views.
async fn write_refs(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
    interner: &Interner,
    rows: &[LegacyRef],
    cursor: &str,
) -> Result<Option<RefsWritten>, sqlx::Error> {
    let mut tx = begin_write_on(conn, backend).await?;
    if !legacy_tables_present(&mut *tx, backend).await? {
        tx.rollback().await?;
        return Ok(None);
    }
    let mut scope = InternScope::default();
    let sources: BTreeSet<(&str, &str, &str)> = rows
        .iter()
        .filter_map(|(source, _, _)| codec::parse_record_uri(source))
        .collect();
    let targets: Vec<RefTarget<'_>> = rows
        .iter()
        .map(|(_, target, _)| RefTarget::parse(target))
        .collect();
    // Every name the batch needs, in one call (T11-4).
    let mut dids: Vec<&str> = sources.iter().map(|(d, _, _)| *d).collect();
    dids.extend(targets.iter().map(RefTarget::did_text));
    let mut collections: Vec<&str> = sources.iter().map(|(_, c, _)| *c).collect();
    collections.extend(targets.iter().filter_map(RefTarget::collection));
    collections.extend(rows.iter().map(|(_, _, collection)| collection.as_str()));
    interner
        .intern(&mut tx, backend, &mut scope, &dids, &collections)
        .await?;

    let keys: Vec<(i64, i64, &str)> = sources
        .iter()
        .map(|(d, c, r)| (scope.dids[*d], scope.collections[*c], *r))
        .collect();
    let ids = record_ids(&mut tx, backend, &keys).await?;

    let mut values: Vec<(i64, i64, i64, &str, i64)> = Vec::new();
    let mut written = RefsWritten::default();
    for ((source, target_uri, legacy_collection), target) in rows.iter().zip(&targets) {
        let found = codec::parse_record_uri(source).and_then(|(d, c, r)| {
            let collection_id = scope.collections[c];
            ids.get(&(scope.dids[d], collection_id, r.to_string()))
                .map(|id| (*id, collection_id))
        });
        let Some((source_id, source_collection_id)) = found else {
            written.skipped += 1;
            continue;
        };
        // The legacy column is kept as it was, even where it disagrees with
        // the source record (only raw SQL could write that): a reader that
        // filters refs by collection sees what it saw before.
        let collection_id = scope.collections[legacy_collection.as_str()];
        if collection_id != source_collection_id {
            written.collection_mismatches += 1;
            tracing::warn!(
                source_uri = source,
                target_uri,
                collection = legacy_collection,
                "record store upgrade: a ref's collection disagrees with its source record"
            );
        }
        let target_collection = match target.collection() {
            Some(nsid) => scope.collections[nsid],
            None => target.kind(),
        };
        values.push((
            source_id,
            scope.dids[target.did_text()],
            target_collection,
            target.rkey(),
            collection_id,
        ));
    }
    for chunk in values.chunks(INSERT_CHUNK) {
        let sql = adapt_sql(
            &format!(
                "INSERT INTO happyview_record_refs_v2 \
                 (source_id, target_did_id, target_collection_id, target_rkey, collection_id) VALUES {} \
                 ON CONFLICT DO NOTHING",
                vec!["(?, ?, ?, ?, ?)"; chunk.len()].join(", ")
            ),
            backend,
        );
        let mut insert = crate::db::query(&sql);
        for (source_id, did_id, collection_id, rkey, source_collection) in chunk {
            insert = insert
                .bind(*source_id)
                .bind(*did_id)
                .bind(*collection_id)
                .bind(*rkey)
                .bind(*source_collection);
        }
        insert.execute(&mut *tx).await?;
    }
    put_setting(&mut tx, backend, REFS_CURSOR, cursor).await?;
    tx.commit().await?;
    interner.remember(scope);
    Ok(Some(written))
}

/// Undo a copy that failed verification: clear both cursors and empty the
/// v2 tables (refs first), so the next boot copies from the start. Interned
/// names stay; an id never changes meaning.
///
/// Refused, returning `false` and changing nothing, once the legacy tables
/// are gone: the v2 tables are then the only copy of every record (a boot
/// that lost a race to another's swap ends up here). The check and the
/// delete share one write transaction, so a swap cannot land between them.
pub async fn reset_copy(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
) -> Result<bool, UpgradeError> {
    let mut tx = begin_write_on(conn, backend).await?;
    if !legacy_tables_present(&mut *tx, backend).await? {
        tx.rollback().await?;
        tracing::warn!(
            "record store upgrade: not resetting the copy; the legacy tables are already gone"
        );
        return Ok(false);
    }
    crate::db::query(&adapt_sql(
        "DELETE FROM happyview_instance_settings WHERE key IN (?, ?)",
        backend,
    ))
    .bind(RECORDS_CURSOR)
    .bind(REFS_CURSOR)
    .execute(&mut *tx)
    .await?;
    let empty: &[&str] = match backend {
        // Transactional on Postgres, and far cheaper than a DELETE that
        // leaves every row dead for vacuum.
        DatabaseBackend::Postgres => &["TRUNCATE happyview_record_refs_v2, happyview_records_v2"],
        DatabaseBackend::Sqlite => &[
            "DELETE FROM happyview_record_refs_v2",
            "DELETE FROM happyview_records_v2",
        ],
    };
    for sql in empty {
        crate::db::query(sql).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    tracing::warn!("record store upgrade: the copy was reset");
    Ok(true)
}

/// The ids interned for `names` (read only; a missing name has none).
async fn interned_ids(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
    table: &str,
    column: &str,
    names: &BTreeSet<&str>,
) -> Result<HashMap<String, i64>, sqlx::Error> {
    let names: Vec<&str> = names.iter().copied().collect();
    let mut ids = HashMap::with_capacity(names.len());
    for chunk in names.chunks(INSERT_CHUNK) {
        let sql = adapt_sql(
            &format!(
                "SELECT id, {column} FROM {table} WHERE {column} IN ({})",
                vec!["?"; chunk.len()].join(", ")
            ),
            backend,
        );
        let mut query = crate::db::query_as::<(i64, String)>(&sql);
        for name in chunk {
            query = query.bind(*name);
        }
        for (id, name) in query.fetch_all(&mut *conn).await? {
            ids.insert(name, id);
        }
    }
    Ok(ids)
}

/// Compare the legacy tables with the copy, in full: every legacy record
/// against its v2 row (body byte for byte, CID in binary, rev, timestamps as
/// microseconds), every legacy ref that has a source against its v2 ref, and
/// the per-collection and ref counts, which with the unique v2 identity
/// prove nothing extra was copied. A sample is also read back through the
/// compatibility view named in `staging`. Runs on the caller's connection,
/// normally inside the swap transaction.
pub async fn verify(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
    staging: CompatNames<'_>,
) -> Result<VerifySummary, UpgradeError> {
    verify_in_batches(conn, backend, staging, COPY_BATCH).await
}

async fn verify_in_batches(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
    staging: CompatNames<'_>,
    batch: i64,
) -> Result<VerifySummary, UpgradeError> {
    refuse_blank_uris(&mut *conn).await?;
    compare_counts(conn).await?;
    let mut summary = VerifySummary::default();
    let mut mismatches = Vec::new();
    compare_records(conn, backend, batch, &mut summary, &mut mismatches).await?;
    if mismatches.is_empty() {
        compare_refs(conn, backend, batch, &mut summary, &mut mismatches).await?;
    }
    if !mismatches.is_empty() {
        return Err(UpgradeError::Mismatch(mismatches.join("; ")));
    }
    compare_sample(conn, backend, staging).await?;
    if summary.unreadable_timestamps > 0 {
        tracing::warn!(
            unreadable_timestamps = summary.unreadable_timestamps,
            "record store upgrade: legacy timestamps that could not be read were copied with their fallbacks"
        );
    }
    if summary.ref_collection_mismatches > 0 {
        tracing::warn!(
            ref_collection_mismatches = summary.ref_collection_mismatches,
            "record store upgrade: refs whose collection disagrees with their source record were kept as they were"
        );
    }
    tracing::info!(
        records = summary.records,
        refs = summary.refs,
        "record store upgrade: verified"
    );
    Ok(summary)
}

async fn compare_counts(conn: &mut AnyConnection) -> Result<(), UpgradeError> {
    let legacy: BTreeMap<String, i64> = crate::db::query_as::<(Option<String>, i64)>(
        "SELECT hv_ref_part(uri, 1), COUNT(*) FROM happyview_records GROUP BY hv_ref_part(uri, 1)",
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|(collection, n)| (collection.unwrap_or_default(), n))
    .collect();
    let copied: BTreeMap<String, i64> = crate::db::query_as::<(String, i64)>(
        "SELECT c.nsid, COUNT(*) FROM happyview_records_v2 r \
         JOIN happyview_collections c ON c.id = r.collection_id GROUP BY c.nsid",
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();
    if legacy != copied {
        let names: BTreeSet<&String> = legacy.keys().chain(copied.keys()).collect();
        let diffs: Vec<String> = names
            .into_iter()
            .filter(|n| legacy.get(*n) != copied.get(*n))
            .map(|n| {
                format!(
                    "{n}: {} legacy rows, {} copied",
                    legacy.get(n).unwrap_or(&0),
                    copied.get(n).unwrap_or(&0)
                )
            })
            .collect();
        return Err(UpgradeError::Mismatch(diffs.join("; ")));
    }

    let (legacy_refs,): (i64,) = crate::db::query_as(
        "SELECT COUNT(*) FROM happyview_record_refs f \
         WHERE EXISTS (SELECT 1 FROM happyview_records r WHERE r.uri = f.source_uri)",
    )
    .fetch_one(&mut *conn)
    .await?;
    let (copied_refs,): (i64,) =
        crate::db::query_as("SELECT COUNT(*) FROM happyview_record_refs_v2")
            .fetch_one(&mut *conn)
            .await?;
    if legacy_refs != copied_refs {
        return Err(UpgradeError::Mismatch(format!(
            "refs: {legacy_refs} legacy refs with a source, {copied_refs} copied"
        )));
    }
    Ok(())
}

/// uri, record, cid, indexed_at, created_at, rev.
type LegacyFields = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// did_id, collection_id, rkey, record, cid, rev, indexed_at, created_at.
type CopiedFields = (
    i64,
    i64,
    String,
    String,
    Vec<u8>,
    Option<String>,
    Option<i64>,
    i64,
);

async fn compare_records(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
    batch: i64,
    summary: &mut VerifySummary,
    mismatches: &mut Vec<String>,
) -> Result<(), UpgradeError> {
    let select = adapt_sql(
        "SELECT uri, CAST(record AS TEXT), CAST(cid AS TEXT), CAST(indexed_at AS TEXT), \
                CAST(created_at AS TEXT), CAST(rev AS TEXT) \
         FROM happyview_records WHERE uri > ? ORDER BY uri LIMIT ?",
        backend,
    );
    let record = match backend {
        DatabaseBackend::Sqlite => "hv_record(body, body_dict)",
        DatabaseBackend::Postgres => "record",
    };
    let mut after = String::new();
    while mismatches.len() < MISMATCHES_NAMED {
        let rows: Vec<LegacyFields> = crate::db::query_as(&select)
            .bind(&after)
            .bind(batch)
            .fetch_all(&mut *conn)
            .await?;
        let Some(last) = rows.last().map(|row| row.0.clone()) else {
            break;
        };
        let mut identities = Vec::with_capacity(rows.len());
        for row in &rows {
            match codec::parse_record_uri(&row.0) {
                Some(identity) => identities.push(identity),
                None => return Err(UpgradeError::BadUri { uri: row.0.clone() }),
            }
        }
        let dids = interned_ids(
            conn,
            backend,
            "happyview_dids",
            "did",
            &identities.iter().map(|(d, _, _)| *d).collect(),
        )
        .await?;
        let collections = interned_ids(
            conn,
            backend,
            "happyview_collections",
            "nsid",
            &identities.iter().map(|(_, c, _)| *c).collect(),
        )
        .await?;
        let keys: Vec<(i64, i64, &str)> = identities
            .iter()
            .filter_map(|(d, c, r)| Some((*dids.get(*d)?, *collections.get(*c)?, *r)))
            .collect();
        let mut copies: HashMap<(i64, i64, String), CopiedFields> =
            HashMap::with_capacity(keys.len());
        for chunk in keys.chunks(LOOKUP_CHUNK) {
            let sql = adapt_sql(
                &format!(
                    "SELECT did_id, collection_id, rkey, {record}, cid, rev, indexed_at, created_at \
                     FROM happyview_records_v2 WHERE {}",
                    rows_matching(&["did_id", "collection_id", "rkey"], chunk.len())
                ),
                backend,
            );
            let mut lookup = crate::db::query_as::<CopiedFields>(&sql);
            for (did_id, collection_id, rkey) in chunk {
                lookup = lookup.bind(*did_id).bind(*collection_id).bind(*rkey);
            }
            for row in lookup.fetch_all(&mut *conn).await? {
                copies.insert((row.0, row.1, row.2.clone()), row);
            }
        }

        for (row, (did, collection, rkey)) in rows.iter().zip(&identities) {
            let (uri, record, cid, indexed_at, created_at, rev) = row;
            let (indexed_us, created_us, unreadable) =
                legacy_times(indexed_at.as_deref(), created_at.as_deref());
            summary.unreadable_timestamps += unreadable.len() as u64;
            summary.records += 1;
            let copy = dids.get(*did).zip(collections.get(*collection)).and_then(
                |(did_id, collection_id)| copies.get(&(*did_id, *collection_id, rkey.to_string())),
            );
            let Some((_, _, _, v2_record, v2_cid, v2_rev, v2_indexed, v2_created)) = copy else {
                mismatches.push(format!("{uri}: missing from the copy"));
                continue;
            };
            let checks = [
                ("record", record == v2_record),
                ("cid", &codec::encode_cid(cid) == v2_cid),
                ("rev", rev == v2_rev),
                ("indexed_at", indexed_us == *v2_indexed),
                ("created_at", created_us == *v2_created),
            ];
            if let Some((field, _)) = checks.iter().find(|(_, same)| !same) {
                mismatches.push(format!("{uri}: {field} differs"));
            }
        }
        mismatches.truncate(MISMATCHES_NAMED);
        after = last;
    }
    Ok(())
}

/// source_id, target_did_id, target_collection_id, target_rkey.
type RefKey<'a> = (i64, i64, i64, &'a str);

/// Which of `uris` are legacy records.
async fn legacy_uris(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
    uris: &BTreeSet<&str>,
) -> Result<std::collections::HashSet<String>, sqlx::Error> {
    let uris: Vec<&str> = uris.iter().copied().collect();
    let mut held = std::collections::HashSet::with_capacity(uris.len());
    for chunk in uris.chunks(INSERT_CHUNK) {
        let sql = adapt_sql(
            &format!(
                "SELECT uri FROM happyview_records WHERE uri IN ({})",
                vec!["?"; chunk.len()].join(", ")
            ),
            backend,
        );
        let mut query = crate::db::query_as::<(String,)>(&sql);
        for uri in chunk {
            query = query.bind(*uri);
        }
        held.extend(
            query
                .fetch_all(&mut *conn)
                .await?
                .into_iter()
                .map(|(uri,)| uri),
        );
    }
    Ok(held)
}

async fn compare_refs(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
    batch: i64,
    summary: &mut VerifySummary,
    mismatches: &mut Vec<String>,
) -> Result<(), UpgradeError> {
    let select = adapt_sql(REFS_AFTER, backend);
    let (mut after_source, mut after_target) = (String::new(), String::new());
    while mismatches.len() < MISMATCHES_NAMED {
        let batch_rows: Vec<LegacyRef> = crate::db::query_as(&select)
            .bind(&after_source)
            .bind(&after_target)
            .bind(batch)
            .fetch_all(&mut *conn)
            .await?;
        let Some((last_source, last_target, _)) = batch_rows.last().cloned() else {
            break;
        };
        (after_source, after_target) = (last_source, last_target);
        // Only refs whose source is a legacy record count, as in
        // compare_counts. Checked here rather than with an EXISTS in the
        // keyset query, which Postgres plans as a merge join that walks
        // happyview_records from its start on every batch.
        let source_uris: BTreeSet<&str> = batch_rows.iter().map(|(s, _, _)| s.as_str()).collect();
        let held = legacy_uris(conn, backend, &source_uris).await?;
        let rows: Vec<&LegacyRef> = batch_rows
            .iter()
            .filter(|(source, _, _)| held.contains(source))
            .collect();
        let mut sources = Vec::with_capacity(rows.len());
        for (source, _, _) in rows.iter().copied() {
            match codec::parse_record_uri(source) {
                Some(identity) => sources.push(identity),
                None => {
                    return Err(UpgradeError::BadUri {
                        uri: source.clone(),
                    });
                }
            }
        }
        let targets: Vec<RefTarget<'_>> = rows
            .iter()
            .map(|(_, target, _)| RefTarget::parse(target))
            .collect();
        let mut did_names: BTreeSet<&str> = sources.iter().map(|(d, _, _)| *d).collect();
        did_names.extend(targets.iter().map(RefTarget::did_text));
        let mut collection_names: BTreeSet<&str> = sources.iter().map(|(_, c, _)| *c).collect();
        collection_names.extend(targets.iter().filter_map(RefTarget::collection));
        collection_names.extend(rows.iter().map(|(_, _, collection)| collection.as_str()));
        let dids = interned_ids(conn, backend, "happyview_dids", "did", &did_names).await?;
        let collections = interned_ids(
            conn,
            backend,
            "happyview_collections",
            "nsid",
            &collection_names,
        )
        .await?;

        let source_keys: BTreeSet<(i64, i64, &str)> = sources
            .iter()
            .filter_map(|(d, c, r)| Some((*dids.get(*d)?, *collections.get(*c)?, *r)))
            .collect();
        let source_keys: Vec<(i64, i64, &str)> = source_keys.into_iter().collect();
        let source_ids = record_ids(conn, backend, &source_keys).await?;

        // The source's collection id and the expected v2 key per ref, or
        // None when a name or the source is not in the copy.
        let expected: Vec<Option<(i64, RefKey<'_>)>> = sources
            .iter()
            .zip(&targets)
            .map(|((d, c, r), target)| {
                let collection_id = *collections.get(*c)?;
                let source_id = *source_ids.get(&(*dids.get(*d)?, collection_id, r.to_string()))?;
                let target_collection = match target.collection() {
                    Some(nsid) => *collections.get(nsid)?,
                    None => target.kind(),
                };
                let target_did = *dids.get(target.did_text())?;
                Some((
                    collection_id,
                    (source_id, target_did, target_collection, target.rkey()),
                ))
            })
            .collect();
        let keys: Vec<RefKey<'_>> = expected.iter().flatten().map(|(_, key)| *key).collect();
        let mut copies: HashMap<(i64, i64, i64, String), i64> = HashMap::with_capacity(keys.len());
        for chunk in keys.chunks(LOOKUP_CHUNK) {
            let sql = adapt_sql(
                &format!(
                    "SELECT source_id, target_did_id, target_collection_id, target_rkey, collection_id \
                     FROM happyview_record_refs_v2 WHERE {}",
                    rows_matching(
                        &[
                            "source_id",
                            "target_did_id",
                            "target_collection_id",
                            "target_rkey"
                        ],
                        chunk.len()
                    )
                ),
                backend,
            );
            let mut lookup = crate::db::query_as::<(i64, i64, i64, String, i64)>(&sql);
            for (source_id, did_id, collection_id, rkey) in chunk {
                lookup = lookup
                    .bind(*source_id)
                    .bind(*did_id)
                    .bind(*collection_id)
                    .bind(*rkey);
            }
            for (source_id, did_id, target_collection, rkey, collection_id) in
                lookup.fetch_all(&mut *conn).await?
            {
                copies.insert((source_id, did_id, target_collection, rkey), collection_id);
            }
        }

        for ((source, target, collection), expected) in rows.iter().copied().zip(&expected) {
            summary.refs += 1;
            // The copy keeps the legacy column, not the source's collection.
            let legacy_collection = collections.get(collection.as_str()).copied();
            let found = expected.and_then(|(source_collection, (s, d, c, r))| {
                copies
                    .get(&(s, d, c, r.to_string()))
                    .map(|copied| (source_collection, *copied))
            });
            match found {
                None => mismatches.push(format!("{source} -> {target}: ref missing from the copy")),
                Some((_, copied)) if Some(copied) != legacy_collection => {
                    mismatches.push(format!("{source} -> {target}: ref collection differs"))
                }
                Some((source_collection, copied)) => {
                    if copied != source_collection {
                        summary.ref_collection_mismatches += 1;
                    }
                }
            }
        }
        mismatches.truncate(MISMATCHES_NAMED);
    }
    Ok(())
}

/// Read up to [`VERIFY_SAMPLE`] random rows back through the staging view,
/// which exercises what raw SQL will read (`hv_record`, `hv_cid_text`,
/// `hv_ts_text`), and compare them with the legacy text.
async fn compare_sample(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
    staging: CompatNames<'_>,
) -> Result<(), UpgradeError> {
    let (min, max): (Option<i64>, Option<i64>) =
        crate::db::query_as("SELECT MIN(id), MAX(id) FROM happyview_records_v2")
            .fetch_one(&mut *conn)
            .await?;
    let (Some(min), Some(max)) = (min, max) else {
        return Ok(());
    };
    let span = (max - min + 1) as u128;
    let candidates: BTreeSet<i64> = (0..VERIFY_SAMPLE * 2)
        .map(|_| min + (uuid::Uuid::new_v4().as_u128() % span) as i64)
        .collect();
    let candidates: Vec<i64> = candidates.into_iter().collect();
    let mut sample: Vec<(String, String, String)> = Vec::new();
    for chunk in candidates.chunks(INSERT_CHUNK) {
        let sql = adapt_sql(
            &format!(
                "SELECT d.did, c.nsid, r.rkey FROM happyview_records_v2 r \
                 JOIN happyview_dids d ON d.id = r.did_id \
                 JOIN happyview_collections c ON c.id = r.collection_id \
                 WHERE r.id IN ({})",
                vec!["?"; chunk.len()].join(", ")
            ),
            backend,
        );
        let mut q = crate::db::query_as::<(String, String, String)>(&sql);
        for id in chunk {
            q = q.bind(*id);
        }
        sample.extend(q.fetch_all(&mut *conn).await?);
    }
    sample.truncate(VERIFY_SAMPLE);

    type Fields = (
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let legacy_sql = adapt_sql(
        "SELECT CAST(record AS TEXT), CAST(cid AS TEXT), CAST(indexed_at AS TEXT), \
                CAST(created_at AS TEXT), CAST(rev AS TEXT) \
         FROM happyview_records WHERE uri = ?",
        backend,
    );
    let view_sql = adapt_sql(
        &format!(
            "SELECT record, cid, indexed_at, created_at, rev FROM {} \
             WHERE did = ? AND collection = ? AND rkey = ?",
            staging.records
        ),
        backend,
    );
    for (did, collection, rkey) in sample {
        let uri = format!("at://{did}/{collection}/{rkey}");
        let old: Option<Fields> = crate::db::query_as(&legacy_sql)
            .bind(&uri)
            .fetch_optional(&mut *conn)
            .await?;
        let new: Option<Fields> = crate::db::query_as(&view_sql)
            .bind(&did)
            .bind(&collection)
            .bind(&rkey)
            .fetch_optional(&mut *conn)
            .await?;
        let (Some(old), Some(new)) = (old, new) else {
            return Err(UpgradeError::Mismatch(format!(
                "{uri}: present on one side only, through the view"
            )));
        };
        let (indexed_us, created_us, _) = legacy_times(old.2.as_deref(), old.3.as_deref());
        let checks = [
            ("record", old.0 == new.0),
            ("cid", old.1 == new.1),
            ("indexed_at", indexed_us.map(codec::render_micros) == new.2),
            (
                "created_at",
                Some(codec::render_micros(created_us)) == new.3,
            ),
            ("rev", old.4 == new.4),
        ];
        if let Some((field, _)) = checks.iter().find(|(_, same)| !same) {
            return Err(UpgradeError::Mismatch(format!(
                "{uri}: {field} reads back differently through the view"
            )));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpgradeOutcome {
    AlreadyDone,
    Upgraded { records: u64, refs: u64 },
}

/// Whether `happyview_records` is still the legacy table (and not yet the
/// compatibility view).
pub async fn legacy_tables_present<'e, E>(
    executor: E,
    backend: DatabaseBackend,
) -> Result<bool, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Any>,
{
    let sql = match backend {
        DatabaseBackend::Sqlite => {
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'happyview_records'"
        }
        DatabaseBackend::Postgres => {
            "SELECT COUNT(*) FROM information_schema.tables \
             WHERE table_schema = current_schema() AND table_name = 'happyview_records' \
             AND table_type = 'BASE TABLE'"
        }
    };
    Ok(crate::db::query_scalar::<i64>(sql)
        .fetch_one(executor)
        .await?
        > 0)
}

/// The indexes HappyView's migrations built on the legacy tables, under
/// every name a release has used (`ALTER TABLE … RENAME` keeps an index's
/// name, so the primary keys are still named for `records`/`record_refs`).
/// Anything else on those tables is a user's own.
const OWN_LEGACY_INDEXES: &[&str] = &[
    "records_pkey",
    "record_refs_pkey",
    "happyview_records_pkey",
    "happyview_record_refs_pkey",
    "idx_records_collection",
    "idx_records_created_at_uri",
    "idx_records_did_collection",
    "idx_records_collection_indexed_at",
    "idx_records_collection_created_at_uri",
    "idx_record_refs_target",
];

/// A user's own index, trigger or rule on a legacy table: the swap's
/// `DROP TABLE` takes it, so its definition is logged first.
struct UserObject {
    kind: String,
    name: String,
    definition: String,
}

/// The user-made indexes, triggers and (Postgres) rules on the two legacy
/// tables. HappyView made none of its own triggers or rules there.
async fn user_objects(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
) -> Result<Vec<UserObject>, sqlx::Error> {
    let sql = match backend {
        // Automatic (primary key) indexes have no `sql`.
        DatabaseBackend::Sqlite => {
            "SELECT type, name, sql FROM sqlite_master \
             WHERE type IN ('index', 'trigger') \
               AND tbl_name IN ('happyview_records', 'happyview_record_refs') \
               AND sql IS NOT NULL ORDER BY type, name"
        }
        DatabaseBackend::Postgres => {
            "SELECT 'index', CAST(c.relname AS TEXT), pg_get_indexdef(i.indexrelid) \
               FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid \
              WHERE i.indrelid IN (to_regclass('happyview_records'), to_regclass('happyview_record_refs')) \
             UNION ALL \
             SELECT 'trigger', CAST(t.tgname AS TEXT), pg_get_triggerdef(t.oid) FROM pg_trigger t \
              WHERE t.tgrelid IN (to_regclass('happyview_records'), to_regclass('happyview_record_refs')) \
                AND NOT t.tgisinternal \
             UNION ALL \
             SELECT 'rule', CAST(r.rulename AS TEXT), pg_get_ruledef(r.oid) FROM pg_rewrite r \
              WHERE r.ev_class IN (to_regclass('happyview_records'), to_regclass('happyview_record_refs')) \
             ORDER BY 1, 2"
        }
    };
    Ok(crate::db::query_as::<(String, String, String)>(sql)
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .filter(|(_, name, _)| !OWN_LEGACY_INDEXES.contains(&name.as_str()))
        .map(|(kind, name, definition)| UserObject {
            kind,
            name,
            definition,
        })
        .collect())
}

/// On Postgres, the objects elsewhere that depend on a legacy table and so
/// would stop its `DROP TABLE`: views, materialized views and rules on other
/// tables, and foreign keys from other tables. Named, so the user can drop
/// them before the upgrade. SQLite has no such dependency: a view there
/// names the table and reads the compatibility view after the swap.
async fn dependent_objects(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
) -> Result<Vec<String>, sqlx::Error> {
    if backend != DatabaseBackend::Postgres {
        return Ok(Vec::new());
    }
    let rows: Vec<(String,)> = crate::db::query_as(
        "WITH legacy AS ( \
           SELECT to_regclass('happyview_records') AS oid \
           UNION ALL SELECT to_regclass('happyview_record_refs')) \
         SELECT DISTINCT CASE c.relkind WHEN 'v' THEN 'view ' WHEN 'm' THEN 'materialized view ' \
                ELSE 'rule ' || r.rulename || ' on ' END || CAST(CAST(c.oid AS regclass) AS TEXT) \
           FROM pg_depend d \
           JOIN pg_rewrite r ON r.oid = d.objid \
           JOIN pg_class c ON c.oid = r.ev_class \
          WHERE d.classid = CAST('pg_rewrite' AS regclass) \
            AND d.refobjid IN (SELECT oid FROM legacy) \
            AND r.ev_class NOT IN (SELECT oid FROM legacy) \
         UNION \
         SELECT 'foreign key ' || con.conname || ' on ' || CAST(CAST(con.conrelid AS regclass) AS TEXT) \
           FROM pg_constraint con \
          WHERE con.contype = 'f' \
            AND con.confrelid IN (SELECT oid FROM legacy) \
            AND con.conrelid NOT IN (SELECT oid FROM legacy) \
         ORDER BY 1",
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().map(|(name,)| name).collect())
}

/// Copy passes `run` makes when the refs copy finds records written behind
/// the records copy (an instance still on the old release writing during a
/// rolling deploy) before it gives up.
const COPY_ATTEMPTS: u32 = 3;

/// [`run_on`] on one connection from `pool`. On SQLite the swap turns the
/// connection's foreign keys off for a moment; after a failure they are
/// turned back on, or the connection is closed rather than returned.
pub async fn run(pool: &AnyPool, backend: DatabaseBackend) -> Result<UpgradeOutcome, UpgradeError> {
    let mut conn = pool.acquire().await?;
    let outcome = run_on(&mut conn, backend).await;
    if outcome.is_err()
        && backend == DatabaseBackend::Sqlite
        && crate::db::query("PRAGMA foreign_keys = ON")
            .execute(&mut *conn)
            .await
            .is_err()
    {
        conn.close_on_drop();
    }
    outcome
}

/// Move the legacy record tables into the compact ones, once, all on
/// `conn`. Runs at boot after the migrations, and blocks until it is done.
/// On Postgres `conn` is the one holding the migration lock, so a second
/// instance waits for the copy as it waits for a migration, and the upgrade
/// needs no second connection. On SQLite nothing locks across processes,
/// so every decision that could lose data is made inside a write
/// transaction that first checks the legacy tables are still tables: a
/// boot that loses a race to another's swap stops with `AlreadyDone` and
/// never empties the compact tables.
///
/// If verification fails nothing is dropped, the copy is reset, and the
/// error names the difference; the next boot copies from the start.
pub async fn run_on(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
) -> Result<UpgradeOutcome, UpgradeError> {
    let started = std::time::Instant::now();
    let (resuming, total) = {
        let mut tx = begin_write_on(conn, backend).await?;
        if !legacy_tables_present(&mut *tx, backend).await? {
            tx.rollback().await?;
            return Ok(UpgradeOutcome::AlreadyDone);
        }
        // Before anything is copied: a full copy that the swap's DROP TABLE
        // then refuses would repeat on every boot.
        let dependent = dependent_objects(&mut tx, backend).await?;
        if !dependent.is_empty() {
            tx.rollback().await?;
            return Err(UpgradeError::DependentObjects(dependent));
        }
        let resuming = get_setting(&mut *tx, backend, RECORDS_CURSOR)
            .await?
            .is_some()
            || get_setting(&mut *tx, backend, REFS_CURSOR).await?.is_some();
        if !resuming {
            // Rows and their cursor commit together, so without a cursor any
            // rows here were not written by this copy. Start from empty.
            crate::db::query("DELETE FROM happyview_record_refs_v2")
                .execute(&mut *tx)
                .await?;
            crate::db::query("DELETE FROM happyview_records_v2")
                .execute(&mut *tx)
                .await?;
        }
        let total = crate::db::query_scalar::<i64>("SELECT COUNT(*) FROM happyview_records")
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        (resuming, total)
    };
    tracing::info!(
        records = total,
        resuming,
        "record store upgrade: moving records into the compact format; this runs once and blocks startup"
    );
    let interner = Interner::new(DID_CACHE_CAPACITY);
    let mut records = CopyProgress::default();
    let mut attempt = 1;
    let refs = loop {
        let pass = copy_records_on(conn, backend, &interner, COPY_BATCH, None).await?;
        records.rows += pass.rows;
        records.identity_mismatches += pass.identity_mismatches;
        records.unreadable_timestamps += pass.unreadable_timestamps;
        if pass.superseded {
            return Ok(another_boot_upgraded());
        }
        match copy_refs_on(conn, backend, &interner, COPY_BATCH, None).await {
            Err(UpgradeError::RecordsPending { next_uri }) if attempt < COPY_ATTEMPTS => {
                tracing::warn!(
                    next_uri,
                    attempt,
                    "record store upgrade: records were written behind the copy; copying them too"
                );
                attempt += 1;
            }
            other => break other?,
        }
    };
    if refs.superseded {
        return Ok(another_boot_upgraded());
    }
    if records.identity_mismatches > 0 {
        tracing::warn!(
            rows = records.identity_mismatches,
            "record store upgrade: rows whose did, collection or rkey disagreed with their uri were stored under their uri"
        );
    }
    if records.unreadable_timestamps > 0 {
        tracing::warn!(
            timestamps = records.unreadable_timestamps,
            "record store upgrade: unreadable timestamps were stored with their fallbacks"
        );
    }
    if refs.skipped > 0 {
        tracing::warn!(
            refs = refs.skipped,
            "record store upgrade: refs whose source record no longer exists were not copied"
        );
    }
    let Some(verified) = swap(conn, backend).await? else {
        return Ok(another_boot_upgraded());
    };
    tracing::info!(
        records = verified.records,
        refs = verified.refs,
        seconds = started.elapsed().as_secs(),
        "record store upgrade complete"
    );
    Ok(UpgradeOutcome::Upgraded {
        records: verified.records,
        refs: verified.refs,
    })
}

fn another_boot_upgraded() -> UpgradeOutcome {
    tracing::info!("record store upgrade: another boot finished the upgrade first");
    UpgradeOutcome::AlreadyDone
}

const STAGING: CompatNames<'static> = CompatNames {
    records: "happyview_records_staging",
    refs: "happyview_record_refs_staging",
};

/// Verify and swap in one transaction, then (SQLite) reclaim the space in
/// checkpointed steps.
/// `None` when another boot swapped first. On any failure the transaction
/// is rolled back first, releasing its locks, and then the copy is reset
/// (which itself refuses once the legacy tables are gone).
///
/// SQLite's foreign keys are off on this connection for the swap: with them
/// on, `DROP TABLE` first deletes every row one by one.
async fn swap(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
) -> Result<Option<VerifySummary>, UpgradeError> {
    if backend == DatabaseBackend::Sqlite {
        crate::db::query("PRAGMA foreign_keys = OFF")
            .execute(&mut *conn)
            .await?;
    }
    let swapped = swap_in_transaction(conn, backend).await;
    if backend == DatabaseBackend::Sqlite {
        crate::db::query("PRAGMA foreign_keys = ON")
            .execute(&mut *conn)
            .await?;
    }
    let summary = match swapped {
        Ok(summary) => summary,
        Err(Refused {
            error,
            rolled_back: false,
        }) => {
            tracing::error!(
                "record store upgrade: the swap could not be rolled back; not resetting the copy"
            );
            return Err(error);
        }
        Err(Refused { error, .. }) => {
            if let Err(reset) = reset_copy(conn, backend).await {
                tracing::error!(error = %reset, "record store upgrade: could not reset the copy");
            }
            return Err(error);
        }
    };
    if summary.is_some() && backend == DatabaseBackend::Sqlite {
        tracing::info!(
            "record store upgrade: returning the legacy tables' pages to the filesystem"
        );
        // In checkpointed steps: one `incremental_vacuum` over every legacy
        // page would grow the WAL to about the legacy tables' size. The swap
        // has committed, so a failure here costs only disk: the pages stay
        // free in the file for SQLite to reuse, and the next
        // `incremental_vacuum` (the hourly cleanup's, or a compression
        // pass's) returns them.
        match crate::maintenance::sqlite::reclaim_free_pages_on(
            conn,
            crate::maintenance::sqlite::RECLAIM_CHUNK_BYTES,
        )
        .await
        {
            Ok(reclaimed) => tracing::info!(
                pages = reclaimed.pages,
                steps = reclaimed.chunks,
                "record store upgrade: returned the legacy tables' pages"
            ),
            Err(e) => tracing::warn!(
                error = %e,
                "record store upgrade: could not return the legacy tables' pages; they stay free in the database file"
            ),
        }
    }
    Ok(summary)
}

/// A swap that failed, and whether its transaction was rolled back (so the
/// connection is free and holds no locks).
struct Refused {
    error: UpgradeError,
    rolled_back: bool,
}

async fn swap_in_transaction(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
) -> Result<Option<VerifySummary>, Refused> {
    let mut tx = begin_write_on(conn, backend).await.map_err(|e| Refused {
        error: e.into(),
        rolled_back: true,
    })?;
    let swapped = async {
        // Another boot may have swapped while this one copied: on SQLite
        // nothing serialises two processes until this transaction's write
        // lock. Then there is nothing left to do, and nothing to undo.
        if !legacy_tables_present(&mut *tx, backend).await? {
            return Ok(None);
        }
        if backend == DatabaseBackend::Postgres {
            // An instance still on the old release could write the legacy
            // tables during the copy. Locked, the verification sees every
            // such write and refuses the swap instead of dropping it.
            crate::db::query(
                "LOCK TABLE happyview_records, happyview_record_refs IN ACCESS EXCLUSIVE MODE",
            )
            .execute(&mut *tx)
            .await?;
        }
        for sql in compat::drop_statements(backend, STAGING)
            .into_iter()
            .chain(compat::create_statements(backend, STAGING))
        {
            crate::db::query(&sql).execute(&mut *tx).await?;
        }
        let summary = verify(&mut tx, backend, STAGING).await?;
        for sql in compat::drop_statements(backend, STAGING) {
            crate::db::query(&sql).execute(&mut *tx).await?;
        }
        for object in user_objects(&mut tx, backend).await? {
            tracing::warn!(
                kind = object.kind,
                name = object.name,
                definition = object.definition,
                "record store upgrade: dropping an object on the old record tables that HappyView did not \
                 create; recreate an index on happyview_records_v2 (see the upgrade notes)"
            );
        }
        crate::db::query("DROP TABLE happyview_record_refs")
            .execute(&mut *tx)
            .await?;
        crate::db::query("DROP TABLE happyview_records")
            .execute(&mut *tx)
            .await?;
        for sql in compat::create_statements(backend, compat::LIVE) {
            crate::db::query(&sql).execute(&mut *tx).await?;
        }
        crate::db::query(&adapt_sql(
            "DELETE FROM happyview_instance_settings WHERE key IN (?, ?)",
            backend,
        ))
        .bind(RECORDS_CURSOR)
        .bind(REFS_CURSOR)
        .execute(&mut *tx)
        .await?;
        Ok::<_, UpgradeError>(Some(summary))
    }
    .await;
    match swapped {
        Ok(Some(summary)) => {
            tx.commit().await.map_err(|e| Refused {
                error: e.into(),
                rolled_back: false,
            })?;
            Ok(Some(summary))
        }
        Ok(None) => {
            tx.rollback().await.map_err(|e| Refused {
                error: e.into(),
                rolled_back: false,
            })?;
            Ok(None)
        }
        Err(error) => {
            // Explicitly, not on drop: a dropped transaction's rollback waits
            // for the connection's next use, and until then it holds its
            // locks, which the reset's TRUNCATE on Postgres would wait on.
            let rolled_back = match tx.rollback().await {
                Ok(()) => true,
                Err(rollback) => {
                    tracing::error!(error = %rollback, "record store upgrade: rollback failed");
                    false
                }
            };
            Err(Refused { error, rolled_back })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::pre_upgrade_pools;

    const NSID: &str = "app.test.up";
    const POST_CID: &str = "bafyreie5737gdxlw5i64vzichcalba3z2v5n6icifvx5xytvske7mr3hpm";

    async fn seed_legacy(pool: &AnyPool, backend: DatabaseBackend, n: usize) {
        let insert = adapt_sql(
            "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at, rev) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            backend,
        );
        let refs = adapt_sql(
            "INSERT INTO happyview_record_refs (source_uri, target_uri, collection) VALUES (?, ?, ?)",
            backend,
        );
        for i in 0..n {
            let did = format!("did:plc:u{}", i % 3);
            let uri = format!("at://{did}/{NSID}/r{i:03}");
            crate::db::query(&insert)
                .bind(&uri)
                .bind(&did)
                .bind(NSID)
                .bind(format!("r{i:03}"))
                .bind(format!(
                    r#"{{"n":{i},"via":"witchsky","subject":"at://did:plc:t/{NSID}/x{i}"}}"#
                ))
                .bind(POST_CID)
                .bind((i % 2 == 0).then_some("2026-01-01T00:00:00+00:00"))
                .bind("2026-01-02T00:00:00.5+00:00")
                .bind((i % 4 == 0).then(|| format!("3l{i:03}")))
                .execute(pool)
                .await
                .expect("seed record");
            crate::db::query(&refs)
                .bind(&uri)
                .bind(format!("at://did:plc:t/{NSID}/x{i}"))
                .bind(NSID)
                .execute(pool)
                .await
                .expect("seed ref");
        }
        // Only SQLite can hold a ref without its record: a row inserted with
        // the foreign keys off, or by raw SQL that turned them off. Postgres
        // always enforces the key.
        if backend == DatabaseBackend::Sqlite {
            let mut conn = pool.acquire().await.expect("acquire");
            crate::db::query("PRAGMA foreign_keys = OFF")
                .execute(&mut *conn)
                .await
                .expect("foreign keys off");
            crate::db::query(&refs)
                .bind(format!("at://did:plc:gone/{NSID}/orphan"))
                .bind("at://did:plc:t")
                .bind(NSID)
                .execute(&mut *conn)
                .await
                .expect("seed an orphan ref");
            crate::db::query("PRAGMA foreign_keys = ON")
                .execute(&mut *conn)
                .await
                .expect("foreign keys on");
        }
    }

    async fn staged(pool: &AnyPool, backend: DatabaseBackend) -> (String, String) {
        let tag = &uuid::Uuid::new_v4().simple().to_string()[..8];
        let (records, refs) = (format!("hvs_records_{tag}"), format!("hvs_refs_{tag}"));
        for sql in crate::record_store::compat::create_statements(
            backend,
            CompatNames {
                records: &records,
                refs: &refs,
            },
        ) {
            crate::db::query(&sql)
                .execute(pool)
                .await
                .expect("staging views");
        }
        (records, refs)
    }

    async fn copy_all(
        pool: &AnyPool,
        backend: DatabaseBackend,
        batch: i64,
    ) -> (CopyProgress, CopyProgress) {
        let interner = Interner::new(64);
        let records = copy_records(pool, backend, &interner, batch, None)
            .await
            .expect("copy records");
        let refs = copy_refs(pool, backend, &interner, batch, None)
            .await
            .expect("copy refs");
        (records, refs)
    }

    /// Verify in batches of `batch`, so small seeds cross batch boundaries.
    async fn verify_batched(
        pool: &AnyPool,
        backend: DatabaseBackend,
        batch: i64,
    ) -> Result<VerifySummary, UpgradeError> {
        let (records, refs) = staged(pool, backend).await;
        let mut conn = pool.acquire().await.expect("acquire");
        verify_in_batches(
            &mut conn,
            backend,
            CompatNames {
                records: &records,
                refs: &refs,
            },
            batch,
        )
        .await
    }

    async fn verify_on(
        pool: &AnyPool,
        backend: DatabaseBackend,
    ) -> Result<VerifySummary, UpgradeError> {
        let (records, refs) = staged(pool, backend).await;
        let mut conn = pool.acquire().await.expect("acquire");
        verify(
            &mut conn,
            backend,
            CompatNames {
                records: &records,
                refs: &refs,
            },
        )
        .await
    }

    async fn exec(pool: &AnyPool, sql: &str) {
        crate::db::query(sql)
            .execute(pool)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }

    #[tokio::test]
    async fn the_copy_moves_every_record_and_ref_and_verifies() {
        for db in pre_upgrade_pools().await {
            let (pool, backend) = (&db.pool, db.backend);
            seed_legacy(pool, backend, 25).await;
            let (records, refs) = copy_all(pool, backend, 10).await;
            assert_eq!(
                (records.rows, records.batches, records.done),
                (25, 3, true),
                "{backend:?}"
            );
            let orphans = u64::from(backend == DatabaseBackend::Sqlite);
            assert_eq!(
                (refs.rows, refs.skipped),
                (25 + orphans, orphans),
                "{backend:?}: an orphan is skipped"
            );
            let summary = verify_on(pool, backend).await.expect("verification passes");
            assert_eq!(
                summary,
                VerifySummary {
                    records: 25,
                    refs: 25,
                    unreadable_timestamps: 0,
                    ref_collection_mismatches: 0,
                },
                "{backend:?}"
            );
            let summary = verify_batched(pool, backend, 7)
                .await
                .expect("verification passes across batches");
            assert_eq!((summary.records, summary.refs), (25, 25), "{backend:?}");
            db.finish().await;
        }
    }

    #[tokio::test]
    async fn an_interrupted_copy_resumes_where_it_stopped() {
        for db in pre_upgrade_pools().await {
            let (pool, backend) = (&db.pool, db.backend);
            seed_legacy(pool, backend, 25).await;
            let first = copy_records(pool, backend, &Interner::new(64), 10, Some(1))
                .await
                .expect("one batch");
            assert_eq!((first.rows, first.done), (10, false));
            let rest = copy_records(pool, backend, &Interner::new(64), 10, None)
                .await
                .expect("the rest");
            assert_eq!(
                (rest.rows, rest.done),
                (15, true),
                "{backend:?}: resumed after the cursor"
            );
            let refs = copy_refs(pool, backend, &Interner::new(64), 10, Some(1))
                .await
                .expect("one batch of refs");
            assert_eq!((refs.rows, refs.done), (10, false));
            copy_refs(pool, backend, &Interner::new(64), 10, None)
                .await
                .expect("the rest of the refs");
            let (v2,): (i64,) = crate::db::query_as("SELECT COUNT(*) FROM happyview_records_v2")
                .fetch_one(pool)
                .await
                .expect("count");
            assert_eq!(v2, 25);
            verify_on(pool, backend).await.expect("verification passes");
            db.finish().await;
        }
    }

    /// Review Focus 3.
    #[tokio::test]
    async fn noncanonical_legacy_rows_copy_by_uri_and_keep_their_text() {
        let rows = [
            // uri, did column, record, cid, indexed_at, created_at
            (
                "at://did:plc:n/app.test.up/1",
                "did:plc:other",
                r#"{"b":1,"a":2}"#,
                "",
                Some("2026-03-16 12:34:56"),
                "2026-10-11T12:34:56.123456789+00:00",
            ),
            (
                "at://did:plc:n/app.test.up/2",
                "did:plc:n",
                "{\"t\":\"\\u00e9\\n\"}",
                "c50",
                None,
                "2026-03-18 12:00:00.123456+00",
            ),
            (
                "at://did:plc:n/app.test.up/3",
                "did:plc:n",
                "{}",
                "QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbdG",
                Some("not a time"),
                "2026-01-01T00:00:00Z",
            ),
        ];
        for db in pre_upgrade_pools().await {
            let (pool, backend) = (&db.pool, db.backend);
            let insert = adapt_sql(
                "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at) \
                 VALUES (?, ?, 'app.test.up', ?, ?, ?, ?, ?)",
                backend,
            );
            for (uri, did, record, cid, indexed_at, created_at) in rows {
                crate::db::query(&insert)
                    .bind(uri)
                    .bind(did)
                    .bind(uri.rsplit('/').next().expect("rkey"))
                    .bind(record)
                    .bind(cid)
                    .bind(indexed_at)
                    .bind(created_at)
                    .execute(pool)
                    .await
                    .expect("seed");
            }
            let progress = copy_records(pool, backend, &Interner::new(8), 100, None)
                .await
                .expect("copy");
            assert_eq!(
                (progress.identity_mismatches, progress.unreadable_timestamps),
                (1, 1),
                "{backend:?}"
            );
            copy_refs(pool, backend, &Interner::new(8), 100, None)
                .await
                .expect("refs");
            let (view, _) = staged(pool, backend).await;
            for (uri, _, record, cid, indexed_at, created_at) in rows {
                let (_, _, rkey) = codec::parse_record_uri(uri).expect("record uri");
                type Row = (String, String, String, Option<String>, String);
                let (did, got_record, got_cid, got_indexed, got_created): Row =
                    crate::db::query_as(&adapt_sql(
                        &format!(
                            "SELECT did, record, cid, indexed_at, created_at FROM {view} \
                             WHERE did = 'did:plc:n' AND collection = 'app.test.up' AND rkey = ?"
                        ),
                        backend,
                    ))
                    .bind(rkey)
                    .fetch_one(pool)
                    .await
                    .unwrap_or_else(|e| panic!("{backend:?} {uri}: {e}"));
                assert_eq!(did, "did:plc:n", "copied by its uri");
                assert_eq!(
                    (got_record.as_str(), got_cid.as_str()),
                    (record, cid),
                    "{backend:?} {uri}"
                );
                let expected_indexed =
                    indexed_at.map(|t| codec::render_micros(codec::parse_micros(t).unwrap_or(0)));
                assert_eq!(got_indexed, expected_indexed, "{backend:?} {uri}");
                assert_eq!(
                    got_created,
                    codec::render_micros(codec::parse_micros(created_at).expect("readable"))
                );
            }
            let mut conn = pool.acquire().await.expect("acquire");
            let summary = verify(
                &mut conn,
                backend,
                CompatNames {
                    records: &view,
                    refs: "unused",
                },
            )
            .await
            .expect("verification passes");
            assert_eq!(summary.unreadable_timestamps, 1, "{backend:?}");
            drop(conn);
            db.finish().await;
        }
    }

    /// T11-2: an unreadable `created_at` takes the row's `indexed_at`, and
    /// `0` only when that is unreadable or NULL too; verification expects
    /// the same values and counts the same timestamps.
    #[tokio::test]
    async fn unreadable_timestamps_fall_back_and_are_counted() {
        let rows = [
            // rkey, indexed_at, created_at, expected indexed_at, expected created_at
            (
                "a",
                Some("2026-02-03T04:05:06Z"),
                "garbage",
                Some(codec::parse_micros("2026-02-03T04:05:06Z").expect("readable")),
                codec::parse_micros("2026-02-03T04:05:06Z").expect("readable"),
            ),
            ("b", Some("also garbage"), "garbage", Some(0), 0),
            ("c", None, "2026-13-45T00:00:00Z", None, 0),
        ];
        for db in pre_upgrade_pools().await {
            let (pool, backend) = (&db.pool, db.backend);
            let insert = adapt_sql(
                "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at) \
                 VALUES (?, 'did:plc:ts', 'app.test.up', ?, '{}', ?, ?, ?)",
                backend,
            );
            for (rkey, indexed_at, created_at, _, _) in rows {
                crate::db::query(&insert)
                    .bind(format!("at://did:plc:ts/app.test.up/{rkey}"))
                    .bind(rkey)
                    .bind(POST_CID)
                    .bind(indexed_at)
                    .bind(created_at)
                    .execute(pool)
                    .await
                    .expect("seed");
            }
            let (records, _) = copy_all(pool, backend, 100).await;
            assert_eq!(records.unreadable_timestamps, 4, "{backend:?}");
            for (rkey, _, _, indexed_at, created_at) in rows {
                let stored: (Option<i64>, i64) = crate::db::query_as(&adapt_sql(
                    "SELECT indexed_at, created_at FROM happyview_records_v2 WHERE rkey = ?",
                    backend,
                ))
                .bind(rkey)
                .fetch_one(pool)
                .await
                .expect("copied row");
                assert_eq!(stored, (indexed_at, created_at), "{backend:?} {rkey}");
            }
            let summary = verify_on(pool, backend)
                .await
                .expect("the fallback verifies");
            assert_eq!(summary.unreadable_timestamps, 4, "{backend:?}");

            // A created_at the copy did not fall back on is still a mismatch.
            exec(
                pool,
                "UPDATE happyview_records_v2 SET created_at = 1 WHERE rkey = 'b'",
            )
            .await;
            let err = verify_on(pool, backend).await.expect_err("a changed time");
            assert!(
                err.to_string()
                    .contains("at://did:plc:ts/app.test.up/b: created_at differs"),
                "{backend:?}: {err}"
            );
            db.finish().await;
        }
    }

    #[tokio::test]
    async fn a_uri_that_is_not_a_record_uri_stops_the_copy() {
        for db in pre_upgrade_pools().await {
            let (pool, backend) = (&db.pool, db.backend);
            exec(
                pool,
                "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, created_at) \
                 VALUES ('at://did:plc:x/onlytwo', 'did:plc:x', 'onlytwo', 'r', '{}', 'c', '2026-01-01T00:00:00Z')",
            )
            .await;
            let err = copy_records(pool, backend, &Interner::new(8), 100, None)
                .await
                .expect_err("refused");
            assert!(
                matches!(&err, UpgradeError::BadUri { uri } if uri == "at://did:plc:x/onlytwo"),
                "{err}"
            );
            db.finish().await;
        }
    }

    #[tokio::test]
    async fn verification_names_a_missing_row_and_a_changed_body() {
        for db in pre_upgrade_pools().await {
            let (pool, backend) = (&db.pool, db.backend);
            seed_legacy(pool, backend, 25).await;
            copy_all(pool, backend, 100).await;
            let body_column = match backend {
                DatabaseBackend::Sqlite => "body = CAST('{\"tampered\":1}' AS BLOB)",
                DatabaseBackend::Postgres => "record = '{\"tampered\":1}'",
            };
            exec(
                pool,
                &format!(
                    "UPDATE happyview_records_v2 SET {body_column} WHERE id = (SELECT MIN(id) FROM happyview_records_v2)"
                ),
            )
            .await;
            let err = verify_on(pool, backend).await.expect_err("a changed body");
            assert!(
                err.to_string().contains("record differs"),
                "{backend:?}: {err}"
            );

            exec(
                pool,
                "DELETE FROM happyview_records_v2 WHERE id = (SELECT MAX(id) FROM happyview_records_v2)",
            )
            .await;
            let err = verify_on(pool, backend).await.expect_err("a missing row");
            assert!(err.to_string().contains(NSID), "{backend:?}: {err}");
            db.finish().await;
        }
    }

    /// The body of the first copied row, one byte changed.
    #[tokio::test]
    async fn verification_catches_a_body_one_byte_off() {
        for db in pre_upgrade_pools().await {
            let (pool, backend) = (&db.pool, db.backend);
            seed_legacy(pool, backend, 25).await;
            copy_all(pool, backend, 100).await;
            let select = match backend {
                DatabaseBackend::Sqlite => {
                    "SELECT id, hv_record(body, body_dict) FROM happyview_records_v2 ORDER BY id DESC LIMIT 1"
                }
                DatabaseBackend::Postgres => {
                    "SELECT id, record FROM happyview_records_v2 ORDER BY id DESC LIMIT 1"
                }
            };
            let (id, body): (i64, String) = crate::db::query_as(select)
                .fetch_one(pool)
                .await
                .expect("a body");
            let tampered = body.replacen("witchsky", "witchskz", 1);
            assert_eq!(tampered.len(), body.len());
            let update = match backend {
                DatabaseBackend::Sqlite => {
                    crate::db::query("UPDATE happyview_records_v2 SET body = ? WHERE id = ?")
                        .bind(tampered.into_bytes())
                }
                DatabaseBackend::Postgres => {
                    crate::db::query("UPDATE happyview_records_v2 SET record = $1 WHERE id = $2")
                        .bind(tampered)
                }
            };
            update.bind(id).execute(pool).await.expect("tamper");
            let err = verify_batched(pool, backend, 7)
                .await
                .expect_err("a body one byte off");
            assert!(
                matches!(&err, UpgradeError::Mismatch(m) if m.contains("record differs") && m.contains("at://did:plc:u")),
                "{backend:?}: {err}"
            );
            db.finish().await;
        }
    }

    #[tokio::test]
    async fn verification_catches_a_changed_cid() {
        for db in pre_upgrade_pools().await {
            let (pool, backend) = (&db.pool, db.backend);
            seed_legacy(pool, backend, 25).await;
            copy_all(pool, backend, 100).await;
            let mut other = codec::encode_cid(POST_CID);
            *other.last_mut().expect("bytes") ^= 1;
            crate::db::query(&adapt_sql(
                "UPDATE happyview_records_v2 SET cid = ? WHERE rkey = 'r013'",
                backend,
            ))
            .bind(other)
            .execute(pool)
            .await
            .expect("tamper");
            let err = verify_batched(pool, backend, 7)
                .await
                .expect_err("a changed cid");
            assert!(
                err.to_string()
                    .contains(&format!("at://did:plc:u1/{NSID}/r013: cid differs")),
                "{backend:?}: {err}"
            );
            db.finish().await;
        }
    }

    #[tokio::test]
    async fn verification_catches_a_missing_or_changed_ref() {
        for db in pre_upgrade_pools().await {
            let (pool, backend) = (&db.pool, db.backend);
            seed_legacy(pool, backend, 25).await;
            copy_all(pool, backend, 100).await;
            exec(
                pool,
                "UPDATE happyview_record_refs_v2 SET target_rkey = 'elsewhere' \
                 WHERE source_id = (SELECT MAX(source_id) FROM happyview_record_refs_v2)",
            )
            .await;
            let err = verify_batched(pool, backend, 7)
                .await
                .expect_err("a changed ref");
            assert!(
                matches!(&err, UpgradeError::Mismatch(m) if m.contains("ref missing from the copy") && m.contains("at://did:plc:t/app.test.up/x")),
                "{backend:?}: {err}"
            );

            exec(
                pool,
                "DELETE FROM happyview_record_refs_v2 \
                 WHERE source_id = (SELECT MIN(source_id) FROM happyview_record_refs_v2)",
            )
            .await;
            let err = verify_batched(pool, backend, 7)
                .await
                .expect_err("a missing ref");
            assert!(err.to_string().contains("refs:"), "{backend:?}: {err}");
            db.finish().await;
        }
    }

    #[tokio::test]
    async fn verification_catches_an_extra_or_renamed_row() {
        for db in pre_upgrade_pools().await {
            let (pool, backend) = (&db.pool, db.backend);
            seed_legacy(pool, backend, 25).await;
            copy_all(pool, backend, 100).await;
            // Same counts, different identity: only the full walk sees it.
            exec(
                pool,
                "UPDATE happyview_records_v2 SET rkey = 'renamed' WHERE rkey = 'r020'",
            )
            .await;
            let err = verify_batched(pool, backend, 7)
                .await
                .expect_err("a renamed row");
            assert!(
                err.to_string().contains(&format!(
                    "at://did:plc:u2/{NSID}/r020: missing from the copy"
                )),
                "{backend:?}: {err}"
            );
            exec(
                pool,
                "UPDATE happyview_records_v2 SET rkey = 'r020' WHERE rkey = 'renamed'",
            )
            .await;
            verify_on(pool, backend).await.expect("restored");

            let body = match backend {
                DatabaseBackend::Sqlite => "body, body_dict",
                DatabaseBackend::Postgres => "record",
            };
            let value = match backend {
                DatabaseBackend::Sqlite => "CAST('{}' AS BLOB), NULL",
                DatabaseBackend::Postgres => "'{}'",
            };
            exec(
                pool,
                &format!(
                    "INSERT INTO happyview_records_v2 (did_id, collection_id, rkey, cid, {body}, created_at) \
                     SELECT did_id, collection_id, 'extra', cid, {value}, created_at \
                     FROM happyview_records_v2 WHERE rkey = 'r000'"
                ),
            )
            .await;
            let err = verify_on(pool, backend).await.expect_err("an extra row");
            assert!(
                err.to_string()
                    .contains(&format!("{NSID}: 25 legacy rows, 26 copied")),
                "{backend:?}: {err}"
            );
            db.finish().await;
        }
    }

    /// T11-3: the failure path empties the copy and its cursors, and a fresh
    /// copy then starts from the beginning.
    #[tokio::test]
    async fn reset_copy_clears_the_copy_and_a_fresh_copy_succeeds() {
        for db in pre_upgrade_pools().await {
            let (pool, backend) = (&db.pool, db.backend);
            seed_legacy(pool, backend, 25).await;
            let interner = Interner::new(64);
            copy_records(pool, backend, &interner, 10, None)
                .await
                .expect("records");
            copy_refs(pool, backend, &interner, 10, Some(1))
                .await
                .expect("some refs");
            let mut conn = pool.acquire().await.expect("acquire");
            assert!(reset_copy(&mut conn, backend).await.expect("reset"));
            drop(conn);
            for table in ["happyview_records_v2", "happyview_record_refs_v2"] {
                let (n,): (i64,) = crate::db::query_as(&format!("SELECT COUNT(*) FROM {table}"))
                    .fetch_one(pool)
                    .await
                    .expect("count");
                assert_eq!(n, 0, "{backend:?}: {table} emptied");
            }
            for key in [RECORDS_CURSOR, REFS_CURSOR] {
                assert_eq!(
                    get_setting(pool, backend, key).await.expect("setting"),
                    None,
                    "{backend:?}: {key} cleared"
                );
            }

            // The interner may still hold ids from before the reset: interned
            // names are kept, so they stay right.
            let (records, refs) = (
                copy_records(pool, backend, &interner, 10, None)
                    .await
                    .expect("records again"),
                copy_refs(pool, backend, &interner, 10, None)
                    .await
                    .expect("refs again"),
            );
            assert_eq!((records.rows, records.done), (25, true), "{backend:?}");
            assert!(refs.done);
            verify_on(pool, backend).await.expect("verification passes");
            db.finish().await;
        }
    }

    /// Copy refs one batch per call, as an interrupted boot would, until
    /// done; returns the rows each call copied.
    async fn copy_refs_resuming(pool: &AnyPool, backend: DatabaseBackend, batch: i64) -> Vec<u64> {
        let mut rows = Vec::new();
        loop {
            let step = copy_refs(pool, backend, &Interner::new(8), batch, Some(1))
                .await
                .expect("one batch of refs");
            if step.rows == 0 {
                return rows;
            }
            rows.push(step.rows);
        }
    }

    async fn count(pool: &AnyPool, table: &str) -> i64 {
        let (n,): (i64,) = crate::db::query_as(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(pool)
            .await
            .expect("count");
        n
    }

    /// M1: an empty uri (and on SQLite a NULL one) sorts before every
    /// cursor; the copy and the verification refuse it by name.
    #[tokio::test]
    async fn a_blank_uri_stops_the_copy_and_the_verification() {
        for db in pre_upgrade_pools().await {
            let (pool, backend) = (&db.pool, db.backend);
            seed_legacy(pool, backend, 3).await;
            let mut blanks = vec![(
                "''",
                r#""" (did did:plc:blank, collection app.test.up, rkey r)"#,
            )];
            if backend == DatabaseBackend::Sqlite {
                blanks.push((
                    "NULL",
                    "NULL (did did:plc:blank, collection app.test.up, rkey r)",
                ));
            }
            for (value, named) in blanks {
                exec(
                    pool,
                    &format!(
                        "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, created_at) \
                         VALUES ({value}, 'did:plc:blank', 'app.test.up', 'r', '{{}}', 'c', '2026-01-01T00:00:00Z')"
                    ),
                )
                .await;
                let err = copy_records(pool, backend, &Interner::new(8), 100, None)
                    .await
                    .expect_err("refused");
                assert!(
                    matches!(&err, UpgradeError::BadUri { uri } if uri == named),
                    "{backend:?}: {err}"
                );
                let err = verify_on(pool, backend).await.expect_err("refused");
                assert!(
                    matches!(&err, UpgradeError::BadUri { uri } if uri == named),
                    "{backend:?}: {err}"
                );
                exec(
                    pool,
                    "DELETE FROM happyview_records WHERE did = 'did:plc:blank'",
                )
                .await;
            }
            db.finish().await;
        }
    }

    /// M2: the refs cursor holds any text, a unit separator included, and
    /// a resumed copy neither re-reads nor skips a ref.
    #[tokio::test]
    async fn the_refs_cursor_round_trips_any_text() {
        for db in pre_upgrade_pools().await {
            let (pool, backend) = (&db.pool, db.backend);
            let source = format!("at://did:plc:s/{NSID}/a\u{1f}b");
            exec(
                pool,
                &format!(
                    "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, created_at) \
                     VALUES ('{source}', 'did:plc:s', '{NSID}', 'a\u{1f}b', '{{}}', 'c', '2026-01-01T00:00:00Z')"
                ),
            )
            .await;
            let insert = adapt_sql(
                "INSERT INTO happyview_record_refs (source_uri, target_uri, collection) VALUES (?, ?, ?)",
                backend,
            );
            for target in ["x\u{1f}y", "x\u{1f}z", "at://did:plc:t\u{1f}", "zz"] {
                crate::db::query(&insert)
                    .bind(&source)
                    .bind(target)
                    .bind(NSID)
                    .execute(pool)
                    .await
                    .expect("seed ref");
            }
            copy_records(pool, backend, &Interner::new(8), 100, None)
                .await
                .expect("records");
            let first = copy_refs(pool, backend, &Interner::new(8), 1, Some(1))
                .await
                .expect("one ref");
            assert_eq!(first.rows, 1);
            let (lowest_target,): (String,) = crate::db::query_as(
                "SELECT target_uri FROM happyview_record_refs ORDER BY source_uri, target_uri LIMIT 1",
            )
            .fetch_one(pool)
            .await
            .expect("first ref");
            let saved = get_setting(pool, backend, REFS_CURSOR)
                .await
                .expect("cursor");
            assert_eq!(
                decode_refs_cursor(saved.as_deref()),
                (source.clone(), lowest_target),
                "{backend:?}"
            );
            assert_eq!(
                copy_refs_resuming(pool, backend, 1).await,
                vec![1, 1, 1],
                "{backend:?}"
            );
            assert_eq!(count(pool, "happyview_record_refs_v2").await, 4);
            verify_batched(pool, backend, 1)
                .await
                .expect("verification passes");
            db.finish().await;
        }
    }

    /// M3: refs wait for every record, or a ref whose source is not copied
    /// yet would be skipped for good.
    #[tokio::test]
    async fn refs_are_not_copied_before_every_record_is() {
        for db in pre_upgrade_pools().await {
            let (pool, backend) = (&db.pool, db.backend);
            seed_legacy(pool, backend, 25).await;
            let err = copy_refs(pool, backend, &Interner::new(8), 10, None)
                .await
                .expect_err("no records copied");
            assert!(
                matches!(&err, UpgradeError::RecordsPending { next_uri } if next_uri.ends_with("/r000")),
                "{backend:?}: {err}"
            );
            copy_records(pool, backend, &Interner::new(8), 10, Some(1))
                .await
                .expect("one batch");
            let err = copy_refs(pool, backend, &Interner::new(8), 10, None)
                .await
                .expect_err("records half copied");
            assert!(
                matches!(err, UpgradeError::RecordsPending { .. }),
                "{backend:?}: {err}"
            );
            assert_eq!(count(pool, "happyview_record_refs_v2").await, 0);
            copy_records(pool, backend, &Interner::new(8), 10, None)
                .await
                .expect("the rest");
            copy_refs(pool, backend, &Interner::new(8), 10, None)
                .await
                .expect("refs once records are done");
            verify_on(pool, backend).await.expect("verification passes");
            db.finish().await;
        }
    }

    /// M5: a resume that falls between two refs of the same source.
    #[tokio::test]
    async fn a_resume_inside_one_sources_refs_copies_each_ref_once() {
        for db in pre_upgrade_pools().await {
            let (pool, backend) = (&db.pool, db.backend);
            seed_legacy(pool, backend, 3).await;
            let insert = adapt_sql(
                "INSERT INTO happyview_record_refs (source_uri, target_uri, collection) VALUES (?, ?, ?)",
                backend,
            );
            let source = format!("at://did:plc:u1/{NSID}/r001");
            for n in 0..4 {
                crate::db::query(&insert)
                    .bind(&source)
                    .bind(format!("at://did:plc:t/{NSID}/many{n}"))
                    .bind(NSID)
                    .execute(pool)
                    .await
                    .expect("seed ref");
            }
            let legacy = count(pool, "happyview_record_refs").await;
            copy_records(pool, backend, &Interner::new(8), 100, None)
                .await
                .expect("records");
            let steps = copy_refs_resuming(pool, backend, 2).await;
            assert_eq!(
                steps.iter().sum::<u64>(),
                legacy as u64,
                "{backend:?}: {steps:?}"
            );
            let orphans = i64::from(backend == DatabaseBackend::Sqlite);
            assert_eq!(
                count(pool, "happyview_record_refs_v2").await,
                legacy - orphans
            );
            let summary = verify_batched(pool, backend, 2)
                .await
                .expect("verification passes");
            assert_eq!(summary.refs, (legacy - orphans) as u64);
            db.finish().await;
        }
    }

    /// I1: a legacy ref whose collection disagrees with its source (raw SQL
    /// could write one) keeps that collection through the copy, the
    /// verification and the refs view, and is counted.
    #[tokio::test]
    async fn a_divergent_ref_collection_survives_the_copy_exactly() {
        for db in pre_upgrade_pools().await {
            let (pool, backend) = (&db.pool, db.backend);
            seed_legacy(pool, backend, 3).await;
            let source = format!("at://did:plc:u1/{NSID}/r001");
            let target = "at://did:plc:t/app.other.thing/odd";
            crate::db::query(&adapt_sql(
                "INSERT INTO happyview_record_refs (source_uri, target_uri, collection) VALUES (?, ?, 'app.other')",
                backend,
            ))
            .bind(&source)
            .bind(target)
            .execute(pool)
            .await
            .expect("seed a divergent ref");
            let (_, refs) = copy_all(pool, backend, 100).await;
            assert_eq!(refs.ref_collection_mismatches, 1, "{backend:?}");
            let summary = verify_batched(pool, backend, 2)
                .await
                .expect("verification passes");
            assert_eq!(summary.ref_collection_mismatches, 1, "{backend:?}");

            let (_, view) = staged(pool, backend).await;
            let rows: Vec<(String, String, String)> = crate::db::query_as(&format!(
                "SELECT source_uri, target_uri, collection FROM {view} ORDER BY source_uri, target_uri"
            ))
            .fetch_all(pool)
            .await
            .expect("read the view");
            let legacy: Vec<(String, String, String)> = crate::db::query_as(
                "SELECT f.source_uri, f.target_uri, f.collection FROM happyview_record_refs f \
                 WHERE EXISTS (SELECT 1 FROM happyview_records r WHERE r.uri = f.source_uri) \
                 ORDER BY f.source_uri, f.target_uri",
            )
            .fetch_all(pool)
            .await
            .expect("read the legacy refs");
            assert_eq!(rows, legacy, "{backend:?}: byte for byte through the view");
            assert!(rows.contains(&(source.clone(), target.to_string(), "app.other".to_string())));

            // Normalising it on the way would be a mismatch.
            exec(
                pool,
                "UPDATE happyview_record_refs_v2 SET collection_id = \
                 (SELECT collection_id FROM happyview_records_v2 WHERE rkey = 'r001') \
                 WHERE target_rkey = 'odd'",
            )
            .await;
            let err = verify_batched(pool, backend, 2)
                .await
                .expect_err("a normalised collection");
            assert!(
                err.to_string()
                    .contains(&format!("{source} -> {target}: ref collection differs")),
                "{backend:?}: {err}"
            );
            db.finish().await;
        }
    }

    /// A migrated, not yet upgraded SQLite file seeded with `n` legacy
    /// records (and their refs), and its path.
    async fn seeded_sqlite_file(n: usize) -> std::path::PathBuf {
        crate::db::install_drivers();
        let path = std::env::temp_dir().join(format!("hv-race-{}.db", uuid::Uuid::new_v4()));
        let pool = sqlite_file_pool(&path).await;
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations/sqlite");
        sqlx::migrate::Migrator::new(dir)
            .await
            .expect("load")
            .run(&pool)
            .await
            .expect("migrate");
        seed_legacy(&pool, DatabaseBackend::Sqlite, n).await;
        pool.close().await;
        path
    }

    /// Another process's pool on the same file: its own connections, WAL and
    /// a busy timeout, as `db::connect` opens one.
    async fn sqlite_file_pool(path: &std::path::Path) -> AnyPool {
        let pool = sqlx::pool::PoolOptions::<sqlx::Any>::new()
            .max_connections(4)
            .connect(&format!("sqlite://{}?mode=rwc", path.display()))
            .await
            .expect("connect");
        crate::db::query("PRAGMA journal_mode = WAL")
            .execute(&pool)
            .await
            .expect("wal");
        pool
    }

    fn remove_sqlite_file(path: &std::path::Path) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    /// Two processes booting one SQLite file at once: whichever loses the
    /// race must never empty the compact tables the winner swapped in.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_upgrades_racing_on_one_sqlite_file_keep_every_record() {
        for round in 0..5 {
            let path = seeded_sqlite_file(200).await;
            let (a, b) = (sqlite_file_pool(&path).await, sqlite_file_pool(&path).await);
            let (ra, rb) = tokio::join!(
                run(&a, DatabaseBackend::Sqlite),
                run(&b, DatabaseBackend::Sqlite)
            );
            // One boot upgrades; the other finds it done, whichever point of
            // the upgrade it had reached.
            let mut outcomes = [
                ra.as_ref().expect("boot a").clone(),
                rb.as_ref().expect("boot b").clone(),
            ];
            outcomes.sort_by_key(|o| matches!(o, UpgradeOutcome::AlreadyDone));
            assert_eq!(
                outcomes,
                [
                    UpgradeOutcome::Upgraded {
                        records: 200,
                        refs: 200
                    },
                    UpgradeOutcome::AlreadyDone
                ],
                "round {round}"
            );
            assert!(
                !legacy_tables_present(&a, DatabaseBackend::Sqlite)
                    .await
                    .expect("check")
            );
            assert_eq!(
                count(&a, "happyview_records_v2").await,
                200,
                "round {round}: {ra:?} / {rb:?}"
            );
            assert_eq!(
                count(&a, "happyview_record_refs_v2").await,
                200,
                "round {round}"
            );
            assert_eq!(
                count(&a, "happyview_records").await,
                200,
                "round {round}: through the view"
            );
            assert_eq!(
                count(&a, "happyview_record_refs").await,
                200,
                "round {round}: through the view"
            );
            // A third boot afterwards changes nothing.
            assert_eq!(
                run(&b, DatabaseBackend::Sqlite).await.expect("again"),
                UpgradeOutcome::AlreadyDone
            );
            assert_eq!(count(&b, "happyview_records_v2").await, 200);
            a.close().await;
            b.close().await;
            remove_sqlite_file(&path);
        }
    }

    /// A reset that comes after the swap (a boot that lost a race and then
    /// failed) is refused: the compact tables are the only copy by then.
    #[tokio::test]
    async fn reset_copy_after_a_completed_swap_leaves_the_records_alone() {
        for db in pre_upgrade_pools().await {
            let (pool, backend) = (&db.pool, db.backend);
            seed_legacy(pool, backend, 12).await;
            run(pool, backend).await.expect("upgrade");
            let mut conn = pool.acquire().await.expect("acquire");
            assert!(
                !reset_copy(&mut conn, backend)
                    .await
                    .expect("refused, not failed")
            );
            drop(conn);
            assert_eq!(count(pool, "happyview_records_v2").await, 12, "{backend:?}");
            assert_eq!(
                count(pool, "happyview_record_refs_v2").await,
                12,
                "{backend:?}"
            );
            db.finish().await;
        }
    }

    /// Everything logged at warn or above while the guard lives, as text.
    #[derive(Clone, Default)]
    struct Logs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Logs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("logs").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
        type Writer = Logs;
        fn make_writer(&'a self) -> Logs {
            self.clone()
        }
    }

    impl Logs {
        fn capture(&self) -> tracing::subscriber::DefaultGuard {
            tracing::subscriber::set_default(
                tracing_subscriber::fmt()
                    .with_writer(self.clone())
                    .with_ansi(false)
                    .with_max_level(tracing::Level::WARN)
                    .finish(),
            )
        }

        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().expect("logs")).into_owned()
        }
    }

    /// A Postgres view on a legacy table would make the swap's `DROP TABLE`
    /// fail after a full copy, on every boot. It is refused up front, by
    /// name, before anything is copied.
    #[tokio::test]
    async fn a_dependent_postgres_view_refuses_the_upgrade_by_name() {
        for db in pre_upgrade_pools().await {
            let (pool, backend) = (&db.pool, db.backend);
            if backend != DatabaseBackend::Postgres {
                db.finish().await;
                continue;
            }
            seed_legacy(pool, backend, 12).await;
            exec(
                pool,
                "CREATE VIEW my_witchsky AS SELECT uri, record FROM happyview_records \
                 WHERE record::jsonb->>'via' = 'witchsky'",
            )
            .await;
            let err = run(pool, backend).await.expect_err("refused");
            let message = err.to_string();
            assert!(
                matches!(err, UpgradeError::DependentObjects(_))
                    && message.contains("view my_witchsky")
                    && message.contains("drop"),
                "{message}"
            );
            assert!(legacy_tables_present(pool, backend).await.expect("check"));
            assert_eq!(
                count(pool, "happyview_records_v2").await,
                0,
                "nothing copied"
            );
            assert_eq!(
                get_setting(pool, backend, RECORDS_CURSOR)
                    .await
                    .expect("cursor"),
                None
            );

            exec(pool, "DROP VIEW my_witchsky").await;
            assert!(matches!(
                run(pool, backend).await.expect("upgrade"),
                UpgradeOutcome::Upgraded { records: 12, .. }
            ));
            exec(
                pool,
                "CREATE VIEW my_witchsky AS SELECT uri, record FROM happyview_records \
                 WHERE record::jsonb->>'via' = 'witchsky'",
            )
            .await;
            assert_eq!(count(pool, "my_witchsky").await, 12);
            db.finish().await;
        }
    }

    /// A user's own index or trigger on a legacy table goes with it, so the
    /// swap logs each one's definition first, and the upgrade goes on.
    #[tokio::test]
    async fn a_custom_index_and_trigger_are_logged_and_dropped() {
        for db in pre_upgrade_pools().await {
            let (pool, backend) = (&db.pool, db.backend);
            seed_legacy(pool, backend, 12).await;
            // Every index the migrations built is on HappyView's own list.
            let mut conn = pool.acquire().await.expect("acquire");
            let own: Vec<String> = user_objects(&mut conn, backend)
                .await
                .expect("list")
                .into_iter()
                .map(|o| o.name)
                .collect();
            assert!(own.is_empty(), "{backend:?}: not on the own list: {own:?}");
            assert!(
                dependent_objects(&mut conn, backend)
                    .await
                    .expect("list")
                    .is_empty()
            );
            drop(conn);
            let statements: &[&str] = match backend {
                DatabaseBackend::Sqlite => &[
                    "CREATE INDEX my_via ON happyview_records (json_extract(record, '$.via'))",
                    "CREATE TRIGGER my_trigger AFTER INSERT ON happyview_records BEGIN SELECT 1; END",
                ],
                DatabaseBackend::Postgres => &[
                    "CREATE INDEX my_via ON happyview_records ((record::jsonb->>'via'))",
                    "CREATE FUNCTION my_noop() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$",
                    "CREATE TRIGGER my_trigger AFTER INSERT ON happyview_records FOR EACH ROW EXECUTE FUNCTION my_noop()",
                ],
            };
            for sql in statements {
                exec(pool, sql).await;
            }
            let logs = Logs::default();
            let outcome = {
                let _capture = logs.capture();
                run(pool, backend).await.expect("upgrade")
            };
            assert!(matches!(
                outcome,
                UpgradeOutcome::Upgraded { records: 12, .. }
            ));
            let text = logs.text();
            for expected in ["my_via", "my_trigger", "happyview_records"] {
                assert!(
                    text.contains(expected),
                    "{backend:?}: {expected} not in {text}"
                );
            }
            assert!(
                text.contains("via"),
                "{backend:?}: the index definition: {text}"
            );
            for own in [
                "idx_records_collection_created_at_uri",
                "idx_record_refs_target",
            ] {
                assert!(
                    !text.contains(own),
                    "{backend:?}: HappyView's own {own} logged: {text}"
                );
            }
            let left: i64 = match backend {
                DatabaseBackend::Sqlite => crate::db::query_scalar(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('my_via', 'my_trigger')",
                ),
                DatabaseBackend::Postgres => crate::db::query_scalar(
                    "SELECT (SELECT COUNT(*) FROM pg_class WHERE relname = 'my_via') \
                          + (SELECT COUNT(*) FROM pg_trigger WHERE tgname = 'my_trigger')",
                ),
            }
            .fetch_one(pool)
            .await
            .expect("count");
            assert_eq!(left, 0, "{backend:?}");
            db.finish().await;
        }
    }

    /// A SQLite view names `happyview_records`, which after the swap is the
    /// compatibility view, so it keeps answering.
    #[tokio::test]
    async fn a_sqlite_user_view_keeps_working_after_the_swap() {
        let pool = crate::test_support::pre_upgrade_memory_pool().await;
        let backend = DatabaseBackend::Sqlite;
        seed_legacy(&pool, backend, 12).await;
        exec(
            &pool,
            "CREATE VIEW my_witchsky AS SELECT uri, record FROM happyview_records \
             WHERE json_extract(record, '$.via') = 'witchsky'",
        )
        .await;
        let before: Vec<(String, String)> =
            crate::db::query_as("SELECT uri, record FROM my_witchsky ORDER BY uri")
                .fetch_all(&pool)
                .await
                .expect("before");
        assert!(matches!(
            run(&pool, backend).await.expect("upgrade"),
            UpgradeOutcome::Upgraded { .. }
        ));
        let after: Vec<(String, String)> =
            crate::db::query_as("SELECT uri, record FROM my_witchsky ORDER BY uri")
                .fetch_all(&pool)
                .await
                .expect("the view still reads");
        assert_eq!(after, before);
        assert_eq!(after.len(), 12);
    }

    #[tokio::test]
    async fn run_swaps_the_legacy_tables_for_views_once() {
        for db in pre_upgrade_pools().await {
            let (pool, backend) = (&db.pool, db.backend);
            seed_legacy(pool, backend, 25).await;
            let before: Vec<(String, String)> = crate::db::query_as(
                "SELECT uri, CAST(record AS TEXT) FROM happyview_records ORDER BY uri",
            )
            .fetch_all(pool)
            .await
            .expect("legacy rows");
            let outcome = run(pool, backend).await.expect("upgrade");
            assert_eq!(
                outcome,
                UpgradeOutcome::Upgraded {
                    records: 25,
                    refs: 25
                },
                "{backend:?}"
            );
            assert!(!legacy_tables_present(pool, backend).await.expect("check"));
            assert_eq!(
                run(pool, backend).await.expect("again"),
                UpgradeOutcome::AlreadyDone
            );
            let after: Vec<(String, String)> =
                crate::db::query_as("SELECT uri, record FROM happyview_records ORDER BY uri")
                    .fetch_all(pool)
                    .await
                    .expect("rows through the view");
            assert_eq!(after, before, "{backend:?}");
            for key in [RECORDS_CURSOR, REFS_CURSOR] {
                assert_eq!(
                    get_setting(pool, backend, key).await.expect("setting"),
                    None
                );
            }
            db.finish().await;
        }
    }

    /// A refused swap rolls back before it resets the copy: on Postgres the
    /// reset's `TRUNCATE` would otherwise wait forever on the swap
    /// transaction's locks.
    #[tokio::test]
    async fn a_refused_swap_resets_the_copy_promptly_and_drops_nothing() {
        for db in pre_upgrade_pools().await {
            let (pool, backend) = (&db.pool, db.backend);
            seed_legacy(pool, backend, 12).await;
            let interner = Interner::new(64);
            copy_records(pool, backend, &interner, 100, None)
                .await
                .expect("copy");
            // A row behind the copy's cursor, which no resumed copy reads.
            crate::db::query(&adapt_sql(
                "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, created_at) \
                 VALUES (?, 'did:plc:a', ?, 'r', '{}', 'c', '2026-01-01T00:00:00Z')",
                backend,
            ))
            .bind(format!("at://did:plc:a/{NSID}/r"))
            .bind(NSID)
            .execute(pool)
            .await
            .expect("late write");
            let err = tokio::time::timeout(std::time::Duration::from_secs(30), run(pool, backend))
                .await
                .expect("the refused swap returns")
                .expect_err("the swap is refused");
            assert!(
                matches!(err, UpgradeError::Mismatch(_))
                    && err.to_string().contains("13 legacy rows, 12 copied"),
                "{backend:?}: {err}"
            );
            assert!(legacy_tables_present(pool, backend).await.expect("check"));
            assert_eq!(count(pool, "happyview_records_v2").await, 0, "{backend:?}");
            assert_eq!(
                get_setting(pool, backend, RECORDS_CURSOR)
                    .await
                    .expect("cursor"),
                None
            );
            // The next boot copies from the start and succeeds.
            assert!(matches!(
                run(pool, backend).await.expect("retry"),
                UpgradeOutcome::Upgraded { records: 13, .. }
            ));
            db.finish().await;
        }
    }
}

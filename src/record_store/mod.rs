//! The one owner of the record tables: `happyview_records_v2` and
//! `happyview_record_refs_v2`, with identity interned in `happyview_dids`
//! and `happyview_collections` ([`intern`]) and bodies encoded per backend
//! ([`compression`]). `happyview_records` and `happyview_record_refs` are
//! compatibility views over them for raw SQL ([`compat`]); HappyView itself
//! never reads or writes through those.
//!
//! Every write of a record, and of the refs it carries, goes through here:
//! Jetstream and backfill pages through [`RecordStore::write_network`] (one
//! [`RecordStore::upsert_batch`]), and `index_put`, `mirror_network_write`,
//! XRPC procedures and linked repos through [`RecordStore::write_local`],
//! which shares its statement. Every read does too: the store's own lookups
//! and [`query`]'s plugin and Lua queries read [`records_relation`]. A
//! lookup by URI parses it and seeks the identity index by interned id; the
//! derived `uri` columns are for output only, since filtering on them scans.
//! The storage format can then change in one place, and the ingest paths
//! cannot drift apart.

pub mod codec;
pub mod compat;
pub mod compression;
pub mod intern;
pub mod query;
pub mod sqlite_functions;
pub mod upgrade;

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use sqlx::{AnyConnection, AnyPool};

use happyview_provider::{Delta, ReadRoute, RecordKey, RecordStorage};

use crate::db::{DatabaseBackend, adapt_sql};
use crate::event_log::{EventLog, Severity, log_event};
use crate::tombstones::WriteCandidate;
use codec::RefTarget;
use intern::{DID_CACHE_CAPACITY, InternScope, Interner};

/// Rows per multi-row upsert: nine bound parameters each, well within either
/// backend's parameter limit, whatever size a PDS makes a page.
pub const RECORDS_PER_UPSERT: usize = 100;

/// Ids per `IN (…)` list when deleting.
const DELETE_CHUNK: usize = 500;

/// Label subjects of claimed collections per page of the orphaned-label
/// sweep, each checked with the external storage.
const LABEL_GC_PAGE: i64 = 500;

/// The id of a DID, bound by text; NULL when the DID was never interned.
pub(crate) const DID_ID: &str = "(SELECT id FROM happyview_dids WHERE did = ?)";
/// The id of a collection, bound by NSID; NULL when never interned.
pub(crate) const COLLECTION_ID: &str = "(SELECT id FROM happyview_collections WHERE nsid = ?)";

/// The records as every read sees them: the legacy columns under their
/// legacy names, plus `id`, the interned ids and the raw microsecond
/// timestamps for seeking and ordering. Readers filter on `did_id` and
/// `collection_id`, never on the derived `uri`, so the planner uses the v2
/// indexes. JSON filters keep reading `record`, unchanged on both backends.
///
/// SQLite joins with `CROSS JOIN`, which its planner never reorders: `r`
/// stays the outer loop, and its index supplies the order a listing asks
/// for instead of a sort.
pub(crate) fn records_relation(backend: DatabaseBackend) -> String {
    let (record, join) = match backend {
        DatabaseBackend::Sqlite => ("hv_record(r.body, r.body_dict)", "CROSS JOIN"),
        DatabaseBackend::Postgres => ("r.record", "JOIN"),
    };
    format!(
        "(SELECT r.id AS id, r.did_id AS did_id, r.collection_id AS collection_id, \
                 'at://' || d.did || '/' || c.nsid || '/' || r.rkey AS uri, \
                 d.did AS did, c.nsid AS collection, r.rkey AS rkey, {record} AS record, \
                 hv_cid_text(r.cid) AS cid, hv_ts_text(r.indexed_at) AS indexed_at, \
                 hv_ts_text(r.created_at) AS created_at, \
                 r.indexed_at AS indexed_at_us, r.created_at AS created_at_us \
            FROM happyview_records_v2 r \
            {join} happyview_dids d ON d.id = r.did_id \
            {join} happyview_collections c ON c.id = r.collection_id) rec"
    )
}

/// The collection listing, newest arrival first, by offset; pinned to
/// `idx_records_v2_collection_indexed_at` by a test.
pub(crate) fn list_recent_sql(backend: DatabaseBackend, by_did: bool) -> String {
    let did = if by_did {
        format!(" AND rec.did_id = {DID_ID}")
    } else {
        String::new()
    };
    format!(
        "SELECT {STORED_COLUMNS} FROM {} WHERE rec.collection_id = {COLLECTION_ID}{did} \
         ORDER BY rec.indexed_at_us DESC LIMIT ? OFFSET ?",
        records_relation(backend)
    )
}

/// A record ready to store: its identity, the JSON text exactly as it will be
/// read back, its CID and, for a network record, its commit rev.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedRecord {
    pub did: String,
    pub collection: String,
    pub rkey: String,
    /// Stored and returned byte-for-byte; never re-serialised here.
    pub record_json: String,
    pub cid: String,
    /// The commit rev when the source carries one; see `crate::tombstones`.
    /// `listRecords` does not, so crawled rows are `None` and are written as
    /// before revs existed, except that any tombstone blocks them.
    pub rev: Option<String>,
}

impl PreparedRecord {
    pub fn uri(&self) -> String {
        format!("at://{}/{}/{}", self.did, self.collection, self.rkey)
    }
}

/// How an upsert treats a row that already exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteKind {
    /// From the network (Jetstream, backfill). Skips rows a tombstone
    /// supersedes, stamps `indexed_at`, never replaces a newer rev, and leaves
    /// an unchanged, already stamped row alone (see `RecordStore::upsert_sql`).
    Network,
    /// A write this instance just made to a PDS and Jetstream will echo:
    /// replaces the body and CID and clears `indexed_at`, so the echo
    /// re-stamps the row.
    Echoed,
    /// Index-only (`index_put`), never echoed: an update replaces the body and
    /// keeps the stored CID and `indexed_at`.
    IndexOnly,
}

/// A row an upsert inserted or changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changed {
    pub uri: String,
}

/// Which step of a local write failed. The row and its refs are separate
/// statements, so the index can hold a correct row whose backlinks are empty
/// until Jetstream re-syncs them; an operator reading the log needs to know
/// whether the record is missing or merely unlinked.
#[derive(Debug, thiserror::Error)]
pub enum LocalWriteError {
    #[error("row not written: {0}")]
    Row(sqlx::Error),
    #[error("row written but its refs not synced: {0}")]
    Refs(sqlx::Error),
}

/// A stored record with its identity, as the read paths return it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredRecord {
    pub uri: String,
    pub did: String,
    pub collection: String,
    pub rkey: String,
    /// The JSON text exactly as it was stored.
    pub record: String,
    pub cid: Option<String>,
    pub indexed_at: Option<String>,
    pub created_at: String,
}

type StoredRow = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
);

impl From<StoredRow> for StoredRecord {
    fn from(row: StoredRow) -> Self {
        let (uri, did, collection, rkey, record, cid, indexed_at, created_at) = row;
        Self {
            uri,
            did,
            collection,
            rkey,
            record,
            cid,
            indexed_at,
            created_at,
        }
    }
}

/// [`records_relation`]'s columns a [`StoredRow`] decodes.
const STORED_COLUMNS: &str = "rec.uri, rec.did, rec.collection, rec.rkey, rec.record, rec.cid, \
     rec.indexed_at, rec.created_at";

/// A handle on the record tables. Cheap to clone; clones share the interned
/// id cache and the body codec.
///
/// With an external [`RecordStorage`] (see [`RecordStore::with_external`]),
/// every collection it claims is read from its [`ReadRoute`] and written as a
/// [`Delta`] to it; nothing for a claimed collection touches the local tables.
#[derive(Clone)]
pub struct RecordStore {
    db: AnyPool,
    backend: DatabaseBackend,
    interner: Arc<Interner>,
    bodies: Arc<compression::BodyCodec>,
    external: Option<Arc<dyn RecordStorage>>,
}

/// A row an upsert inserted or changed: its id, its collection's id, and
/// its index in the records the upsert was given.
struct WrittenRow {
    id: i64,
    collection_id: i64,
    index: usize,
}

/// The last copy of each URI in `records`, in first-seen order: Postgres
/// refuses to upsert one row twice in a statement (`21000`).
fn last_copies(records: &[PreparedRecord]) -> Vec<&PreparedRecord> {
    let mut last: HashMap<String, usize> = HashMap::new();
    for (i, rec) in records.iter().enumerate() {
        last.insert(rec.uri(), i);
    }
    records
        .iter()
        .enumerate()
        .filter(|(i, rec)| last[&rec.uri()] == *i)
        .map(|(_, rec)| rec)
        .collect()
}

fn record_key(record: &PreparedRecord) -> RecordKey {
    RecordKey {
        did: record.did.clone(),
        collection: record.collection.clone(),
        rkey: record.rkey.clone(),
    }
}

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(", ")
}

/// The `at://` strings each body carries, sorted.
fn ref_targets<'a>(bodies: impl Iterator<Item = &'a str>) -> Vec<Vec<String>> {
    bodies
        .map(|body| {
            let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
            let mut targets: Vec<String> = crate::record_refs::extract_at_uris(&value)
                .into_iter()
                .collect();
            targets.sort();
            targets
        })
        .collect()
}

impl RecordStore {
    pub fn new(db: AnyPool, backend: DatabaseBackend) -> Self {
        Self::with_bodies(db, backend, compression::BodyCodec::disabled())
    }

    pub fn with_bodies(
        db: AnyPool,
        backend: DatabaseBackend,
        bodies: compression::BodyCodec,
    ) -> Self {
        Self {
            db,
            backend,
            interner: Arc::new(Interner::new(DID_CACHE_CAPACITY)),
            bodies: Arc::new(bodies),
            external: None,
        }
    }

    /// This store, with `external` serving every collection it claims.
    pub fn with_external(mut self, external: Arc<dyn RecordStorage>) -> Self {
        self.external = Some(external);
        self
    }

    /// The external storage, whatever it claims.
    pub fn external(&self) -> Option<&Arc<dyn RecordStorage>> {
        self.external.as_ref()
    }

    /// The external storage when it claims `collection`.
    pub fn external_route(&self, collection: &str) -> Option<&Arc<dyn RecordStorage>> {
        self.external.as_ref().filter(|ext| ext.claims(collection))
    }

    /// Where reads of `collection` run: the external storage's route when it
    /// claims the collection, otherwise this instance's own database.
    pub fn read_route(&self, collection: Option<&str>) -> ReadRoute {
        match collection.and_then(|c| self.external_route(c).map(|ext| ext.read_route(c))) {
            Some(route) => route,
            None => self.local_route(),
        }
    }

    fn local_route(&self) -> ReadRoute {
        ReadRoute {
            pool: self.db.clone(),
            backend: self.backend,
        }
    }

    /// The store the server shares: on SQLite its codec holds each
    /// collection's newest dictionary under `RECORD_COMPRESSION`. Postgres
    /// stores bodies as text and never reads `happyview_zstd_dicts`.
    pub async fn shared(db: AnyPool, backend: DatabaseBackend) -> Result<Self, sqlx::Error> {
        let bodies = match backend {
            DatabaseBackend::Sqlite => {
                compression::BodyCodec::load(&db, compression::compression_from_env()).await?
            }
            DatabaseBackend::Postgres => compression::BodyCodec::disabled(),
        };
        Ok(Self::with_bodies(db, backend, bodies))
    }

    pub fn bodies(&self) -> &Arc<compression::BodyCodec> {
        &self.bodies
    }

    pub fn interner(&self) -> &Arc<Interner> {
        &self.interner
    }

    pub fn db(&self) -> &AnyPool {
        &self.db
    }

    pub fn backend(&self) -> DatabaseBackend {
        self.backend
    }

    /// The multi-row upsert of `rows` records for `kind`.
    fn upsert_sql(&self, kind: WriteKind, rows: usize) -> String {
        let t = "happyview_records_v2";
        let (columns, values, set_body) = match self.backend {
            DatabaseBackend::Sqlite => (
                "did_id, collection_id, rkey, cid, body, body_dict, rev, indexed_at, created_at",
                "(?, ?, ?, ?, ?, ?, ?, ?, ?)",
                "body = excluded.body, body_dict = excluded.body_dict",
            ),
            DatabaseBackend::Postgres => (
                "did_id, collection_id, rkey, cid, record, rev, indexed_at, created_at",
                "(?, ?, ?, ?, ?, ?, ?, ?)",
                "record = excluded.record",
            ),
        };
        let conflict = match kind {
            WriteKind::Network => {
                // The row changed (CID, decoded body, or a local write awaiting
                // its echo) and the incoming rev is not older. The decoded
                // body, not the CID alone: a Lua record hook can replace a
                // body under an unchanged CID, and a recompressed row's bytes
                // differ from an identical incoming body's. A missing rev on
                // either side (a local write, a `listRecords` row, a row from
                // before revs were stored) does not block the update.
                let (body_changed, cid_changed) = match self.backend {
                    DatabaseBackend::Sqlite => (
                        format!(
                            "hv_record({t}.body, {t}.body_dict) IS NOT hv_record(excluded.body, excluded.body_dict)"
                        ),
                        format!("{t}.cid IS NOT excluded.cid"),
                    ),
                    DatabaseBackend::Postgres => (
                        format!("{t}.record IS DISTINCT FROM excluded.record"),
                        format!("{t}.cid IS DISTINCT FROM excluded.cid"),
                    ),
                };
                format!(
                    "DO UPDATE SET {set_body}, cid = excluded.cid, indexed_at = excluded.indexed_at, \
                     rev = COALESCE(excluded.rev, {t}.rev) \
                     WHERE ({cid_changed} OR {body_changed} OR {t}.indexed_at IS NULL) \
                       AND ({t}.rev IS NULL OR excluded.rev IS NULL OR {})",
                    crate::db::rev_compare(self.backend, "excluded.rev", ">=", &format!("{t}.rev"))
                )
            }
            WriteKind::Echoed => {
                format!("DO UPDATE SET {set_body}, cid = excluded.cid, indexed_at = NULL")
            }
            WriteKind::IndexOnly => format!("DO UPDATE SET {set_body}"),
        };
        adapt_sql(
            &format!(
                "INSERT INTO {t} ({columns}) VALUES {} \
                 ON CONFLICT (did_id, collection_id, rkey) {conflict} \
                 RETURNING id, did_id, collection_id, rkey",
                vec![values; rows].join(", ")
            ),
            self.backend,
        )
    }

    /// Intern every DID and collection `records` and their ref `targets`
    /// name, in one call (two transactions interning overlapping names in
    /// several calls can deadlock on Postgres), then upsert the rows and
    /// return the ones inserted or changed: the `Network` clause's `WHERE`
    /// leaves unchanged rows out, and they keep their refs and `indexed_at`.
    /// Ids land in `scope`; the caller remembers them once it has committed.
    async fn upsert_rows(
        &self,
        conn: &mut AnyConnection,
        kind: WriteKind,
        records: &[&PreparedRecord],
        targets: &[Vec<String>],
        scope: &mut InternScope,
    ) -> Result<Vec<WrittenRow>, sqlx::Error> {
        let parsed: Vec<RefTarget<'_>> = targets
            .iter()
            .flatten()
            .map(|t| RefTarget::parse(t))
            .collect();
        let mut dids: Vec<&str> = records.iter().map(|r| r.did.as_str()).collect();
        let mut collections: Vec<&str> = records.iter().map(|r| r.collection.as_str()).collect();
        dids.extend(parsed.iter().map(RefTarget::did_text));
        collections.extend(parsed.iter().filter_map(RefTarget::collection));
        self.interner
            .intern(conn, self.backend, scope, &dids, &collections)
            .await?;

        let now = codec::now_micros();
        let indexed_at = (kind == WriteKind::Network).then_some(now);
        let mut by_identity: HashMap<(i64, i64, &str), usize> = HashMap::new();
        for (index, rec) in records.iter().enumerate() {
            by_identity.insert(
                (
                    scope.dids[&rec.did],
                    scope.collections[&rec.collection],
                    rec.rkey.as_str(),
                ),
                index,
            );
        }
        let mut written = Vec::new();
        for chunk in records.chunks(RECORDS_PER_UPSERT) {
            let sql = self.upsert_sql(kind, chunk.len());
            let mut upsert = crate::db::query_as::<(i64, i64, i64, String)>(&sql);
            for rec in chunk {
                let collection_id = scope.collections[&rec.collection];
                let rev = match kind {
                    WriteKind::Network => rec.rev.as_deref(),
                    WriteKind::Echoed | WriteKind::IndexOnly => None,
                };
                upsert = upsert
                    .bind(scope.dids[&rec.did])
                    .bind(collection_id)
                    .bind(rec.rkey.as_str())
                    .bind(codec::encode_cid(&rec.cid));
                upsert = match self.backend {
                    DatabaseBackend::Sqlite => {
                        let (body, body_dict) = self.bodies.encode(collection_id, &rec.record_json);
                        upsert.bind(body).bind(body_dict)
                    }
                    DatabaseBackend::Postgres => upsert.bind(rec.record_json.as_str()),
                };
                upsert = upsert.bind(rev).bind(indexed_at).bind(now);
            }
            for (id, did_id, collection_id, rkey) in upsert.fetch_all(&mut *conn).await? {
                if let Some(index) = by_identity.get(&(did_id, collection_id, rkey.as_str())) {
                    written.push(WrittenRow {
                        id,
                        collection_id,
                        index: *index,
                    });
                }
            }
        }
        Ok(written)
    }

    /// Replace the refs of the `written` rows with their bodies' `targets`
    /// (already interned into `scope`). The one writer of
    /// `happyview_record_refs_v2` rows.
    async fn sync_refs(
        &self,
        conn: &mut AnyConnection,
        scope: &InternScope,
        written: &[WrittenRow],
        targets: &[Vec<String>],
    ) -> Result<(), sqlx::Error> {
        let ids: Vec<i64> = written.iter().map(|w| w.id).collect();
        for chunk in ids.chunks(DELETE_CHUNK) {
            let sql = adapt_sql(
                &format!(
                    "DELETE FROM happyview_record_refs_v2 WHERE source_id IN ({})",
                    placeholders(chunk.len())
                ),
                self.backend,
            );
            let mut delete = crate::db::query(&sql);
            for id in chunk {
                delete = delete.bind(*id);
            }
            delete.execute(&mut *conn).await?;
        }
        let mut rows: Vec<(i64, i64, i64, &str, i64)> = Vec::new();
        for w in written {
            for target in &targets[w.index] {
                let parsed = RefTarget::parse(target);
                let target_collection = match parsed.collection() {
                    Some(nsid) => scope.collections[nsid],
                    None => parsed.kind(),
                };
                rows.push((
                    w.id,
                    scope.dids[parsed.did_text()],
                    target_collection,
                    parsed.rkey(),
                    w.collection_id,
                ));
            }
        }
        for chunk in rows.chunks(crate::record_refs::REFS_PER_INSERT) {
            let sql = adapt_sql(
                &format!(
                    "INSERT INTO happyview_record_refs_v2 \
                     (source_id, target_did_id, target_collection_id, target_rkey, collection_id) VALUES {} \
                     ON CONFLICT DO NOTHING",
                    vec!["(?, ?, ?, ?, ?)"; chunk.len()].join(", ")
                ),
                self.backend,
            );
            let mut insert = crate::db::query(&sql);
            for (source, did_id, collection_id, rkey, source_collection) in chunk {
                insert = insert
                    .bind(*source)
                    .bind(*did_id)
                    .bind(*collection_id)
                    .bind(*rkey)
                    .bind(*source_collection);
            }
            insert.execute(&mut *conn).await?;
        }
        Ok(())
    }

    /// Upsert `records` on the caller's connection, then replace the refs of
    /// exactly the rows inserted or changed. A URI repeated in `records` is
    /// written once, with its last copy. Returns the rows written. The ids
    /// interned are not cached: the caller owns the transaction, and an id
    /// a rolled-back transaction created can be handed to another name.
    ///
    /// A `Network` batch must run on a `db::begin_write` transaction: on
    /// Postgres it locks every DID and then every URI of the batch, in one
    /// call each, before its first check, then skips any row a record or DID
    /// tombstone supersedes, so a reordered or replayed write cannot bring
    /// back a record deleted live (see `crate::tombstones`). Callers run the
    /// transaction inside `db::retry_on_busy`.
    pub async fn upsert_batch(
        &self,
        conn: &mut AnyConnection,
        kind: WriteKind,
        records: &[PreparedRecord],
    ) -> Result<Vec<Changed>, sqlx::Error> {
        let mut scope = InternScope::default();
        self.upsert_batch_in(conn, kind, records, &mut scope).await
    }

    async fn upsert_batch_in(
        &self,
        conn: &mut AnyConnection,
        kind: WriteKind,
        records: &[PreparedRecord],
        scope: &mut InternScope,
    ) -> Result<Vec<Changed>, sqlx::Error> {
        if records.is_empty() {
            return Ok(Vec::new());
        }
        let mut records = last_copies(records);
        if kind == WriteKind::Network {
            let uris: Vec<String> = records.iter().map(|rec| rec.uri()).collect();
            let dids: Vec<&str> = records.iter().map(|rec| rec.did.as_str()).collect();
            let uri_refs: Vec<&str> = uris.iter().map(String::as_str).collect();
            crate::tombstones::lock_dids(&mut *conn, self.backend, &dids).await?;
            crate::tombstones::lock_uris(&mut *conn, self.backend, &uri_refs).await?;
            let candidates: Vec<WriteCandidate<'_>> = records
                .iter()
                .zip(&uris)
                .map(|(rec, uri)| WriteCandidate {
                    uri,
                    did: &rec.did,
                    rev: rec.rev.as_deref(),
                })
                .collect();
            let blocked =
                crate::tombstones::blocked_writes(&mut *conn, self.backend, &candidates).await?;
            if !blocked.is_empty() {
                tracing::debug!(?blocked, "skipping records a tombstone supersedes");
                records.retain(|rec| !blocked.contains(&rec.uri()));
            }
        }
        let targets = ref_targets(records.iter().map(|rec| rec.record_json.as_str()));
        let written = self
            .upsert_rows(conn, kind, &records, &targets, scope)
            .await?;
        self.sync_refs(conn, scope, &written, &targets).await?;
        Ok(written
            .iter()
            .map(|w| Changed {
                uri: records[w.index].uri(),
            })
            .collect())
    }

    /// [`Self::upsert_batch`] of network `records` in its own
    /// `db::begin_write` transaction on `pool` (backfill passes its own
    /// pool), caching the ids it interned once it commits. Callers run it
    /// inside `db::retry_on_busy`.
    pub async fn write_network(
        &self,
        pool: &AnyPool,
        records: &[PreparedRecord],
    ) -> Result<Vec<Changed>, sqlx::Error> {
        if records.is_empty() {
            return Ok(Vec::new());
        }
        let mut scope = InternScope::default();
        let mut tx = crate::db::begin_write(pool, self.backend).await?;
        let written = self
            .upsert_batch_in(&mut tx, WriteKind::Network, records, &mut scope)
            .await?;
        tx.commit().await?;
        self.interner.remember(scope);
        Ok(written)
    }

    /// One local write (`Echoed` or `IndexOnly`): the row, then its refs in
    /// a transaction of their own, as two steps whose failures are told
    /// apart. A `Network` kind is refused: it needs the locks and tombstone
    /// checks of [`Self::upsert_batch`].
    pub async fn write_local(
        &self,
        kind: WriteKind,
        record: &PreparedRecord,
    ) -> Result<(), LocalWriteError> {
        if kind == WriteKind::Network {
            return Err(LocalWriteError::Row(sqlx::Error::InvalidArgument(
                "a network write goes through upsert_batch, not write_local".to_string(),
            )));
        }
        if let Some(ext) = self.external_route(&record.collection) {
            // Shown by the claiming store until its own copy (the echo)
            // arrives.
            let created_at_us = serde_json::from_str::<Value>(&record.record_json)
                .ok()
                .and_then(|body| {
                    body.get("createdAt")
                        .and_then(Value::as_str)
                        .and_then(codec::parse_micros)
                })
                .unwrap_or_else(codec::now_micros);
            let delta = Delta::Pending {
                cid: record.cid.clone(),
                rev: record.rev.clone(),
                body: record.record_json.clone(),
                created_at_us,
            };
            return ext
                .apply_delta(&record_key(record), delta, None)
                .await
                .map_err(LocalWriteError::Row);
        }
        let records = [record];
        let targets = ref_targets(std::iter::once(record.record_json.as_str()));
        let mut scope = InternScope::default();
        let written = async {
            let mut tx = crate::db::begin_write(&self.db, self.backend).await?;
            let written = self
                .upsert_rows(&mut tx, kind, &records, &targets, &mut scope)
                .await?;
            tx.commit().await?;
            Ok::<_, sqlx::Error>(written)
        }
        .await
        .map_err(LocalWriteError::Row)?;
        // Every id in `scope` was committed with the row.
        let refs = async {
            let mut tx = crate::db::begin_write(&self.db, self.backend).await?;
            self.sync_refs(&mut tx, &scope, &written, &targets).await?;
            tx.commit().await
        }
        .await
        .map_err(LocalWriteError::Refs);
        self.interner.remember(scope);
        refs
    }

    /// `(id, rev)` of the stored record with this identity, through the
    /// identity index.
    async fn find(
        &self,
        conn: &mut AnyConnection,
        did: &str,
        collection: &str,
        rkey: &str,
    ) -> Result<Option<(i64, Option<String>)>, sqlx::Error> {
        let sql = adapt_sql(
            &format!(
                "SELECT id, rev FROM happyview_records_v2 \
                 WHERE did_id = {DID_ID} AND collection_id = {COLLECTION_ID} AND rkey = ?"
            ),
            self.backend,
        );
        crate::db::query_as(&sql)
            .bind(did)
            .bind(collection)
            .bind(rkey)
            .fetch_optional(&mut *conn)
            .await
    }

    /// Delete the records `ids` and their refs on the caller's connection.
    /// Returns the records deleted.
    async fn delete_ids(&self, conn: &mut AnyConnection, ids: &[i64]) -> Result<u64, sqlx::Error> {
        let mut deleted = 0;
        for chunk in ids.chunks(DELETE_CHUNK) {
            for (table, column) in [
                ("happyview_record_refs_v2", "source_id"),
                ("happyview_records_v2", "id"),
            ] {
                let sql = adapt_sql(
                    &format!(
                        "DELETE FROM {table} WHERE {column} IN ({})",
                        placeholders(chunk.len())
                    ),
                    self.backend,
                );
                let mut delete = crate::db::query(&sql);
                for id in chunk {
                    delete = delete.bind(*id);
                }
                let affected = delete.execute(&mut *conn).await?.rows_affected();
                if table == "happyview_records_v2" {
                    deleted += affected;
                }
            }
        }
        Ok(deleted)
    }

    /// Delete one record and its refs. Idempotent; says whether a row was
    /// there.
    pub async fn delete(&self, uri: &str) -> Result<bool, sqlx::Error> {
        let Some((did, collection, rkey)) = codec::parse_record_uri(uri) else {
            return Ok(false);
        };
        if let Some(ext) = self.external_route(collection) {
            // Hidden from this instance's view; the claiming store keeps it.
            let key = RecordKey {
                did: did.to_string(),
                collection: collection.to_string(),
                rkey: rkey.to_string(),
            };
            ext.apply_delta(&key, Delta::Hide, None).await?;
            return Ok(true);
        }
        let mut tx = crate::db::begin_write(&self.db, self.backend).await?;
        let deleted = match self.find(&mut tx, did, collection, rkey).await? {
            Some((id, _)) => self.delete_ids(&mut tx, &[id]).await? > 0,
            None => false,
        };
        tx.commit().await?;
        Ok(deleted)
    }

    /// Delete one record. A delete that carries a rev deletes only a stored row
    /// whose rev is unknown or not newer, and, unless a newer row survives it,
    /// leaves a tombstone in the same transaction, so a reordered or replayed
    /// create cannot bring the record back. A delete that finds no row
    /// tombstones only when a create could still be on its way: the account
    /// holds network records in the collection, or a backfill still writing
    /// covers it (`backfill_running`, which the caller asks the record source
    /// for).
    /// Otherwise it is a delete for a record this instance never held, and the
    /// network sends millions of those. A delete without a rev (a dead-letter
    /// retry) deletes the row whatever its rev and tombstones it at the rev it
    /// was stored at, if any, so a replay of that create cannot bring it back.
    /// The record's refs go with it; a surviving newer row keeps its own.
    #[allow(clippy::too_many_arguments)]
    pub async fn delete_network(
        &self,
        pool: &AnyPool,
        uri: &str,
        did: &str,
        collection: &str,
        rev: Option<&str>,
        now: &str,
        backfill_running: bool,
    ) -> Result<(), sqlx::Error> {
        let backend = self.backend;
        let mut tx = crate::db::begin_write(pool, backend).await?;
        crate::tombstones::lock_dids(&mut tx, backend, &[did]).await?;
        crate::tombstones::lock_uris(&mut tx, backend, &[uri]).await?;
        let rkey = codec::parse_record_uri(uri)
            .map(|(_, _, rkey)| rkey)
            .unwrap_or_default();
        let found = self.find(&mut tx, did, collection, rkey).await?;
        match rev {
            Some(rev) => {
                // TIDs order by bytes, which is how Rust compares strings
                // (and what `COLLATE "C"` gave the SQL comparison).
                let deletable = found
                    .as_ref()
                    .filter(|(_, stored)| stored.as_deref().is_none_or(|stored| stored <= rev));
                let deleted = match deletable {
                    Some((id, _)) => self.delete_ids(&mut tx, &[*id]).await? > 0,
                    None => false,
                };
                let survived = !deleted && found.is_some();
                let tombstone = deleted
                    || (!survived
                        && self
                            .create_may_follow(&mut tx, did, collection, backfill_running)
                            .await?);
                if tombstone {
                    crate::tombstones::write_record_tombstone(&mut tx, backend, uri, rev, now)
                        .await?;
                }
            }
            None => {
                if let Some((id, stored_rev)) = found {
                    self.delete_ids(&mut tx, &[id]).await?;
                    if let Some(stored_rev) = stored_rev {
                        crate::tombstones::write_record_tombstone(
                            &mut tx,
                            backend,
                            uri,
                            &stored_rev,
                            now,
                        )
                        .await?;
                    }
                }
            }
        }
        tx.commit().await
    }

    /// [`Self::delete_network`] without a rev, but only while the stored row
    /// still holds `expected` as its body: a dead-letter retry whose hook now
    /// skips the record removes what the failed-open delivery stored, and
    /// leaves a row a newer write has replaced since. The check and the
    /// delete run in one transaction under the record's locks. Returns
    /// whether a row was removed.
    pub async fn delete_network_if_body(
        &self,
        pool: &AnyPool,
        uri: &str,
        did: &str,
        collection: &str,
        expected: &Value,
        now: &str,
    ) -> Result<bool, sqlx::Error> {
        let backend = self.backend;
        let Some((_, _, rkey)) = codec::parse_record_uri(uri) else {
            return Ok(false);
        };
        let mut tx = crate::db::begin_write(pool, backend).await?;
        crate::tombstones::lock_dids(&mut tx, backend, &[did]).await?;
        crate::tombstones::lock_uris(&mut tx, backend, &[uri]).await?;
        let sql = adapt_sql(&Self::get_sql(backend), backend);
        let stored = crate::db::query_as::<StoredRow>(&sql)
            .bind(did)
            .bind(collection)
            .bind(rkey)
            .fetch_optional(&mut *tx)
            .await?
            .map(StoredRecord::from);
        let holds_expected = stored
            .and_then(|r| serde_json::from_str::<Value>(&r.record).ok())
            .is_some_and(|body| &body == expected);
        if !holds_expected {
            return Ok(false);
        }
        let Some((id, stored_rev)) = self.find(&mut tx, did, collection, rkey).await? else {
            return Ok(false);
        };
        self.delete_ids(&mut tx, &[id]).await?;
        if let Some(stored_rev) = stored_rev {
            crate::tombstones::write_record_tombstone(&mut tx, backend, uri, &stored_rev, now)
                .await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    /// Whether a create for a record `did` holds no row for could still arrive
    /// out of order: the account holds network records in `collection` (live
    /// ingest or a past backfill reached it), or a backfill covering the
    /// collection may still write (`backfill_running`).
    async fn create_may_follow(
        &self,
        conn: &mut AnyConnection,
        did: &str,
        collection: &str,
        backfill_running: bool,
    ) -> Result<bool, sqlx::Error> {
        let sql = adapt_sql(
            &format!(
                "SELECT 1 FROM happyview_records_v2 \
                 WHERE did_id = {DID_ID} AND collection_id = {COLLECTION_ID} \
                 AND indexed_at IS NOT NULL LIMIT 1"
            ),
            self.backend,
        );
        let held = crate::db::query(&sql)
            .bind(did)
            .bind(collection)
            .fetch_optional(&mut *conn)
            .await?
            .is_some();
        Ok(held || backfill_running)
    }

    /// The ids of up to `limit` of the network records `did` holds,
    /// optionally only in `collections`.
    async fn held_network_ids<'e, E>(
        &self,
        executor: E,
        did: &str,
        collections: Option<&[String]>,
        before_rev: Option<&str>,
        limit: i64,
    ) -> Result<Vec<i64>, sqlx::Error>
    where
        E: sqlx::Executor<'e, Database = sqlx::Any>,
    {
        let mut select = format!(
            "SELECT id FROM happyview_records_v2 WHERE did_id = {DID_ID} AND indexed_at IS NOT NULL"
        );
        if let Some(collections) = collections {
            select.push_str(&format!(
                " AND collection_id IN (SELECT id FROM happyview_collections WHERE nsid IN ({}))",
                placeholders(collections.len())
            ));
        }
        if before_rev.is_some() {
            select.push_str(&format!(
                " AND (rev IS NULL OR {})",
                crate::db::rev_compare(self.backend, "rev", "<", "?")
            ));
        }
        select.push_str(" LIMIT ?");
        let select = adapt_sql(&select, self.backend);

        let mut query = crate::db::query_as::<(i64,)>(&select).bind(did);
        for collection in collections.unwrap_or_default() {
            query = query.bind(collection.as_str());
        }
        if let Some(before_rev) = before_rev {
            query = query.bind(before_rev);
        }
        Ok(query
            .bind(limit)
            .fetch_all(executor)
            .await?
            .into_iter()
            .map(|(id,)| id)
            .collect())
    }

    /// Whether `did` holds any network record (`indexed_at` set), optionally
    /// only in `collections`: locally, or, with an external storage, in the
    /// claimed collections among them (all of them when `None`). A storage
    /// that cannot answer counts as holding, so a caller deciding whether to
    /// check the account's PDS checks it. Reads without a lock or a write
    /// transaction.
    pub async fn holds_network_records(
        &self,
        did: &str,
        collections: Option<&[String]>,
    ) -> Result<bool, sqlx::Error> {
        if self.holds_local_network_records(did, collections).await? {
            return Ok(true);
        }
        let Some(ext) = &self.external else {
            return Ok(false);
        };
        let claimed: Option<Vec<String>> = collections.map(|collections| {
            collections
                .iter()
                .filter(|c| ext.claims(c))
                .cloned()
                .collect()
        });
        if claimed.as_ref().is_some_and(Vec::is_empty) {
            return Ok(false);
        }
        Ok(ext
            .holds_account(did, claimed.as_deref())
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(did, error = %e, "the record storage could not say whether it holds an account");
                true
            }))
    }

    /// [`Self::holds_network_records`] in this instance's own tables only.
    pub async fn holds_local_network_records(
        &self,
        did: &str,
        collections: Option<&[String]>,
    ) -> Result<bool, sqlx::Error> {
        Ok(!self
            .held_network_ids(&self.db, did, collections, None, 1)
            .await?
            .is_empty())
    }

    /// Delete up to `limit` of `did`'s network records and their refs on the
    /// caller's transaction, which must hold the DID's lock. Returns how many
    /// were selected, not how many rows the delete found: a delete that does
    /// not take the DID's lock (admin, XRPC, plugin) can remove one in
    /// between on Postgres, and a short count would end the caller's batch
    /// loop while the account still holds records.
    pub async fn purge_network_batch(
        &self,
        conn: &mut AnyConnection,
        did: &str,
        collections: Option<&[String]>,
        limit: i64,
    ) -> Result<u64, sqlx::Error> {
        let ids = self
            .held_network_ids(&mut *conn, did, collections, None, limit)
            .await?;
        self.delete_ids(conn, &ids).await?;
        Ok(ids.len() as u64)
    }

    /// Up to `limit` DIDs after `after_did`, in ascending order, that hold
    /// network records in `collections`, read on `pool`. The query of the
    /// replay job's held-DID sweep, keyed by DID rather than by row id.
    pub async fn held_dids_page(
        &self,
        pool: &AnyPool,
        collections: &[String],
        after_did: Option<&str>,
        limit: i64,
    ) -> Result<Vec<String>, sqlx::Error> {
        if collections.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = vec!["?"; collections.len()].join(", ");
        let sql = crate::db::adapt_sql(
            &format!(
                "SELECT d.did FROM happyview_dids d WHERE d.did > ? AND EXISTS ( \
                   SELECT 1 FROM happyview_records_v2 r \
                   WHERE r.did_id = d.id AND r.indexed_at IS NOT NULL AND r.collection_id IN ( \
                     SELECT id FROM happyview_collections WHERE nsid IN ({placeholders}))) \
                 ORDER BY d.did LIMIT ?"
            ),
            self.backend,
        );
        let mut query = crate::db::query_as::<(String,)>(&sql).bind(after_did.unwrap_or(""));
        for collection in collections {
            query = query.bind(collection.as_str());
        }
        Ok(query
            .bind(limit)
            .fetch_all(pool)
            .await?
            .into_iter()
            .map(|(did,)| did)
            .collect())
    }

    /// [`Self::holds_local_network_records`], counting only rows older than
    /// `before_rev` (or with no rev).
    pub async fn holds_network_records_before(
        &self,
        did: &str,
        collections: Option<&[String]>,
        before_rev: &str,
    ) -> Result<bool, sqlx::Error> {
        Ok(!self
            .held_network_ids(&self.db, did, collections, Some(before_rev), 1)
            .await?
            .is_empty())
    }

    /// [`Self::purge_network_batch`] for rows older than `before_rev` (or with
    /// no rev): what a marker replayed from the archive supersedes.
    pub async fn purge_network_batch_before(
        &self,
        conn: &mut AnyConnection,
        did: &str,
        collections: Option<&[String]>,
        before_rev: &str,
        limit: i64,
    ) -> Result<u64, sqlx::Error> {
        let ids = self
            .held_network_ids(&mut *conn, did, collections, Some(before_rev), limit)
            .await?;
        self.delete_ids(conn, &ids).await?;
        Ok(ids.len() as u64)
    }

    /// Delete up to `limit` records of `collection`, and their refs, in one
    /// write transaction. Returns how many went.
    pub async fn delete_collection_batch(
        &self,
        collection: &str,
        limit: i64,
    ) -> Result<u64, sqlx::Error> {
        if let Some(ext) = self.external_route(collection) {
            return ext.delete_collection_batch(collection, limit).await;
        }
        let mut tx = crate::db::begin_write(&self.db, self.backend).await?;
        let ids: Vec<i64> = crate::db::query_as::<(i64,)>(&adapt_sql(
            &format!(
                "SELECT id FROM happyview_records_v2 WHERE collection_id = {COLLECTION_ID} LIMIT ?"
            ),
            self.backend,
        ))
        .bind(collection)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|(id,)| id)
        .collect();
        let deleted = self.delete_ids(&mut tx, &ids).await?;
        tx.commit().await?;
        Ok(deleted)
    }

    pub async fn count_collection(&self, collection: &str) -> Result<i64, sqlx::Error> {
        match self.external_route(collection) {
            Some(ext) => Self::count_claimed(ext, collection).await,
            None => Self::count_collection_on(&self.local_route(), collection).await,
        }
    }

    /// A claimed collection's count: the storage's own when it gives one,
    /// otherwise counted on its read route.
    async fn count_claimed(
        ext: &Arc<dyn RecordStorage>,
        collection: &str,
    ) -> Result<i64, sqlx::Error> {
        match ext.count(collection).await? {
            Some(n) => Ok(n),
            None => Self::count_collection_on(&ext.read_route(collection), collection).await,
        }
    }

    async fn count_collection_on(route: &ReadRoute, collection: &str) -> Result<i64, sqlx::Error> {
        crate::db::query_scalar::<i64>(&adapt_sql(
            &format!(
                "SELECT COUNT(*) FROM happyview_records_v2 WHERE collection_id = {COLLECTION_ID}"
            ),
            route.backend,
        ))
        .bind(collection)
        .fetch_one(&route.pool)
        .await
    }

    /// The SQL of [`Self::get`]: one row by identity, through the identity
    /// index (pinned by a test).
    pub(crate) fn get_sql(backend: DatabaseBackend) -> String {
        format!(
            "SELECT {STORED_COLUMNS} FROM {} \
             WHERE rec.did_id = {DID_ID} AND rec.collection_id = {COLLECTION_ID} AND rec.rkey = ?",
            records_relation(backend)
        )
    }

    pub async fn get(&self, uri: &str) -> Result<Option<StoredRecord>, sqlx::Error> {
        let Some((_, collection, _)) = codec::parse_record_uri(uri) else {
            return Ok(None);
        };
        Self::get_on(&self.read_route(Some(collection)), uri).await
    }

    /// [`Self::get`] on `route`, unrouted: the plugin query DSL runs it on
    /// the route it already chose.
    pub(crate) async fn get_on(
        route: &ReadRoute,
        uri: &str,
    ) -> Result<Option<StoredRecord>, sqlx::Error> {
        let Some((did, collection, rkey)) = codec::parse_record_uri(uri) else {
            return Ok(None);
        };
        let sql = adapt_sql(&Self::get_sql(route.backend), route.backend);
        Ok(crate::db::query_as::<StoredRow>(&sql)
            .bind(did)
            .bind(collection)
            .bind(rkey)
            .fetch_optional(&route.pool)
            .await?
            .map(StoredRecord::from))
    }

    /// The stored body and rev of `uri` on `route`, unrouted: what a
    /// dead-letter retry checks before it changes a claimed record's view.
    pub(crate) async fn stored_version_on(
        route: &ReadRoute,
        uri: &str,
    ) -> Result<Option<(String, Option<String>)>, sqlx::Error> {
        let Some((did, collection, rkey)) = codec::parse_record_uri(uri) else {
            return Ok(None);
        };
        let record = match route.backend {
            DatabaseBackend::Sqlite => "hv_record(body, body_dict)",
            DatabaseBackend::Postgres => "record",
        };
        let sql = adapt_sql(
            &format!(
                "SELECT {record}, rev FROM happyview_records_v2 \
                 WHERE did_id = {DID_ID} AND collection_id = {COLLECTION_ID} AND rkey = ?"
            ),
            route.backend,
        );
        crate::db::query_as::<(String, Option<String>)>(&sql)
            .bind(did)
            .bind(collection)
            .bind(rkey)
            .fetch_optional(&route.pool)
            .await
    }

    pub async fn stored_cid(&self, uri: &str) -> Result<Option<String>, sqlx::Error> {
        let Some((did, collection, rkey)) = codec::parse_record_uri(uri) else {
            return Ok(None);
        };
        let route = self.read_route(Some(collection));
        let sql = adapt_sql(
            &format!(
                "SELECT hv_cid_text(cid) FROM happyview_records_v2 \
                 WHERE did_id = {DID_ID} AND collection_id = {COLLECTION_ID} AND rkey = ?"
            ),
            route.backend,
        );
        Ok(crate::db::query_as::<(Option<String>,)>(&sql)
            .bind(did)
            .bind(collection)
            .bind(rkey)
            .fetch_optional(&route.pool)
            .await?
            .and_then(|(cid,)| cid))
    }

    /// A collection's records, most recently arrived first, optionally only
    /// `did`'s, by offset.
    pub async fn list_recent(
        &self,
        collection: &str,
        did: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<StoredRecord>, sqlx::Error> {
        let route = self.read_route(Some(collection));
        let sql = adapt_sql(
            &list_recent_sql(route.backend, did.is_some()),
            route.backend,
        );
        let mut query = crate::db::query_as::<StoredRow>(&sql).bind(collection);
        if let Some(did) = did {
            query = query.bind(did);
        }
        Ok(query
            .bind(limit)
            .bind(offset)
            .fetch_all(&route.pool)
            .await?
            .into_iter()
            .map(StoredRecord::from)
            .collect())
    }

    pub async fn count_all(&self) -> Result<i64, sqlx::Error> {
        if self.external.is_some() {
            return Ok(self
                .count_by_collection()
                .await?
                .iter()
                .map(|(_, n)| n)
                .sum());
        }
        crate::db::query_scalar::<i64>("SELECT COUNT(*) FROM happyview_records_v2")
            .fetch_one(&self.db)
            .await
    }

    /// Records per collection, largest first. A claimed collection is
    /// counted in the external storage, in place of any local rows; like a
    /// local one, it is listed only when it holds records.
    pub async fn count_by_collection(&self) -> Result<Vec<(String, i64)>, sqlx::Error> {
        let mut counts = crate::db::query_as::<(String, i64)>(
            "SELECT c.nsid, n.cnt FROM \
             (SELECT collection_id, COUNT(*) AS cnt FROM happyview_records_v2 GROUP BY collection_id) n \
             JOIN happyview_collections c ON c.id = n.collection_id \
             ORDER BY n.cnt DESC, c.nsid",
        )
        .fetch_all(&self.db)
        .await?;
        let Some(ext) = &self.external else {
            return Ok(counts);
        };
        let claimed = ext.claimed();
        counts.retain(|(nsid, _)| !claimed.contains(nsid));
        for collection in claimed {
            let n = Self::count_claimed(ext, &collection).await?;
            if n > 0 {
                counts.push((collection, n));
            }
        }
        counts.sort_by(|(a, x), (b, y)| y.cmp(x).then_with(|| a.cmp(b)));
        Ok(counts)
    }

    /// The first record of `collection` whose `field_sql` (a validated
    /// expression from `query::record_text_field_sql`) equals `value`, as
    /// `(uri, cid)`.
    pub async fn lookup_by_text_field(
        &self,
        collection: &str,
        field_sql: &str,
        value: &str,
    ) -> Result<Option<(String, String)>, sqlx::Error> {
        let route = self.read_route(Some(collection));
        let sql = adapt_sql(
            &format!(
                "SELECT rec.uri, rec.cid FROM {} \
                 WHERE rec.collection_id = {COLLECTION_ID} AND {field_sql} = ? LIMIT 1",
                records_relation(route.backend)
            ),
            route.backend,
        );
        crate::db::query_as::<(String, String)>(&sql)
            .bind(collection)
            .bind(value)
            .fetch_optional(&route.pool)
            .await
    }

    /// Delete every record label whose subject this instance no longer holds
    /// (moved from `labeler::delete_orphaned_labels`; its rationale stays
    /// there). The subject is looked up by identity, not by derived URI.
    ///
    /// With an external storage, labels of claimed collections are left out
    /// of that statement and kept or deleted on the storage's
    /// [`RecordStorage::is_live`], a page of subjects at a time.
    pub async fn delete_orphaned_labels(&self) -> Result<u64, sqlx::Error> {
        const ORPHANED: &str = "DELETE FROM happyview_labels WHERE uri LIKE 'at://%' AND NOT EXISTS ( \
               SELECT 1 FROM happyview_records_v2 r \
                WHERE r.did_id = (SELECT id FROM happyview_dids WHERE did = hv_ref_part(happyview_labels.uri, 0)) \
                  AND r.collection_id = (SELECT id FROM happyview_collections WHERE nsid = hv_ref_part(happyview_labels.uri, 1)) \
                  AND r.rkey = hv_ref_part(happyview_labels.uri, 2))";
        let claimed = self
            .external
            .as_ref()
            .map(|ext| ext.claimed())
            .unwrap_or_default();
        let (Some(ext), false) = (&self.external, claimed.is_empty()) else {
            return crate::db::query(ORPHANED)
                .execute(&self.db)
                .await
                .map(|r| r.rows_affected());
        };

        let sql = adapt_sql(
            &format!(
                "{ORPHANED} AND COALESCE(hv_ref_part(uri, 1), '') NOT IN ({})",
                placeholders(claimed.len())
            ),
            self.backend,
        );
        let mut local = crate::db::query(&sql);
        for collection in &claimed {
            local = local.bind(collection.as_str());
        }
        let mut deleted = local.execute(&self.db).await?.rows_affected();

        let page_sql = adapt_sql(
            &format!(
                "SELECT DISTINCT uri FROM happyview_labels \
                 WHERE uri LIKE 'at://%' AND hv_ref_part(uri, 1) IN ({}) AND uri > ? \
                 ORDER BY uri LIMIT ?",
                placeholders(claimed.len())
            ),
            self.backend,
        );
        let mut after = String::new();
        loop {
            let mut page = crate::db::query_as::<(String,)>(&page_sql);
            for collection in &claimed {
                page = page.bind(collection.as_str());
            }
            let uris: Vec<String> = page
                .bind(after.as_str())
                .bind(LABEL_GC_PAGE)
                .fetch_all(&self.db)
                .await?
                .into_iter()
                .map(|(uri,)| uri)
                .collect();
            let Some(last) = uris.last().cloned() else {
                return Ok(deleted);
            };
            let mut dead = Vec::new();
            for uri in &uris {
                if !ext.is_live(uri).await? {
                    dead.push(uri.as_str());
                }
            }
            if !dead.is_empty() {
                let sql = adapt_sql(
                    &format!(
                        "DELETE FROM happyview_labels WHERE uri IN ({})",
                        placeholders(dead.len())
                    ),
                    self.backend,
                );
                let mut delete = crate::db::query(&sql);
                for uri in dead {
                    delete = delete.bind(uri);
                }
                deleted += delete.execute(&self.db).await?.rows_affected();
            }
            if (uris.len() as i64) < LABEL_GC_PAGE {
                return Ok(deleted);
            }
            after = last;
        }
    }

    /// Whether any record holds a ref: the probe that tells the one-time
    /// refs rebuild whether refs are already maintained live.
    pub async fn has_refs(&self) -> Result<bool, sqlx::Error> {
        let (n,): (i64,) = crate::db::query_as(
            "SELECT COUNT(*) FROM (SELECT 1 FROM happyview_record_refs_v2 LIMIT 1) AS any_ref",
        )
        .fetch_one(&self.db)
        .await?;
        Ok(n > 0)
    }

    /// One page of the one-time refs rebuild on the caller's transaction:
    /// re-sync the refs of up to `limit` records after the opaque cursor
    /// `after` (`""` starts at the beginning). Returns the records walked and
    /// the cursor of the last one, or `None` when there were none.
    ///
    /// The page's URIs are locked (on Postgres) before its bodies are read,
    /// so a network write to one of them cannot land between the read and
    /// the refs rewrite and be left with the older body's refs. The caller's
    /// transaction must be a `db::begin_write` one: on SQLite a deferred
    /// transaction that reads first cannot upgrade to a writer while another
    /// holds the lock.
    pub async fn rebuild_refs_page(
        &self,
        conn: &mut AnyConnection,
        after: &str,
        limit: i64,
    ) -> Result<(u64, Option<String>), sqlx::Error> {
        // An empty or pre-upgrade (uri) cursor restarts the walk; refs are
        // replaced, not added, so walking a record twice is harmless.
        let after: i64 = after.parse().unwrap_or(0);
        let page: Vec<(i64, String, String, String)> = crate::db::query_as(&adapt_sql(
            "SELECT r.id, d.did, c.nsid, r.rkey FROM happyview_records_v2 r \
             JOIN happyview_dids d ON d.id = r.did_id \
             JOIN happyview_collections c ON c.id = r.collection_id \
             WHERE r.id > ? ORDER BY r.id LIMIT ?",
            self.backend,
        ))
        .bind(after)
        .bind(limit)
        .fetch_all(&mut *conn)
        .await?;
        let Some(last) = page.last().map(|(id, ..)| id.to_string()) else {
            return Ok((0, None));
        };
        let uris: Vec<String> = page
            .iter()
            .map(|(_, did, nsid, rkey)| format!("at://{did}/{nsid}/{rkey}"))
            .collect();
        let uri_refs: Vec<&str> = uris.iter().map(String::as_str).collect();
        crate::tombstones::lock_uris(&mut *conn, self.backend, &uri_refs).await?;

        let record = match self.backend {
            DatabaseBackend::Sqlite => "hv_record(body, body_dict)",
            DatabaseBackend::Postgres => "record",
        };
        let ids: Vec<i64> = page.iter().map(|(id, ..)| *id).collect();
        let mut bodies: Vec<(i64, i64, String)> = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(DELETE_CHUNK) {
            let sql = adapt_sql(
                &format!(
                    "SELECT id, collection_id, {record} FROM happyview_records_v2 WHERE id IN ({})",
                    placeholders(chunk.len())
                ),
                self.backend,
            );
            let mut query = crate::db::query_as::<(i64, i64, String)>(&sql);
            for id in chunk {
                query = query.bind(*id);
            }
            // A record deleted since the page was read is simply missing.
            bodies.extend(query.fetch_all(&mut *conn).await?);
        }
        let targets = ref_targets(bodies.iter().map(|(_, _, body)| body.as_str()));
        let parsed: Vec<RefTarget<'_>> = targets
            .iter()
            .flatten()
            .map(|t| RefTarget::parse(t))
            .collect();
        let dids: Vec<&str> = parsed.iter().map(RefTarget::did_text).collect();
        let collections: Vec<&str> = parsed.iter().filter_map(RefTarget::collection).collect();
        // The caller commits, so these ids are not cached.
        let mut scope = InternScope::default();
        self.interner
            .intern(conn, self.backend, &mut scope, &dids, &collections)
            .await?;
        let written: Vec<WrittenRow> = bodies
            .iter()
            .enumerate()
            .map(|(index, (id, collection_id, _))| WrittenRow {
                id: *id,
                collection_id: *collection_id,
                index,
            })
            .collect();
        self.sync_refs(conn, &scope, &written, &targets).await?;
        Ok((page.len() as u64, Some(last)))
    }

    /// A record reached its PDS but not this index. The write the caller made
    /// stands, so this is reported rather than returned: a warning in the log
    /// and a `record.index_failed` event an operator can find.
    pub async fn report_index_failure(&self, uri: &str, error: &(dyn std::fmt::Display + Sync)) {
        tracing::warn!(uri, error = %error, "record written to its PDS but not indexed");
        log_event(
            &self.db,
            EventLog {
                event_type: "record.index_failed".to_string(),
                severity: Severity::Warn,
                actor_did: None,
                subject: Some(uri.to_string()),
                detail: serde_json::json!({ "error": error.to_string() }),
            },
            self.backend,
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::adapt_sql;
    use crate::test_support::test_pools;

    const NSID: &str = "app.test.store";

    fn unique_did() -> String {
        format!("did:plc:rs{}", uuid::Uuid::new_v4().simple())
    }

    fn tid(n: i64) -> String {
        crate::tid::tid_from_unix_microseconds(1_700_000_000_000_000 + n)
    }

    fn prepared(did: &str, rkey: &str, body: &str, cid: &str, rev: Option<&str>) -> PreparedRecord {
        PreparedRecord {
            did: did.to_string(),
            collection: NSID.to_string(),
            rkey: rkey.to_string(),
            record_json: body.to_string(),
            cid: cid.to_string(),
            rev: rev.map(str::to_string),
        }
    }

    async fn row(store: &RecordStore, uri: &str) -> Option<(String, String, Option<String>)> {
        crate::db::query_as::<(String, String, Option<String>)>(&adapt_sql(
            "SELECT record, cid, indexed_at FROM happyview_records WHERE uri = ?",
            store.backend(),
        ))
        .bind(uri)
        .fetch_optional(store.db())
        .await
        .expect("read the row")
    }

    async fn stored_rev(store: &RecordStore, uri: &str) -> Option<String> {
        crate::db::query_scalar::<Option<String>>(&adapt_sql(
            "SELECT rev FROM happyview_records WHERE uri = ?",
            store.backend(),
        ))
        .bind(uri)
        .fetch_one(store.db())
        .await
        .expect("read the rev")
    }

    async fn ref_count(store: &RecordStore, uri: &str) -> i64 {
        crate::db::query_scalar::<i64>(&adapt_sql(
            "SELECT COUNT(*) FROM happyview_record_refs WHERE source_uri = ?",
            store.backend(),
        ))
        .bind(uri)
        .fetch_one(store.db())
        .await
        .expect("count refs")
    }

    /// `upsert_batch` with `Network` on its own `begin_write` transaction,
    /// committed.
    async fn upsert_network(store: &RecordStore, records: &[PreparedRecord]) -> Vec<Changed> {
        let mut tx = crate::db::begin_write(store.db(), store.backend())
            .await
            .expect("begin");
        let written = store
            .upsert_batch(&mut tx, WriteKind::Network, records)
            .await
            .expect("upsert");
        tx.commit().await.expect("commit");
        written
    }

    #[tokio::test]
    async fn a_network_redelivery_changes_nothing_and_a_newer_body_does() {
        for (pool, backend) in test_pools().await {
            let store = RecordStore::new(pool.clone(), backend);
            let did = unique_did();
            let first = prepared(&did, "a", r#"{"text":"one","via":"witchsky"}"#, "c1", None);
            let written = store
                .write_network(&pool, std::slice::from_ref(&first))
                .await
                .expect("write");
            assert_eq!(written, vec![Changed { uri: first.uri() }]);
            let again = store
                .write_network(&pool, std::slice::from_ref(&first))
                .await
                .expect("redeliver");
            assert!(
                again.is_empty(),
                "{backend:?}: an identical redelivery changes nothing"
            );
            let edited = prepared(&did, "a", r#"{"text":"two"}"#, "c2", None);
            let changed = store.write_network(&pool, &[edited]).await.expect("edit");
            assert_eq!(changed.len(), 1);
            let (record, cid, indexed_at) = row(&store, &first.uri()).await.expect("row exists");
            assert_eq!((record.as_str(), cid.as_str()), (r#"{"text":"two"}"#, "c2"));
            assert!(indexed_at.is_some(), "a network write stamps indexed_at");
            store.delete(&first.uri()).await.expect("clean up");
        }
    }

    #[tokio::test]
    async fn a_repeated_uri_in_one_batch_is_written_once_with_its_last_copy() {
        for (pool, backend) in test_pools().await {
            let store = RecordStore::new(pool.clone(), backend);
            let did = unique_did();
            let batch = [
                prepared(&did, "a", r#"{"n":1}"#, "c1", None),
                prepared(&did, "a", r#"{"n":2}"#, "c2", None),
            ];
            let written = store.write_network(&pool, &batch).await.expect("write");
            assert_eq!(written.len(), 1, "{backend:?}");
            let (record, _, _) = row(&store, &batch[0].uri()).await.expect("row");
            assert_eq!(record, r#"{"n":2}"#);
            store.delete(&batch[0].uri()).await.expect("clean up");
        }
    }

    #[tokio::test]
    async fn an_echoed_write_clears_indexed_at_and_an_index_only_write_keeps_the_cid() {
        for (pool, backend) in test_pools().await {
            let store = RecordStore::new(pool.clone(), backend);
            let did = unique_did();
            let network = prepared(&did, "a", r#"{"n":1}"#, "c1", None);
            store
                .write_network(&pool, std::slice::from_ref(&network))
                .await
                .expect("network write");

            let echoed = prepared(&did, "a", r#"{"n":2}"#, "c2", None);
            store
                .write_local(WriteKind::Echoed, &echoed)
                .await
                .expect("echoed write");
            let (record, cid, indexed_at) = row(&store, &network.uri()).await.expect("row");
            assert_eq!(
                (record.as_str(), cid.as_str(), indexed_at),
                (r#"{"n":2}"#, "c2", None)
            );

            let index_only = prepared(&did, "a", r#"{"n":3}"#, "c3", None);
            store
                .write_local(WriteKind::IndexOnly, &index_only)
                .await
                .expect("index-only write");
            let (record, cid, _) = row(&store, &network.uri()).await.expect("row");
            assert_eq!(
                (record.as_str(), cid.as_str()),
                (r#"{"n":3}"#, "c2"),
                "{backend:?}"
            );
            store.delete(&network.uri()).await.expect("clean up");
        }
    }

    #[tokio::test]
    async fn deleting_a_record_deletes_its_refs() {
        for (pool, backend) in test_pools().await {
            let store = RecordStore::new(pool.clone(), backend);
            let did = unique_did();
            let rec = prepared(
                &did,
                "a",
                r#"{"subject":"at://did:plc:x/app.test.store/b"}"#,
                "c1",
                None,
            );
            store
                .write_local(WriteKind::Echoed, &rec)
                .await
                .expect("write");
            assert_eq!(ref_count(&store, &rec.uri()).await, 1);
            assert!(store.delete(&rec.uri()).await.expect("delete"));
            assert_eq!(ref_count(&store, &rec.uri()).await, 0, "{backend:?}");
            assert!(!store.delete(&rec.uri()).await.expect("delete again"));
        }
    }

    #[tokio::test]
    async fn a_collection_batch_delete_takes_refs_with_it() {
        for (pool, backend) in test_pools().await {
            let store = RecordStore::new(pool.clone(), backend);
            let did = unique_did();
            let collection = format!("app.test.batch{}", uuid::Uuid::new_v4().simple());
            let mut recs = Vec::new();
            for rkey in ["a", "b", "c"] {
                let mut rec = prepared(
                    &did,
                    rkey,
                    r#"{"subject":"at://did:plc:x/app.test.store/z"}"#,
                    "c",
                    None,
                );
                rec.collection = collection.clone();
                recs.push(rec);
            }
            store.write_network(&pool, &recs).await.expect("write");
            assert_eq!(store.count_collection(&collection).await.expect("count"), 3);
            assert_eq!(
                store
                    .delete_collection_batch(&collection, 2)
                    .await
                    .expect("batch"),
                2
            );
            assert_eq!(
                store
                    .delete_collection_batch(&collection, 2)
                    .await
                    .expect("batch"),
                1
            );
            assert_eq!(store.count_collection(&collection).await.expect("count"), 0);
            for rec in &recs {
                assert_eq!(ref_count(&store, &rec.uri()).await, 0, "{backend:?}");
            }
        }
    }

    #[tokio::test]
    async fn a_purge_batch_deletes_network_records_and_keeps_local_ones() {
        for (pool, backend) in test_pools().await {
            let store = RecordStore::new(pool.clone(), backend);
            let did = unique_did();
            let network = prepared(&did, "net", r#"{"n":1}"#, "c1", None);
            let local = prepared(&did, "local", r#"{"n":2}"#, "c2", None);
            store
                .write_network(&pool, std::slice::from_ref(&network))
                .await
                .expect("network");
            store
                .write_local(WriteKind::Echoed, &local)
                .await
                .expect("local");
            assert!(
                store
                    .holds_network_records(&did, None)
                    .await
                    .expect("holds")
            );
            let mut tx = crate::db::begin_write(&pool, backend).await.expect("begin");
            assert_eq!(
                store
                    .purge_network_batch(&mut tx, &did, None, 10)
                    .await
                    .expect("purge"),
                1
            );
            tx.commit().await.expect("commit");
            assert!(row(&store, &network.uri()).await.is_none(), "{backend:?}");
            assert!(row(&store, &local.uri()).await.is_some(), "{backend:?}");
            assert!(
                !store
                    .holds_network_records(&did, None)
                    .await
                    .expect("holds")
            );
            store.delete(&local.uri()).await.expect("clean up");
        }
    }

    #[tokio::test]
    async fn an_index_failure_is_logged_as_an_event() {
        for (pool, backend) in test_pools().await {
            let store = RecordStore::new(pool.clone(), backend);
            let uri = format!("at://{}/app.test.store/x", unique_did());
            store.report_index_failure(&uri, &"disk full").await;
            let (count,): (i64,) = crate::db::query_as(&adapt_sql(
                "SELECT COUNT(*) FROM happyview_event_logs WHERE event_type = 'record.index_failed' AND subject = ?",
                backend,
            ))
            .bind(&uri)
            .fetch_one(&pool)
            .await
            .expect("count events");
            assert_eq!(count, 1, "{backend:?}");
        }
    }

    #[tokio::test]
    async fn a_local_write_that_cannot_reach_its_table_names_the_row() {
        let pool = crate::test_support::memory_pool().await;
        let store = RecordStore::new(pool, DatabaseBackend::Sqlite);
        let err = store
            .write_local(
                WriteKind::Echoed,
                &prepared("did:plc:a", "a", "{}", "c", None),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, LocalWriteError::Row(_)), "{err}");
    }

    #[tokio::test]
    async fn an_upsert_batch_skips_what_a_tombstone_supersedes() {
        for (pool, backend) in test_pools().await {
            let store = RecordStore::new(pool.clone(), backend);
            let did = unique_did();
            let gone_did = unique_did();
            let deleted = prepared(&did, "deleted", r#"{"n":1}"#, "c1", Some(&tid(10)));
            let recreated = prepared(&did, "recreated", r#"{"n":1}"#, "c1", Some(&tid(30)));
            let unversioned = prepared(&did, "unversioned", r#"{"n":1}"#, "c1", None);
            let of_gone_did = prepared(&gone_did, "a", r#"{"n":1}"#, "c1", Some(&tid(10)));

            let mut tx = crate::db::begin_write(&pool, backend).await.expect("begin");
            for rec in [&deleted, &recreated, &unversioned] {
                crate::tombstones::write_record_tombstone(
                    &mut tx,
                    backend,
                    &rec.uri(),
                    &tid(20),
                    "2026-01-01T00:00:00+00:00",
                )
                .await
                .expect("tombstone");
            }
            crate::tombstones::write_did_tombstone(
                &mut tx,
                backend,
                &gone_did,
                &tid(20),
                None,
                "2026-01-01T00:00:00+00:00",
            )
            .await
            .expect("did tombstone");
            tx.commit().await.expect("commit");

            let written = upsert_network(
                &store,
                &[
                    deleted.clone(),
                    recreated.clone(),
                    unversioned.clone(),
                    of_gone_did.clone(),
                ],
            )
            .await;
            assert_eq!(
                written,
                vec![Changed {
                    uri: recreated.uri()
                }],
                "{backend:?}"
            );
            for rec in [&deleted, &unversioned, &of_gone_did] {
                assert!(
                    row(&store, &rec.uri()).await.is_none(),
                    "{backend:?}: {}",
                    rec.uri()
                );
            }
            store.delete(&recreated.uri()).await.expect("clean up");
        }
    }

    #[tokio::test]
    async fn an_upsert_batch_never_replaces_a_newer_rev_and_keeps_the_stored_rev_without_one() {
        for (pool, backend) in test_pools().await {
            let store = RecordStore::new(pool.clone(), backend);
            let did = unique_did();
            let newer = prepared(&did, "a", r#"{"n":2}"#, "c2", Some(&tid(20)));
            assert_eq!(
                upsert_network(&store, std::slice::from_ref(&newer))
                    .await
                    .len(),
                1
            );

            let older = prepared(&did, "a", r#"{"n":1}"#, "c1", Some(&tid(10)));
            assert!(
                upsert_network(&store, &[older]).await.is_empty(),
                "{backend:?}"
            );
            let (record, cid, _) = row(&store, &newer.uri()).await.expect("row");
            assert_eq!((record.as_str(), cid.as_str()), (r#"{"n":2}"#, "c2"));

            let crawled = prepared(&did, "a", r#"{"n":3}"#, "c3", None);
            assert_eq!(
                upsert_network(&store, &[crawled]).await.len(),
                1,
                "{backend:?}"
            );
            let (record, _, _) = row(&store, &newer.uri()).await.expect("row");
            assert_eq!(record, r#"{"n":3}"#);
            assert_eq!(
                stored_rev(&store, &newer.uri()).await,
                Some(tid(20)),
                "{backend:?}"
            );
            store.delete(&newer.uri()).await.expect("clean up");
        }
    }

    #[tokio::test]
    async fn an_upsert_batch_writes_refs_for_changed_rows_and_rolls_back_with_its_caller() {
        for (pool, backend) in test_pools().await {
            let store = RecordStore::new(pool.clone(), backend);
            let did = unique_did();
            let linked = prepared(
                &did,
                "a",
                r#"{"subject":"at://did:plc:x/app.test.store/b"}"#,
                "c1",
                None,
            );
            assert_eq!(
                upsert_network(&store, std::slice::from_ref(&linked))
                    .await
                    .len(),
                1
            );
            assert_eq!(ref_count(&store, &linked.uri()).await, 1, "{backend:?}");

            let unlinked = prepared(&did, "a", r#"{"n":1}"#, "c2", None);
            let mut tx = crate::db::begin_write(&pool, backend).await.expect("begin");
            let written = store
                .upsert_batch(&mut tx, WriteKind::Network, &[unlinked])
                .await
                .expect("upsert");
            assert_eq!(written.len(), 1);
            tx.rollback().await.expect("rollback");
            let (record, cid, _) = row(&store, &linked.uri()).await.expect("row");
            assert_eq!(
                (cid.as_str(), ref_count(&store, &linked.uri()).await),
                ("c1", 1),
                "{backend:?}: {record}"
            );

            let mut tx = crate::db::begin_write(&pool, backend).await.expect("begin");
            let echoed = prepared(&did, "a", r#"{"n":2}"#, "c3", None);
            store
                .upsert_batch(&mut tx, WriteKind::Echoed, &[echoed])
                .await
                .expect("echoed upsert");
            tx.commit().await.expect("commit");
            let (_, cid, indexed_at) = row(&store, &linked.uri()).await.expect("row");
            assert_eq!((cid.as_str(), indexed_at), ("c3", None), "{backend:?}");
            assert_eq!(ref_count(&store, &linked.uri()).await, 0, "{backend:?}");
            store.delete(&linked.uri()).await.expect("clean up");
        }
    }

    #[tokio::test]
    async fn an_older_network_delete_leaves_the_newer_row_and_its_refs() {
        for (pool, backend) in test_pools().await {
            let store = RecordStore::new(pool.clone(), backend);
            let did = unique_did();
            let rec = prepared(
                &did,
                "a",
                r#"{"subject":"at://did:plc:x/app.test.store/b"}"#,
                "c1",
                Some(&tid(20)),
            );
            store
                .write_network(&pool, std::slice::from_ref(&rec))
                .await
                .expect("write");
            let now = crate::db::now_rfc3339();
            store
                .delete_network(&pool, &rec.uri(), &did, NSID, Some(&tid(10)), &now, false)
                .await
                .expect("older delete");
            assert!(row(&store, &rec.uri()).await.is_some(), "{backend:?}");
            assert_eq!(ref_count(&store, &rec.uri()).await, 1, "{backend:?}");

            store
                .delete_network(&pool, &rec.uri(), &did, NSID, Some(&tid(30)), &now, false)
                .await
                .expect("newer delete");
            assert!(row(&store, &rec.uri()).await.is_none(), "{backend:?}");
            assert_eq!(ref_count(&store, &rec.uri()).await, 0, "{backend:?}");
            crate::db::query(&adapt_sql(
                "DELETE FROM happyview_record_tombstones WHERE uri = ?",
                backend,
            ))
            .bind(rec.uri())
            .execute(&pool)
            .await
            .expect("clean up");
        }
    }
    /// A row an unlocked delete removes between the purge's select and its
    /// delete still counts as purged: the batch was full, so the caller's
    /// loop must go on. Postgres only; SQLite's `BEGIN IMMEDIATE` serialises
    /// the two.
    #[tokio::test]
    async fn a_purge_batch_counts_a_row_a_concurrent_delete_took() {
        for (pool, backend) in test_pools().await {
            if backend != DatabaseBackend::Postgres {
                continue;
            }
            let store = RecordStore::new(pool.clone(), backend);
            let did = unique_did();
            let taken = prepared(&did, "taken", r#"{"n":1}"#, "c1", None);
            let kept = prepared(&did, "kept", r#"{"n":2}"#, "c2", None);
            store
                .write_network(&pool, &[taken.clone(), kept.clone()])
                .await
                .expect("write");

            // An uncommitted delete holds `taken`'s row lock: the purge's
            // select still sees the row, and its delete waits on the lock.
            let mut other = pool.begin().await.expect("begin");
            crate::db::query(&adapt_sql(
                "DELETE FROM happyview_records WHERE uri = ?",
                backend,
            ))
            .bind(taken.uri())
            .execute(&mut *other)
            .await
            .expect("concurrent delete");

            let purge = tokio::spawn({
                let (store, did) = (store.clone(), did.clone());
                async move {
                    let mut tx = crate::db::begin_write(store.db(), store.backend()).await?;
                    let purged = store.purge_network_batch(&mut tx, &did, None, 2).await?;
                    tx.commit().await?;
                    Ok::<_, sqlx::Error>(purged)
                }
            });
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            assert!(
                !purge.is_finished(),
                "the purge's delete waits on the row lock"
            );
            other.commit().await.expect("commit the concurrent delete");

            let purged = purge.await.expect("join").expect("purge");
            assert_eq!(purged, 2, "both selected rows count, though one was gone");
            assert!(
                !store
                    .holds_network_records(&did, None)
                    .await
                    .expect("holds")
            );
        }
    }

    #[tokio::test]
    async fn a_network_write_is_refused_by_write_local() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let store = RecordStore::new(pool, DatabaseBackend::Sqlite);
        let rec = prepared(&unique_did(), "a", "{}", "c", None);
        let err = store
            .write_local(WriteKind::Network, &rec)
            .await
            .unwrap_err();
        assert!(matches!(err, LocalWriteError::Row(_)), "{err}");
        assert!(row(&store, &rec.uri()).await.is_none());
    }

    #[tokio::test]
    async fn get_and_list_recent_read_what_was_written() {
        for (pool, backend) in test_pools().await {
            let store = RecordStore::new(pool.clone(), backend);
            let did = unique_did();
            let collection = format!("app.test.read{}", uuid::Uuid::new_v4().simple());
            let mut first = prepared(&did, "a", r#"{"via":"witchsky","n":1}"#, "c1", None);
            first.collection = collection.clone();
            let mut second = prepared(&did, "b", r#"{"n":2}"#, "c2", None);
            second.collection = collection.clone();
            store
                .write_network(&pool, std::slice::from_ref(&first))
                .await
                .expect("first");
            store
                .write_network(&pool, std::slice::from_ref(&second))
                .await
                .expect("second");

            let got = store
                .get(&first.uri())
                .await
                .expect("get")
                .expect("present");
            assert_eq!(
                (
                    got.did.as_str(),
                    got.collection.as_str(),
                    got.rkey.as_str(),
                    got.record.as_str(),
                    got.cid.as_deref()
                ),
                (
                    did.as_str(),
                    collection.as_str(),
                    "a",
                    r#"{"via":"witchsky","n":1}"#,
                    Some("c1")
                ),
                "{backend:?}"
            );
            assert_eq!(
                store
                    .stored_cid(&second.uri())
                    .await
                    .expect("cid")
                    .as_deref(),
                Some("c2")
            );
            assert!(
                store
                    .get(&format!("at://{did}/{collection}/missing"))
                    .await
                    .expect("get")
                    .is_none()
            );

            let listed = store
                .list_recent(&collection, None, 10, 0)
                .await
                .expect("list");
            assert_eq!(listed.len(), 2, "{backend:?}");
            let by_did = store
                .list_recent(&collection, Some(&did), 1, 1)
                .await
                .expect("list by did");
            assert_eq!(by_did.len(), 1);

            let counts = store.count_by_collection().await.expect("counts");
            assert!(
                counts.contains(&(collection.clone(), 2)),
                "{backend:?}: {counts:?}"
            );
            assert!(store.count_all().await.expect("count") >= 2);
            store.delete(&first.uri()).await.expect("clean up");
            store.delete(&second.uri()).await.expect("clean up");
        }
    }

    /// A storage claiming one collection, read on `route`, that answers
    /// `count` with `counted`.
    struct CountingStorage {
        collection: String,
        route: ReadRoute,
        counted: Option<i64>,
    }

    #[async_trait::async_trait]
    impl RecordStorage for CountingStorage {
        fn claims(&self, collection: &str) -> bool {
            collection == self.collection
        }

        fn read_route(&self, _collection: &str) -> ReadRoute {
            self.route.clone()
        }

        async fn apply_delta(
            &self,
            _key: &RecordKey,
            _delta: Delta,
            _for_rev: Option<&str>,
        ) -> Result<(), sqlx::Error> {
            Ok(())
        }

        async fn forget_account(
            &self,
            _did: &str,
            _collections: Option<&[String]>,
        ) -> Result<u64, sqlx::Error> {
            Ok(0)
        }

        async fn delete_collection_batch(
            &self,
            _collection: &str,
            _limit: i64,
        ) -> Result<u64, sqlx::Error> {
            Ok(0)
        }

        async fn is_live(&self, _uri: &str) -> Result<bool, sqlx::Error> {
            Ok(false)
        }

        async fn count(&self, _collection: &str) -> Result<Option<i64>, sqlx::Error> {
            Ok(self.counted)
        }

        fn claimed(&self) -> Vec<String> {
            vec![self.collection.clone()]
        }
    }

    /// A claimed collection's count is the storage's own when it gives one,
    /// and the read route is not queried for it (here a closed pool, which
    /// would fail); `None` counts on the route as before.
    #[tokio::test]
    async fn a_storage_count_replaces_the_scan() {
        for (pool, backend) in test_pools().await {
            let did = unique_did();
            let collection = format!("app.test.counted{}", uuid::Uuid::new_v4().simple());
            let records: Vec<PreparedRecord> = ["a", "b"]
                .into_iter()
                .map(|rkey| {
                    let mut record = prepared(&did, rkey, r#"{"n":1}"#, "c", None);
                    record.collection = collection.clone();
                    record
                })
                .collect();
            let held = RecordStore::new(pool.clone(), backend);
            held.write_network(&pool, &records).await.expect("write");

            let closed = crate::test_support::memory_pool().await;
            closed.close().await;
            for (route, counted, expected) in [
                (
                    ReadRoute {
                        pool: closed,
                        backend: DatabaseBackend::Sqlite,
                    },
                    Some(7),
                    7,
                ),
                (
                    ReadRoute {
                        pool: pool.clone(),
                        backend,
                    },
                    None,
                    2,
                ),
            ] {
                // An instance holding nothing locally, so its totals are the
                // claimed collection's alone.
                let store = RecordStore::new(
                    crate::test_support::migrated_memory_pool().await,
                    DatabaseBackend::Sqlite,
                )
                .with_external(Arc::new(CountingStorage {
                    collection: collection.clone(),
                    route,
                    counted,
                }));
                assert_eq!(
                    store.count_collection(&collection).await.expect("count"),
                    expected,
                    "{backend:?} {counted:?}"
                );
                assert_eq!(
                    store.count_by_collection().await.expect("counts"),
                    vec![(collection.clone(), expected)],
                    "{backend:?} {counted:?}"
                );
                assert_eq!(
                    store.count_all().await.expect("total"),
                    expected,
                    "{backend:?} {counted:?}"
                );
            }
            for record in &records {
                held.delete(&record.uri()).await.expect("clean up");
            }
        }
    }

    #[tokio::test]
    async fn the_orphaned_label_sweep_keeps_account_labels_and_labels_of_held_records() {
        for (pool, backend) in test_pools().await {
            let store = RecordStore::new(pool.clone(), backend);
            let did = unique_did();
            let held = prepared(&did, "held", "{}", "c", None);
            store
                .write_network(&pool, std::slice::from_ref(&held))
                .await
                .expect("write");
            let gone = format!("at://{did}/app.test.store/gone");
            let insert = adapt_sql(
                "INSERT INTO happyview_labels (src, uri, val, cts) VALUES ('did:plc:labeler', ?, 'spam', '2026-01-01T00:00:00Z')",
                backend,
            );
            for subject in [held.uri(), gone.clone(), did.clone()] {
                crate::db::query(&insert)
                    .bind(subject)
                    .execute(&pool)
                    .await
                    .expect("label");
            }
            store.delete_orphaned_labels().await.expect("sweep");
            let left: Vec<(String,)> = crate::db::query_as(&adapt_sql(
                "SELECT uri FROM happyview_labels WHERE uri IN (?, ?, ?) ORDER BY uri",
                backend,
            ))
            .bind(held.uri())
            .bind(&gone)
            .bind(&did)
            .fetch_all(&pool)
            .await
            .expect("labels left");
            let mut expected = vec![held.uri(), did.clone()];
            expected.sort();
            assert_eq!(
                left.into_iter().map(|(u,)| u).collect::<Vec<_>>(),
                expected,
                "{backend:?}"
            );
            crate::db::query(&adapt_sql(
                "DELETE FROM happyview_labels WHERE uri IN (?, ?)",
                backend,
            ))
            .bind(held.uri())
            .bind(&did)
            .execute(&pool)
            .await
            .expect("clean up labels");
            store.delete(&held.uri()).await.expect("clean up");
        }
    }

    #[tokio::test]
    async fn the_xrpc_listing_walks_the_collection_indexed_at_index() {
        for (pool, backend) in crate::test_support::test_pools().await {
            for by_did in [false, true] {
                let literals: &[&str] = if by_did {
                    &["'app.test.post'", "'did:plc:a'", "50", "0"]
                } else {
                    &["'app.test.post'", "50", "0"]
                };
                let sql =
                    crate::test_support::inline_binds(&list_recent_sql(backend, by_did), literals);
                let plan = crate::test_support::query_plan(&pool, backend, &sql).await;
                crate::test_support::assert_index_without_sort(
                    &plan,
                    "idx_records_v2_collection_indexed_at",
                    backend,
                );
            }
        }
    }

    /// A lookup by URI seeks the identity index by interned id; it never
    /// filters the derived `uri`, which would scan every record. Seeded with
    /// many accounts and analyzed, so the planner weighs a real choice.
    #[tokio::test]
    async fn a_get_by_uri_seeks_the_identity_index() {
        for db in crate::test_support::pre_upgrade_pools().await {
            let (pool, backend) = (&db.pool, db.backend);
            upgrade::run(pool, backend).await.expect("upgrade");
            let store = RecordStore::new(pool.clone(), backend);
            let records: Vec<PreparedRecord> = (0..2000)
                .map(|i| PreparedRecord {
                    did: format!("did:plc:plan{}", i % 400),
                    collection: format!("app.test.plan{}", i % 2),
                    rkey: format!("r{i}"),
                    record_json: "{}".to_string(),
                    cid: "c".to_string(),
                    rev: None,
                })
                .collect();
            store.write_network(pool, &records).await.expect("seed");
            crate::db::query("ANALYZE")
                .execute(pool)
                .await
                .expect("analyze");
            let sql = crate::test_support::inline_binds(
                &RecordStore::get_sql(backend),
                &["'did:plc:plan7'", "'app.test.plan1'", "'r7'"],
            );
            let plan = crate::test_support::query_plan(pool, backend, &sql).await;
            assert!(
                plan.iter()
                    .any(|line| line.contains("idx_records_v2_identity")),
                "{backend:?}: {plan:#?}"
            );
            assert!(
                !plan.iter().any(|line| line.contains("SCAN r")
                    || line.contains("Seq Scan on happyview_records_v2")),
                "{backend:?}: {plan:#?}"
            );
            db.finish().await;
        }
    }

    #[tokio::test]
    async fn a_rewrite_replaces_refs_across_insert_chunks() {
        for (pool, backend) in test_pools().await {
            let store = RecordStore::new(pool.clone(), backend);
            let did = unique_did();
            let targets: Vec<String> = (0..(crate::record_refs::REFS_PER_INSERT + 5))
                .map(|i| format!("at://did:plc:t/app.test.post/{i}"))
                .collect();
            let body = serde_json::json!({ "refs": targets }).to_string();
            let mut rec = prepared(&did, "many", &body, "c1", None);
            store
                .write_network(&pool, std::slice::from_ref(&rec))
                .await
                .expect("write");
            assert_eq!(
                ref_count(&store, &rec.uri()).await as usize,
                crate::record_refs::REFS_PER_INSERT + 5,
                "{backend:?}: every ref lands across insert chunks"
            );

            rec.record_json = "{}".to_string();
            rec.cid = "c2".to_string();
            store
                .write_network(&pool, std::slice::from_ref(&rec))
                .await
                .expect("rewrite");
            assert_eq!(ref_count(&store, &rec.uri()).await, 0, "{backend:?}");
            store.delete(&rec.uri()).await.expect("clean up");
        }
    }

    async fn stored(
        store: &RecordStore,
        did: &str,
        rkey: &str,
    ) -> (String, String, Option<String>, Option<String>) {
        crate::db::query_as(&adapt_sql(
            "SELECT record, cid, rev, indexed_at FROM happyview_records WHERE did = ? AND collection = ? AND rkey = ?",
            store.backend(),
        ))
        .bind(did)
        .bind(NSID)
        .bind(rkey)
        .fetch_one(store.db())
        .await
        .expect("read through the view")
    }

    /// Moved from `db.rs`: an older delivery never replaces a newer rev, and
    /// a write without a rev applies but keeps the stored one.
    #[tokio::test]
    async fn the_upsert_never_replaces_a_newer_rev() {
        for (pool, backend) in test_pools().await {
            let store = RecordStore::new(pool.clone(), backend);
            let did = unique_did();
            for (cid, rev, expected) in [
                ("c50", Some(tid(50)), ("c50", Some(tid(50)))),
                ("c40", Some(tid(40)), ("c50", Some(tid(50)))),
                ("c60", Some(tid(60)), ("c60", Some(tid(60)))),
                // A write without a rev applies but keeps the stored rev, so
                // an older delivery after it is still refused.
                ("c-none", None, ("c-none", Some(tid(60)))),
                ("c10", Some(tid(10)), ("c-none", Some(tid(60)))),
                ("c70", Some(tid(70)), ("c70", Some(tid(70)))),
            ] {
                let rec = prepared(&did, "r", "{}", cid, rev.as_deref());
                store.write_network(&pool, &[rec]).await.expect("upsert");
                let (_, got_cid, got_rev, _) = stored(&store, &did, "r").await;
                assert_eq!(
                    (got_cid.as_str(), got_rev),
                    (expected.0, expected.1),
                    "after {cid} ({backend:?})"
                );
            }
            store
                .delete(&format!("at://{did}/{NSID}/r"))
                .await
                .expect("clean up");
        }
    }

    /// A body a hook changed under the same CID is written; an identical
    /// redelivery of a row since recompressed (different stored bytes, same
    /// decoded body) is not.
    #[tokio::test]
    async fn a_changed_body_under_the_same_cid_updates_and_a_recompressed_redelivery_does_not() {
        for (pool, backend) in test_pools().await {
            let store = RecordStore::with_bodies(
                pool.clone(),
                backend,
                compression::BodyCodec::new(compression::Compression::Zstd),
            );
            let did = unique_did();
            let collection = format!("app.test.hook{}", uuid::Uuid::new_v4().simple());
            let bulk: Vec<PreparedRecord> = compression::sample_posts(1000)
                .into_iter()
                .enumerate()
                .map(|(i, body)| PreparedRecord {
                    did: did.clone(),
                    collection: collection.clone(),
                    rkey: format!("bulk{i}"),
                    record_json: body,
                    cid: format!("cb{i}"),
                    rev: None,
                })
                .collect();
            store.write_network(&pool, &bulk).await.expect("bulk write");

            let mut rec = bulk[0].clone();
            rec.record_json = r#"{"text":"replaced by a hook","via":"witchsky"}"#.to_string();
            assert_eq!(
                store
                    .write_network(&pool, std::slice::from_ref(&rec))
                    .await
                    .expect("changed body")
                    .len(),
                1,
                "{backend:?}"
            );

            if backend == DatabaseBackend::Sqlite {
                let (collection_id,): (i64,) =
                    crate::db::query_as("SELECT id FROM happyview_collections WHERE nsid = ?")
                        .bind(&collection)
                        .fetch_one(&pool)
                        .await
                        .expect("collection id");
                compression::train(&pool, store.bodies(), collection_id, 4096)
                    .await
                    .expect("train");
                while compression::recompress_batch(
                    &pool,
                    store.bodies(),
                    collection_id,
                    500,
                    compression::RECOMPRESS_BUDGET_BYTES,
                )
                .await
                .expect("recompress")
                .more
                {}
                let (dict,): (Option<i64>,) = crate::db::query_as(&format!(
                    "SELECT body_dict FROM happyview_records_v2 WHERE collection_id = {collection_id} AND rkey = 'bulk1'"
                ))
                .fetch_one(&pool)
                .await
                .expect("body_dict");
                assert!(
                    dict.is_some_and(|d| d > 0),
                    "the row was recompressed: {dict:?}"
                );
            }
            for again in [&rec, &bulk[1]] {
                let before = store
                    .get(&again.uri())
                    .await
                    .expect("get")
                    .expect("present");
                assert!(
                    store
                        .write_network(&pool, std::slice::from_ref(again))
                        .await
                        .expect("identical redelivery")
                        .is_empty(),
                    "{backend:?} {}",
                    again.rkey
                );
                let after = store
                    .get(&again.uri())
                    .await
                    .expect("get")
                    .expect("present");
                assert_eq!(
                    (after.record, after.indexed_at),
                    (before.record, before.indexed_at),
                    "{backend:?}"
                );
            }
            while store
                .delete_collection_batch(&collection, 500)
                .await
                .expect("clean up")
                > 0
            {}
        }
    }

    /// Record targets and odd targets (a bare DID, a handle URI with a path,
    /// a trailing slash) are kept exactly, and backlinks find the record one.
    #[tokio::test]
    async fn backlinks_find_record_targets_and_keep_odd_targets() {
        use happyview_plugin_sdk::wire::BacklinksQuery;
        for (pool, backend) in test_pools().await {
            let store = RecordStore::new(pool.clone(), backend);
            let did = unique_did();
            let target = format!("at://{}/app.test.target/t", unique_did());
            let body = format!(
                r#"{{"subject":"{target}","mentions":["at://did:plc:odd","at://alice.example.com/a/b/c","at://did:plc:x/"]}}"#
            );
            let rec = prepared(&did, "src", &body, "c", None);
            store
                .write_network(&pool, std::slice::from_ref(&rec))
                .await
                .expect("write");

            let page = query::backlinks_query(
                &pool,
                backend,
                BacklinksQuery {
                    uri: target.clone(),
                    collection: NSID.to_string(),
                    did: None,
                    limit: Some(10),
                    cursor: None,
                },
            )
            .await
            .expect("backlinks");
            assert_eq!(page.records.len(), 1, "{backend:?}");
            let odd = query::backlinks_query(
                &pool,
                backend,
                BacklinksQuery {
                    uri: "at://did:plc:odd".to_string(),
                    collection: NSID.to_string(),
                    did: Some(did.clone()),
                    limit: Some(10),
                    cursor: None,
                },
            )
            .await
            .expect("backlinks to an odd target");
            assert_eq!(odd.records.len(), 1, "{backend:?}");

            let mut targets: Vec<String> = crate::db::query_as::<(String,)>(&adapt_sql(
                "SELECT target_uri FROM happyview_record_refs WHERE source_uri = ?",
                backend,
            ))
            .bind(rec.uri())
            .fetch_all(&pool)
            .await
            .expect("refs")
            .into_iter()
            .map(|(t,)| t)
            .collect();
            targets.sort();
            let mut expected = vec![
                target.clone(),
                "at://did:plc:odd".to_string(),
                "at://alice.example.com/a/b/c".to_string(),
                "at://did:plc:x/".to_string(),
            ];
            expected.sort();
            assert_eq!(targets, expected, "{backend:?}");
            store.delete(&rec.uri()).await.expect("clean up");
        }
    }

    #[tokio::test]
    async fn the_live_views_keep_the_raw_sql_contract() {
        for (pool, backend) in test_pools().await {
            let store = RecordStore::new(pool.clone(), backend);
            let did = unique_did();
            let rec = prepared(&did, "via", r#"{"text":"hi","via":"witchsky"}"#, "c", None);
            store
                .write_network(&pool, std::slice::from_ref(&rec))
                .await
                .expect("write");
            let (filter, placeholder) = match backend {
                DatabaseBackend::Sqlite => ("json_extract(record, '$.via')", "?"),
                DatabaseBackend::Postgres => ("record::jsonb->>'via'", "$1"),
            };
            let rows = crate::plugin::host::run_query(
                &pool,
                &format!(
                    "SELECT uri, record FROM happyview_records WHERE did = {placeholder} AND {filter} = 'witchsky'"
                ),
                &[serde_json::json!(did)],
            )
            .await
            .expect("raw SQL through the guard");
            assert_eq!(rows.len(), 1, "{backend:?}");
            assert_eq!(rows[0]["record"], serde_json::json!(rec.record_json));
            store.delete(&rec.uri()).await.expect("clean up");
        }
    }

    /// An id interned in a transaction that rolled back is never cached:
    /// SQLite hands a freed id to the next name inserted, and a cached stale
    /// id would file one account's records under another.
    #[tokio::test]
    async fn a_rolled_back_intern_never_reaches_the_cache() {
        for (pool, backend) in test_pools().await {
            let store = RecordStore::new(pool.clone(), backend);
            let (rolled_back, other) = (unique_did(), unique_did());
            let mut tx = crate::db::begin_write(&pool, backend).await.expect("begin");
            store
                .upsert_batch(
                    &mut tx,
                    WriteKind::Network,
                    &[prepared(&rolled_back, "a", "{}", "c", None)],
                )
                .await
                .expect("upsert");
            tx.rollback().await.expect("rollback");
            assert_eq!(
                store.interner().cached_did(&rolled_back),
                None,
                "{backend:?}"
            );

            // On SQLite this takes the id the rollback freed.
            let theirs = prepared(&other, "b", r#"{"who":"other"}"#, "c", None);
            store
                .write_network(&pool, std::slice::from_ref(&theirs))
                .await
                .expect("other write");
            assert!(store.interner().cached_did(&other).is_some(), "{backend:?}");
            let mine = prepared(&rolled_back, "a", r#"{"who":"mine"}"#, "c", None);
            store
                .write_network(&pool, std::slice::from_ref(&mine))
                .await
                .expect("retried write");
            for rec in [&theirs, &mine] {
                let got = store.get(&rec.uri()).await.expect("get").expect("present");
                assert_eq!(
                    (got.did, got.record),
                    (rec.did.clone(), rec.record_json.clone()),
                    "{backend:?}"
                );
                store.delete(&rec.uri()).await.expect("clean up");
            }
        }
    }

    struct Xorshift(u64);

    impl Xorshift {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
            items[(self.next() % items.len() as u64) as usize]
        }
    }

    /// JSON text as a PDS or a Lua hook might hand it over: arbitrary key
    /// order, repeated keys, whitespace, escapes, non-ASCII, big numbers.
    fn random_json(rng: &mut Xorshift, depth: u32) -> String {
        let ws = |rng: &mut Xorshift| rng.pick(&["", " ", "\n  ", "\t"]).to_string();
        match rng.next() % 6 {
            0 | 1 if depth > 0 => {
                let n = rng.next() % 5;
                let fields: Vec<String> = (0..n)
                    .map(|_| {
                        let key =
                            rng.pick(&["text", "via", "a", "z", "é", "$type", "createdAt", "text"]);
                        format!(
                            "{}\"{key}\"{}:{}{}",
                            ws(rng),
                            ws(rng),
                            ws(rng),
                            random_json(rng, depth - 1)
                        )
                    })
                    .collect();
                format!("{{{}}}", fields.join(","))
            }
            2 if depth > 0 => {
                let n = rng.next() % 4;
                let items: Vec<String> = (0..n).map(|_| random_json(rng, depth - 1)).collect();
                format!("[{}{}]", items.join(","), ws(rng))
            }
            3 => rng
                .pick(&["0", "-0.5", "1e3", "12345678901234567890", "3.14159"])
                .to_string(),
            4 => rng.pick(&["true", "false", "null"]).to_string(),
            _ => format!(
                "\"{}\"",
                rng.pick(&[
                    "hi",
                    "\\\"quoted\\\"",
                    "line\\nbreak",
                    "\\u00e9 é 日本 🦋",
                    "tab\\t",
                    "at://did:plc:x/app.test.store/y",
                    ""
                ])
            ),
        }
    }

    /// Arbitrary bodies come back byte-identical through the store and the
    /// `happyview_records` view, with their CIDs, whether SQLite holds them
    /// raw, compressed by the recompression pass, or compressed on write.
    #[tokio::test]
    async fn arbitrary_bodies_round_trip_byte_for_byte_compressed_and_raw() {
        for (pool, backend) in test_pools().await {
            let codec = compression::BodyCodec::new(compression::Compression::Zstd);
            let store = RecordStore::with_bodies(pool.clone(), backend, codec);
            let did = unique_did();
            let collection = format!("app.test.rt{}", uuid::Uuid::new_v4().simple());
            let mut rng = Xorshift(0xfeed_beef_1234_5678);
            let fixed = [
                "{}",
                r#"{"text":"é 日本 🦋 \u00e9 \"q\" \\ \n","z":1,"a":2}"#,
                r#"{ "b" : 1 , "a" : [ ] , "b" : 2 }"#,
            ];
            let bodies: Vec<String> = (0..400)
                .map(|i| match i {
                    0..=2 => fixed[i].to_string(),
                    _ if i % 3 == 0 => {
                        compression::sample_posts(1)[0].replace("post 0", &format!("post {i}"))
                    }
                    _ => format!("{{\"n\":{i},\"body\":{}}}", random_json(&mut rng, 4)),
                })
                .collect();
            let cids = [
                "bafyreie5737gdxlw5i64vzichcalba3z2v5n6icifvx5xytvske7mr3hpm",
                "",
                "c50",
                "QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbdG",
            ];
            let records: Vec<PreparedRecord> = bodies
                .iter()
                .enumerate()
                .map(|(i, body)| PreparedRecord {
                    did: did.clone(),
                    collection: collection.clone(),
                    rkey: format!("r{i:04}"),
                    record_json: body.clone(),
                    cid: cids[i % cids.len()].to_string(),
                    rev: None,
                })
                .collect();
            // 0..130 are written before a dictionary exists, then compressed
            // by the recompression pass; 130..270 are compressed on write;
            // 270.. go through a store without a codec and stay raw.
            store
                .write_network(&pool, &records[..130])
                .await
                .expect("first segment");
            if backend == DatabaseBackend::Sqlite {
                let (collection_id,): (i64,) =
                    crate::db::query_as("SELECT id FROM happyview_collections WHERE nsid = ?")
                        .bind(&collection)
                        .fetch_one(&pool)
                        .await
                        .expect("collection id");
                compression::train(&pool, store.bodies(), collection_id, 4096)
                    .await
                    .expect("train");
                while compression::recompress_batch(
                    &pool,
                    store.bodies(),
                    collection_id,
                    500,
                    compression::RECOMPRESS_BUDGET_BYTES,
                )
                .await
                .expect("recompress")
                .more
                {}
            }
            for rec in &records[130..270] {
                store
                    .write_local(WriteKind::Echoed, rec)
                    .await
                    .expect("compressed on write");
            }
            let plain = RecordStore::new(pool.clone(), backend);
            for rec in &records[270..] {
                plain
                    .write_local(WriteKind::Echoed, rec)
                    .await
                    .expect("raw write");
            }
            for rec in &records {
                let got = store.get(&rec.uri()).await.expect("get").expect("present");
                assert_eq!(got.record, rec.record_json, "{backend:?} {}", rec.rkey);
                assert_eq!(
                    got.cid.as_deref(),
                    Some(rec.cid.as_str()),
                    "{backend:?} {}",
                    rec.rkey
                );
                let (raw, cid): (String, String) = crate::db::query_as(&adapt_sql(
                    "SELECT record, cid FROM happyview_records WHERE did = ? AND collection = ? AND rkey = ?",
                    backend,
                ))
                .bind(&did)
                .bind(&collection)
                .bind(&rec.rkey)
                .fetch_one(&pool)
                .await
                .expect("view read");
                assert_eq!(raw, rec.record_json, "{backend:?} {} view", rec.rkey);
                assert_eq!(cid, rec.cid, "{backend:?} {} view cid", rec.rkey);
            }
            if backend == DatabaseBackend::Sqlite {
                // Every compressible row is compressed: a row under
                // MIN_COMPRESS_BYTES, or one zstd could not shrink, is
                // `RAW_CONSIDERED` (0) with its stored bytes unchanged, and
                // any other row has a dictionary and a smaller body. Only the
                // codec-less segment is NULL.
                let rows: Vec<(String, Option<i64>, i64)> = crate::db::query_as(
                    "SELECT r.rkey, r.body_dict, length(r.body) FROM happyview_records_v2 r \
                     JOIN happyview_collections c ON c.id = r.collection_id WHERE c.nsid = ?",
                )
                .bind(&collection)
                .fetch_all(&pool)
                .await
                .expect("stored bodies");
                assert_eq!(rows.len(), records.len());
                let mut positive = [0usize; 2];
                for (rkey, dict, stored_len) in rows {
                    let i: usize = rkey[1..].parse().unwrap();
                    let raw_len = records[i].record_json.len() as i64;
                    if i >= 270 {
                        assert_eq!(dict, None, "{rkey} stays raw");
                        assert_eq!(stored_len, raw_len, "{rkey}");
                        continue;
                    }
                    match dict {
                        None => panic!("{rkey} was never considered for compression"),
                        Some(0) => {
                            assert_eq!(stored_len, raw_len, "{rkey} raw bytes");
                            if raw_len >= compression::MIN_COMPRESS_BYTES as i64 {
                                // zstd could not shrink it; the generated
                                // bodies are too varied to expect that of
                                // the structured posts.
                                assert!(
                                    !i.is_multiple_of(3) || i < 3,
                                    "{rkey} is a compressible post"
                                );
                            }
                        }
                        Some(id) => {
                            assert!(id > 0);
                            assert!(stored_len < raw_len, "{rkey} is smaller compressed");
                            positive[usize::from(i >= 130)] += 1;
                        }
                    }
                }
                assert!(positive.iter().all(|n| *n >= 40), "{positive:?}");
                // The view and `hv_record` agree for every row, compressed or not.
                let rows: Vec<(String, String)> = crate::db::query_as(
                    "SELECT hv_record(r.body, r.body_dict), v.record FROM happyview_records_v2 r \
                     JOIN happyview_collections c ON c.id = r.collection_id \
                     JOIN happyview_records v ON v.collection = c.nsid AND v.rkey = r.rkey \
                     WHERE c.nsid = ?",
                )
                .bind(&collection)
                .fetch_all(&pool)
                .await
                .expect("hv_record");
                assert_eq!(rows.len(), records.len());
                assert!(rows.iter().all(|(a, b)| a == b));
            }
            for rec in &records {
                store.delete(&rec.uri()).await.expect("clean up");
            }
        }
    }
}

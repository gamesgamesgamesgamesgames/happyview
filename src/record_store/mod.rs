//! The one owner of `happyview_records` and `happyview_record_refs`.
//!
//! Every write of a record, and of the refs it carries, goes through here:
//! Jetstream (a batch of one) and backfill pages through
//! [`RecordStore::upsert_batch`], and `index_put`, `mirror_network_write`,
//! XRPC procedures and linked repos through [`RecordStore::write_local`],
//! which shares its statement. The storage format can then change in one
//! place, and the two ingest paths cannot drift apart.

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use sqlx::{AnyConnection, AnyPool};

use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::event_log::{EventLog, Severity, log_event};
use crate::tombstones::WriteCandidate;

/// Rows per multi-row upsert: nine bound parameters each, well within either
/// backend's parameter limit, whatever size a PDS makes a page.
pub const RECORDS_PER_UPSERT: usize = 100;

/// URIs per `IN (…)` list when deleting by URI.
const DELETE_CHUNK: usize = 500;

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
    /// an unchanged, already stamped row alone (see `db::record_upsert_where`).
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

/// A handle on the record tables. Cheap to clone.
#[derive(Clone)]
pub struct RecordStore {
    db: AnyPool,
    backend: DatabaseBackend,
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

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(", ")
}

fn conflict_clause(backend: DatabaseBackend, kind: WriteKind) -> String {
    match kind {
        WriteKind::Network => format!(
            "ON CONFLICT (uri) DO UPDATE SET record = EXCLUDED.record, cid = EXCLUDED.cid, \
             indexed_at = EXCLUDED.indexed_at, rev = COALESCE(EXCLUDED.rev, happyview_records.rev) \
             WHERE {}",
            crate::db::record_upsert_where(backend)
        ),
        WriteKind::Echoed => "ON CONFLICT (uri) DO UPDATE SET record = EXCLUDED.record, \
                              cid = EXCLUDED.cid, indexed_at = NULL"
            .to_string(),
        WriteKind::IndexOnly => {
            "ON CONFLICT (uri) DO UPDATE SET record = EXCLUDED.record".to_string()
        }
    }
}

impl RecordStore {
    pub fn new(db: AnyPool, backend: DatabaseBackend) -> Self {
        Self { db, backend }
    }

    pub fn db(&self) -> &AnyPool {
        &self.db
    }

    pub fn backend(&self) -> DatabaseBackend {
        self.backend
    }

    /// Upsert `records` on the caller's connection, then replace the refs of
    /// exactly the rows inserted or changed. A URI repeated in `records` is
    /// written once, with its last copy. Returns the rows written.
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
                tracing::debug!(
                    skipped = blocked.len(),
                    "skipping records a tombstone supersedes"
                );
                records.retain(|rec| !blocked.contains(&rec.uri()));
            }
        }
        let mut written = Vec::new();
        for chunk in records.chunks(RECORDS_PER_UPSERT) {
            let rows = self.upsert_rows(conn, kind, chunk).await?;
            self.sync_refs_for(conn, chunk, &rows).await?;
            written.extend(rows.into_iter().map(|uri| Changed { uri }));
        }
        Ok(written)
    }

    /// One multi-row upsert per `RECORDS_PER_UPSERT` rows. Returns the URIs
    /// inserted or changed: the `Network` clause's `WHERE` leaves unchanged
    /// rows out, and they keep their refs and `indexed_at`.
    async fn upsert_rows(
        &self,
        conn: &mut AnyConnection,
        kind: WriteKind,
        records: &[&PreparedRecord],
    ) -> Result<Vec<String>, sqlx::Error> {
        let now = now_rfc3339();
        let indexed_at = match kind {
            WriteKind::Network => Some(now.as_str()),
            WriteKind::Echoed | WriteKind::IndexOnly => crate::db::NO_INDEXED_AT,
        };
        let mut written = Vec::new();
        for chunk in records.chunks(RECORDS_PER_UPSERT) {
            let values = vec!["(?, ?, ?, ?, ?, ?, ?, ?, ?)"; chunk.len()].join(", ");
            let sql = adapt_sql(
                &format!(
                    "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at, rev) \
                     VALUES {values} {} RETURNING uri",
                    conflict_clause(self.backend, kind)
                ),
                self.backend,
            );
            let mut upsert = crate::db::query_as::<(String,)>(&sql);
            for rec in chunk {
                let rev = match kind {
                    WriteKind::Network => rec.rev.as_deref(),
                    WriteKind::Echoed | WriteKind::IndexOnly => None,
                };
                upsert = upsert
                    .bind(rec.uri())
                    .bind(rec.did.as_str())
                    .bind(rec.collection.as_str())
                    .bind(rec.rkey.as_str())
                    .bind(rec.record_json.as_str())
                    .bind(rec.cid.as_str())
                    .bind(indexed_at)
                    .bind(now.as_str())
                    .bind(rev);
            }
            written.extend(
                upsert
                    .fetch_all(&mut *conn)
                    .await?
                    .into_iter()
                    .map(|(uri,)| uri),
            );
        }
        Ok(written)
    }

    /// Replace the refs of the `written` rows with those their bodies carry.
    async fn sync_refs_for(
        &self,
        conn: &mut AnyConnection,
        records: &[&PreparedRecord],
        written: &[String],
    ) -> Result<(), sqlx::Error> {
        if written.is_empty() {
            return Ok(());
        }
        for chunk in written.chunks(DELETE_CHUNK) {
            let sql = adapt_sql(
                &format!(
                    "DELETE FROM happyview_record_refs WHERE source_uri IN ({})",
                    placeholders(chunk.len())
                ),
                self.backend,
            );
            let mut delete = crate::db::query(&sql);
            for uri in chunk {
                delete = delete.bind(uri.as_str());
            }
            delete.execute(&mut *conn).await?;
        }

        let written: HashSet<&str> = written.iter().map(String::as_str).collect();
        let mut refs: Vec<(String, String, &str)> = Vec::new();
        for rec in records {
            let uri = rec.uri();
            if !written.contains(uri.as_str()) {
                continue;
            }
            let value: Value = serde_json::from_str(&rec.record_json).unwrap_or(Value::Null);
            for target in crate::record_refs::extract_at_uris(&value) {
                refs.push((uri.clone(), target, rec.collection.as_str()));
            }
        }
        for chunk in refs.chunks(crate::record_refs::REFS_PER_INSERT) {
            let sql = adapt_sql(
                &format!(
                    "INSERT INTO happyview_record_refs (source_uri, target_uri, collection) VALUES {} ON CONFLICT DO NOTHING",
                    vec!["(?, ?, ?)"; chunk.len()].join(", ")
                ),
                self.backend,
            );
            let mut insert = crate::db::query(&sql);
            for (source, target, collection) in chunk {
                insert = insert
                    .bind(source.as_str())
                    .bind(target.as_str())
                    .bind(*collection);
            }
            insert.execute(&mut *conn).await?;
        }
        Ok(())
    }

    /// [`Self::upsert_batch`] of network `records` in its own
    /// `db::begin_write` transaction on `pool` (backfill passes its own
    /// pool). Callers run it inside `db::retry_on_busy`.
    pub async fn write_network(
        &self,
        pool: &AnyPool,
        records: &[PreparedRecord],
    ) -> Result<Vec<Changed>, sqlx::Error> {
        if records.is_empty() {
            return Ok(Vec::new());
        }
        let mut tx = crate::db::begin_write(pool, self.backend).await?;
        let written = self
            .upsert_batch(&mut tx, WriteKind::Network, records)
            .await?;
        tx.commit().await?;
        Ok(written)
    }

    /// One local write (`Echoed` or `IndexOnly`): the row, then its refs, as
    /// two steps whose failures are told apart.
    pub async fn write_local(
        &self,
        kind: WriteKind,
        record: &PreparedRecord,
    ) -> Result<(), LocalWriteError> {
        debug_assert!(
            kind != WriteKind::Network,
            "network writes go through upsert_batch"
        );
        {
            let mut conn = self.db.acquire().await.map_err(LocalWriteError::Row)?;
            self.upsert_rows(&mut conn, kind, &[record])
                .await
                .map_err(LocalWriteError::Row)?;
        }
        let value: Value = serde_json::from_str(&record.record_json).unwrap_or(Value::Null);
        crate::record_refs::sync_refs(
            &self.db,
            &record.uri(),
            &record.collection,
            &value,
            self.backend,
        )
        .await
        .map_err(LocalWriteError::Refs)
    }

    /// Delete one record and its refs. Idempotent; says whether a row was
    /// there.
    pub async fn delete(&self, uri: &str) -> Result<bool, sqlx::Error> {
        let mut tx = crate::db::begin_write(&self.db, self.backend).await?;
        let deleted = self.delete_uris(&mut tx, &[uri.to_string()]).await? > 0;
        tx.commit().await?;
        Ok(deleted)
    }

    /// Delete `uris` and their refs on the caller's connection. Returns the
    /// records deleted.
    async fn delete_uris(
        &self,
        conn: &mut AnyConnection,
        uris: &[String],
    ) -> Result<u64, sqlx::Error> {
        let mut deleted = 0;
        for chunk in uris.chunks(DELETE_CHUNK) {
            for (table, column) in [
                ("happyview_record_refs", "source_uri"),
                ("happyview_records", "uri"),
            ] {
                let sql = adapt_sql(
                    &format!(
                        "DELETE FROM {table} WHERE {column} IN ({})",
                        placeholders(chunk.len())
                    ),
                    self.backend,
                );
                let mut delete = crate::db::query(&sql);
                for uri in chunk {
                    delete = delete.bind(uri.as_str());
                }
                let affected = delete.execute(&mut *conn).await?.rows_affected();
                if table == "happyview_records" {
                    deleted += affected;
                }
            }
        }
        Ok(deleted)
    }

    /// Delete one record. A delete that carries a rev deletes only a stored row
    /// whose rev is unknown or not newer, and, unless a newer row survives it,
    /// leaves a tombstone in the same transaction, so a reordered or replayed
    /// create cannot bring the record back. A delete that finds no row
    /// tombstones only when a create could still be on its way: the account
    /// holds network records in the collection, or a backfill still writing
    /// covers it.
    /// Otherwise it is a delete for a record this instance never held, and the
    /// network sends millions of those. A delete without a rev (a dead-letter
    /// retry) deletes the row whatever its rev and tombstones it at the rev it
    /// was stored at, if any, so a replay of that create cannot bring it back.
    /// The record's refs go with it; a surviving newer row keeps its own.
    pub async fn delete_network(
        &self,
        pool: &AnyPool,
        uri: &str,
        did: &str,
        collection: &str,
        rev: Option<&str>,
        now: &str,
    ) -> Result<(), sqlx::Error> {
        let backend = self.backend;
        let mut tx = crate::db::begin_write(pool, backend).await?;
        crate::tombstones::lock_dids(&mut tx, backend, &[did]).await?;
        crate::tombstones::lock_uris(&mut tx, backend, &[uri]).await?;
        match rev {
            Some(rev) => {
                let sql = adapt_sql(
                    &format!(
                        "DELETE FROM happyview_records WHERE uri = ? AND (rev IS NULL OR {})",
                        crate::db::rev_compare(backend, "rev", "<=", "CAST(? AS TEXT)")
                    ),
                    backend,
                );
                let deleted = crate::db::query(&sql)
                    .bind(uri)
                    .bind(rev)
                    .execute(&mut *tx)
                    .await?
                    .rows_affected()
                    > 0;
                let survived = !deleted && {
                    let sql = adapt_sql("SELECT 1 FROM happyview_records WHERE uri = ?", backend);
                    crate::db::query(&sql)
                        .bind(uri)
                        .fetch_optional(&mut *tx)
                        .await?
                        .is_some()
                };
                if !survived {
                    self.delete_refs(&mut tx, uri).await?;
                }
                let tombstone = deleted
                    || (!survived && self.create_may_follow(&mut tx, did, collection).await?);
                if tombstone {
                    crate::tombstones::write_record_tombstone(&mut tx, backend, uri, rev, now)
                        .await?;
                }
            }
            None => {
                let sql = adapt_sql(
                    "DELETE FROM happyview_records WHERE uri = ? RETURNING rev",
                    backend,
                );
                let deleted: Option<(Option<String>,)> = crate::db::query_as(&sql)
                    .bind(uri)
                    .fetch_optional(&mut *tx)
                    .await?;
                self.delete_refs(&mut tx, uri).await?;
                if let Some((Some(stored_rev),)) = deleted {
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
        tx.commit().await
    }

    async fn delete_refs(&self, conn: &mut AnyConnection, uri: &str) -> Result<(), sqlx::Error> {
        crate::db::query(&adapt_sql(
            "DELETE FROM happyview_record_refs WHERE source_uri = ?",
            self.backend,
        ))
        .bind(uri)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// Whether a create for a record `did` holds no row for could still arrive
    /// out of order: the account holds network records in `collection` (live
    /// ingest or a past backfill reached it), or a backfill covering the
    /// collection may still write: running, or pausing or cancelling (both stop
    /// once in-flight pages are written).
    async fn create_may_follow(
        &self,
        conn: &mut AnyConnection,
        did: &str,
        collection: &str,
    ) -> Result<bool, sqlx::Error> {
        let sql = adapt_sql(
            "SELECT 1 FROM happyview_records \
             WHERE did = ? AND collection = ? AND indexed_at IS NOT NULL LIMIT 1",
            self.backend,
        );
        let held = crate::db::query(&sql)
            .bind(did)
            .bind(collection)
            .fetch_optional(&mut *conn)
            .await?
            .is_some();
        if held {
            return Ok(true);
        }
        let sql = adapt_sql(
            "SELECT 1 FROM happyview_backfill_jobs \
             WHERE status IN ('running', 'pausing', 'cancelling') \
             AND (collection IS NULL OR collection = ?) LIMIT 1",
            self.backend,
        );
        Ok(crate::db::query(&sql)
            .bind(collection)
            .fetch_optional(&mut *conn)
            .await?
            .is_some())
    }

    /// Up to `limit` of the network records `did` holds, optionally only in
    /// `collections`.
    async fn held_network_uris<'e, E>(
        &self,
        executor: E,
        did: &str,
        collections: Option<&[String]>,
        limit: i64,
    ) -> Result<Vec<String>, sqlx::Error>
    where
        E: sqlx::Executor<'e, Database = sqlx::Any>,
    {
        let mut select =
            "SELECT uri FROM happyview_records WHERE did = ? AND indexed_at IS NOT NULL"
                .to_string();
        if let Some(collections) = collections {
            select.push_str(&format!(
                " AND collection IN ({})",
                placeholders(collections.len())
            ));
        }
        select.push_str(" LIMIT ?");
        let select = adapt_sql(&select, self.backend);

        let mut query = crate::db::query_as::<(String,)>(&select).bind(did);
        for collection in collections.unwrap_or_default() {
            query = query.bind(collection.as_str());
        }
        Ok(query
            .bind(limit)
            .fetch_all(executor)
            .await?
            .into_iter()
            .map(|(uri,)| uri)
            .collect())
    }

    /// Whether `did` holds any network record (`indexed_at` set), optionally
    /// only in `collections`. Reads without a lock or a write transaction.
    pub async fn holds_network_records(
        &self,
        did: &str,
        collections: Option<&[String]>,
    ) -> Result<bool, sqlx::Error> {
        Ok(!self
            .held_network_uris(&self.db, did, collections, 1)
            .await?
            .is_empty())
    }

    /// Delete up to `limit` of `did`'s network records and their refs on the
    /// caller's transaction, which must hold the DID's lock. Returns how many
    /// went.
    pub async fn purge_network_batch(
        &self,
        conn: &mut AnyConnection,
        did: &str,
        collections: Option<&[String]>,
        limit: i64,
    ) -> Result<u64, sqlx::Error> {
        let uris = self
            .held_network_uris(&mut *conn, did, collections, limit)
            .await?;
        self.delete_uris(conn, &uris).await
    }

    /// Delete up to `limit` records of `collection`, and their refs, in one
    /// write transaction. Returns how many went.
    pub async fn delete_collection_batch(
        &self,
        collection: &str,
        limit: i64,
    ) -> Result<u64, sqlx::Error> {
        let mut tx = crate::db::begin_write(&self.db, self.backend).await?;
        let uris: Vec<String> = crate::db::query_as::<(String,)>(&adapt_sql(
            "SELECT uri FROM happyview_records WHERE collection = ? LIMIT ?",
            self.backend,
        ))
        .bind(collection)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|(uri,)| uri)
        .collect();
        let deleted = self.delete_uris(&mut tx, &uris).await?;
        tx.commit().await?;
        Ok(deleted)
    }

    pub async fn count_collection(&self, collection: &str) -> Result<i64, sqlx::Error> {
        crate::db::query_scalar::<i64>(&adapt_sql(
            "SELECT COUNT(*) FROM happyview_records WHERE collection = ?",
            self.backend,
        ))
        .bind(collection)
        .fetch_one(&self.db)
        .await
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
                .delete_network(&pool, &rec.uri(), &did, NSID, Some(&tid(10)), &now)
                .await
                .expect("older delete");
            assert!(row(&store, &rec.uri()).await.is_some(), "{backend:?}");
            assert_eq!(ref_count(&store, &rec.uri()).await, 1, "{backend:?}");

            store
                .delete_network(&pool, &rec.uri(), &did, NSID, Some(&tid(30)), &now)
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
}

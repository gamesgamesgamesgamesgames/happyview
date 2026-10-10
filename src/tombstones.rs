//! Record and DID tombstones, and the check that keeps out-of-order
//! deliveries from resurrecting deleted records.
//!
//! Jetstream (v1 and v2) carries each commit's `rev`, a TID that sorts by
//! time. A live delete leaves a record tombstone keyed by URI; an account
//! deletion leaves one DID tombstone. A create or update is skipped when a
//! tombstone at or after its rev exists. A write with no rev (a `listRecords`
//! crawl row, a dead-letter retry) cannot be ordered against a tombstone, so
//! any tombstone blocks it: a delete was seen within the retention window,
//! and bringing the record back is the worse mistake.
//!
//! A check and the write it guards run in one transaction from
//! `db::begin_write`, inside `db::retry_on_busy`. On SQLite that transaction
//! holds the write lock from its first statement. On Postgres, writers on the
//! same record or account serialize on transaction-scoped advisory locks:
//! `lock_uris` and `lock_did`, taken before the check.

use std::collections::{HashMap, HashSet};

use sqlx::{AnyConnection, AnyPool};

use crate::db::{DatabaseBackend, adapt_sql, begin_write, rev_compare};

/// The first key of the two-key `pg_advisory_xact_lock` taken on a record
/// URI. A different class from `DID_LOCK_CLASS`, so a DID and a URI never
/// share a lock, and from the one-key (bigint) locks used elsewhere.
const URI_LOCK_CLASS: i32 = 0x6876_7201;

/// The first key of the two-key `pg_advisory_xact_lock` taken on a DID.
const DID_LOCK_CLASS: i32 = 0x6876_6401;

/// Hours a tombstone is kept unless `RECORD_TOMBSTONE_RETENTION_HOURS` says
/// otherwise: longer than any reconnect or replay window.
pub const DEFAULT_RECORD_TOMBSTONE_RETENTION_HOURS: u32 = 72;

/// URIs or DIDs per tombstone lookup, well under either backend's bound
/// parameter limit.
const LOOKUP_CHUNK: usize = 500;

/// Records deleted per purge transaction, so a large account's purge does not
/// hold the SQLite write lock against ingest for one long transaction.
const PURGE_BATCH: i64 = 500;

/// Tombstones deleted per sweep statement.
const SWEEP_BATCH: i64 = 5000;

/// Parse `RECORD_TOMBSTONE_RETENTION_HOURS`. `0` keeps tombstones
/// indefinitely; anything unparseable falls back to the default.
pub fn parse_retention_hours(raw: Option<&str>) -> u32 {
    raw.and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(DEFAULT_RECORD_TOMBSTONE_RETENTION_HOURS)
}

pub fn retention_hours() -> u32 {
    parse_retention_hours(
        std::env::var("RECORD_TOMBSTONE_RETENTION_HOURS")
            .ok()
            .as_deref(),
    )
}

/// A pending network write, as the tombstone check needs it.
pub struct WriteCandidate<'a> {
    pub uri: &'a str,
    pub did: &'a str,
    pub rev: Option<&'a str>,
}

/// Whether a tombstone at `tombstone_rev` supersedes a write at `rev`.
fn superseded(tombstone_rev: &str, rev: Option<&str>) -> bool {
    match rev {
        None => true,
        Some(rev) => tombstone_rev >= rev,
    }
}

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(", ")
}

async fn advisory_xact_lock(
    conn: &mut AnyConnection,
    class: i32,
    key: &str,
) -> Result<(), sqlx::Error> {
    crate::db::query(&format!(
        "SELECT pg_advisory_xact_lock({class}, hashtext($1))"
    ))
    .bind(key)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// On Postgres, lock each of `uris` until the caller's transaction ends, so
/// a tombstone check and the write it guards are not interleaved with another
/// writer's on the same record. Locks are taken in sorted order with
/// duplicates dropped, so two batches that share URIs cannot deadlock on
/// them (a deadlock between two URIs whose hashes collide is still possible;
/// Postgres aborts one with `40P01`, which `retry_on_busy` retries). Lock
/// every URI a transaction will touch here, before its first check. A no-op
/// on SQLite, where `begin_write` already holds the database's write lock.
pub async fn lock_uris(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
    uris: &[&str],
) -> Result<(), sqlx::Error> {
    if backend != DatabaseBackend::Postgres {
        return Ok(());
    }
    let mut uris = uris.to_vec();
    uris.sort_unstable();
    uris.dedup();
    for uri in uris {
        advisory_xact_lock(conn, URI_LOCK_CLASS, uri).await?;
    }
    Ok(())
}

/// On Postgres, lock the account `did` until the caller's transaction ends,
/// so a DID tombstone, its purge and a write for the account are not
/// interleaved. Take it before any `lock_uris` in the same transaction. A
/// no-op on SQLite.
pub async fn lock_did(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
    did: &str,
) -> Result<(), sqlx::Error> {
    if backend != DatabaseBackend::Postgres {
        return Ok(());
    }
    advisory_xact_lock(conn, DID_LOCK_CLASS, did).await
}

/// The URIs among `candidates` that a record or DID tombstone supersedes.
/// Runs on the caller's connection so it sees, and is ordered with, the
/// caller's transaction.
pub async fn blocked_writes(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
    candidates: &[WriteCandidate<'_>],
) -> Result<HashSet<String>, sqlx::Error> {
    let mut blocked = HashSet::new();
    if candidates.is_empty() {
        return Ok(blocked);
    }

    let mut record_revs: HashMap<String, String> = HashMap::new();
    for chunk in candidates.chunks(LOOKUP_CHUNK) {
        let sql = adapt_sql(
            &format!(
                "SELECT uri, rev FROM happyview_record_tombstones WHERE uri IN ({})",
                placeholders(chunk.len())
            ),
            backend,
        );
        let mut query = crate::db::query_as::<(String, String)>(&sql);
        for candidate in chunk {
            query = query.bind(candidate.uri);
        }
        record_revs.extend(query.fetch_all(&mut *conn).await?);
    }

    let mut dids: Vec<&str> = candidates.iter().map(|c| c.did).collect();
    dids.sort_unstable();
    dids.dedup();
    let mut did_revs: HashMap<String, String> = HashMap::new();
    for chunk in dids.chunks(LOOKUP_CHUNK) {
        let sql = adapt_sql(
            &format!(
                "SELECT did, rev FROM happyview_did_tombstones WHERE did IN ({})",
                placeholders(chunk.len())
            ),
            backend,
        );
        let mut query = crate::db::query_as::<(String, String)>(&sql);
        for did in chunk {
            query = query.bind(*did);
        }
        did_revs.extend(query.fetch_all(&mut *conn).await?);
    }

    for candidate in candidates {
        let by_record = record_revs
            .get(candidate.uri)
            .is_some_and(|rev| superseded(rev, candidate.rev));
        let by_did = did_revs
            .get(candidate.did)
            .is_some_and(|rev| superseded(rev, candidate.rev));
        if by_record || by_did {
            blocked.insert(candidate.uri.to_string());
        }
    }
    Ok(blocked)
}

/// Record that `uri` was deleted at `rev`. A repeated delete keeps the newest
/// rev. Takes the URI's lock (`lock_uris`), so call it inside a
/// `begin_write` transaction, after locking every URI that transaction
/// touches.
pub async fn write_record_tombstone(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
    uri: &str,
    rev: &str,
    now: &str,
) -> Result<(), sqlx::Error> {
    lock_uris(conn, backend, &[uri]).await?;
    let sql = adapt_sql(
        &format!(
            "INSERT INTO happyview_record_tombstones (uri, rev, deleted_at) VALUES (?, ?, ?) \
             ON CONFLICT (uri) DO UPDATE SET rev = excluded.rev, deleted_at = excluded.deleted_at \
             WHERE {}",
            rev_compare(
                backend,
                "excluded.rev",
                ">",
                "happyview_record_tombstones.rev"
            )
        ),
        backend,
    );
    crate::db::query(&sql)
        .bind(uri)
        .bind(rev)
        .bind(now)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Record that the account `did` was deleted. `rev` orders the deletion
/// against commit revs; `seq` is the Jetstream seq, when there is one.
/// Takes the DID's lock (`lock_did`), so call it inside a `begin_write`
/// transaction.
pub async fn write_did_tombstone(
    conn: &mut AnyConnection,
    backend: DatabaseBackend,
    did: &str,
    rev: &str,
    seq: Option<i64>,
    now: &str,
) -> Result<(), sqlx::Error> {
    lock_did(conn, backend, did).await?;
    let sql = adapt_sql(
        &format!(
            "INSERT INTO happyview_did_tombstones (did, rev, seq, deleted_at) VALUES (?, ?, ?, ?) \
             ON CONFLICT (did) DO UPDATE SET rev = excluded.rev, seq = excluded.seq, deleted_at = excluded.deleted_at \
             WHERE {}",
            rev_compare(backend, "excluded.rev", ">", "happyview_did_tombstones.rev")
        ),
        backend,
    );
    crate::db::query(&sql)
        .bind(did)
        .bind(rev)
        .bind(seq)
        .bind(now)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Delete `did`'s network records (`indexed_at` set) and their refs, in
/// batches, optionally only in `collections`. Local writes still awaiting
/// their echo are left alone. Each batch is its own `begin_write`
/// transaction holding the DID's lock. Returns the records deleted.
pub async fn purge_did_records(
    db: &AnyPool,
    backend: DatabaseBackend,
    did: &str,
    collections: Option<&[String]>,
) -> Result<u64, sqlx::Error> {
    if collections.is_some_and(<[String]>::is_empty) {
        return Ok(0);
    }
    let mut purged = 0;
    loop {
        let deleted =
            crate::db::retry_on_busy(|| purge_batch(db, backend, did, collections)).await?;
        purged += deleted;
        if (deleted as i64) < PURGE_BATCH {
            return Ok(purged);
        }
    }
}

async fn purge_batch(
    db: &AnyPool,
    backend: DatabaseBackend,
    did: &str,
    collections: Option<&[String]>,
) -> Result<u64, sqlx::Error> {
    let mut select =
        "SELECT uri FROM happyview_records WHERE did = ? AND indexed_at IS NOT NULL".to_string();
    if let Some(collections) = collections {
        select.push_str(&format!(
            " AND collection IN ({})",
            placeholders(collections.len())
        ));
    }
    select.push_str(" LIMIT ?");
    let select = adapt_sql(&select, backend);

    let mut tx = begin_write(db, backend).await?;
    lock_did(&mut tx, backend, did).await?;
    let mut query = crate::db::query_as::<(String,)>(&select).bind(did);
    for collection in collections.unwrap_or_default() {
        query = query.bind(collection.as_str());
    }
    let uris: Vec<String> = query
        .bind(PURGE_BATCH)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|(uri,)| uri)
        .collect();
    if uris.is_empty() {
        tx.commit().await?;
        return Ok(0);
    }

    for (table, column) in [
        ("happyview_record_refs", "source_uri"),
        ("happyview_records", "uri"),
    ] {
        let sql = adapt_sql(
            &format!(
                "DELETE FROM {table} WHERE {column} IN ({})",
                placeholders(uris.len())
            ),
            backend,
        );
        let mut delete = crate::db::query(&sql);
        for uri in &uris {
            delete = delete.bind(uri.as_str());
        }
        delete.execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(uris.len() as u64)
}

/// Delete tombstones older than `retention_hours`, in batches. `0` keeps
/// them. Returns the rows deleted across both tables.
pub async fn sweep(db: &AnyPool, backend: DatabaseBackend, retention_hours: u32) -> u64 {
    if retention_hours == 0 {
        return 0;
    }
    // Same `+00:00` RFC 3339 form `now_rfc3339` writes, so the TEXT
    // comparison orders correctly on SQLite.
    let cutoff =
        (chrono::Utc::now() - chrono::Duration::hours(i64::from(retention_hours))).to_rfc3339();
    let mut deleted = 0;
    for (table, key) in [
        ("happyview_record_tombstones", "uri"),
        ("happyview_did_tombstones", "did"),
    ] {
        let sql = adapt_sql(
            &format!(
                "DELETE FROM {table} WHERE {key} IN \
                 (SELECT {key} FROM {table} WHERE deleted_at < ? LIMIT ?)"
            ),
            backend,
        );
        loop {
            match crate::db::retry_on_busy(|| {
                crate::db::query(&sql)
                    .bind(&cutoff)
                    .bind(SWEEP_BATCH)
                    .execute(db)
            })
            .await
            {
                Ok(result) => {
                    deleted += result.rows_affected();
                    if (result.rows_affected() as i64) < SWEEP_BATCH {
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!(table, error = %e, "failed to sweep tombstones");
                    break;
                }
            }
        }
    }
    deleted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{begin_write, now_rfc3339};
    use crate::test_support::test_pools;

    const POST: &str = "app.test.post";
    const LIKE: &str = "app.test.like";

    fn tid(n: i64) -> String {
        crate::tid::tid_from_unix_microseconds(1_700_000_000_000_000 + n)
    }

    fn unique(prefix: &str) -> String {
        format!("{prefix}{}", uuid::Uuid::new_v4().simple())
    }

    async fn is_blocked(
        conn: &mut AnyConnection,
        backend: DatabaseBackend,
        uri: &str,
        did: &str,
        rev: Option<&str>,
    ) -> bool {
        blocked_writes(conn, backend, &[WriteCandidate { uri, did, rev }])
            .await
            .expect("tombstone lookup")
            .contains(uri)
    }

    async fn seed_record(
        pool: &AnyPool,
        backend: DatabaseBackend,
        did: &str,
        collection: &str,
        rkey: &str,
        indexed_at: Option<&str>,
    ) -> String {
        let uri = format!("at://{did}/{collection}/{rkey}");
        let sql = adapt_sql(
            "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at) \
             VALUES (?, ?, ?, ?, '{}', 'bafy', ?, ?)",
            backend,
        );
        crate::db::query(&sql)
            .bind(&uri)
            .bind(did)
            .bind(collection)
            .bind(rkey)
            .bind(indexed_at)
            .bind(now_rfc3339())
            .execute(pool)
            .await
            .expect("seed record");
        uri
    }

    async fn rkeys(pool: &AnyPool, backend: DatabaseBackend, did: &str) -> Vec<String> {
        let sql = adapt_sql(
            "SELECT rkey FROM happyview_records WHERE did = ? ORDER BY rkey",
            backend,
        );
        crate::db::query_as::<(String,)>(&sql)
            .bind(did)
            .fetch_all(pool)
            .await
            .expect("read rkeys")
            .into_iter()
            .map(|(r,)| r)
            .collect()
    }

    async fn clean_up(pool: &AnyPool, backend: DatabaseBackend, like: &str) {
        for sql in [
            "DELETE FROM happyview_record_refs WHERE source_uri LIKE ?",
            "DELETE FROM happyview_records WHERE uri LIKE ?",
            "DELETE FROM happyview_record_tombstones WHERE uri LIKE ?",
            "DELETE FROM happyview_did_tombstones WHERE did LIKE ?",
        ] {
            let pattern = if sql.contains("did_tombstones") {
                like.trim_start_matches("at://").to_string()
            } else {
                like.to_string()
            };
            crate::db::query(&adapt_sql(sql, backend))
                .bind(pattern)
                .execute(pool)
                .await
                .expect("clean up");
        }
    }

    #[test]
    fn retention_defaults_to_seventy_two_hours() {
        assert_eq!(parse_retention_hours(None), 72);
        assert_eq!(parse_retention_hours(Some("banana")), 72);
        assert_eq!(parse_retention_hours(Some(" 24 ")), 24);
        assert_eq!(parse_retention_hours(Some("0")), 0, "0 keeps tombstones");
    }

    #[test]
    fn a_tombstone_supersedes_its_rev_and_older_and_every_unversioned_write() {
        assert!(tid(1) < tid(1_000_000), "TIDs sort bytewise in time order");
        assert!(superseded(&tid(5), Some(&tid(4))));
        assert!(superseded(&tid(5), Some(&tid(5))));
        assert!(!superseded(&tid(5), Some(&tid(6))));
        assert!(superseded(&tid(5), None));
    }

    #[tokio::test]
    async fn tombstones_block_older_and_unversioned_writes() {
        for (pool, backend) in test_pools().await {
            let did = unique("did:plc:tomb");
            let gone = unique("did:plc:gone");
            let dead = format!("at://{did}/{POST}/dead");
            let alive = format!("at://{did}/{POST}/alive");
            let orphan = format!("at://{gone}/{POST}/x");
            let mut conn = pool.acquire().await.expect("connection");
            write_record_tombstone(&mut conn, backend, &dead, &tid(50), &now_rfc3339())
                .await
                .expect("record tombstone");
            write_did_tombstone(&mut conn, backend, &gone, &tid(50), Some(7), &now_rfc3339())
                .await
                .expect("did tombstone");

            for (uri, owner, rev, blocked, why) in [
                (&dead, &did, Some(tid(40)), true, "an older create"),
                (
                    &dead,
                    &did,
                    Some(tid(50)),
                    true,
                    "a create at the delete's rev",
                ),
                (&dead, &did, Some(tid(60)), false, "a newer create"),
                (&dead, &did, None, true, "a create without a rev"),
                (&alive, &did, None, false, "a record never deleted"),
                (
                    &orphan,
                    &gone,
                    Some(tid(40)),
                    true,
                    "an older write for a deleted account",
                ),
                (
                    &orphan,
                    &gone,
                    None,
                    true,
                    "an unversioned write for a deleted account",
                ),
                (
                    &orphan,
                    &gone,
                    Some(tid(60)),
                    false,
                    "a write after the deletion",
                ),
            ] {
                assert_eq!(
                    is_blocked(&mut conn, backend, uri, owner, rev.as_deref()).await,
                    blocked,
                    "{why} ({backend:?})"
                );
            }
            drop(conn);
            clean_up(&pool, backend, &format!("at://{did}%")).await;
            clean_up(&pool, backend, &format!("at://{gone}%")).await;
        }
    }

    #[tokio::test]
    async fn a_record_tombstone_keeps_the_newest_rev() {
        for (pool, backend) in test_pools().await {
            let uri = format!("at://{}/{POST}/r", unique("did:plc:newest"));
            let mut conn = pool.acquire().await.expect("connection");
            for rev in [tid(50), tid(40), tid(60)] {
                write_record_tombstone(&mut conn, backend, &uri, &rev, &now_rfc3339())
                    .await
                    .expect("tombstone");
            }
            let (rev,): (String,) = crate::db::query_as(&adapt_sql(
                "SELECT rev FROM happyview_record_tombstones WHERE uri = ?",
                backend,
            ))
            .bind(&uri)
            .fetch_one(&mut *conn)
            .await
            .expect("read tombstone");
            assert_eq!(rev, tid(60), "{backend:?}");
            drop(conn);
            clean_up(&pool, backend, &uri).await;
        }
    }

    #[tokio::test]
    async fn a_purge_removes_only_the_dids_network_records_and_their_refs() {
        for (pool, backend) in test_pools().await {
            let did = unique("did:plc:purge");
            let other = unique("did:plc:keep");
            let now = now_rfc3339();
            let linked = seed_record(&pool, backend, &did, POST, "a", Some(&now)).await;
            seed_record(&pool, backend, &did, POST, "b", Some(&now)).await;
            seed_record(&pool, backend, &did, LIKE, "c", Some(&now)).await;
            seed_record(&pool, backend, &did, POST, "local", None).await;
            seed_record(&pool, backend, &other, POST, "x", Some(&now)).await;
            crate::db::query(&adapt_sql(
                "INSERT INTO happyview_record_refs (source_uri, target_uri, collection) VALUES (?, ?, ?)",
                backend,
            ))
            .bind(&linked)
            .bind("at://did:plc:t/app.test.post/1")
            .bind(POST)
            .execute(&pool)
            .await
            .expect("seed ref");

            let purged =
                purge_did_records(&pool, backend, &did, Some([POST.to_string()].as_slice()))
                    .await
                    .expect("collection purge");
            assert_eq!(purged, 2, "{backend:?}");
            assert_eq!(rkeys(&pool, backend, &did).await, vec!["c", "local"]);
            let (refs,): (i64,) = crate::db::query_as(&adapt_sql(
                "SELECT COUNT(*) FROM happyview_record_refs WHERE source_uri = ?",
                backend,
            ))
            .bind(&linked)
            .fetch_one(&pool)
            .await
            .expect("count refs");
            assert_eq!(refs, 0, "a purged record's refs go with it");

            assert_eq!(
                purge_did_records(&pool, backend, &did, None)
                    .await
                    .expect("purge"),
                1
            );
            assert_eq!(
                rkeys(&pool, backend, &did).await,
                vec!["local"],
                "a local write awaiting its echo is not a network record"
            );
            assert_eq!(rkeys(&pool, backend, &other).await, vec!["x"]);
            assert_eq!(
                purge_did_records(&pool, backend, &did, Some(&[][..]))
                    .await
                    .expect("empty"),
                0
            );

            clean_up(&pool, backend, &format!("at://{did}%")).await;
            clean_up(&pool, backend, &format!("at://{other}%")).await;
        }
    }

    #[tokio::test]
    async fn the_sweep_deletes_tombstones_past_retention() {
        for (pool, backend) in test_pools().await {
            let tag = unique("sweep");
            let old = (chrono::Utc::now() - chrono::Duration::hours(100)).to_rfc3339();
            let recent = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
            {
                let mut conn = pool.acquire().await.expect("connection");
                for (suffix, at) in [("old", &old), ("new", &recent)] {
                    write_record_tombstone(
                        &mut conn,
                        backend,
                        &format!("at://did:plc:{tag}/{suffix}"),
                        &tid(1),
                        at,
                    )
                    .await
                    .expect("record tombstone");
                    write_did_tombstone(
                        &mut conn,
                        backend,
                        &format!("did:plc:{tag}{suffix}"),
                        &tid(1),
                        None,
                        at,
                    )
                    .await
                    .expect("did tombstone");
                }
            }

            assert_eq!(sweep(&pool, backend, 0).await, 0, "0 keeps tombstones");
            assert!(sweep(&pool, backend, 72).await >= 2);

            let (records,): (i64,) = crate::db::query_as(&adapt_sql(
                "SELECT COUNT(*) FROM happyview_record_tombstones WHERE uri LIKE ?",
                backend,
            ))
            .bind(format!("at://did:plc:{tag}/%"))
            .fetch_one(&pool)
            .await
            .expect("count record tombstones");
            let (dids,): (i64,) = crate::db::query_as(&adapt_sql(
                "SELECT COUNT(*) FROM happyview_did_tombstones WHERE did LIKE ?",
                backend,
            ))
            .bind(format!("did:plc:{tag}%"))
            .fetch_one(&pool)
            .await
            .expect("count did tombstones");
            assert_eq!(
                (records, dids),
                (1, 1),
                "only the recent ones stay ({backend:?})"
            );

            clean_up(&pool, backend, &format!("at://did:plc:{tag}%")).await;
        }
    }

    /// Whether another session could take `class`'s advisory lock on `key`
    /// right now. Postgres only.
    async fn lock_is_free(pool: &AnyPool, class: i32, key: &str) -> bool {
        let mut tx = pool.begin().await.expect("probe transaction");
        let (free,): (bool,) = crate::db::query_as(&format!(
            "SELECT pg_try_advisory_xact_lock({class}, hashtext($1))"
        ))
        .bind(key)
        .fetch_one(&mut *tx)
        .await
        .expect("probe the lock");
        tx.rollback().await.expect("release the probe");
        free
    }

    #[tokio::test]
    async fn the_advisory_locks_are_no_ops_on_sqlite() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let mut tx = begin_write(&pool, DatabaseBackend::Sqlite)
            .await
            .expect("begin");
        lock_uris(
            &mut tx,
            DatabaseBackend::Sqlite,
            &["at://did:plc:a/app.test.post/1"],
        )
        .await
        .expect("no-op uri lock");
        lock_did(&mut tx, DatabaseBackend::Sqlite, "did:plc:a")
            .await
            .expect("no-op did lock");
        tx.commit().await.expect("commit");
    }

    #[tokio::test]
    async fn the_advisory_locks_hold_until_the_transaction_ends_on_postgres() {
        for (pool, backend) in test_pools().await {
            if backend != DatabaseBackend::Postgres {
                continue;
            }
            let did = unique("did:plc:lock");
            let first = format!("at://{did}/{POST}/1");
            let second = format!("at://{did}/{POST}/2");

            let mut tx = begin_write(&pool, backend).await.expect("begin");
            lock_uris(&mut tx, backend, &[&second, &first, &second])
                .await
                .expect("lock uris");
            lock_did(&mut tx, backend, &did).await.expect("lock did");
            for uri in [&first, &second] {
                assert!(
                    !lock_is_free(&pool, URI_LOCK_CLASS, uri).await,
                    "{uri} is locked"
                );
            }
            assert!(
                !lock_is_free(&pool, DID_LOCK_CLASS, &did).await,
                "the DID is locked"
            );
            assert!(
                lock_is_free(&pool, URI_LOCK_CLASS, &did).await,
                "a DID and a URI never share a lock"
            );
            assert!(lock_is_free(&pool, DID_LOCK_CLASS, &first).await);
            tx.commit().await.expect("commit");
            assert!(
                lock_is_free(&pool, URI_LOCK_CLASS, &first).await,
                "released at commit"
            );
            assert!(
                lock_is_free(&pool, DID_LOCK_CLASS, &did).await,
                "released at commit"
            );
        }
    }

    #[tokio::test]
    async fn tombstone_writes_lock_their_key_on_postgres() {
        for (pool, backend) in test_pools().await {
            if backend != DatabaseBackend::Postgres {
                continue;
            }
            let did = unique("did:plc:tlock");
            let uri = format!("at://{did}/{POST}/1");

            let mut tx = begin_write(&pool, backend).await.expect("begin");
            write_record_tombstone(&mut tx, backend, &uri, &tid(1), &now_rfc3339())
                .await
                .expect("record tombstone");
            write_did_tombstone(&mut tx, backend, &did, &tid(1), None, &now_rfc3339())
                .await
                .expect("did tombstone");
            assert!(!lock_is_free(&pool, URI_LOCK_CLASS, &uri).await);
            assert!(!lock_is_free(&pool, DID_LOCK_CLASS, &did).await);
            tx.commit().await.expect("commit");

            clean_up(&pool, backend, &format!("at://{did}%")).await;
        }
    }

    #[tokio::test]
    async fn a_purge_waits_for_the_dids_lock_on_postgres() {
        for (pool, backend) in test_pools().await {
            if backend != DatabaseBackend::Postgres {
                continue;
            }
            let did = unique("did:plc:plock");
            seed_record(&pool, backend, &did, POST, "a", Some(&now_rfc3339())).await;

            let mut held = begin_write(&pool, backend).await.expect("begin");
            lock_did(&mut held, backend, &did).await.expect("lock did");
            let purge = tokio::spawn({
                let (pool, did) = (pool.clone(), did.clone());
                async move { purge_did_records(&pool, backend, &did, None).await }
            });
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            assert!(!purge.is_finished(), "the purge waits for the DID's lock");
            held.commit().await.expect("release");
            assert_eq!(purge.await.expect("purge task").expect("purge"), 1);

            clean_up(&pool, backend, &format!("at://{did}%")).await;
        }
    }
}

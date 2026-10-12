use crate::db::{DatabaseBackend, adapt_sql};
use serde_json::Value;
use std::collections::HashSet;

/// Recursively walk a JSON value and collect all string values starting with "at://".
pub fn extract_at_uris(value: &Value) -> HashSet<String> {
    let mut uris = HashSet::new();
    collect_at_uris(value, &mut uris);
    uris
}

/// Collect every blob CID a record references.
///
/// A blob ref is a `blob` object carrying `ref: { "$link": <cid> }`. Matching on
/// the `$link` alone would also pick up record links, which are not blobs, so
/// the surrounding object must declare `"$type": "blob"`.
pub fn extract_blob_cids(value: &Value) -> HashSet<String> {
    let mut cids = HashSet::new();
    collect_blob_cids(value, &mut cids);
    cids
}

fn collect_blob_cids(value: &Value, cids: &mut HashSet<String>) {
    match value {
        Value::Object(obj) => {
            if obj.get("$type").and_then(Value::as_str) == Some("blob")
                && let Some(link) = obj
                    .get("ref")
                    .and_then(|r| r.get("$link"))
                    .and_then(Value::as_str)
            {
                cids.insert(link.to_string());
            }
            for v in obj.values() {
                collect_blob_cids(v, cids);
            }
        }
        Value::Array(arr) => {
            for item in arr {
                collect_blob_cids(item, cids);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod blob_ref_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn finds_blobs_at_any_depth() {
        let record = json!({
            "text": "hi",
            "embed": {
                "images": [
                    { "image": { "$type": "blob", "ref": { "$link": "bafyimg1" },
                                 "mimeType": "image/png", "size": 1 } },
                    { "image": { "$type": "blob", "ref": { "$link": "bafyimg2" },
                                 "mimeType": "image/png", "size": 2 } }
                ]
            }
        });
        let cids = extract_blob_cids(&record);
        assert_eq!(cids.len(), 2);
        assert!(cids.contains("bafyimg1"));
        assert!(cids.contains("bafyimg2"));
    }

    #[test]
    fn ignores_record_links_that_are_not_blobs() {
        // A strong ref carries a $link too; treating it as a blob would list
        // CIDs that getBlob cannot serve.
        let record = json!({
            "subject": { "uri": "at://did:plc:x/c/r", "cid": { "$link": "bafyrecord" } }
        });
        assert!(extract_blob_cids(&record).is_empty());
    }

    #[test]
    fn a_blob_without_a_link_is_skipped_rather_than_panicking() {
        let record = json!({ "image": { "$type": "blob", "mimeType": "image/png" } });
        assert!(extract_blob_cids(&record).is_empty());
    }
}

fn collect_at_uris(value: &Value, uris: &mut HashSet<String>) {
    match value {
        Value::String(s) if s.starts_with("at://") => {
            uris.insert(s.clone());
        }
        Value::Array(arr) => {
            for item in arr {
                collect_at_uris(item, uris);
            }
        }
        Value::Object(obj) => {
            for v in obj.values() {
                collect_at_uris(v, uris);
            }
        }
        _ => {}
    }
}

/// Refs per multi-row INSERT: three bound parameters each, 900 a statement.
/// That is well within the bundled SQLite's limit of 32766, and within the
/// 999 of SQLite builds before 3.32 too.
pub const REFS_PER_INSERT: usize = 300;

/// Instance setting recording that the one-time refs rebuild has run.
pub const REFS_REBUILT_SETTING: &str = "record_refs_rebuilt";

/// Instance setting holding the last source uri a running rebuild finished,
/// so an interrupted rebuild resumes instead of restarting. Removed once the
/// rebuild completes.
const REFS_REBUILD_CURSOR_SETTING: &str = "record_refs_rebuild_cursor";

/// Records per rebuild page. One transaction per page.
const REBUILD_PAGE_SIZE: i64 = 1000;

#[derive(Debug, PartialEq, Eq)]
pub enum RebuildOutcome {
    /// The setting says it ran on an earlier boot.
    AlreadyDone,
    /// Refs already existed, so they are being maintained live.
    NotNeeded,
    Rebuilt {
        records: u64,
    },
}

pub(crate) async fn put_setting(
    conn: &mut sqlx::AnyConnection,
    backend: DatabaseBackend,
    key: &str,
    value: &str,
) -> Result<(), sqlx::Error> {
    let sql = adapt_sql(
        "INSERT INTO happyview_instance_settings (key, value, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT (key) DO UPDATE SET value = ?, updated_at = ?",
        backend,
    );
    let now = crate::db::now_rfc3339();
    crate::db::query(&sql)
        .bind(key)
        .bind(value)
        .bind(&now)
        .bind(value)
        .bind(&now)
        .execute(conn)
        .await?;
    Ok(())
}

pub(crate) async fn get_setting(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    key: &str,
) -> Result<Option<String>, sqlx::Error> {
    let sql = adapt_sql(
        "SELECT value FROM happyview_instance_settings WHERE key = ?",
        backend,
    );
    Ok(crate::db::query_as::<(String,)>(&sql)
        .bind(key)
        .fetch_optional(db)
        .await?
        .map(|(value,)| value))
}

/// Populate `happyview_record_refs` for records indexed before the table
/// existed, at most once per instance.
///
/// The old check rebuilt whenever the table happened to be empty, which on a
/// collection whose records reference nothing meant a full OFFSET scan and
/// rewrite on every boot. An instance setting now records that the rebuild
/// ran, and the walk is keyset-paged on `uri`. Each page commits together with
/// a resume cursor, and the done marker is written only after the last page,
/// so an interrupted rebuild picks up where it stopped.
pub async fn rebuild_once(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
) -> Result<RebuildOutcome, sqlx::Error> {
    Ok(rebuild_pages(db, backend, None)
        .await?
        .unwrap_or(RebuildOutcome::AlreadyDone))
}

/// `rebuild_once`, stopping without marking done after `max_pages` pages
/// (`None` result) so tests can interrupt it.
async fn rebuild_pages(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    max_pages: Option<usize>,
) -> Result<Option<RebuildOutcome>, sqlx::Error> {
    if get_setting(db, backend, REFS_REBUILT_SETTING)
        .await?
        .is_some()
    {
        return Ok(Some(RebuildOutcome::AlreadyDone));
    }

    let cursor = get_setting(db, backend, REFS_REBUILD_CURSOR_SETTING).await?;
    if cursor.is_none() {
        let (has_refs,): (i64,) = crate::db::query_as(
            "SELECT COUNT(*) FROM (SELECT 1 FROM happyview_record_refs LIMIT 1) AS any_ref",
        )
        .fetch_one(db)
        .await?;
        if has_refs > 0 {
            let mut conn = db.acquire().await?;
            put_setting(&mut conn, backend, REFS_REBUILT_SETTING, "true").await?;
            return Ok(Some(RebuildOutcome::NotNeeded));
        }
    }

    // Record that a rebuild has started before any page commits, so a crash
    // or error ahead of the first cursor write resumes on the next boot rather
    // than mistaking its own (or live ingest's) refs for an already-built table.
    if cursor.is_none() {
        crate::db::retry_on_busy(|| async {
            let mut conn = db.acquire().await?;
            put_setting(&mut conn, backend, REFS_REBUILD_CURSOR_SETTING, "").await
        })
        .await?;
    }

    let store = crate::record_store::RecordStore::new(db.clone(), backend);
    let mut after = cursor.unwrap_or_default();
    let mut records: u64 = 0;
    let mut pages = 0usize;
    loop {
        if max_pages.is_some_and(|max| pages >= max) {
            return Ok(None);
        }
        // The page is read and rewritten on one write transaction, so a
        // deferred SQLite transaction never has to upgrade its read lock.
        let (walked, last) = crate::db::retry_on_busy(|| async {
            let mut tx = crate::db::begin_write(db, backend).await?;
            let (walked, last) = store
                .rebuild_refs_page(&mut tx, &after, REBUILD_PAGE_SIZE)
                .await?;
            if let Some(last) = &last {
                put_setting(&mut tx, backend, REFS_REBUILD_CURSOR_SETTING, last).await?;
            }
            tx.commit().await?;
            Ok((walked, last))
        })
        .await?;
        let Some(last) = last else { break };
        after = last;

        pages += 1;
        let before = records;
        records += walked;
        if before / 10_000 != records / 10_000 {
            tracing::info!(records, "record_refs rebuild progress");
        }
    }

    crate::db::retry_on_busy(|| async {
        let mut tx = db.begin().await?;
        put_setting(&mut tx, backend, REFS_REBUILT_SETTING, "true").await?;
        let delete_sql = adapt_sql(
            "DELETE FROM happyview_instance_settings WHERE key = ?",
            backend,
        );
        crate::db::query(&delete_sql)
            .bind(REFS_REBUILD_CURSOR_SETTING)
            .execute(&mut *tx)
            .await?;
        tx.commit().await
    })
    .await?;
    Ok(Some(RebuildOutcome::Rebuilt { records }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_top_level_uri() {
        let val = json!({"subject": "at://did:plc:abc/com.example/123"});
        let uris = extract_at_uris(&val);
        assert_eq!(uris.len(), 1);
        assert!(uris.contains("at://did:plc:abc/com.example/123"));
    }

    #[test]
    fn extracts_nested_uri() {
        let val = json!({"outer": {"inner": "at://did:plc:abc/col/rkey"}});
        let uris = extract_at_uris(&val);
        assert_eq!(uris.len(), 1);
        assert!(uris.contains("at://did:plc:abc/col/rkey"));
    }

    #[test]
    fn extracts_uris_from_arrays() {
        let val = json!({"refs": ["at://did:plc:a/col/1", "at://did:plc:b/col/2"]});
        let uris = extract_at_uris(&val);
        assert_eq!(uris.len(), 2);
    }

    #[test]
    fn ignores_non_at_strings() {
        let val = json!({"url": "https://example.com", "name": "test"});
        let uris = extract_at_uris(&val);
        assert!(uris.is_empty());
    }

    #[test]
    fn empty_object_returns_empty() {
        let uris = extract_at_uris(&json!({}));
        assert!(uris.is_empty());
    }

    #[test]
    fn deduplicates_repeated_uris() {
        let val = json!({"a": "at://did:plc:x/c/1", "b": "at://did:plc:x/c/1"});
        let uris = extract_at_uris(&val);
        assert_eq!(uris.len(), 1);
    }

    async fn seed_record(pool: &sqlx::AnyPool, rkey: &str, body: &Value) {
        crate::db::query(
            "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at) \
             VALUES (?, 'did:plc:r', 'app.test.post', ?, ?, 'bafytest', '2026-01-01T00:00:00+00:00', '2026-01-01T00:00:00+00:00')",
        )
        .bind(format!("at://did:plc:r/app.test.post/{rkey}"))
        .bind(rkey)
        .bind(body.to_string())
        .execute(pool)
        .await
        .expect("seed record");
    }

    /// A ref live ingest would have written for `source`'s `subject`.
    async fn live_ref(pool: &sqlx::AnyPool, source: &str, body: &Value) {
        crate::db::query(
            "INSERT INTO happyview_record_refs (source_uri, target_uri, collection) \
             VALUES (?, ?, 'app.test.post')",
        )
        .bind(source)
        .bind(body["subject"].as_str().expect("a subject"))
        .execute(pool)
        .await
        .expect("live ref");
    }

    async fn refs(pool: &sqlx::AnyPool) -> i64 {
        crate::db::query_as::<(i64,)>("SELECT COUNT(*) FROM happyview_record_refs")
            .fetch_one(pool)
            .await
            .expect("count refs")
            .0
    }

    async fn seed_ref_bearing_records(pool: &sqlx::AnyPool, count: usize) {
        // More than one page, so the keyset walk is exercised.
        for i in 0..count {
            let body = if i % 500 == 0 {
                json!({"subject": format!("at://did:plc:t/app.test.post/{i}")})
            } else {
                json!({"text": "plain"})
            };
            seed_record(pool, &format!("{i:05}"), &body).await;
        }
    }

    #[tokio::test]
    async fn the_refs_rebuild_runs_once() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let backend = DatabaseBackend::Sqlite;
        seed_ref_bearing_records(&pool, 1500).await;

        assert_eq!(
            rebuild_once(&pool, backend).await.expect("rebuild"),
            RebuildOutcome::Rebuilt { records: 1500 }
        );
        assert_eq!(refs(&pool).await, 3);

        // Refs empty again (say, every ref-bearing record was deleted): a
        // reboot must not walk the whole table a second time.
        crate::db::query("DELETE FROM happyview_record_refs")
            .execute(&pool)
            .await
            .expect("clear refs");
        assert_eq!(
            rebuild_once(&pool, backend).await.expect("second boot"),
            RebuildOutcome::AlreadyDone
        );
        assert_eq!(refs(&pool).await, 0);
    }

    #[tokio::test]
    async fn an_interrupted_rebuild_resumes_and_is_not_marked_done() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let backend = DatabaseBackend::Sqlite;
        seed_ref_bearing_records(&pool, 2500).await;

        // Stop after one page (records 0..1000: refs at 0 and 500).
        assert_eq!(rebuild_pages(&pool, backend, Some(1)).await.unwrap(), None);
        assert_eq!(refs(&pool).await, 2);
        assert!(
            get_setting(&pool, backend, REFS_REBUILT_SETTING)
                .await
                .unwrap()
                .is_none(),
            "a partial run is never marked done"
        );

        // The next run resumes from the cursor, not from the top, and finishes.
        assert_eq!(
            rebuild_once(&pool, backend).await.expect("resume"),
            RebuildOutcome::Rebuilt { records: 1500 }
        );
        assert_eq!(refs(&pool).await, 5);
        assert!(
            get_setting(&pool, backend, REFS_REBUILD_CURSOR_SETTING)
                .await
                .unwrap()
                .is_none()
        );

        assert_eq!(
            rebuild_once(&pool, backend).await.expect("again"),
            RebuildOutcome::AlreadyDone
        );
    }

    #[tokio::test]
    async fn an_install_that_already_has_refs_is_marked_without_a_rebuild() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let backend = DatabaseBackend::Sqlite;
        let body = json!({"subject": "at://did:plc:t/app.test.post/1"});
        seed_record(&pool, "a", &body).await;
        live_ref(&pool, "at://did:plc:r/app.test.post/a", &body).await;
        seed_record(&pool, "b", &body).await;

        assert_eq!(
            rebuild_once(&pool, backend).await.expect("rebuild"),
            RebuildOutcome::NotNeeded
        );
        assert_eq!(
            refs(&pool).await,
            1,
            "an install with refs is left as it is"
        );
        assert_eq!(
            rebuild_once(&pool, backend).await.expect("again"),
            RebuildOutcome::AlreadyDone
        );
    }

    #[tokio::test]
    async fn a_rebuild_that_never_committed_a_page_resumes_instead_of_skipping() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let backend = DatabaseBackend::Sqlite;
        seed_ref_bearing_records(&pool, 1200).await;

        // Starts, then stops before committing a page.
        assert_eq!(rebuild_pages(&pool, backend, Some(0)).await.unwrap(), None);
        assert_eq!(refs(&pool).await, 0);

        // Live ingest writes a ref in the meantime.
        let body = json!({"subject": "at://did:plc:t/app.test.post/live"});
        seed_record(&pool, "live", &body).await;
        live_ref(&pool, "at://did:plc:r/app.test.post/live", &body).await;

        assert_eq!(
            rebuild_once(&pool, backend).await.expect("resume"),
            RebuildOutcome::Rebuilt { records: 1201 }
        );
        assert_eq!(refs(&pool).await, 3 + 1);
    }
}

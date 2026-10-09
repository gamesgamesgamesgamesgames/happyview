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

/// Refs per multi-row INSERT: three bound parameters each, under SQLite's 999.
pub const REFS_PER_INSERT: usize = 300;

/// Replace `source_uri`'s refs with those in `record`, on a connection the
/// caller owns, so they can share the caller's transaction.
pub async fn sync_refs_in(
    conn: &mut sqlx::AnyConnection,
    source_uri: &str,
    collection: &str,
    record: &Value,
    backend: DatabaseBackend,
) -> Result<(), sqlx::Error> {
    let delete_sql = adapt_sql(
        "DELETE FROM happyview_record_refs WHERE source_uri = ?",
        backend,
    );
    crate::db::query(&delete_sql)
        .bind(source_uri)
        .execute(&mut *conn)
        .await?;

    let targets: Vec<String> = extract_at_uris(record).into_iter().collect();
    for chunk in targets.chunks(REFS_PER_INSERT) {
        let placeholders = vec!["(?, ?, ?)"; chunk.len()].join(", ");
        let insert_sql = adapt_sql(
            &format!(
                "INSERT INTO happyview_record_refs (source_uri, target_uri, collection) VALUES {placeholders} ON CONFLICT DO NOTHING"
            ),
            backend,
        );
        let mut insert = crate::db::query(&insert_sql);
        for target in chunk {
            insert = insert
                .bind(source_uri)
                .bind(target.as_str())
                .bind(collection);
        }
        insert.execute(&mut *conn).await?;
    }

    Ok(())
}

/// Update record_refs for a given source record: delete the old refs and
/// insert the new ones, in one transaction.
pub async fn sync_refs(
    db: &sqlx::AnyPool,
    source_uri: &str,
    collection: &str,
    record: &Value,
    backend: DatabaseBackend,
) -> Result<(), sqlx::Error> {
    let mut tx = db.begin().await?;
    sync_refs_in(&mut tx, source_uri, collection, record, backend).await?;
    tx.commit().await
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

    #[tokio::test]
    async fn sync_refs_replaces_a_records_refs_in_one_go() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let backend = DatabaseBackend::Sqlite;
        crate::db::query(
            "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at) \
             VALUES ('at://did:plc:s/app.test.post/1', 'did:plc:s', 'app.test.post', '1', '{}', 'bafyreiabc', '2026-01-01T00:00:00+00:00', '2026-01-01T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .expect("seed record");
        let targets: Vec<String> = (0..(REFS_PER_INSERT + 5))
            .map(|i| format!("at://did:plc:t/app.test.post/{i}"))
            .collect();
        let record = json!({ "refs": targets });

        sync_refs(
            &pool,
            "at://did:plc:s/app.test.post/1",
            "app.test.post",
            &record,
            backend,
        )
        .await
        .expect("sync refs");
        let (count,): (i64,) = crate::db::query_as("SELECT COUNT(*) FROM happyview_record_refs")
            .fetch_one(&pool)
            .await
            .expect("count refs");
        assert_eq!(
            count as usize,
            REFS_PER_INSERT + 5,
            "every ref lands across insert chunks"
        );

        sync_refs(
            &pool,
            "at://did:plc:s/app.test.post/1",
            "app.test.post",
            &json!({}),
            backend,
        )
        .await
        .expect("clear refs");
        let (count,): (i64,) = crate::db::query_as("SELECT COUNT(*) FROM happyview_record_refs")
            .fetch_one(&pool)
            .await
            .expect("count refs");
        assert_eq!(count, 0);
    }
}

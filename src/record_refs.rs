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

/// Update record_refs for a given source record.
/// Deletes old refs and inserts new ones.
pub async fn sync_refs(
    db: &sqlx::AnyPool,
    source_uri: &str,
    collection: &str,
    record: &Value,
    backend: DatabaseBackend,
) -> Result<(), sqlx::Error> {
    let uris = extract_at_uris(record);

    // Delete existing refs for this source
    let delete_sql = adapt_sql(
        "DELETE FROM happyview_record_refs WHERE source_uri = ?",
        backend,
    );
    crate::db::query(&delete_sql)
        .bind(source_uri)
        .execute(db)
        .await?;

    // Insert new refs
    let insert_sql = adapt_sql(
        "INSERT INTO happyview_record_refs (source_uri, target_uri, collection) VALUES (?, ?, ?) ON CONFLICT DO NOTHING",
        backend,
    );
    for target_uri in &uris {
        crate::db::query(&insert_sql)
            .bind(source_uri)
            .bind(target_uri)
            .bind(collection)
            .execute(db)
            .await?;
    }

    Ok(())
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
}

//! Checks shared by the interop harnesses, so every host is tested the same
//! way.

// Each harness is its own crate and uses a different subset.
#![allow(dead_code)]

use happyview::jobs::native::migrate_space_repo::{expected_commit_hash, replay_batches};
use happyview::spaces::types::SpaceRecord;
use serde_json::{Value, json};

/// Which record values a migration sample carries.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Sample {
    /// Every data-model type, including `$link`, `$bytes` and integers past
    /// 32 bits. What a real repo holds: any record with a blob has a `$link`.
    Full,
    /// Plain JSON only. For hosts known to encode the typed values wrongly, so
    /// the replay machinery can still be checked apart from that defect.
    PlainJson,
}

/// Records as HappyView would hold them before a migration, CIDs included.
///
/// The values are chosen to catch encoding disagreements rather than to look
/// realistic: keys out of canonical order, nesting, unicode, negative numbers,
/// nulls, and (in [`Sample::Full`]) large integers and lexicon `$bytes` and
/// `$link`. There are more than a batch's worth, so the replay spans several
/// `applyWrites` calls.
pub fn records_awaiting_migration(
    space_uri: &str,
    author_did: &str,
    sample: Sample,
) -> Vec<SpaceRecord> {
    let mut records: Vec<SpaceRecord> = (0..57)
        .map(|i| {
            let (collection, mut record) = if i % 2 == 0 {
                (
                    "com.example.note",
                    json!({
                        "$type": "com.example.note",
                        "zeta": i,
                        "text": format!("note {i} — ünïcödé ✓"),
                        "alpha": { "nested": [1, -2, 65_535], "flag": i % 4 == 0 },
                        "nothing": null,
                    }),
                )
            } else {
                (
                    "com.example.reaction",
                    json!({ "$type": "com.example.reaction", "emoji": "🏃", "n": -i }),
                )
            };
            if sample == Sample::Full {
                let extra = record.as_object_mut().unwrap();
                extra.insert("big".into(), json!(9_007_199_254_740_991_i64));
                extra.insert(
                    "subject".into(),
                    json!({ "$link": "bafyreihn37cccblw3mmwmmm6jewugcohecjf5kpzuciirdvisasq7k3siu" }),
                );
                extra.insert("raw".into(), json!({ "$bytes": "AAECAwQFBgc" }));
            }
            let rkey = format!("3mig{i:010}");
            let cid = happyview::cid_verify::compute_record_cid(&record)
                .expect("sample records encode")
                .to_string();
            SpaceRecord {
                uri: format!("{space_uri}/{author_did}/{collection}/{rkey}"),
                space_id: "interop".into(),
                author_did: author_did.into(),
                collection: collection.into(),
                rkey,
                record,
                cid,
                indexed_at: String::new(),
            }
        })
        .collect();
    // The order `list_all_space_records` hands the migration.
    records.sort_by(|a, b| (&a.collection, &a.rkey).cmp(&(&b.collection, &b.rkey)));
    records
}

/// Replay `records` onto `base` with the migration job's own request bodies,
/// then return the host's latest commit.
///
/// If the host derives any CID differently from HappyView, or reads the batches
/// differently, its commit disagrees with [`migration_expects`] and every
/// migration to that host aborts.
///
/// The job sends these bodies through an OAuth session; a password session
/// reaches the same handler, which is what lets this run without an OAuth
/// client.
pub async fn replay_as_migration(
    base: &str,
    token: &str,
    space_uri: &str,
    author_did: &str,
    records: &[SpaceRecord],
) -> Value {
    let http = reqwest::Client::new();
    let batches = replay_batches(space_uri, author_did, records);
    assert!(batches.len() > 1, "the sample must span several batches");

    for body in &batches {
        let resp = http
            .post(format!("{base}/xrpc/com.atproto.space.applyWrites"))
            .bearer_auth(token)
            .json(body)
            .send()
            .await
            .expect("applyWrites request");
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        assert!(
            status.is_success(),
            "applyWrites rejected a batch ({status}): {text}"
        );
    }

    let resp = http
        .get(format!("{base}/xrpc/com.atproto.space.getLatestCommit"))
        .query(&[("space", space_uri), ("repo", author_did)])
        .bearer_auth(token)
        .send()
        .await
        .expect("getLatestCommit request");
    assert_eq!(resp.status().as_u16(), 200);
    resp.json::<Value>().await.expect("commit json")["commit"].clone()
}

/// Records whose CID on the host differs from HappyView's, as
/// `(collection/rkey, ours, theirs)`. Empty when they all agree.
///
/// A hash mismatch says only that the sets differ; this says which record and
/// so which value the two implementations encode differently.
pub async fn cid_disagreements(
    base: &str,
    token: &str,
    space_uri: &str,
    author_did: &str,
    records: &[SpaceRecord],
) -> Vec<(String, String, String)> {
    let http = reqwest::Client::new();
    let mut theirs = std::collections::HashMap::new();
    let mut collections: Vec<&str> = records.iter().map(|r| r.collection.as_str()).collect();
    collections.dedup();
    for collection in collections {
        let mut cursor: Option<String> = None;
        loop {
            let mut query = vec![
                ("space", space_uri.to_string()),
                ("repo", author_did.to_string()),
                ("collection", collection.to_string()),
                ("limit", "100".to_string()),
            ];
            if let Some(c) = &cursor {
                query.push(("cursor", c.clone()));
            }
            let body: Value = http
                .get(format!("{base}/xrpc/com.atproto.space.listRecords"))
                .query(&query)
                .bearer_auth(token)
                .send()
                .await
                .expect("listRecords request")
                .json()
                .await
                .expect("listRecords json");
            let page = body["records"].as_array().cloned().unwrap_or_default();
            for r in &page {
                theirs.insert(
                    format!("{collection}/{}", r["rkey"].as_str().unwrap_or_default()),
                    r["cid"].as_str().unwrap_or_default().to_string(),
                );
            }
            match body["cursor"].as_str() {
                Some(next) if !page.is_empty() && cursor.as_deref() != Some(next) => {
                    cursor = Some(next.to_string())
                }
                _ => break,
            }
        }
    }

    records
        .iter()
        .filter_map(|r| {
            let key = format!("{}/{}", r.collection, r.rkey);
            let host = theirs
                .get(&key)
                .cloned()
                .unwrap_or_else(|| "<missing>".into());
            (host != r.cid).then(|| (key, r.cid.clone(), host))
        })
        .collect()
}

/// The hash the migration requires the host's commit to carry.
pub fn migration_expects(records: &[SpaceRecord]) -> [u8; 32] {
    expected_commit_hash(records)
}

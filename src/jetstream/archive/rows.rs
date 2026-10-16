//! Archive rows as the events live ingest applies, and the exact filter a
//! replay runs on decoded rows (the plan only prunes, with false positives).

use std::collections::HashSet;

use serde::Deserialize;

use super::segment::Row;
use crate::jetstream::event::{Commit, CursorPoint, Event, EventKind};

pub const KIND_CREATE: u8 = 1;
pub const KIND_UPDATE: u8 = 2;
pub const KIND_DELETE: u8 = 3;
pub const KIND_IDENTITY: u8 = 4;
pub const KIND_ACCOUNT: u8 = 5;
pub const KIND_SYNC: u8 = 6;
pub const KIND_RESYNC_CREATE: u8 = 7;

pub fn is_commit(kind: u8) -> bool {
    matches!(
        kind,
        KIND_CREATE | KIND_UPDATE | KIND_DELETE | KIND_RESYNC_CREATE
    )
}

pub fn is_marker(kind: u8) -> bool {
    matches!(kind, KIND_ACCOUNT | KIND_SYNC)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKinds {
    Commits,
    Markers,
    Both,
}

/// What a sweep keeps of a decoded block.
#[derive(Clone, Debug)]
pub struct RowFilter {
    pub kinds: RowKinds,
    /// Collections a commit must be in. Markers have none.
    pub collections: HashSet<String>,
    /// When set, the only DIDs kept.
    pub dids: Option<HashSet<String>>,
    /// Exclusive.
    pub after_seq: u64,
    /// Inclusive: the sweep's pinned tip.
    pub through_seq: u64,
}

impl RowFilter {
    pub fn keep(&self, row: &Row) -> bool {
        if row.seq <= self.after_seq || row.seq > self.through_seq {
            return false;
        }
        let kind_ok = match self.kinds {
            RowKinds::Commits => is_commit(row.kind),
            RowKinds::Markers => is_marker(row.kind),
            RowKinds::Both => is_commit(row.kind) || is_marker(row.kind),
        };
        if !kind_ok {
            return false;
        }
        if is_commit(row.kind) && !self.collections.contains(&row.collection) {
            return false;
        }
        self.dids
            .as_ref()
            .is_none_or(|dids| dids.contains(&row.did))
    }
}

#[derive(Deserialize)]
struct AccountMarker {
    active: bool,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Deserialize)]
struct SyncMarker {
    #[serde(default)]
    rev: Option<String>,
}

/// `row` as the event the live consumer would have applied. `None` for
/// identity rows and kinds this build doesn't know; an error for a payload
/// that doesn't decode.
pub fn row_to_event(row: Row) -> Result<Option<Event>, String> {
    let cursor = CursorPoint::V2 {
        seq: row.seq as i64,
        witnessed_us: row.witnessed_at,
    };
    let kind = match row.kind {
        KIND_CREATE | KIND_UPDATE | KIND_RESYNC_CREATE | KIND_DELETE => {
            let operation = match row.kind {
                KIND_UPDATE => "update",
                KIND_DELETE => "delete",
                _ => "create",
            };
            let (record, cid) = if row.kind == KIND_DELETE {
                (None, None)
            } else {
                let record = crate::cid_verify::dag_cbor_to_json(&row.payload)
                    .map_err(|e| format!("record at seq {} does not decode: {e}", row.seq))?;
                let cid = crate::cid_verify::dag_cbor_cid(&row.payload)
                    .ok_or_else(|| format!("record at seq {} has no CID", row.seq))?;
                (Some(record), Some(cid.to_string()))
            };
            EventKind::Commit(Commit {
                rev: Some(row.rev).filter(|r| !r.is_empty()),
                operation: operation.to_string(),
                collection: row.collection,
                rkey: row.rkey,
                record,
                cid,
            })
        }
        KIND_ACCOUNT => {
            let marker: AccountMarker = serde_ipld_dagcbor::from_slice(&row.payload)
                .map_err(|e| format!("account marker at seq {} does not decode: {e}", row.seq))?;
            EventKind::Account {
                active: marker.active,
                status: marker.status,
            }
        }
        KIND_SYNC => {
            let marker: SyncMarker = serde_ipld_dagcbor::from_slice(&row.payload)
                .map_err(|e| format!("sync marker at seq {} does not decode: {e}", row.seq))?;
            EventKind::Sync { rev: marker.rev }
        }
        _ => return Ok(None),
    };
    Ok(Some(Event {
        did: row.did,
        cursor,
        kind,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jetstream::archive::segment::{decode_block, read_block_frame, read_index};
    use std::io::Cursor;

    fn row(seq: u64, kind: u8, collection: &str, did: &str, payload: Vec<u8>) -> Row {
        Row {
            seq,
            witnessed_at: 1_750_000_000_000_000 + seq as i64,
            indexed_at: 0,
            kind,
            collection: collection.into(),
            did: did.into(),
            rkey: if is_commit(kind) {
                format!("r{seq}")
            } else {
                String::new()
            },
            rev: if is_commit(kind) {
                "3migo7fijwi2k".into()
            } else {
                String::new()
            },
            payload,
        }
    }

    fn cbor(value: serde_json::Value) -> Vec<u8> {
        crate::cid_verify::record_to_dag_cbor(&value).unwrap()
    }

    fn filter(kinds: RowKinds, dids: Option<&[&str]>) -> RowFilter {
        RowFilter {
            kinds,
            collections: ["app.test.post".to_string()].into(),
            dids: dids.map(|d| d.iter().map(|s| s.to_string()).collect()),
            after_seq: 10,
            through_seq: 20,
        }
    }

    #[test]
    fn the_filter_is_exact() {
        let post = |seq, kind| row(seq, kind, "app.test.post", "did:plc:a", vec![]);
        let f = filter(RowKinds::Commits, None);
        assert!(!f.keep(&post(10, KIND_CREATE)), "afterSeq is exclusive");
        assert!(f.keep(&post(11, KIND_CREATE)));
        assert!(f.keep(&post(20, KIND_DELETE)), "the tip is inclusive");
        assert!(!f.keep(&post(21, KIND_CREATE)));
        assert!(f.keep(&post(12, KIND_RESYNC_CREATE)));
        assert!(!f.keep(&row(12, KIND_CREATE, "app.test.like", "did:plc:a", vec![])));
        assert!(
            !f.keep(&row(12, KIND_ACCOUNT, "", "did:plc:a", vec![])),
            "commits only"
        );

        let f = filter(RowKinds::Markers, Some(&["did:plc:a"]));
        assert!(f.keep(&row(12, KIND_ACCOUNT, "", "did:plc:a", vec![])));
        assert!(f.keep(&row(12, KIND_SYNC, "", "did:plc:a", vec![])));
        assert!(
            !f.keep(&row(12, KIND_ACCOUNT, "", "did:plc:b", vec![])),
            "other DID"
        );
        assert!(!f.keep(&row(12, KIND_IDENTITY, "", "did:plc:a", vec![])));
        assert!(!f.keep(&post(12, KIND_CREATE)), "markers only");

        let f = filter(RowKinds::Both, Some(&["did:plc:a"]));
        assert!(f.keep(&post(12, KIND_CREATE)));
        assert!(f.keep(&row(12, KIND_SYNC, "", "did:plc:a", vec![])));
        assert!(!f.keep(&row(12, KIND_CREATE, "app.test.post", "did:plc:b", vec![])));
    }

    #[test]
    fn golden_rows_become_creates_with_their_record_and_cid() {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/jetstream/corpus/golden_corpus_segment.jss"
        ))
        .unwrap();
        let cids: Vec<String> = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/jetstream/corpus/car_expected_records.tsv"
        ))
        .unwrap()
        .lines()
        .map(|l| l.split('\t').nth(1).unwrap().to_string())
        .collect();
        let mut file = Cursor::new(&bytes);
        let mut events = Vec::new();
        for entry in read_index(&mut file, bytes.len() as u64).unwrap() {
            for row in decode_block(&read_block_frame(&mut file, &entry).unwrap()).unwrap() {
                events.push(row_to_event(row).unwrap().unwrap());
            }
        }
        assert_eq!(events.len(), 23);
        for (i, (event, cid)) in events.iter().zip(&cids).enumerate() {
            assert_eq!(event.did, "did:plc:53iucmf5ldftgqyf2ac7hmhd");
            assert_eq!(
                event.cursor,
                CursorPoint::V2 {
                    seq: i as i64 + 1,
                    witnessed_us: 1_750_000_000_000_001 + i as i64
                }
            );
            let EventKind::Commit(commit) = &event.kind else {
                panic!("a commit")
            };
            assert_eq!(commit.operation, "create");
            assert_eq!(commit.rev.as_deref(), Some("3migo7fijwi2k"));
            assert_eq!(commit.cid.as_deref(), Some(cid.as_str()));
            let record = commit.record.as_ref().expect("a record");
            assert_eq!(
                crate::cid_verify::verify_record_cid(cid, record),
                crate::cid_verify::CidCheck::Match
            );
        }
    }

    #[test]
    fn updates_deletes_and_resyncs_map_to_their_operations() {
        let body = cbor(serde_json::json!({ "$type": "app.test.post", "text": "hi" }));
        let op = |kind, payload: Vec<u8>| {
            let event = row_to_event(row(12, kind, "app.test.post", "did:plc:a", payload))
                .unwrap()
                .unwrap();
            let EventKind::Commit(commit) = event.kind else {
                panic!()
            };
            commit
        };
        assert_eq!(op(KIND_UPDATE, body.clone()).operation, "update");
        assert_eq!(op(KIND_RESYNC_CREATE, body.clone()).operation, "create");
        let delete = op(KIND_DELETE, vec![]);
        assert_eq!(delete.operation, "delete");
        assert_eq!((delete.record, delete.cid), (None, None));
        assert_eq!(delete.rev.as_deref(), Some("3migo7fijwi2k"));
        assert_eq!(delete.rkey, "r12");
    }

    #[test]
    fn account_and_sync_markers_decode_their_cbor() {
        let account = cbor(serde_json::json!({
            "seq": 99, "did": "did:plc:a", "time": "2026-10-01T00:00:00.000Z",
            "active": false, "status": "takendown"
        }));
        let event = row_to_event(row(12, KIND_ACCOUNT, "", "did:plc:a", account))
            .unwrap()
            .unwrap();
        assert_eq!(
            event.kind,
            EventKind::Account {
                active: false,
                status: Some("takendown".into())
            }
        );

        let active =
            cbor(serde_json::json!({ "seq": 1, "did": "did:plc:a", "time": "t", "active": true }));
        let event = row_to_event(row(13, KIND_ACCOUNT, "", "did:plc:a", active))
            .unwrap()
            .unwrap();
        assert_eq!(
            event.kind,
            EventKind::Account {
                active: true,
                status: None
            }
        );

        let sync = cbor(serde_json::json!({
            "seq": 7, "did": "did:plc:a", "time": "t", "rev": "3mabc", "blocks": { "$bytes": "AAAA" }
        }));
        let event = row_to_event(row(14, KIND_SYNC, "", "did:plc:a", sync))
            .unwrap()
            .unwrap();
        assert_eq!(
            event.kind,
            EventKind::Sync {
                rev: Some("3mabc".into())
            }
        );
    }

    #[test]
    fn identity_and_unknown_kinds_are_skipped_and_bad_payloads_are_errors() {
        assert_eq!(
            row_to_event(row(12, KIND_IDENTITY, "", "did:plc:a", vec![])).unwrap(),
            None
        );
        assert_eq!(
            row_to_event(row(12, 42, "", "did:plc:a", vec![])).unwrap(),
            None
        );
        assert!(
            row_to_event(row(
                12,
                KIND_CREATE,
                "app.test.post",
                "did:plc:a",
                vec![0xff]
            ))
            .is_err()
        );
        assert!(row_to_event(row(12, KIND_ACCOUNT, "", "did:plc:a", vec![0xff])).is_err());
    }

    #[test]
    fn indexed_at_is_ignored_for_the_cursor() {
        // `witnessed_us` drives tombstone revs and host-change resumes, so it is
        // the witnessed time even when the archive one day sets indexed_at.
        let mut r = row(12, KIND_DELETE, "app.test.post", "did:plc:a", vec![]);
        r.indexed_at = 5;
        let event = row_to_event(r).unwrap().unwrap();
        assert_eq!(
            event.cursor,
            CursorPoint::V2 {
                seq: 12,
                witnessed_us: 1_750_000_000_000_012
            }
        );
    }
}

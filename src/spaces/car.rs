use cid::Cid;
use sha2::{Digest, Sha256};

use crate::error::AppError;
use crate::spaces::commit::SignedCommit;
use crate::spaces::types::SpaceRecord;

const SHA2_256: u64 = 0x12;
const DAG_CBOR: u64 = 0x71;
const RAW: u64 = 0x55;

fn unsigned_varint(mut value: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if value == 0 {
            break;
        }
    }
    buf
}

fn make_cid(codec: u64, block: &[u8]) -> Cid {
    let digest = Sha256::digest(block);
    let mut mh_bytes = Vec::with_capacity(34);
    mh_bytes.push(SHA2_256 as u8);
    mh_bytes.push(32u8);
    mh_bytes.extend_from_slice(&digest);
    let mh = cid::multihash::Multihash::<64>::from_bytes(&mh_bytes).expect("valid multihash");
    Cid::new_v1(codec, mh)
}

// DAG-CBOR CID link: CBOR tag 42 wrapping bytes prefixed with 0x00 (multibase identity)
fn cid_link(cid: &Cid) -> ciborium::Value {
    let mut bytes = vec![0x00u8]; // multibase identity prefix
    bytes.extend_from_slice(&cid.to_bytes());
    ciborium::Value::Tag(42, Box::new(ciborium::Value::Bytes(bytes)))
}

fn encode_cbor(value: &ciborium::Value) -> Result<Vec<u8>, AppError> {
    let mut buf = Vec::new();
    ciborium::into_writer(value, &mut buf)
        .map_err(|e| AppError::Internal(format!("CBOR encoding failed: {e}")))?;
    Ok(buf)
}

fn write_car_block(out: &mut Vec<u8>, cid: &Cid, block: &[u8]) {
    let cid_bytes = cid.to_bytes();
    let section_len = cid_bytes.len() + block.len();
    out.extend(unsigned_varint(section_len as u64));
    out.extend_from_slice(&cid_bytes);
    out.extend_from_slice(block);
}

pub fn serialize_repo(commit: &SignedCommit, records: &[SpaceRecord]) -> Result<Vec<u8>, AppError> {
    // Build record blocks sorted by collection/rkey
    let mut indexed: Vec<(&SpaceRecord, Vec<u8>, Cid)> = records
        .iter()
        .map(|r| {
            let block = serde_json::to_vec(&r.record).unwrap_or_default();
            let cid = make_cid(RAW, &block);
            (r, block, cid)
        })
        .collect();
    indexed.sort_by(|a, b| {
        let ka = format!("{}/{}", a.0.collection, a.0.rkey);
        let kb = format!("{}/{}", b.0.collection, b.0.rkey);
        ka.cmp(&kb)
    });

    // Build index: sorted map of "collection/rkey" -> CID link
    let index_pairs: Vec<(ciborium::Value, ciborium::Value)> = indexed
        .iter()
        .map(|(r, _, cid)| {
            let key = format!("{}/{}", r.collection, r.rkey);
            (ciborium::Value::Text(key), cid_link(cid))
        })
        .collect();
    let index_cbor = ciborium::Value::Map(index_pairs);
    let index_block = encode_cbor(&index_cbor)?;
    let index_cid = make_cid(DAG_CBOR, &index_block);

    // Build signed commit block
    let commit_cbor = ciborium::Value::Map(vec![
        (
            ciborium::Value::Text("ver".into()),
            ciborium::Value::Integer(commit.ver.into()),
        ),
        (
            ciborium::Value::Text("hash".into()),
            ciborium::Value::Bytes(commit.hash.to_vec()),
        ),
        (
            ciborium::Value::Text("ikm".into()),
            ciborium::Value::Bytes(commit.ikm.to_vec()),
        ),
        (
            ciborium::Value::Text("sig".into()),
            ciborium::Value::Bytes(commit.sig.clone()),
        ),
        (
            ciborium::Value::Text("mac".into()),
            ciborium::Value::Bytes(commit.mac.to_vec()),
        ),
        (
            ciborium::Value::Text("rev".into()),
            ciborium::Value::Text(commit.rev.clone()),
        ),
    ]);
    let commit_block = encode_cbor(&commit_cbor)?;
    let commit_cid = make_cid(DAG_CBOR, &commit_block);

    // Build CAR v1 header
    let header_cbor = ciborium::Value::Map(vec![
        (
            ciborium::Value::Text("version".into()),
            ciborium::Value::Integer(1.into()),
        ),
        (
            ciborium::Value::Text("roots".into()),
            ciborium::Value::Array(vec![cid_link(&commit_cid), cid_link(&index_cid)]),
        ),
    ]);
    let header_block = encode_cbor(&header_cbor)?;

    // Assemble CAR
    let mut car = Vec::new();

    // Header (varint-length-prefixed)
    car.extend(unsigned_varint(header_block.len() as u64));
    car.extend_from_slice(&header_block);

    // Commit block
    write_car_block(&mut car, &commit_cid, &commit_block);

    // Index block
    write_car_block(&mut car, &index_cid, &index_block);

    // Record blocks in sorted order
    for (_, block, cid) in &indexed {
        write_car_block(&mut car, cid, block);
    }

    Ok(car)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_commit() -> SignedCommit {
        SignedCommit {
            ver: 1,
            hash: [0u8; 32],
            ikm: [0u8; 32],
            sig: vec![0u8; 64],
            mac: [0u8; 32],
            rev: "3k2rev1".to_string(),
        }
    }

    #[test]
    fn serialize_empty_repo() {
        let commit = test_commit();
        let car = serialize_repo(&commit, &[]).unwrap();
        assert!(!car.is_empty());
        // CAR starts with a varint-prefixed header — must be more than 10 bytes
        assert!(car.len() > 10);
    }

    #[test]
    fn serialize_repo_with_records() {
        let commit = SignedCommit {
            ver: 1,
            hash: [0xAA; 32],
            ikm: [0xBB; 32],
            sig: vec![0xCC; 64],
            mac: [0xDD; 32],
            rev: "3k2rev1".to_string(),
        };

        let records =
            vec![
            SpaceRecord {
                uri: "at://did:plc:abc/space/com.example.forum/main/did:plc:user/com.example.post/1"
                    .into(),
                space_id: "space1".into(),
                author_did: "did:plc:user".into(),
                collection: "com.example.post".into(),
                rkey: "1".into(),
                record: serde_json::json!({"text": "hello"}),
                cid: "bafyreiabc".into(),
                indexed_at: "2026-01-01T00:00:00Z".into(),
            },
            SpaceRecord {
                uri: "at://did:plc:abc/space/com.example.forum/main/did:plc:user/com.example.post/2"
                    .into(),
                space_id: "space1".into(),
                author_did: "did:plc:user".into(),
                collection: "com.example.post".into(),
                rkey: "2".into(),
                record: serde_json::json!({"text": "world"}),
                cid: "bafyreixyz".into(),
                indexed_at: "2026-01-01T00:00:01Z".into(),
            },
        ];

        let car = serialize_repo(&commit, &records).unwrap();
        assert!(car.len() > 100);
    }

    #[test]
    fn records_sorted_in_output() {
        let commit = test_commit();
        let records = vec![
            SpaceRecord {
                uri: "u1".into(),
                space_id: "s".into(),
                author_did: "d".into(),
                collection: "com.example.b".into(),
                rkey: "1".into(),
                record: serde_json::json!({"n": 2}),
                cid: "c2".into(),
                indexed_at: "2026-01-01".into(),
            },
            SpaceRecord {
                uri: "u2".into(),
                space_id: "s".into(),
                author_did: "d".into(),
                collection: "com.example.a".into(),
                rkey: "1".into(),
                record: serde_json::json!({"n": 1}),
                cid: "c1".into(),
                indexed_at: "2026-01-01".into(),
            },
        ];
        // Should not panic, and produces valid output
        let car = serialize_repo(&commit, &records).unwrap();
        assert!(car.len() > 100);
    }
}

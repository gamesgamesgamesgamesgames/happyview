use cid::Cid;
use cid::multihash::Multihash;
use ipld_core::ipld::Ipld;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::str::FromStr;

const DAG_CBOR_CODEC: u64 = 0x71;
const SHA2_256_CODE: u64 = 0x12;

/// Outcome of checking a claimed CID against a record's content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CidCheck {
    /// The claimed CID matches the CID recomputed from the record content.
    Match,
    /// The claimed CID is present but does not match the record content
    /// (malformed or content-mismatched) — the caller should reject the record.
    Mismatch,
    /// Verification was not attempted or not possible (no claimed CID, or the
    /// value could not be encoded to DAG-CBOR) — the caller should proceed
    /// without rejecting, to avoid dropping records over an encoder limitation.
    Skipped,
}

fn atproto_json_to_ipld(value: &Value) -> Option<Ipld> {
    Some(match value {
        Value::Null => Ipld::Null,
        Value::Bool(b) => Ipld::Bool(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ipld::Integer(i as i128)
            } else if let Some(u) = n.as_u64() {
                Ipld::Integer(u as i128)
            } else if let Some(f) = n.as_f64() {
                Ipld::Float(f)
            } else {
                return None;
            }
        }
        Value::String(s) => Ipld::String(s.clone()),
        Value::Array(arr) => {
            let mut items = Vec::with_capacity(arr.len());
            for v in arr {
                items.push(atproto_json_to_ipld(v)?);
            }
            Ipld::List(items)
        }
        Value::Object(obj) => {
            if obj.len() == 1 {
                if let Some(Value::String(link)) = obj.get("$link") {
                    return Some(Ipld::Link(Cid::from_str(link).ok()?));
                }
                if let Some(Value::String(b64)) = obj.get("$bytes") {
                    let bytes =
                        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
                            .ok()?;
                    return Some(Ipld::Bytes(bytes));
                }
            }
            let mut map = BTreeMap::new();
            for (k, v) in obj {
                map.insert(k.clone(), atproto_json_to_ipld(v)?);
            }
            Ipld::Map(map)
        }
    })
}

pub fn record_to_dag_cbor(value: &Value) -> Option<Vec<u8>> {
    let ipld = atproto_json_to_ipld(value)?;
    serde_ipld_dagcbor::to_vec(&ipld).ok()
}

pub fn dag_cbor_cid(cbor: &[u8]) -> Option<Cid> {
    let digest = Sha256::digest(cbor);

    // multihash: <code=0x12><len=0x20><digest>
    let mut mh_bytes = Vec::with_capacity(2 + digest.len());
    mh_bytes.push(SHA2_256_CODE as u8);
    mh_bytes.push(digest.len() as u8);
    mh_bytes.extend_from_slice(&digest);
    let multihash = Multihash::<64>::from_bytes(&mh_bytes).ok()?;

    Some(Cid::new_v1(DAG_CBOR_CODEC, multihash))
}

pub fn compute_record_cid(value: &Value) -> Option<Cid> {
    dag_cbor_cid(&record_to_dag_cbor(value)?)
}

pub fn verify_record_cid(claimed_cid: &str, value: &Value) -> CidCheck {
    if claimed_cid.is_empty() {
        return CidCheck::Skipped;
    }
    let computed = match compute_record_cid(value) {
        Some(cid) => cid,
        None => return CidCheck::Skipped,
    };
    match Cid::from_str(claimed_cid) {
        Ok(claimed) if claimed == computed => CidCheck::Match,
        _ => CidCheck::Mismatch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const EMPTY_MAP_CID: &str = "bafyreigbtj4x7ip5legnfznufuopl4sg4knzc2cof6duas4b3q2fy6swua";
    const A1_CID: &str = "bafyreihltcnuuyqp2jm24aqydpnlj7b6w3ogwrplomrjtg5rifv44mmjey";
    const ORDERING_CID: &str = "bafyreihbaf6v4gjeo76rl6ncekrny5lwbgyjf7zdw2m7w77xsjm3xvige4";

    #[test]
    fn computes_known_cid_for_empty_map() {
        assert_eq!(
            compute_record_cid(&json!({}))
                .expect("encodable")
                .to_string(),
            EMPTY_MAP_CID
        );
    }

    #[test]
    fn computes_known_cid_for_small_record() {
        assert_eq!(
            compute_record_cid(&json!({ "a": 1 }))
                .expect("encodable")
                .to_string(),
            A1_CID
        );
    }

    #[test]
    fn uses_length_first_canonical_key_ordering() {
        // Input order is deliberately NOT the canonical order.
        assert_eq!(
            compute_record_cid(&json!({ "aa": 2, "b": 1 }))
                .expect("encodable")
                .to_string(),
            ORDERING_CID
        );
    }

    #[test]
    fn verify_matches_recomputed_cid() {
        let value = json!({
            "$type": "app.bsky.feed.post",
            "text": "hello",
            "createdAt": "2023-01-01T00:00:00.000Z"
        });
        let cid = compute_record_cid(&value).expect("encodable").to_string();
        assert_eq!(verify_record_cid(&cid, &value), CidCheck::Match);
    }

    #[test]
    fn verify_detects_content_mismatch() {
        assert_eq!(
            verify_record_cid(EMPTY_MAP_CID, &json!({ "text": "hello" })),
            CidCheck::Mismatch
        );
    }

    #[test]
    fn verify_treats_unparseable_claimed_cid_as_mismatch() {
        assert_eq!(
            verify_record_cid("not-a-real-cid", &json!({ "text": "hi" })),
            CidCheck::Mismatch
        );
    }

    #[test]
    fn verify_skips_when_no_claimed_cid() {
        assert_eq!(
            verify_record_cid("", &json!({ "text": "hi" })),
            CidCheck::Skipped
        );
    }

    #[test]
    fn link_encodes_as_ipld_link_not_string() {
        let as_link = json!({ "ref": { "$link": EMPTY_MAP_CID } });
        let as_string = json!({ "ref": EMPTY_MAP_CID });
        assert_ne!(
            compute_record_cid(&as_link).expect("encodable"),
            compute_record_cid(&as_string).expect("encodable"),
        );
    }

    #[test]
    fn bytes_encode_as_byte_string_not_text() {
        let as_bytes = json!({ "data": { "$bytes": "aGVsbG8=" } }); // "hello"
        let as_string = json!({ "data": "aGVsbG8=" });
        assert_ne!(
            compute_record_cid(&as_bytes).expect("encodable"),
            compute_record_cid(&as_string).expect("encodable"),
        );
    }
}

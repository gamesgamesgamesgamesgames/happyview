use cid::Cid;
use cid::multihash::Multihash;
use ipld_core::ipld::Ipld;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::str::FromStr;

const DAG_CBOR_CODEC: u64 = 0x71;
/// The codec a PDS assigns a blob: bytes with no interpretation, as opposed to
/// a record, which is dag-cbor.
const RAW_CODEC: u64 = 0x55;
const SHA2_256_CODE: u64 = 0x12;

/// The codec for `$bytes` in the atproto data model — the one place that
/// encoding is decided, so every producer and consumer of `$bytes` agrees.
///
/// The data model specifies RFC-4648 §4 base64 in which `=` padding is
/// **optional**. Both forms name the same bytes, so both must decode:
/// jetstream emits `$bytes` unpadded, our own signer emits it padded, and a
/// signature we wrote ourselves comes back off the firehose two characters
/// shorter. A padding-strict engine reads that as corruption and reports a
/// record we signed as unverifiable — which downstream becomes an accusation
/// of forgery aimed at the record's author.
///
/// Encoding is unchanged from `general_purpose::STANDARD` (padding on,
/// standard alphabet); only the decoder is made indifferent.
pub const BYTES_B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::general_purpose::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
    );

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
                    let bytes = base64::Engine::decode(&BYTES_B64, b64).ok()?;
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

/// `<code=0x12><len=0x20><digest>`, which both CID forms below carry: they
/// differ only in the codec naming how the bytes are to be read.
fn sha256_multihash(bytes: &[u8]) -> Option<Multihash<64>> {
    let digest = Sha256::digest(bytes);

    let mut mh_bytes = Vec::with_capacity(2 + digest.len());
    mh_bytes.push(SHA2_256_CODE as u8);
    mh_bytes.push(digest.len() as u8);
    mh_bytes.extend_from_slice(&digest);
    Multihash::<64>::from_bytes(&mh_bytes).ok()
}

pub fn dag_cbor_cid(cbor: &[u8]) -> Option<Cid> {
    Some(Cid::new_v1(DAG_CBOR_CODEC, sha256_multihash(cbor)?))
}

/// The CID of arbitrary bytes: CIDv1, raw codec, sha2-256.
///
/// This is what a PDS assigns an uploaded blob and what a blob ref's `$link`
/// carries, so a CID minted here is the same string the network would produce
/// for the same bytes. Keyed storage can therefore use it as the identity of
/// the content: there is no way to file bytes under a CID they do not hash to.
pub fn raw_cid(bytes: &[u8]) -> Option<Cid> {
    Some(Cid::new_v1(RAW_CODEC, sha256_multihash(bytes)?))
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
    /// Raw-codec CIDs, computed outside this crate so a regression in
    /// `raw_cid` cannot agree with its own expectation. The encoder that
    /// produced them was checked against a live blob ref from `bsky.app`'s
    /// profile record, which decoded to version 1, codec 0x55, sha2-256.
    const EMPTY_RAW_CID: &str = "bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku";
    const HAPPYVIEW_RAW_CID: &str = "bafkreicpj5vjguacisspt5tjas2gbzub4c26ylz5nkgwczms23lnd7hwm4";
    const A1_CID: &str = "bafyreihltcnuuyqp2jm24aqydpnlj7b6w3ogwrplomrjtg5rifv44mmjey";
    const ORDERING_CID: &str = "bafyreihbaf6v4gjeo76rl6ncekrny5lwbgyjf7zdw2m7w77xsjm3xvige4";

    #[test]
    fn raw_cid_matches_what_the_network_would_mint() {
        assert_eq!(
            raw_cid(b"").expect("empty bytes CID").to_string(),
            EMPTY_RAW_CID
        );
        assert_eq!(
            raw_cid(b"happyview").expect("CID").to_string(),
            HAPPYVIEW_RAW_CID
        );
    }

    #[test]
    fn raw_cid_carries_the_raw_codec_and_the_content_digest() {
        let bytes = b"some artifact bytes";
        let cid = raw_cid(bytes).expect("CID");

        assert_eq!(cid.version(), cid::Version::V1);
        assert_eq!(cid.codec(), RAW_CODEC);
        assert_eq!(cid.hash().code(), SHA2_256_CODE);
        // The digest is over the content itself, which is what makes the CID
        // usable as storage identity.
        assert_eq!(cid.hash().digest(), Sha256::digest(bytes).as_slice());
    }

    /// The codec is the whole difference, so the same bytes must not land on
    /// the same CID under both. A blob filed under a record's CID would be
    /// unfetchable by any conforming client.
    #[test]
    fn a_blob_and_a_record_over_identical_bytes_get_different_cids() {
        let bytes = b"\xa0";
        assert_ne!(
            raw_cid(bytes).expect("raw"),
            dag_cbor_cid(bytes).expect("dag-cbor")
        );
    }

    #[test]
    fn a_raw_cid_round_trips_through_its_string_form() {
        let cid = raw_cid(b"happyview").expect("CID");
        assert_eq!(Cid::from_str(&cid.to_string()).expect("parse"), cid);
    }

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

    /// `=` padding on `$bytes` is optional in the data model, so the same
    /// bytes may arrive either way and must yield the same CID. If the
    /// unpadded form fails to encode, `verify_record_cid` degrades to
    /// `Skipped` and the record is indexed with its CID unchecked.
    #[test]
    fn padded_and_unpadded_bytes_produce_the_same_cid() {
        let padded = json!({ "sig": { "$bytes": "3q2+7w==" } });
        let unpadded = json!({ "sig": { "$bytes": "3q2+7w" } });

        let padded_cid = compute_record_cid(&padded).expect("padded is encodable");
        assert_eq!(
            compute_record_cid(&unpadded),
            Some(padded_cid),
            "unpadded $bytes must encode to the same CID"
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

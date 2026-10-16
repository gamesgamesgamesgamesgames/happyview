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

/// A DAG-CBOR record (the bytes a repo stores, and the payload of a Jetstream
/// archive row) as atproto JSON: links as `{"$link": cid}` and bytes as
/// `{"$bytes": base64}`, unpadded as Jetstream emits them. The inverse of
/// [`record_to_dag_cbor`] for every value the atproto data model allows.
pub fn dag_cbor_to_json(cbor: &[u8]) -> Result<Value, String> {
    let ipld: Ipld = serde_ipld_dagcbor::from_slice(cbor).map_err(|e| e.to_string())?;
    ipld_to_atproto_json(ipld)
}

fn ipld_to_atproto_json(ipld: Ipld) -> Result<Value, String> {
    Ok(match ipld {
        Ipld::Null => Value::Null,
        Ipld::Bool(b) => Value::Bool(b),
        Ipld::Integer(i) => {
            if let Ok(i) = i64::try_from(i) {
                Value::from(i)
            } else if let Ok(u) = u64::try_from(i) {
                Value::from(u)
            } else {
                return Err(format!("integer {i} is out of range"));
            }
        }
        Ipld::Float(f) => serde_json::Number::from_f64(f)
            .map(Value::Number)
            .ok_or_else(|| format!("float {f} has no JSON form"))?,
        Ipld::String(s) => Value::String(s),
        Ipld::Bytes(bytes) => serde_json::json!({
            "$bytes": base64::Engine::encode(&base64::engine::general_purpose::STANDARD_NO_PAD, bytes)
        }),
        Ipld::List(items) => Value::Array(
            items
                .into_iter()
                .map(ipld_to_atproto_json)
                .collect::<Result<_, _>>()?,
        ),
        Ipld::Map(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| Ok((k, ipld_to_atproto_json(v)?)))
                .collect::<Result<_, String>>()?,
        ),
        Ipld::Link(cid) => serde_json::json!({ "$link": cid.to_string() }),
    })
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

    /// Split the golden segment's raw record payloads out without the segment
    /// reader (Task 3), so this test does not depend on it: walk the blocks by
    /// the index and read the payload column of each.
    fn golden_payloads() -> Vec<Vec<u8>> {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/jetstream/corpus/golden_corpus_segment.jss"
        ))
        .expect("golden segment");
        let u32_at = |b: &[u8], o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let u64_at = |b: &[u8], o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        let block_count = u32_at(&bytes, 14) as usize;
        let index = u64_at(&bytes, 90) as usize;
        let mut payloads = Vec::new();
        for i in 0..block_count {
            let entry = index + i * 52;
            let offset = u64_at(&bytes, entry) as usize;
            let size = u32_at(&bytes, entry + 8) as usize;
            let frame = &bytes[offset + 8..offset + 8 + size];
            let block = zstd::bulk::decompress(frame, 64 << 20).expect("block");
            let n = u32_at(&block, 0) as usize;
            // seq, witnessed_at, indexed_at (8 bytes each), kind, collection_len (1 each)
            let mut at = 4 + n * (8 + 8 + 8 + 1 + 1);
            let did_lens: Vec<usize> = (0..n)
                .map(|j| {
                    u16::from_le_bytes(block[at + 2 * j..at + 2 * j + 2].try_into().unwrap())
                        as usize
                })
                .collect();
            at += 2 * n;
            let rkey_lens: Vec<usize> = (0..n).map(|j| block[at + j] as usize).collect();
            at += n;
            let rev_lens: Vec<usize> = (0..n).map(|j| block[at + j] as usize).collect();
            at += n;
            let payload_lens: Vec<usize> = (0..n)
                .map(|j| u32_at(&block, at + 4 * j) as usize)
                .collect();
            at += 4 * n;
            let collection_lens: Vec<usize> =
                (0..n).map(|j| block[4 + n * 25 + j] as usize).collect();
            at += collection_lens.iter().sum::<usize>()
                + did_lens.iter().sum::<usize>()
                + rkey_lens.iter().sum::<usize>()
                + rev_lens.iter().sum::<usize>();
            for len in payload_lens {
                payloads.push(block[at..at + len].to_vec());
                at += len;
            }
            assert_eq!(at, block.len(), "no trailing bytes");
        }
        payloads
    }

    fn golden_cids() -> Vec<String> {
        std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/jetstream/corpus/car_expected_records.tsv"
        ))
        .expect("tsv")
        .lines()
        .map(|line| line.split('\t').nth(1).expect("cid column").to_string())
        .collect()
    }

    #[test]
    fn golden_records_round_trip_through_atproto_json() {
        let payloads = golden_payloads();
        let cids = golden_cids();
        assert_eq!(payloads.len(), 23);
        assert_eq!(cids.len(), 23);
        for (payload, cid) in payloads.iter().zip(&cids) {
            assert_eq!(
                &dag_cbor_cid(payload).unwrap().to_string(),
                cid,
                "CID of the raw payload"
            );
            let json = dag_cbor_to_json(payload).expect("decodes");
            assert!(
                json.get("$type").is_some(),
                "a record names its type: {json}"
            );
            assert_eq!(
                record_to_dag_cbor(&json).as_deref(),
                Some(payload.as_slice()),
                "JSON re-encodes to the same bytes"
            );
            assert_eq!(verify_record_cid(cid, &json), CidCheck::Match);
        }
    }

    #[test]
    fn bytes_and_links_use_the_atproto_json_forms() {
        let cid = dag_cbor_cid(b"x").unwrap();
        let ipld = Ipld::Map(BTreeMap::from([
            ("$type".to_string(), Ipld::String("app.test.thing".into())),
            ("blob".to_string(), Ipld::Link(cid)),
            (
                "raw".to_string(),
                Ipld::Bytes(vec![0xde, 0xad, 0xbe, 0xef, 0x01]),
            ),
            ("n".to_string(), Ipld::Integer(-7)),
            ("big".to_string(), Ipld::Integer(i64::MAX as i128)),
            (
                "list".to_string(),
                Ipld::List(vec![Ipld::Null, Ipld::Bool(true)]),
            ),
        ]));
        let cbor = serde_ipld_dagcbor::to_vec(&ipld).unwrap();
        let json = dag_cbor_to_json(&cbor).unwrap();
        assert_eq!(
            json["blob"],
            serde_json::json!({ "$link": cid.to_string() })
        );
        // Unpadded, as Jetstream emits `$bytes`.
        assert_eq!(json["raw"], serde_json::json!({ "$bytes": "3q2+7wE" }));
        assert_eq!(json["n"], -7);
        assert_eq!(json["big"], i64::MAX);
        assert_eq!(json["list"], serde_json::json!([null, true]));
        assert_eq!(record_to_dag_cbor(&json).unwrap(), cbor);
    }

    #[test]
    fn undecodable_cbor_is_an_error_not_a_panic() {
        assert!(dag_cbor_to_json(&[]).is_err());
        assert!(dag_cbor_to_json(&[0xff, 0x00]).is_err());
        let too_big = serde_ipld_dagcbor::to_vec(&Ipld::Integer(u64::MAX as i128 + 1));
        if let Ok(bytes) = too_big {
            assert!(dag_cbor_to_json(&bytes).is_err());
        }
    }
}

//! did:plc operation construction, signing, and DID derivation.
//!
//! This crate is deliberately I/O-free: it builds operations, signs them, and
//! derives identifiers, but never talks to a PLC directory. Submitting an
//! operation and reading a DID's audit log are HTTP concerns and stay with the
//! service that owns the HTTP client.

use std::collections::BTreeMap;

use base64::Engine;
use p256::ecdsa::{SigningKey, signature::Signer};
use sha2::{Digest, Sha256};

/// Errors produced while building, signing, or deriving did:plc operations.
#[derive(Debug, thiserror::Error)]
pub enum PlcError {
    #[error("DAG-CBOR encoding failed: {0}")]
    Encoding(String),
    #[error("invalid signing key: {0}")]
    InvalidKey(String),
    #[error("no CID in PLC log entry")]
    MissingCid,
    #[error("invalid PLC operation: {0}")]
    InvalidOperation(String),
}

/// Parameters for building a PLC genesis operation.
pub struct PlcGenesisParams {
    /// Rotation keys as did:key strings, highest authority first. Within 72
    /// hours of an operation, a key listed before the one that signed it can
    /// nullify it.
    pub rotation_keys: Vec<String>,
    /// Verification methods by id, without the `#` (e.g. `atproto`,
    /// `atproto_kem`), each a did:key.
    pub verification_methods: BTreeMap<String, String>,
    /// `at://` handles. Empty is valid: an identity needs no handle.
    pub also_known_as: Vec<String>,
    /// Service entries: (key, type, endpoint) — e.g. ("atproto_labeler", "AtprotoLabeler", "https://...")
    pub service_entries: Vec<(String, String, String)>,
}

/// Build the unsigned genesis operation (no `sig` field).
pub fn build_unsigned_genesis(params: &PlcGenesisParams) -> serde_json::Value {
    let mut services = serde_json::Map::new();
    for (key, svc_type, endpoint) in &params.service_entries {
        services.insert(
            key.clone(),
            serde_json::json!({
                "type": svc_type,
                "endpoint": endpoint,
            }),
        );
    }

    serde_json::json!({
        "type": "plc_operation",
        "rotationKeys": &params.rotation_keys,
        "verificationMethods": &params.verification_methods,
        "alsoKnownAs": &params.also_known_as,
        "services": services,
        "prev": null,
    })
}

// The limits are the stricter of the did:plc spec (v0.2) and the directory
// server (`packages/server/src/constraints.ts`), which is what plc.directory
// enforces. The server allows 10 rotation keys where the spec allows 5, and
// the spec allows 7500 bytes where the server allows 4000.
const MAX_OP_BYTES: usize = 4000;
const MAX_ROTATION_KEYS: usize = 5;
const MAX_VERIFICATION_METHODS: usize = 10;
const MAX_SERVICES: usize = 10;
const MAX_ALSO_KNOWN_AS: usize = 10;
const MAX_ALSO_KNOWN_AS_LENGTH: usize = 258;
const MAX_ID_LENGTH: usize = 32;
const MAX_DID_KEY_LENGTH: usize = 256;
const MAX_SERVICE_TYPE_LENGTH: usize = 256;
const MAX_SERVICE_ENDPOINT_LENGTH: usize = 512;

/// Multicodec prefixes of the two key types a rotation key may be.
const P256_MULTICODEC: [u8; 2] = [0x80, 0x24];
const SECP256K1_MULTICODEC: [u8; 2] = [0xe7, 0x01];

/// Check a signed operation against the directory's limits, so a violation
/// fails here with its reason rather than as a 400 from the directory.
pub fn check_operation(signed_op: &serde_json::Value) -> Result<(), PlcError> {
    let invalid = |msg: String| Err(PlcError::InvalidOperation(msg));

    let size = serde_ipld_dagcbor::to_vec(signed_op)
        .map_err(|e| PlcError::Encoding(e.to_string()))?
        .len();
    if size > MAX_OP_BYTES {
        return invalid(format!(
            "operation is {size} bytes of DAG-CBOR, over the {MAX_OP_BYTES}-byte limit"
        ));
    }

    let rotation_keys = string_array(signed_op, "rotationKeys")?;
    if rotation_keys.is_empty() || rotation_keys.len() > MAX_ROTATION_KEYS {
        return invalid(format!(
            "an operation needs 1 to {MAX_ROTATION_KEYS} rotation keys, not {}",
            rotation_keys.len()
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for key in &rotation_keys {
        if !seen.insert(key) {
            return invalid(format!("rotation key listed twice: {key}"));
        }
        match did_key_multicodec(key) {
            Some(prefix) if prefix == P256_MULTICODEC || prefix == SECP256K1_MULTICODEC => {}
            _ => {
                return invalid(format!(
                    "rotation key is not a P-256 or secp256k1 did:key: {key}"
                ));
            }
        }
    }

    let methods = object(signed_op, "verificationMethods")?;
    if methods.len() > MAX_VERIFICATION_METHODS {
        return invalid(format!(
            "{} verification methods, over the limit of {MAX_VERIFICATION_METHODS}",
            methods.len()
        ));
    }
    for (id, key) in methods {
        check_id(id, "verification method")?;
        let key = key.as_str().unwrap_or_default();
        if key.len() > MAX_DID_KEY_LENGTH || did_key_multicodec(key).is_none() {
            return invalid(format!("verification method {id} is not a did:key: {key}"));
        }
    }

    let services = object(signed_op, "services")?;
    if services.len() > MAX_SERVICES {
        return invalid(format!(
            "{} services, over the limit of {MAX_SERVICES}",
            services.len()
        ));
    }
    for (id, service) in services {
        check_id(id, "service")?;
        let service_type = service["type"].as_str().unwrap_or_default();
        let endpoint = service["endpoint"].as_str().unwrap_or_default();
        if service_type.len() > MAX_SERVICE_TYPE_LENGTH {
            return invalid(format!(
                "service {id}'s type is over {MAX_SERVICE_TYPE_LENGTH} characters"
            ));
        }
        if endpoint.len() > MAX_SERVICE_ENDPOINT_LENGTH {
            return invalid(format!(
                "service {id}'s endpoint is over {MAX_SERVICE_ENDPOINT_LENGTH} characters"
            ));
        }
    }

    let aliases = string_array(signed_op, "alsoKnownAs")?;
    if aliases.len() > MAX_ALSO_KNOWN_AS {
        return invalid(format!(
            "{} alsoKnownAs entries, over the limit of {MAX_ALSO_KNOWN_AS}",
            aliases.len()
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for alias in &aliases {
        if alias.len() > MAX_ALSO_KNOWN_AS_LENGTH {
            return invalid(format!(
                "alsoKnownAs entry is over {MAX_ALSO_KNOWN_AS_LENGTH} characters: {alias}"
            ));
        }
        if !seen.insert(alias) {
            return invalid(format!("alsoKnownAs entry listed twice: {alias}"));
        }
    }

    Ok(())
}

fn string_array<'a>(op: &'a serde_json::Value, field: &str) -> Result<Vec<&'a str>, PlcError> {
    op[field]
        .as_array()
        .ok_or_else(|| PlcError::InvalidOperation(format!("{field} must be an array")))?
        .iter()
        .map(|v| {
            v.as_str().ok_or_else(|| {
                PlcError::InvalidOperation(format!("every {field} entry must be a string"))
            })
        })
        .collect()
}

fn object<'a>(
    op: &'a serde_json::Value,
    field: &str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, PlcError> {
    op[field]
        .as_object()
        .ok_or_else(|| PlcError::InvalidOperation(format!("{field} must be an object")))
}

fn check_id(id: &str, kind: &str) -> Result<(), PlcError> {
    if id.is_empty() || id.len() > MAX_ID_LENGTH || id.contains('#') {
        return Err(PlcError::InvalidOperation(format!(
            "{kind} id must be 1 to {MAX_ID_LENGTH} characters with no '#': {id:?}"
        )));
    }
    Ok(())
}

/// The multicodec prefix of a base58btc `did:key`, or `None` when it isn't one.
fn did_key_multicodec(did_key: &str) -> Option<[u8; 2]> {
    let multibase = did_key.strip_prefix("did:key:")?;
    if !multibase.starts_with('z') {
        return None;
    }
    let (_, bytes) = multibase::decode(multibase).ok()?;
    Some([*bytes.first()?, *bytes.get(1)?])
}

/// Sign an unsigned PLC operation with the rotation key.
///
/// The signature covers the DAG-CBOR encoding of the unsigned operation
/// (all fields except `sig`). ECDSA P-256 internally SHA-256 hashes the
/// message before signing. The signed operation is checked with
/// [`check_operation`] before it is returned.
pub fn sign_operation(
    unsigned_op: &serde_json::Value,
    rotation_key: &SigningKey,
) -> Result<serde_json::Value, PlcError> {
    let cbor =
        serde_ipld_dagcbor::to_vec(unsigned_op).map_err(|e| PlcError::Encoding(e.to_string()))?;

    // p256 Signer::sign hashes the message with SHA-256 internally (standard ECDSA)
    let signature: p256::ecdsa::Signature = rotation_key.sign(&cbor);

    // The PLC directory verifies with `@atproto/crypto`, which runs `verifySig`
    // with `lowS: true` unless a caller opts into malleable signatures — and no
    // caller there does. P-256 leaves S wherever ECDSA put it, so about half of
    // all operations came back `400 Invalid signature on op` until this
    // normalization; the other half were accepted, which made it read as a flaky
    // PLC rather than a signing bug.
    let signature = signature.normalize_s();

    let sig_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature.to_bytes());

    let mut signed = unsigned_op.clone();
    signed
        .as_object_mut()
        .ok_or_else(|| PlcError::Encoding("operation must be a JSON object".into()))?
        .insert("sig".to_string(), serde_json::json!(sig_b64));
    check_operation(&signed)?;
    Ok(signed)
}

/// Derive the `did:plc:` identifier from a **signed** genesis operation.
///
/// Steps:
/// 1. DAG-CBOR encode the signed operation
/// 2. SHA-256 hash the encoding
/// 3. Base32-lower encode the hash (RFC 4648 lowercase, no padding)
/// 4. Truncate to 24 characters
/// 5. Prefix with `did:plc:`
pub fn derive_did(signed_op: &serde_json::Value) -> Result<String, PlcError> {
    let cbor =
        serde_ipld_dagcbor::to_vec(signed_op).map_err(|e| PlcError::Encoding(e.to_string()))?;
    let hash = Sha256::digest(&cbor);
    let encoded = data_encoding::BASE32_NOPAD.encode(&hash).to_lowercase();
    let truncated = &encoded[..24];
    Ok(format!("did:plc:{truncated}"))
}

/// Extract the `cid` field from a PLC audit log entry (used as `prev` in update operations).
pub fn extract_prev_cid(last_op: &serde_json::Value) -> Result<String, PlcError> {
    last_op["cid"]
        .as_str()
        .map(String::from)
        .ok_or(PlcError::MissingCid)
}

/// Build an unsigned PLC update operation.
///
/// Unlike a genesis operation, this has `prev` set to the CID of the last operation
/// and preserves existing fields from the current DID document.
pub fn build_update_operation(
    prev: &str,
    rotation_keys: Vec<String>,
    verification_methods: serde_json::Map<String, serde_json::Value>,
    also_known_as: Vec<String>,
    services: serde_json::Map<String, serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "type": "plc_operation",
        "rotationKeys": rotation_keys,
        "verificationMethods": verification_methods,
        "alsoKnownAs": also_known_as,
        "services": services,
        "prev": prev,
    })
}

/// Convert raw P-256 private key bytes to a did:key multibase string.
///
/// Uses the same multikey format as `extract_public_key_multibase` in server.rs:
/// multicodec varint prefix 0x8024 (P-256) + compressed public key, base58btc-encoded.
pub fn private_key_to_did_key(key_bytes: &[u8]) -> Result<String, PlcError> {
    let signing_key =
        SigningKey::from_slice(key_bytes).map_err(|e| PlcError::InvalidKey(e.to_string()))?;
    let public_key = signing_key.verifying_key();
    let compressed = public_key.to_sec1_point(true);

    // Multikey: 0x8024 varint prefix for P-256 + compressed public key bytes
    let mut multikey_bytes = vec![0x80, 0x24];
    multikey_bytes.extend_from_slice(compressed.as_bytes());
    let encoded = multibase::encode(multibase::Base::Base58Btc, &multikey_bytes);
    Ok(format!("did:key:{encoded}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::Rng;

    /// Generate a test P-256 signing key using rand 0.9 (avoids rand_core version mismatch
    /// with p256's SigningKey::random which expects rand_core 0.6).
    fn test_signing_key() -> SigningKey {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        SigningKey::from_slice(&bytes[..]).unwrap()
    }

    fn test_did_key() -> String {
        private_key_to_did_key(&test_signing_key().to_bytes()).unwrap()
    }

    fn test_params() -> PlcGenesisParams {
        PlcGenesisParams {
            rotation_keys: vec![test_did_key()],
            verification_methods: BTreeMap::from([("atproto".into(), test_did_key())]),
            also_known_as: vec![],
            service_entries: vec![],
        }
    }

    fn signed(params: &PlcGenesisParams) -> Result<serde_json::Value, PlcError> {
        sign_operation(&build_unsigned_genesis(params), &test_signing_key())
    }

    fn assert_invalid(result: Result<serde_json::Value, PlcError>, expected: &str) {
        match result {
            Err(PlcError::InvalidOperation(msg)) => {
                assert!(
                    msg.contains(expected),
                    "{msg:?} should mention {expected:?}"
                )
            }
            other => panic!("expected InvalidOperation mentioning {expected:?}, got {other:?}"),
        }
    }

    #[test]
    fn build_unsigned_genesis_structure() {
        let params = PlcGenesisParams {
            rotation_keys: vec!["did:key:zRotation".into()],
            verification_methods: BTreeMap::from([("atproto".into(), "did:key:zSigning".into())]),
            also_known_as: vec![],
            service_entries: vec![(
                "atproto_labeler".into(),
                "AtprotoLabeler".into(),
                "https://example.com".into(),
            )],
        };

        let op = build_unsigned_genesis(&params);
        assert_eq!(op["type"], "plc_operation");
        assert_eq!(op["prev"], serde_json::Value::Null);
        assert_eq!(op["rotationKeys"][0], "did:key:zRotation");
        assert_eq!(op["verificationMethods"]["atproto"], "did:key:zSigning");
        assert_eq!(op["services"]["atproto_labeler"]["type"], "AtprotoLabeler");
        assert_eq!(
            op["services"]["atproto_labeler"]["endpoint"],
            "https://example.com"
        );
        assert_eq!(op["alsoKnownAs"].as_array().unwrap().len(), 0);
        // No sig field on unsigned op
        assert!(op.get("sig").is_none());
    }

    /// The PLC directory verifies operations with `@atproto/crypto`, whose
    /// `verifySig` defaults to `lowS: true` — a signature whose S value sits in
    /// the upper half of the curve order is rejected as malleable. P-256 signing
    /// does not normalize S on its own, so roughly half of all operations were
    /// refused with `Invalid signature on op` until `sign_operation` normalized
    /// it. One key would reproduce that only half the time; 64 make a
    /// regression a certainty rather than a flake.
    #[test]
    fn sign_operation_produces_low_s_signatures() {
        let unsigned = build_unsigned_genesis(&test_params());

        for i in 0..64 {
            let key = test_signing_key();
            let signed = sign_operation(&unsigned, &key).unwrap();
            let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(signed["sig"].as_str().unwrap())
                .unwrap();
            let sig = p256::ecdsa::Signature::from_slice(&raw).unwrap();
            assert_eq!(
                sig.normalize_s(),
                sig,
                "signature {i} has a high S value and PLC would reject it as malleable"
            );
        }
    }

    #[test]
    fn sign_operation_adds_sig() {
        let unsigned = build_unsigned_genesis(&test_params());

        let key = test_signing_key();
        let signed = sign_operation(&unsigned, &key).unwrap();

        assert!(signed.get("sig").is_some());
        let sig = signed["sig"].as_str().unwrap();
        // base64url-encoded P-256 ECDSA signature should be non-empty
        assert!(!sig.is_empty());
        // All other fields preserved
        assert_eq!(signed["type"], "plc_operation");
        assert_eq!(signed["prev"], serde_json::Value::Null);
    }

    #[test]
    fn derive_did_format() {
        let unsigned = build_unsigned_genesis(&test_params());
        let key = test_signing_key();
        let signed = sign_operation(&unsigned, &key).unwrap();

        let did = derive_did(&signed).unwrap();
        assert!(did.starts_with("did:plc:"));
        // 24-char truncated hash after prefix
        let suffix = did.strip_prefix("did:plc:").unwrap();
        assert_eq!(suffix.len(), 24);
        // Should be lowercase base32 (a-z, 2-7)
        assert!(
            suffix
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        );
    }

    #[test]
    fn derive_did_deterministic() {
        let unsigned = build_unsigned_genesis(&test_params());
        let key = test_signing_key();
        let signed = sign_operation(&unsigned, &key).unwrap();

        let did1 = derive_did(&signed).unwrap();
        let did2 = derive_did(&signed).unwrap();
        assert_eq!(did1, did2);
    }

    #[test]
    fn private_key_to_did_key_roundtrip() {
        let key = test_signing_key();
        let key_bytes = key.to_bytes();
        let did_key = private_key_to_did_key(&key_bytes).unwrap();
        assert!(did_key.starts_with("did:key:z"));
    }

    #[test]
    fn private_key_to_did_key_rejects_invalid_bytes() {
        let result = private_key_to_did_key(&[0x00; 32]);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("invalid signing key"),
            "error should mention invalid: {msg}"
        );
    }

    #[test]
    fn extract_prev_cid_missing_field() {
        let op = serde_json::json!({"type": "plc_operation"});
        let result = extract_prev_cid(&op);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("CID"), "error should mention CID: {msg}");
    }

    #[test]
    fn extract_prev_cid_returns_cid() {
        let op = serde_json::json!({"type": "plc_operation", "cid": "bafyrei..."});
        assert_eq!(extract_prev_cid(&op).unwrap(), "bafyrei...");
    }

    #[test]
    fn build_update_operation_sets_prev() {
        let op = build_update_operation(
            "bafyprev",
            vec!["did:key:zRotation".into()],
            serde_json::Map::new(),
            vec!["at://example.test".into()],
            serde_json::Map::new(),
        );
        assert_eq!(op["type"], "plc_operation");
        assert_eq!(op["prev"], "bafyprev");
        assert_eq!(op["rotationKeys"][0], "did:key:zRotation");
        assert_eq!(op["alsoKnownAs"][0], "at://example.test");
    }

    #[test]
    fn genesis_carries_every_rotation_key_method_and_alias_in_order() {
        let creator = test_did_key();
        let instance = test_did_key();
        let params = PlcGenesisParams {
            rotation_keys: vec![creator.clone(), instance.clone()],
            verification_methods: BTreeMap::from([
                ("atproto".into(), test_did_key()),
                ("atproto_kem".into(), test_did_key()),
            ]),
            also_known_as: vec!["at://space.example.com".into()],
            service_entries: vec![(
                "atproto_pds".into(),
                "AtprotoPersonalDataServer".into(),
                "https://happyview.example.com".into(),
            )],
        };

        let op = signed(&params).unwrap();
        assert_eq!(op["rotationKeys"], serde_json::json!([creator, instance]));
        assert!(op["verificationMethods"]["atproto_kem"].is_string());
        assert_eq!(op["alsoKnownAs"][0], "at://space.example.com");
        derive_did(&op).unwrap();
    }

    #[test]
    fn a_secp256k1_rotation_key_is_accepted() {
        let k256 = k256_did_key();
        let mut params = test_params();
        params.rotation_keys.insert(0, k256);
        signed(&params).unwrap();
    }

    #[test]
    fn more_than_five_rotation_keys_are_refused() {
        let mut params = test_params();
        params.rotation_keys = (0..6).map(|_| test_did_key()).collect();
        assert_invalid(signed(&params), "1 to 5 rotation keys");
    }

    #[test]
    fn no_rotation_key_is_refused() {
        let mut params = test_params();
        params.rotation_keys.clear();
        assert_invalid(signed(&params), "1 to 5 rotation keys");
    }

    #[test]
    fn a_duplicate_rotation_key_is_refused() {
        let mut params = test_params();
        params.rotation_keys.push(params.rotation_keys[0].clone());
        assert_invalid(signed(&params), "listed twice");
    }

    #[test]
    fn an_ed25519_rotation_key_is_refused() {
        let mut bytes = vec![0xed, 0x01];
        bytes.extend_from_slice(&[7u8; 32]);
        let ed25519 = format!(
            "did:key:{}",
            multibase::encode(multibase::Base::Base58Btc, &bytes)
        );
        let mut params = test_params();
        params.rotation_keys = vec![ed25519];
        assert_invalid(signed(&params), "not a P-256 or secp256k1");
    }

    #[test]
    fn a_verification_method_id_with_a_hash_is_refused() {
        let mut params = test_params();
        params
            .verification_methods
            .insert("#atproto_kem".into(), test_did_key());
        assert_invalid(signed(&params), "no '#'");
    }

    #[test]
    fn a_verification_method_that_is_not_a_did_key_is_refused() {
        let mut params = test_params();
        params
            .verification_methods
            .insert("atproto_kem".into(), "did:web:example.com".into());
        assert_invalid(signed(&params), "not a did:key");
    }

    #[test]
    fn an_operation_over_4000_bytes_is_refused() {
        let mut params = test_params();
        params.service_entries = (0..9)
            .map(|i| {
                (
                    format!("svc{i}"),
                    "Example".into(),
                    format!("https://{}.example.com", "a".repeat(480)),
                )
            })
            .collect();
        assert_invalid(signed(&params), "4000-byte limit");
    }

    #[test]
    fn a_duplicate_alias_is_refused() {
        let mut params = test_params();
        params.also_known_as = vec!["at://a.example.com".into(), "at://a.example.com".into()];
        assert_invalid(signed(&params), "listed twice");
    }

    fn k256_did_key() -> String {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let key = k256::ecdsa::SigningKey::from_slice(&bytes).unwrap();
        let mut multikey = vec![0xe7, 0x01];
        multikey.extend_from_slice(key.verifying_key().to_sec1_point(true).as_bytes());
        format!(
            "did:key:{}",
            multibase::encode(multibase::Base::Base58Btc, &multikey)
        )
    }
}

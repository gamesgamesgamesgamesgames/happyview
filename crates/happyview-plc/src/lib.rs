//! did:plc operation construction, signing, and DID derivation.
//!
//! This crate is deliberately I/O-free: it builds operations, signs them, and
//! derives identifiers, but never talks to a PLC directory. Submitting an
//! operation and reading a DID's audit log are HTTP concerns and stay with the
//! service that owns the HTTP client.

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
}

/// Parameters for building a PLC genesis operation.
pub struct PlcGenesisParams {
    /// The rotation key in did:key multibase format (e.g. "did:key:z...")
    pub rotation_key_did_key: String,
    /// The signing key in did:key multibase format (e.g. "did:key:z...")
    pub signing_key_did_key: String,
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
        "rotationKeys": [&params.rotation_key_did_key],
        "verificationMethods": {
            "atproto": &params.signing_key_did_key,
        },
        "alsoKnownAs": [],
        "services": services,
        "prev": null,
    })
}

/// Sign an unsigned PLC operation with the rotation key.
///
/// The signature covers the DAG-CBOR encoding of the unsigned operation
/// (all fields except `sig`). ECDSA P-256 internally SHA-256 hashes the
/// message before signing.
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

    fn test_params() -> PlcGenesisParams {
        PlcGenesisParams {
            rotation_key_did_key: "did:key:zTest".into(),
            signing_key_did_key: "did:key:zTest".into(),
            service_entries: vec![],
        }
    }

    #[test]
    fn build_unsigned_genesis_structure() {
        let params = PlcGenesisParams {
            rotation_key_did_key: "did:key:zRotation".into(),
            signing_key_did_key: "did:key:zSigning".into(),
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
}

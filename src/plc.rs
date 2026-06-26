use crate::error::AppError;
use base64::Engine;
use p256::ecdsa::{SigningKey, signature::Signer};
use sha2::{Digest, Sha256};

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
) -> Result<serde_json::Value, AppError> {
    let cbor = serde_ipld_dagcbor::to_vec(unsigned_op)
        .map_err(|e| AppError::Internal(format!("DAG-CBOR encoding failed: {e}")))?;

    // p256 Signer::sign hashes the message with SHA-256 internally (standard ECDSA)
    let signature: p256::ecdsa::Signature = rotation_key.sign(&cbor);
    let sig_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature.to_bytes());

    let mut signed = unsigned_op.clone();
    signed
        .as_object_mut()
        .unwrap()
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
pub fn derive_did(signed_op: &serde_json::Value) -> Result<String, AppError> {
    let cbor = serde_ipld_dagcbor::to_vec(signed_op)
        .map_err(|e| AppError::Internal(format!("DAG-CBOR encoding failed: {e}")))?;
    let hash = Sha256::digest(&cbor);
    let encoded = data_encoding::BASE32_NOPAD.encode(&hash).to_lowercase();
    let truncated = &encoded[..24];
    Ok(format!("did:plc:{truncated}"))
}

/// Submit a signed PLC operation (genesis or update) to the PLC directory.
///
/// POST `{plc_url}/{did}` with the signed operation as JSON body.
pub async fn submit_operation(
    http: &reqwest::Client,
    plc_url: &str,
    did: &str,
    signed_op: &serde_json::Value,
) -> Result<(), AppError> {
    let url = format!("{}/{}", plc_url.trim_end_matches('/'), did);
    let resp = http
        .post(&url)
        .json(signed_op)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("PLC submission failed: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::Internal(format!(
            "PLC directory returned {status}: {body}"
        )));
    }
    Ok(())
}

/// Backwards-compatible alias for `submit_operation`.
pub async fn submit_genesis(
    http: &reqwest::Client,
    plc_url: &str,
    did: &str,
    signed_op: &serde_json::Value,
) -> Result<(), AppError> {
    submit_operation(http, plc_url, did, signed_op).await
}

/// Fetch the last PLC audit log entry for a DID.
///
/// GET `{plc_url}/{did}/log/last` returns the last operation with a `cid` field.
pub async fn fetch_last_operation(
    http: &reqwest::Client,
    plc_url: &str,
    did: &str,
) -> Result<serde_json::Value, AppError> {
    let url = format!("{}/{}/log/last", plc_url.trim_end_matches('/'), did);
    let resp = http
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("failed to fetch PLC log: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::Internal(format!(
            "PLC directory returned {status} for log/last: {body}"
        )));
    }

    resp.json()
        .await
        .map_err(|e| AppError::Internal(format!("failed to parse PLC log: {e}")))
}

/// Extract the `cid` field from a PLC audit log entry (used as `prev` in update operations).
pub fn extract_prev_cid(last_op: &serde_json::Value) -> Result<String, AppError> {
    last_op["cid"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| AppError::Internal("no CID in PLC log entry".into()))
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

/// Decrypt an encrypted key from the database and return the raw bytes.
pub fn decrypt_key(enc_b64: &str, encryption_key: &[u8; 32]) -> Result<Vec<u8>, AppError> {
    let encrypted = base64::engine::general_purpose::STANDARD
        .decode(enc_b64)
        .map_err(|e| AppError::Internal(format!("failed to decode key: {e}")))?;

    crate::plugin::encryption::decrypt(encryption_key, &encrypted)
        .map_err(|e| AppError::Internal(format!("failed to decrypt key: {e}")))
}

/// Convert raw P-256 private key bytes to a did:key multibase string.
///
/// Uses the same multikey format as `extract_public_key_multibase` in server.rs:
/// multicodec varint prefix 0x8024 (P-256) + compressed public key, base58btc-encoded.
pub fn private_key_to_did_key(key_bytes: &[u8]) -> Result<String, AppError> {
    let signing_key = SigningKey::from_bytes(key_bytes.into())
        .map_err(|e| AppError::Internal(format!("invalid signing key: {e}")))?;
    let public_key = signing_key.verifying_key();
    let compressed = public_key.to_encoded_point(true);

    // Multikey: 0x8024 varint prefix for P-256 + compressed public key bytes
    let mut multikey_bytes = vec![0x80, 0x24];
    multikey_bytes.extend_from_slice(compressed.as_bytes());
    let encoded = multibase::encode(multibase::Base::Base58Btc, &multikey_bytes);
    Ok(format!("did:key:{encoded}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::RngCore;

    /// Generate a test P-256 signing key using rand 0.9 (avoids rand_core version mismatch
    /// with p256's SigningKey::random which expects rand_core 0.6).
    fn test_signing_key() -> SigningKey {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        SigningKey::from_bytes((&bytes[..]).into()).unwrap()
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

    #[test]
    fn sign_operation_adds_sig() {
        let params = PlcGenesisParams {
            rotation_key_did_key: "did:key:zTest".into(),
            signing_key_did_key: "did:key:zTest".into(),
            service_entries: vec![],
        };
        let unsigned = build_unsigned_genesis(&params);

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
        let params = PlcGenesisParams {
            rotation_key_did_key: "did:key:zTest".into(),
            signing_key_did_key: "did:key:zTest".into(),
            service_entries: vec![],
        };
        let unsigned = build_unsigned_genesis(&params);
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
        let params = PlcGenesisParams {
            rotation_key_did_key: "did:key:zTest".into(),
            signing_key_did_key: "did:key:zTest".into(),
            service_entries: vec![],
        };
        let unsigned = build_unsigned_genesis(&params);
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
    fn decrypt_key_invalid_base64() {
        let encryption_key = [0x42u8; 32];
        let result = decrypt_key("not valid base64!!!", &encryption_key);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("decode"),
            "error should mention decoding: {msg}"
        );
    }

    #[test]
    fn decrypt_key_wrong_encryption_key() {
        let correct_key = [0x42u8; 32];
        let wrong_key = [0x99u8; 32];

        let plaintext = [0xAAu8; 32];
        let encrypted = crate::plugin::encryption::encrypt(&correct_key, &plaintext).unwrap();
        let enc_b64 = base64::engine::general_purpose::STANDARD.encode(&encrypted);

        let result = decrypt_key(&enc_b64, &wrong_key);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("decrypt"),
            "error should mention decryption: {msg}"
        );
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
}

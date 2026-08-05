//! Attestation signing for plugin records.
//!
//! Implements the ATProtocol attestation spec:
//! - Computes CID with $sig metadata for replay protection
//! - Signs using ECDSA (P-256 or K-256)
//! - Adds inline signatures to records

use cid::Cid;
use k256::ecdsa::{Signature, SigningKey, signature::Signer};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;

// Multihash code for SHA2-256
const SHA2_256_CODE: u64 = 0x12;
// DAG-CBOR codec
const DAG_CBOR_CODEC: u64 = 0x71;

/// Attestation signer for HappyView
pub struct AttestationSigner {
    /// The signing key (K-256/secp256k1)
    signing_key: SigningKey,
    /// The key identifier (e.g., "did:web:happyview.example#attestation")
    key_id: String,
    /// The signature type identifier
    sig_type: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AttestationError {
    #[error("Failed to encode record: {0}")]
    Encoding(String),
    #[error("Failed to sign: {0}")]
    Signing(String),
    #[error("Invalid key: {0}")]
    InvalidKey(String),
    #[error("Record missing required field: {0}")]
    MissingField(String),
}

impl AttestationSigner {
    /// Create a new signer from a hex-encoded private key
    pub fn from_hex(
        private_key_hex: &str,
        key_id: String,
        sig_type: String,
    ) -> Result<Self, AttestationError> {
        let key_bytes = hex::decode(private_key_hex)
            .map_err(|e| AttestationError::InvalidKey(format!("invalid hex: {}", e)))?;

        let signing_key = SigningKey::from_slice(&key_bytes[..])
            .map_err(|e| AttestationError::InvalidKey(format!("invalid key: {}", e)))?;

        Ok(Self {
            signing_key,
            key_id,
            sig_type,
        })
    }

    /// Create a new signer with a test key (for testing only)
    #[cfg(test)]
    pub fn for_testing(key_id: String, sig_type: String) -> Self {
        // Fixed test key (32 bytes of 0x01) - DO NOT USE IN PRODUCTION
        let test_key_bytes = [0x01u8; 32];
        let signing_key = SigningKey::from_slice(&test_key_bytes[..]).expect("valid test key");
        Self {
            signing_key,
            key_id,
            sig_type,
        }
    }

    /// Get the public key in compressed format (for verification)
    pub fn public_key_bytes(&self) -> Vec<u8> {
        use k256::ecdsa::VerifyingKey;
        let verifying_key = VerifyingKey::from(&self.signing_key);
        verifying_key.to_sec1_point(true).as_bytes().to_vec()
    }

    /// Sign a record and add the signature to the signatures array.
    ///
    /// # Arguments
    /// * `record` - The record to sign (will be modified to add signature)
    /// * `repository_did` - The DID of the repository (for replay protection)
    ///
    /// # Returns
    /// The CID of the signed content
    pub fn sign_record(
        &self,
        record: &mut Value,
        repository_did: &str,
    ) -> Result<Cid, AttestationError> {
        let obj = record
            .as_object_mut()
            .ok_or_else(|| AttestationError::Encoding("record must be an object".into()))?;

        let existing_signatures = obj.remove("signatures");

        let body = Self::signable_body(obj, &self.sig_type, repository_did);
        let cid = self.compute_cid_current(&body)?;

        // Sign the CID bytes
        let signature = self.sign_cid(&cid)?;

        // Create inline signature object
        let inline_sig = serde_json::json!({
            "$type": &self.sig_type,
            "key": &self.key_id,
            "signature": {
                "$bytes": base64::Engine::encode(&crate::cid_verify::BYTES_B64, &signature)
            }
        });

        // Add to signatures array
        let signatures = obj
            .entry("signatures")
            .or_insert_with(|| Value::Array(vec![]));

        if let Value::Array(arr) = signatures {
            // Restore any existing signatures
            if let Some(Value::Array(existing)) = existing_signatures {
                for sig in existing {
                    arr.push(sig);
                }
            }
            arr.push(inline_sig);
        }

        Ok(cid)
    }

    #[cfg(test)]
    pub fn sign_record_legacy(
        &self,
        record: &mut Value,
        repository_did: &str,
    ) -> Result<Cid, AttestationError> {
        let obj = record
            .as_object_mut()
            .ok_or_else(|| AttestationError::Encoding("record must be an object".into()))?;
        let existing_signatures = obj.remove("signatures");

        let body = Self::signable_body(obj, &self.sig_type, repository_did);
        let cid = self.compute_cid_legacy(&body)?;
        let signature = self.sign_cid(&cid)?;

        let inline_sig = serde_json::json!({
            "$type": &self.sig_type,
            "key": &self.key_id,
            "signature": {
                "$bytes": base64::Engine::encode(&crate::cid_verify::BYTES_B64, &signature)
            }
        });

        let signatures = obj
            .entry("signatures")
            .or_insert_with(|| Value::Array(vec![]));
        if let Value::Array(arr) = signatures {
            if let Some(Value::Array(existing)) = existing_signatures {
                for sig in existing {
                    arr.push(sig);
                }
            }
            arr.push(inline_sig);
        }

        Ok(cid)
    }

    fn compute_cid_current(&self, obj: &Map<String, Value>) -> Result<Cid, AttestationError> {
        let value = Value::Object(obj.clone());
        let cbor = crate::cid_verify::record_to_dag_cbor(&value).ok_or_else(|| {
            AttestationError::Encoding("record cannot be encoded as DAG-CBOR".into())
        })?;
        crate::cid_verify::dag_cbor_cid(&cbor)
            .ok_or_else(|| AttestationError::Encoding("failed to build CID".into()))
    }

    fn compute_cid_legacy(&self, obj: &Map<String, Value>) -> Result<Cid, AttestationError> {
        let cbor_value = legacy_json_to_cbor(&Value::Object(obj.clone()));

        let mut cbor = Vec::new();
        ciborium::into_writer(&cbor_value, &mut cbor)
            .map_err(|e| AttestationError::Encoding(format!("CBOR encoding failed: {}", e)))?;

        let digest = Sha256::digest(&cbor);

        // Create multihash: varint(code) || varint(size) || digest
        let mut multihash_bytes = Vec::new();
        multihash_bytes.push(SHA2_256_CODE as u8);
        multihash_bytes.push(32u8);
        multihash_bytes.extend_from_slice(&digest);

        let multihash =
            cid::multihash::Multihash::<64>::from_bytes(&multihash_bytes).expect("valid multihash");

        Ok(Cid::new_v1(DAG_CBOR_CODEC, multihash))
    }

    fn signable_body(
        record: &Map<String, Value>,
        sig_type: &str,
        repository_did: &str,
    ) -> Map<String, Value> {
        let mut obj = record.clone();
        obj.remove("signatures");
        obj.insert(
            "$sig".to_string(),
            serde_json::json!({
                "$type": sig_type,
                "repository": repository_did,
            }),
        );
        obj
    }

    fn sign_cid(&self, cid: &Cid) -> Result<Vec<u8>, AttestationError> {
        let cid_bytes = cid.to_bytes();

        // Sign using k256 ECDSA (automatically uses low-S)
        let signature: Signature = self.signing_key.sign(&cid_bytes);

        Ok(signature.to_bytes().to_vec())
    }

    pub fn verify_record_signature_detailed(
        &self,
        record: &Value,
        signature_obj: &Value,
        repository_did: &str,
    ) -> Result<SignatureVerification, AttestationError> {
        use k256::ecdsa::{VerifyingKey, signature::Verifier};

        // Check key ID matches
        let key = signature_obj
            .get("key")
            .and_then(|k| k.as_str())
            .ok_or_else(|| AttestationError::MissingField("signature.key".into()))?;

        if key != self.key_id {
            return Ok(SignatureVerification::Invalid);
        }

        // Extract signature bytes
        let sig_bytes_b64 = signature_obj
            .get("signature")
            .and_then(|s| s.get("$bytes"))
            .and_then(|b| b.as_str())
            .ok_or_else(|| AttestationError::MissingField("signature.signature.$bytes".into()))?;

        let sig_bytes = base64::Engine::decode(&crate::cid_verify::BYTES_B64, sig_bytes_b64)
            .map_err(|e| AttestationError::Encoding(format!("invalid base64: {e}")))?;

        let signature = Signature::from_slice(&sig_bytes[..])
            .map_err(|e| AttestationError::Signing(format!("invalid signature bytes: {e}")))?;

        // Recompute the signed body (same as signing)
        let obj = record
            .as_object()
            .ok_or_else(|| AttestationError::Encoding("record must be an object".into()))?;
        let body = Self::signable_body(obj, &self.sig_type, repository_did);

        let verifying_key = VerifyingKey::from(&self.signing_key);
        let matches = |cid: &Cid| verifying_key.verify(&cid.to_bytes(), &signature).is_ok();

        // Current encoding first: it is the common path for anything signed
        // after the encoder was corrected.
        if matches(&self.compute_cid_current(&body)?) {
            return Ok(SignatureVerification::Valid(SignatureEncoding::Current));
        }

        // Fall back to the pre-fix encoding. This does not weaken the check —
        // the same key still has to have signed a CID derived from this exact
        // content; only the canonicalisation differs.
        if matches(&self.compute_cid_legacy(&body)?) {
            tracing::warn!(
                key_id = %self.key_id,
                repository = %repository_did,
                "legacy attestation signature verified — signed before the DAG-CBOR \
                 encoder was corrected; the fallback can be removed once these stop appearing"
            );
            return Ok(SignatureVerification::Valid(SignatureEncoding::Legacy));
        }

        Ok(SignatureVerification::Invalid)
    }

    /// Verify that a signature in a record was produced by this signer.
    pub fn verify_record_signature(
        &self,
        record: &Value,
        signature_obj: &Value,
        repository_did: &str,
    ) -> Result<bool, AttestationError> {
        Ok(self
            .verify_record_signature_detailed(record, signature_obj, repository_did)?
            .is_valid())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureEncoding {
    Current,
    Legacy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureVerification {
    Valid(SignatureEncoding),
    Invalid,
}

impl SignatureVerification {
    pub fn is_valid(&self) -> bool {
        matches!(self, SignatureVerification::Valid(_))
    }

    pub fn is_legacy(&self) -> bool {
        matches!(
            self,
            SignatureVerification::Valid(SignatureEncoding::Legacy)
        )
    }
}

fn legacy_json_to_cbor(value: &Value) -> ciborium::Value {
    match value {
        Value::Null => ciborium::Value::Null,
        Value::Bool(b) => ciborium::Value::Bool(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                ciborium::Value::Integer(i.into())
            } else if let Some(u) = n.as_u64() {
                ciborium::Value::Integer(u.into())
            } else if let Some(f) = n.as_f64() {
                ciborium::Value::Float(f)
            } else {
                ciborium::Value::Null
            }
        }
        Value::String(s) => {
            // Check for $bytes encoding (base64)
            ciborium::Value::Text(s.clone())
        }
        Value::Array(arr) => ciborium::Value::Array(arr.iter().map(legacy_json_to_cbor).collect()),
        Value::Object(obj) => {
            // Handle special $bytes encoding for binary data
            if obj.len() == 1
                && let Some(Value::String(b64)) = obj.get("$bytes")
                && let Ok(bytes) = base64::Engine::decode(&crate::cid_verify::BYTES_B64, b64)
            {
                return ciborium::Value::Bytes(bytes);
            }

            // Sort keys lexicographically for deterministic encoding
            let mut pairs: Vec<_> = obj
                .iter()
                .map(|(k, v)| (ciborium::Value::Text(k.clone()), legacy_json_to_cbor(v)))
                .collect();
            pairs.sort_by(|a, b| {
                if let (ciborium::Value::Text(ka), ciborium::Value::Text(kb)) = (&a.0, &b.0) {
                    ka.cmp(kb)
                } else {
                    std::cmp::Ordering::Equal
                }
            });

            ciborium::Value::Map(pairs)
        }
    }
}

pub type SharedAttestationSigner = Arc<AttestationSigner>;

pub fn load_from_env() -> Result<Option<AttestationSigner>, AttestationError> {
    let private_key = match std::env::var("ATTESTATION_PRIVATE_KEY") {
        Ok(k) => k,
        Err(_) => return Ok(None),
    };

    let key_id = std::env::var("ATTESTATION_KEY_ID")
        .unwrap_or_else(|_| "did:web:localhost#attestation".to_string());

    let sig_type = std::env::var("ATTESTATION_SIG_TYPE")
        .unwrap_or_else(|_| "games.gamesgamesgamesgames.attestation".to_string());

    Ok(Some(AttestationSigner::from_hex(
        &private_key,
        key_id,
        sig_type,
    )?))
}

pub async fn load_or_generate(
    db: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
    public_url: &str,
) -> Result<AttestationSigner, AttestationError> {
    use crate::db::adapt_sql;

    if let Some(signer) = load_from_env()? {
        tracing::info!("Attestation signer loaded from environment variables");
        return Ok(signer);
    }

    let host = public_url
        .strip_prefix("https://")
        .or_else(|| public_url.strip_prefix("http://"))
        .unwrap_or(public_url)
        .split('/')
        .next()
        .unwrap_or("localhost")
        .split(':')
        .next()
        .unwrap_or("localhost")
        .to_string();
    let default_key_id = format!("did:web:{host}#attestation");
    let default_sig_type = "games.gamesgamesgamesgames.attestation".to_string();

    let sql = adapt_sql(
        "SELECT value FROM happyview_instance_settings WHERE key = ?",
        backend,
    );
    let existing: Option<(String,)> = crate::db::query_as(&sql)
        .bind("attestation_private_key")
        .fetch_optional(db)
        .await
        .map_err(|e| AttestationError::Encoding(format!("db query failed: {e}")))?;

    if let Some((hex_key,)) = existing {
        let key_id: Option<(String,)> = crate::db::query_as(&sql)
            .bind("attestation_key_id")
            .fetch_optional(db)
            .await
            .map_err(|e| AttestationError::Encoding(format!("db query failed: {e}")))?;
        let sig_type: Option<(String,)> = crate::db::query_as(&sql)
            .bind("attestation_sig_type")
            .fetch_optional(db)
            .await
            .map_err(|e| AttestationError::Encoding(format!("db query failed: {e}")))?;

        tracing::info!("Attestation signer loaded from database");
        return AttestationSigner::from_hex(
            &hex_key,
            key_id.map(|r| r.0).unwrap_or(default_key_id),
            sig_type.map(|r| r.0).unwrap_or(default_sig_type),
        );
    }

    tracing::info!("Generating new attestation signing key");
    let hex_key = {
        // Generate 32 random bytes for a K-256 private key
        use rand::Rng;
        let mut key_bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut key_bytes);
        // Validate it's a valid K-256 scalar by trying to construct a SigningKey
        let _ = SigningKey::from_slice(&key_bytes[..])
            .map_err(|e| AttestationError::InvalidKey(format!("generated invalid key: {e}")))?;
        hex::encode(key_bytes)
    };

    let upsert_sql = adapt_sql(
        "INSERT INTO happyview_instance_settings (key, value, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        backend,
    );
    let now = crate::db::now_rfc3339();

    for (k, v) in [
        ("attestation_private_key", hex_key.as_str()),
        ("attestation_key_id", default_key_id.as_str()),
        ("attestation_sig_type", default_sig_type.as_str()),
    ] {
        crate::db::query(&upsert_sql)
            .bind(k)
            .bind(v)
            .bind(&now)
            .execute(db)
            .await
            .map_err(|e| AttestationError::Encoding(format!("failed to persist key: {e}")))?;
    }

    tracing::info!(key_id = %default_key_id, "Attestation signing key generated and persisted");

    AttestationSigner::from_hex(&hex_key, default_key_id, default_sig_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sign_record() {
        let signer = AttestationSigner::for_testing(
            "did:web:test.example#signing".to_string(),
            "test.signature".to_string(),
        );

        let mut record = serde_json::json!({
            "$type": "games.gamesgamesgamesgames.actor.game",
            "game": {"platform": "steam", "externalId": "440"},
            "platform": "steam",
            "createdAt": "2024-01-01T00:00:00Z"
        });

        let cid = signer
            .sign_record(&mut record, "did:plc:testuser")
            .expect("signing should succeed");

        // Verify signature was added
        let signatures = record["signatures"].as_array().expect("signatures array");
        assert_eq!(signatures.len(), 1);

        let sig = &signatures[0];
        assert_eq!(sig["$type"], "test.signature");
        assert_eq!(sig["key"], "did:web:test.example#signing");
        assert!(sig["signature"]["$bytes"].is_string());

        // CID should be valid
        assert!(!cid.to_bytes().is_empty());
    }

    #[test]
    fn test_deterministic_cid() {
        let signer = AttestationSigner::for_testing(
            "did:web:test.example#signing".to_string(),
            "test.signature".to_string(),
        );

        // Same record should produce same CID (before signature)
        let record1 = serde_json::json!({
            "a": 1,
            "b": 2,
            "c": {"nested": true}
        });

        let record2 = serde_json::json!({
            "c": {"nested": true},
            "a": 1,
            "b": 2
        });

        let mut r1 = record1.clone();
        let mut r2 = record2.clone();

        let cid1 = signer.sign_record(&mut r1, "did:plc:test").unwrap();
        let cid2 = signer.sign_record(&mut r2, "did:plc:test").unwrap();

        assert_eq!(cid1, cid2);
    }

    #[test]
    fn test_verify_record_signature() {
        let signer = AttestationSigner::for_testing(
            "did:web:test.example#signing".to_string(),
            "test.signature".to_string(),
        );

        let original = serde_json::json!({
            "$type": "games.gamesgamesgamesgames.contribution",
            "contributionType": "correction",
            "changes": {"name": "Fixed Name"},
            "createdAt": "2024-01-01T00:00:00Z"
        });

        let mut record = original.clone();
        signer
            .sign_record(&mut record, "did:plc:contributor")
            .expect("signing should succeed");

        let sig = &record["signatures"].as_array().unwrap()[0];

        assert!(
            signer
                .verify_record_signature(&record, sig, "did:plc:contributor")
                .unwrap()
        );

        assert!(
            !signer
                .verify_record_signature(&record, sig, "did:plc:wrong")
                .unwrap()
        );
    }

    #[test]
    fn test_verify_rejects_wrong_key_id() {
        let signer = AttestationSigner::for_testing(
            "did:web:test.example#signing".to_string(),
            "test.signature".to_string(),
        );

        let forged_sig = serde_json::json!({
            "$type": "test.signature",
            "key": "did:web:evil.example#signing",
            "signature": { "$bytes": "AAAA" }
        });

        let record = serde_json::json!({
            "contributionType": "correction",
            "changes": {"name": "test"}
        });

        assert!(
            !signer
                .verify_record_signature(&record, &forged_sig, "did:plc:test")
                .unwrap()
        );
    }

    #[test]
    fn test_verify_rejects_tampered_record() {
        let signer = AttestationSigner::for_testing(
            "did:web:test.example#signing".to_string(),
            "test.signature".to_string(),
        );

        let mut record = serde_json::json!({
            "contributionType": "correction",
            "changes": {"name": "Original"},
            "createdAt": "2024-01-01T00:00:00Z"
        });

        signer
            .sign_record(&mut record, "did:plc:test")
            .expect("signing should succeed");

        let sig = record["signatures"].as_array().unwrap()[0].clone();

        // Tamper with the record
        record["changes"]["name"] = serde_json::json!("Tampered");

        assert!(
            !signer
                .verify_record_signature(&record, &sig, "did:plc:test")
                .unwrap()
        );
    }

    #[tokio::test]
    async fn test_load_or_generate_creates_key() {
        sqlx::any::install_default_drivers();
        let pool = sqlx::pool::PoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::db::query(
            "CREATE TABLE happyview_instance_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL DEFAULT '')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let signer = load_or_generate(
            &pool,
            crate::db::DatabaseBackend::Sqlite,
            "https://happyview.example.com",
        )
        .await
        .expect("should generate a key");

        assert_eq!(signer.key_id, "did:web:happyview.example.com#attestation");

        let signer2 = load_or_generate(
            &pool,
            crate::db::DatabaseBackend::Sqlite,
            "https://happyview.example.com",
        )
        .await
        .expect("should load from DB");

        assert_eq!(signer.public_key_bytes(), signer2.public_key_bytes());
    }

    #[tokio::test]
    async fn test_load_or_generate_sign_verify_roundtrip() {
        sqlx::any::install_default_drivers();
        let pool = sqlx::pool::PoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::db::query(
            "CREATE TABLE happyview_instance_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL DEFAULT '')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let signer = load_or_generate(
            &pool,
            crate::db::DatabaseBackend::Sqlite,
            "https://example.com",
        )
        .await
        .unwrap();

        let mut record = serde_json::json!({
            "contributionType": "correction",
            "changes": {"name": "Test"},
        });

        signer.sign_record(&mut record, "did:plc:user123").unwrap();

        let sig = &record["signatures"].as_array().unwrap()[0];
        assert!(
            signer
                .verify_record_signature(&record, sig, "did:plc:user123")
                .unwrap()
        );
    }

    fn test_signer() -> AttestationSigner {
        AttestationSigner::for_testing(
            "did:web:test.example#signing".to_string(),
            "test.signature".to_string(),
        )
    }

    fn ordering_sensitive_record() -> Value {
        serde_json::json!({ "aa": 2, "b": 1 })
    }

    #[test]
    fn current_cid_matches_canonical_dag_cbor() {
        let signer = test_signer();
        let body = AttestationSigner::signable_body(
            ordering_sensitive_record().as_object().unwrap(),
            &signer.sig_type,
            "did:plc:test",
        );
        let expected =
            crate::cid_verify::compute_record_cid(&Value::Object(body.clone())).expect("encodable");
        assert_eq!(
            signer.compute_cid_current(&body).unwrap(),
            expected,
            "signing CID must equal what a conforming implementation derives"
        );
    }

    #[test]
    fn legacy_and_current_cids_actually_differ() {
        let signer = test_signer();
        let body = AttestationSigner::signable_body(
            ordering_sensitive_record().as_object().unwrap(),
            &signer.sig_type,
            "did:plc:test",
        );
        assert_ne!(
            signer.compute_cid_current(&body).unwrap(),
            signer.compute_cid_legacy(&body).unwrap(),
        );
    }

    #[test]
    fn new_signatures_verify_as_current() {
        let signer = test_signer();
        let mut record = ordering_sensitive_record();
        signer.sign_record(&mut record, "did:plc:test").unwrap();
        let sig = record["signatures"][0].clone();

        assert_eq!(
            signer
                .verify_record_signature_detailed(&record, &sig, "did:plc:test")
                .unwrap(),
            SignatureVerification::Valid(SignatureEncoding::Current),
        );
    }

    #[test]
    fn pre_fix_signatures_still_verify() {
        let signer = test_signer();
        let mut record = ordering_sensitive_record();
        signer
            .sign_record_legacy(&mut record, "did:plc:test")
            .unwrap();
        let sig = record["signatures"][0].clone();

        let outcome = signer
            .verify_record_signature_detailed(&record, &sig, "did:plc:test")
            .unwrap();
        assert_eq!(
            outcome,
            SignatureVerification::Valid(SignatureEncoding::Legacy),
        );
        assert!(outcome.is_legacy());
        assert!(
            signer
                .verify_record_signature(&record, &sig, "did:plc:test")
                .unwrap()
        );
    }

    #[test]
    fn pre_fix_signatures_with_links_still_verify() {
        let signer = test_signer();
        let mut record = serde_json::json!({
            "ref": { "$link": "bafyreigbtj4x7ip5legnfznufuopl4sg4knzc2cof6duas4b3q2fy6swua" },
            "b": 1,
            "aa": 2
        });
        signer
            .sign_record_legacy(&mut record, "did:plc:test")
            .unwrap();
        let sig = record["signatures"][0].clone();

        assert!(
            signer
                .verify_record_signature_detailed(&record, &sig, "did:plc:test")
                .unwrap()
                .is_legacy()
        );
    }

    /// The data model makes `=` padding optional on `$bytes`, and at least one
    /// major implementation (jetstream) omits it. A signature we emitted padded
    /// comes back off the firehose 86 characters instead of 88, and must still
    /// verify — the padding carries no information, only the bytes do.
    #[test]
    fn signatures_verify_with_unpadded_bytes() {
        let signer = test_signer();
        let mut record = ordering_sensitive_record();
        signer.sign_record(&mut record, "did:plc:test").unwrap();

        let padded = record["signatures"][0]["signature"]["$bytes"]
            .as_str()
            .unwrap()
            .to_string();
        let unpadded = padded.trim_end_matches('=').to_string();
        assert_ne!(
            padded, unpadded,
            "signing must emit padding for this to test anything"
        );

        record["signatures"][0]["signature"]["$bytes"] = serde_json::json!(unpadded);
        let sig = record["signatures"][0].clone();

        assert_eq!(
            signer
                .verify_record_signature_detailed(&record, &sig, "did:plc:test")
                .unwrap(),
            SignatureVerification::Valid(SignatureEncoding::Current),
        );
    }

    #[test]
    fn fallback_does_not_accept_tampered_records() {
        let signer = test_signer();

        for legacy in [false, true] {
            let mut record = serde_json::json!({ "aa": 2, "b": 1, "text": "original" });
            if legacy {
                signer
                    .sign_record_legacy(&mut record, "did:plc:test")
                    .unwrap();
            } else {
                signer.sign_record(&mut record, "did:plc:test").unwrap();
            }
            let sig = record["signatures"][0].clone();

            record["text"] = serde_json::json!("tampered");
            assert_eq!(
                signer
                    .verify_record_signature_detailed(&record, &sig, "did:plc:test")
                    .unwrap(),
                SignatureVerification::Invalid,
                "tampered record must be rejected (legacy signature: {legacy})"
            );
        }
    }

    #[test]
    fn fallback_preserves_repository_binding() {
        let signer = test_signer();
        let mut record = ordering_sensitive_record();
        signer
            .sign_record_legacy(&mut record, "did:plc:original")
            .unwrap();
        let sig = record["signatures"][0].clone();

        assert!(
            signer
                .verify_record_signature_detailed(&record, &sig, "did:plc:original")
                .unwrap()
                .is_valid()
        );
        assert_eq!(
            signer
                .verify_record_signature_detailed(&record, &sig, "did:plc:attacker")
                .unwrap(),
            SignatureVerification::Invalid,
        );
    }

    #[test]
    fn fallback_rejects_forged_signatures() {
        let signer = test_signer();
        let mut record = ordering_sensitive_record();
        signer.sign_record(&mut record, "did:plc:test").unwrap();

        let forged = serde_json::json!({
            "$type": "test.signature",
            "key": "did:web:test.example#signing",
            "signature": { "$bytes": base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD, [0x01u8; 64]) }
        });
        assert_eq!(
            signer
                .verify_record_signature_detailed(&record, &forged, "did:plc:test")
                .unwrap(),
            SignatureVerification::Invalid,
        );
    }
}

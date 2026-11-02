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

    /// Create a new signer with a fixed test key. Not gated behind
    /// `#[cfg(test)]`: an integration test under `tests/` compiles this crate
    /// as an ordinary dependency, not with `cfg(test)`, so a unit-test-only
    /// constructor is unreachable there — the same reason `test_support`
    /// stays ungated. For testing only; the key is fixed and public.
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
    let private_key = match crate::config::vars::var("ATTESTATION_PRIVATE_KEY") {
        Some(k) => k,
        None => return Ok(None),
    };

    let key_id = crate::config::vars::var("ATTESTATION_KEY_ID")
        .unwrap_or_else(|| "did:web:localhost#attestation".to_string());

    let sig_type = crate::config::vars::var("ATTESTATION_SIG_TYPE")
        .unwrap_or_else(|| "games.gamesgamesgamesgames.attestation".to_string());

    Ok(Some(AttestationSigner::from_hex(
        &private_key,
        key_id,
        sig_type,
    )?))
}

/// The setting an earlier version kept the key in. Reserved, so the generic
/// settings routes can neither show nor replace it.
pub const LEGACY_KEY_SETTING: &str = "attestation_private_key";

/// A key as it goes in `happyview_attestation_key`.
fn seal_key(
    encryption_key: Option<&[u8; 32]>,
    hex_key: &str,
) -> Result<(String, i32), AttestationError> {
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
    Ok(match encryption_key {
        Some(key) => (
            BASE64.encode(
                crate::plugin::encryption::encrypt(key, hex_key.as_bytes())
                    .map_err(|e| AttestationError::Encoding(e.to_string()))?,
            ),
            1,
        ),
        None => (hex_key.to_string(), 0),
    })
}

fn open_key(
    encryption_key: Option<&[u8; 32]>,
    stored: &str,
    encrypted: bool,
) -> Result<String, AttestationError> {
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
    if !encrypted {
        return Ok(stored.to_string());
    }
    let unreadable = || {
        AttestationError::InvalidKey(
            "the stored attestation key can't be read with this TOKEN_ENCRYPTION_KEY".to_string(),
        )
    };
    let key = encryption_key.ok_or_else(unreadable)?;
    let bytes = BASE64.decode(stored).map_err(|_| unreadable())?;
    let plain = crate::plugin::encryption::decrypt(key, &bytes).map_err(|_| unreadable())?;
    String::from_utf8(plain).map_err(|_| unreadable())
}

async fn read_key(
    db: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
) -> Result<Option<(String, i64)>, AttestationError> {
    let sql = crate::db::adapt_sql(
        "SELECT private_key, CAST(encrypted AS BIGINT) FROM happyview_attestation_key WHERE id = 1",
        backend,
    );
    crate::db::query_as(&sql)
        .fetch_optional(db)
        .await
        .map_err(|e| AttestationError::Encoding(format!("db query failed: {e}")))
}

/// Store a key unless one is already there, so instances booting together
/// settle on one key.
async fn insert_key<'e, E>(
    db: E,
    backend: crate::db::DatabaseBackend,
    encryption_key: Option<&[u8; 32]>,
    hex_key: &str,
) -> Result<(), AttestationError>
where
    E: sqlx::Executor<'e, Database = sqlx::Any>,
{
    let (stored, encrypted) = seal_key(encryption_key, hex_key)?;
    let sql = crate::db::adapt_sql(
        "INSERT INTO happyview_attestation_key (id, private_key, encrypted, created_at) \
         VALUES (1, ?, ?, ?) ON CONFLICT (id) DO NOTHING",
        backend,
    );
    crate::db::query(&sql)
        .bind(stored)
        .bind(encrypted)
        .bind(crate::db::now_rfc3339())
        .execute(db)
        .await
        .map_err(|e| AttestationError::Encoding(format!("failed to persist key: {e}")))?;
    Ok(())
}

/// Bring a stored key up to date: out of the settings, then encrypted once the
/// instance has a key to encrypt it with. The key itself never changes, because
/// signatures already published have to keep verifying. A failure leaves it
/// where it was for the next boot.
async fn migrate_key(
    db: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
    encryption_key: Option<&[u8; 32]>,
) -> Result<(), AttestationError> {
    use crate::db::adapt_sql;
    let fail = |e: sqlx::Error| AttestationError::Encoding(format!("failed to move key: {e}"));

    let select = adapt_sql(
        "SELECT value FROM happyview_instance_settings WHERE key = ?",
        backend,
    );
    let legacy: Option<(String,)> = crate::db::query_as(&select)
        .bind(LEGACY_KEY_SETTING)
        .fetch_optional(db)
        .await
        .map_err(fail)?;
    if let Some((hex_key,)) = legacy {
        let mut tx = db.begin().await.map_err(fail)?;
        insert_key(&mut *tx, backend, encryption_key, &hex_key).await?;
        let delete = adapt_sql(
            "DELETE FROM happyview_instance_settings WHERE key = ?",
            backend,
        );
        crate::db::query(&delete)
            .bind(LEGACY_KEY_SETTING)
            .execute(&mut *tx)
            .await
            .map_err(fail)?;
        tx.commit().await.map_err(fail)?;
    }

    if let (Some(_), Some((stored, 0))) = (encryption_key, read_key(db, backend).await?) {
        let (sealed, _) = seal_key(encryption_key, &stored)?;
        let update = adapt_sql(
            "UPDATE happyview_attestation_key SET private_key = ?, encrypted = 1 \
             WHERE id = 1 AND encrypted = 0 AND private_key = ?",
            backend,
        );
        crate::db::query(&update)
            .bind(sealed)
            .bind(stored)
            .execute(db)
            .await
            .map_err(fail)?;
    }
    Ok(())
}

pub async fn load_or_generate(
    db: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
    public_url: &str,
    encryption_key: Option<&[u8; 32]>,
) -> Result<AttestationSigner, AttestationError> {
    use crate::db::adapt_sql;

    // Before the environment is consulted, so a key from an earlier boot is
    // moved even on an instance that now signs with one from its environment.
    migrate_key(db, backend, encryption_key).await?;

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
    if let Some((stored, encrypted)) = read_key(db, backend).await? {
        // An unreadable key is an error, never a reason to generate another:
        // that would strand every signature the old one made.
        let hex_key = open_key(encryption_key, &stored, encrypted != 0)?;
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

    insert_key(db, backend, encryption_key, &hex_key).await?;

    let upsert_sql = adapt_sql(
        "INSERT INTO happyview_instance_settings (key, value, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        backend,
    );
    let now = crate::db::now_rfc3339();

    for (k, v) in [
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

    // Reread rather than use what was generated: another instance may have
    // stored first.
    let (stored, encrypted) = read_key(db, backend)
        .await?
        .ok_or_else(|| AttestationError::Encoding("attestation key was not stored".to_string()))?;
    AttestationSigner::from_hex(
        &open_key(encryption_key, &stored, encrypted != 0)?,
        default_key_id,
        default_sig_type,
    )
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
        let pool = crate::test_support::migrated_memory_pool().await;

        let signer = load_or_generate(
            &pool,
            crate::db::DatabaseBackend::Sqlite,
            "https://happyview.example.com",
            None,
        )
        .await
        .expect("should generate a key");

        assert_eq!(signer.key_id, "did:web:happyview.example.com#attestation");

        let signer2 = load_or_generate(
            &pool,
            crate::db::DatabaseBackend::Sqlite,
            "https://happyview.example.com",
            None,
        )
        .await
        .expect("should load from DB");

        assert_eq!(signer.public_key_bytes(), signer2.public_key_bytes());
    }

    #[tokio::test]
    async fn test_load_or_generate_sign_verify_roundtrip() {
        let pool = crate::test_support::migrated_memory_pool().await;

        let signer = load_or_generate(
            &pool,
            crate::db::DatabaseBackend::Sqlite,
            "https://example.com",
            None,
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

    const BACKEND: crate::db::DatabaseBackend = crate::db::DatabaseBackend::Sqlite;
    const URL: &str = "https://happyview.example.com";
    const LEGACY_HEX: &str = "4c0883a69102937d6231471b5dbb6204fe5129617082796fe9f4a4c7e6f9a1b2";
    const KEY: [u8; 32] = [4; 32];

    async fn pool_with_legacy_key() -> sqlx::AnyPool {
        let pool = crate::test_support::migrated_memory_pool().await;
        crate::db::query(
            "INSERT INTO happyview_instance_settings (key, value, updated_at) VALUES (?, ?, ?)",
        )
        .bind(LEGACY_KEY_SETTING)
        .bind(LEGACY_HEX)
        .bind(crate::db::now_rfc3339())
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    async fn legacy_rows(pool: &sqlx::AnyPool) -> i64 {
        let (count,): (i64,) =
            crate::db::query_as("SELECT COUNT(*) FROM happyview_instance_settings WHERE key = ?")
                .bind(LEGACY_KEY_SETTING)
                .fetch_one(pool)
                .await
                .unwrap();
        count
    }

    fn legacy_public_key() -> Vec<u8> {
        AttestationSigner::from_hex(LEGACY_HEX, "k".into(), "t".into())
            .unwrap()
            .public_key_bytes()
    }

    /// The same key comes out, so what it signed before the move still verifies.
    #[tokio::test]
    async fn an_existing_key_moves_out_of_settings_unchanged_and_encrypted() {
        let pool = pool_with_legacy_key().await;

        let signer = load_or_generate(&pool, BACKEND, URL, Some(&KEY))
            .await
            .unwrap();

        assert_eq!(signer.public_key_bytes(), legacy_public_key());
        assert_eq!(legacy_rows(&pool).await, 0);
        let (stored, encrypted) = read_key(&pool, BACKEND).await.unwrap().unwrap();
        assert_eq!(encrypted, 1);
        assert!(!stored.contains(LEGACY_HEX));
    }

    #[tokio::test]
    async fn a_new_key_is_stored_encrypted_and_never_in_settings() {
        let pool = crate::test_support::migrated_memory_pool().await;

        let first = load_or_generate(&pool, BACKEND, URL, Some(&KEY))
            .await
            .unwrap();
        let again = load_or_generate(&pool, BACKEND, URL, Some(&KEY))
            .await
            .unwrap();

        assert_eq!(first.public_key_bytes(), again.public_key_bytes());
        assert_eq!(legacy_rows(&pool).await, 0);
        assert_eq!(read_key(&pool, BACKEND).await.unwrap().unwrap().1, 1);
    }

    /// With no encryption key the signing key still leaves the settings, and
    /// the first boot that has one encrypts it.
    #[tokio::test]
    async fn without_an_encryption_key_it_moves_and_a_later_key_seals_it() {
        let pool = pool_with_legacy_key().await;

        let signer = load_or_generate(&pool, BACKEND, URL, None).await.unwrap();
        assert_eq!(signer.public_key_bytes(), legacy_public_key());
        assert_eq!(legacy_rows(&pool).await, 0);
        assert_eq!(read_key(&pool, BACKEND).await.unwrap().unwrap().1, 0);

        let signer = load_or_generate(&pool, BACKEND, URL, Some(&KEY))
            .await
            .unwrap();
        assert_eq!(signer.public_key_bytes(), legacy_public_key());
        assert_eq!(read_key(&pool, BACKEND).await.unwrap().unwrap().1, 1);
    }

    /// Generating a replacement would strand every signature the key made.
    #[tokio::test]
    async fn a_key_that_will_not_open_is_an_error_and_is_not_replaced() {
        let pool = pool_with_legacy_key().await;
        load_or_generate(&pool, BACKEND, URL, Some(&KEY))
            .await
            .unwrap();
        let before = read_key(&pool, BACKEND).await.unwrap();

        for key in [Some(&[8u8; 32]), None] {
            assert!(load_or_generate(&pool, BACKEND, URL, key).await.is_err());
            assert_eq!(read_key(&pool, BACKEND).await.unwrap(), before);
        }
        let signer = load_or_generate(&pool, BACKEND, URL, Some(&KEY))
            .await
            .unwrap();
        assert_eq!(signer.public_key_bytes(), legacy_public_key());
    }

    /// A second boot, or a second instance, finding the key already moved keeps
    /// what's stored.
    #[tokio::test]
    async fn the_move_is_safe_to_repeat() {
        let pool = pool_with_legacy_key().await;
        migrate_key(&pool, BACKEND, Some(&KEY)).await.unwrap();
        let before = read_key(&pool, BACKEND).await.unwrap();

        migrate_key(&pool, BACKEND, Some(&KEY)).await.unwrap();

        assert_eq!(read_key(&pool, BACKEND).await.unwrap(), before);
    }
}

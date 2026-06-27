use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::Utc;
use p256::ecdsa::SigningKey;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sqlx::AnyPool;
use uuid::Uuid;

use crate::db::{DatabaseBackend, adapt_sql};
use crate::error::AppError;
use crate::plc::private_key_to_did_key;
use crate::plugin::encryption::{decrypt, encrypt};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationMethod {
    pub id: String,
    pub fragment_id: String,
    pub key_type: String,
    pub public_key_multibase: String,
    pub created_at: String,
}

type VerificationMethodRow = (String, String, String, String, String);

fn parse_row(r: VerificationMethodRow) -> VerificationMethod {
    VerificationMethod {
        id: r.0,
        fragment_id: r.1,
        key_type: r.2,
        public_key_multibase: r.3,
        created_at: r.4,
    }
}

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

pub async fn list_methods(
    db: &AnyPool,
    backend: DatabaseBackend,
) -> Result<Vec<VerificationMethod>, AppError> {
    let sql = adapt_sql(
        "SELECT id, fragment_id, key_type, public_key_multibase, created_at FROM happyview_verification_methods ORDER BY created_at",
        backend,
    );

    let rows: Vec<VerificationMethodRow> = sqlx::query_as(&sql)
        .fetch_all(db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list verification methods: {e}")))?;

    Ok(rows.into_iter().map(parse_row).collect())
}

pub async fn get_method_by_fragment(
    db: &AnyPool,
    backend: DatabaseBackend,
    fragment_id: &str,
) -> Result<Option<VerificationMethod>, AppError> {
    let sql = adapt_sql(
        "SELECT id, fragment_id, key_type, public_key_multibase, created_at FROM happyview_verification_methods WHERE fragment_id = ?",
        backend,
    );

    let row: Option<VerificationMethodRow> = sqlx::query_as(&sql)
        .bind(fragment_id)
        .fetch_optional(db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to get verification method: {e}")))?;

    Ok(row.map(parse_row))
}

pub async fn create_method(
    db: &AnyPool,
    backend: DatabaseBackend,
    fragment_id: &str,
    encryption_key: &[u8; 32],
) -> Result<VerificationMethod, AppError> {
    let (private_key_bytes, public_key_multibase) = generate_p256_keypair()?;

    let encrypted = encrypt(encryption_key, &private_key_bytes)
        .map_err(|e| AppError::Internal(format!("failed to encrypt verification key: {e}")))?;
    let encrypted_b64 = STANDARD.encode(&encrypted);

    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();

    let sql = adapt_sql(
        "INSERT INTO happyview_verification_methods (id, fragment_id, key_type, public_key_multibase, private_key_enc, created_at) VALUES (?, ?, 'Multikey', ?, ?, ?)",
        backend,
    );

    sqlx::query(&sql)
        .bind(&id)
        .bind(fragment_id)
        .bind(&public_key_multibase)
        .bind(encrypted_b64.as_bytes())
        .bind(&now)
        .execute(db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create verification method: {e}")))?;

    Ok(VerificationMethod {
        id,
        fragment_id: fragment_id.to_string(),
        key_type: "Multikey".to_string(),
        public_key_multibase,
        created_at: now,
    })
}

pub async fn delete_method(
    db: &AnyPool,
    backend: DatabaseBackend,
    fragment_id: &str,
) -> Result<bool, AppError> {
    let sql = adapt_sql(
        "DELETE FROM happyview_verification_methods WHERE fragment_id = ?",
        backend,
    );

    let result = sqlx::query(&sql)
        .bind(fragment_id)
        .execute(db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete verification method: {e}")))?;

    Ok(result.rows_affected() > 0)
}

pub async fn get_private_key_bytes(
    db: &AnyPool,
    backend: DatabaseBackend,
    fragment_id: &str,
    encryption_key: &[u8; 32],
) -> Result<Option<Vec<u8>>, AppError> {
    let sql = adapt_sql(
        "SELECT private_key_enc FROM happyview_verification_methods WHERE fragment_id = ?",
        backend,
    );

    let row: Option<(Vec<u8>,)> = sqlx::query_as(&sql)
        .bind(fragment_id)
        .fetch_optional(db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to fetch verification key: {e}")))?;

    let Some((encrypted_raw,)) = row else {
        return Ok(None);
    };

    let encrypted_b64 = String::from_utf8(encrypted_raw)
        .map_err(|e| AppError::Internal(format!("invalid private_key_enc encoding: {e}")))?;
    let encrypted = STANDARD
        .decode(&encrypted_b64)
        .map_err(|e| AppError::Internal(format!("failed to decode private_key_enc: {e}")))?;
    let key_bytes = decrypt(encryption_key, &encrypted)
        .map_err(|e| AppError::Internal(format!("failed to decrypt verification key: {e}")))?;

    Ok(Some(key_bytes))
}

// ---------------------------------------------------------------------------
// Key generation
// ---------------------------------------------------------------------------

fn generate_p256_keypair() -> Result<(Vec<u8>, String), AppError> {
    let mut rng_bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut rng_bytes);

    let signing_key = SigningKey::from_bytes((&rng_bytes[..]).into())
        .map_err(|e| AppError::Internal(format!("failed to generate verification key: {e}")))?;

    let verifying_key = signing_key.verifying_key();
    let compressed = verifying_key.to_encoded_point(true);

    // Multikey format: 0x8024 varint for P-256 + compressed public key, base58btc
    let mut multikey_bytes = vec![0x80, 0x24];
    multikey_bytes.extend_from_slice(compressed.as_bytes());
    let public_key_multibase = multibase::encode(multibase::Base::Base58Btc, &multikey_bytes);

    Ok((rng_bytes.to_vec(), public_key_multibase))
}

pub fn private_key_bytes_to_signing_key(key_bytes: &[u8]) -> Result<SigningKey, AppError> {
    SigningKey::from_bytes(key_bytes.into())
        .map_err(|e| AppError::Internal(format!("invalid verification signing key: {e}")))
}

pub fn private_key_bytes_to_did_key(key_bytes: &[u8]) -> Result<String, AppError> {
    private_key_to_did_key(key_bytes)
}

// ---------------------------------------------------------------------------
// Auto-provision
// ---------------------------------------------------------------------------

/// Ensure `#atproto_space` verification method exists; create it if not.
pub async fn ensure_atproto_space_method(
    db: &AnyPool,
    backend: DatabaseBackend,
    encryption_key: &[u8; 32],
) -> Result<VerificationMethod, AppError> {
    if let Some(existing) = get_method_by_fragment(db, backend, "#atproto_space").await? {
        return Ok(existing);
    }
    create_method(db, backend, "#atproto_space", encryption_key).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_p256_keypair_produces_multibase_key() {
        let (key_bytes, multibase) = generate_p256_keypair().unwrap();
        assert_eq!(key_bytes.len(), 32);
        // base58btc multibase starts with 'z'
        assert!(
            multibase.starts_with('z'),
            "expected base58btc prefix: {multibase}"
        );
    }

    #[test]
    fn private_key_bytes_to_signing_key_roundtrip() {
        let (key_bytes, _) = generate_p256_keypair().unwrap();
        let signing_key = private_key_bytes_to_signing_key(&key_bytes).unwrap();
        // Re-derive bytes should equal original
        assert_eq!(signing_key.to_bytes().as_slice(), key_bytes.as_slice());
    }

    #[test]
    fn private_key_bytes_to_did_key_format() {
        let (key_bytes, _) = generate_p256_keypair().unwrap();
        let did_key = private_key_bytes_to_did_key(&key_bytes).unwrap();
        assert!(
            did_key.starts_with("did:key:z"),
            "expected did:key: prefix: {did_key}"
        );
    }
}

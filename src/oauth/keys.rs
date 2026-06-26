use p256::ecdsa::SigningKey;
use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::plugin::encryption::{decrypt, encrypt};

/// A generated ES256 DPoP keypair with its JWK representation.
#[derive(Debug, Clone, Serialize)]
pub struct DpopKeypair {
    /// The private key as a JWK (returned to the app, also stored encrypted).
    pub private_jwk: serde_json::Value,
    /// The public key as a JWK.
    pub public_jwk: serde_json::Value,
    /// The JWK thumbprint (RFC 7638) using SHA-256.
    pub thumbprint: String,
}

/// Generate a new ES256 (P-256) DPoP keypair.
pub fn generate_dpop_keypair() -> Result<DpopKeypair, AppError> {
    let mut rng_bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut rng_bytes);

    let signing_key = SigningKey::from_bytes((&rng_bytes[..]).into())
        .map_err(|e| AppError::Internal(format!("failed to generate signing key: {e}")))?;

    let verifying_key = signing_key.verifying_key();
    let public_point = verifying_key.to_encoded_point(false);

    let x_bytes = public_point
        .x()
        .ok_or_else(|| AppError::Internal("missing x coordinate".into()))?;
    let y_bytes = public_point
        .y()
        .ok_or_else(|| AppError::Internal("missing y coordinate".into()))?;

    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    let x_b64 = URL_SAFE_NO_PAD.encode(x_bytes);
    let y_b64 = URL_SAFE_NO_PAD.encode(y_bytes);
    let d_b64 = URL_SAFE_NO_PAD.encode(rng_bytes);

    let public_jwk = serde_json::json!({
        "kty": "EC",
        "crv": "P-256",
        "x": x_b64,
        "y": y_b64,
    });

    let private_jwk = serde_json::json!({
        "kty": "EC",
        "crv": "P-256",
        "x": x_b64,
        "y": y_b64,
        "d": d_b64,
    });

    let thumbprint = compute_jwk_thumbprint(&public_jwk)?;

    Ok(DpopKeypair {
        private_jwk,
        public_jwk,
        thumbprint,
    })
}

/// Compute the JWK Thumbprint (RFC 7638) using SHA-256.
///
/// For EC keys, the canonical JSON is: {"crv":"...","kty":"EC","x":"...","y":"..."}
/// (alphabetical order of required members).
pub fn compute_jwk_thumbprint(jwk: &serde_json::Value) -> Result<String, AppError> {
    let kty = jwk["kty"]
        .as_str()
        .ok_or_else(|| AppError::Internal("JWK missing kty".into()))?;
    let crv = jwk["crv"]
        .as_str()
        .ok_or_else(|| AppError::Internal("JWK missing crv".into()))?;
    let x = jwk["x"]
        .as_str()
        .ok_or_else(|| AppError::Internal("JWK missing x".into()))?;
    let y = jwk["y"]
        .as_str()
        .ok_or_else(|| AppError::Internal("JWK missing y".into()))?;

    // RFC 7638: lexicographic order of required members
    let canonical = format!(
        r#"{{"crv":"{}","kty":"{}","x":"{}","y":"{}"}}"#,
        crv, kty, x, y
    );

    let hash = Sha256::digest(canonical.as_bytes());
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    Ok(URL_SAFE_NO_PAD.encode(hash))
}

/// Store a DPoP key in the database with the private key encrypted.
#[allow(clippy::too_many_arguments)]
pub async fn store_dpop_key(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    encryption_key: &[u8; 32],
    id: &str,
    provision_id: &str,
    api_client_id: &str,
    keypair: &DpopKeypair,
    pkce_challenge: Option<&str>,
) -> Result<(), AppError> {
    let private_jwk_bytes = serde_json::to_vec(&keypair.private_jwk)
        .map_err(|e| AppError::Internal(format!("failed to serialize JWK: {e}")))?;

    let encrypted = encrypt(encryption_key, &private_jwk_bytes)
        .map_err(|e| AppError::Internal(format!("failed to encrypt DPoP key: {e}")))?;

    let now = now_rfc3339();
    let sql = adapt_sql(
        "INSERT INTO happyview_dpop_keys (id, provision_id, api_client_id, private_key_enc, jwk_thumbprint, pkce_challenge, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        backend,
    );

    sqlx::query(&sql)
        .bind(id)
        .bind(provision_id)
        .bind(api_client_id)
        .bind(&encrypted)
        .bind(&keypair.thumbprint)
        .bind(pkce_challenge)
        .bind(&now)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to store DPoP key: {e}")))?;

    Ok(())
}

/// Retrieve and decrypt a DPoP key by provision_id.
pub async fn get_dpop_key(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    encryption_key: &[u8; 32],
    provision_id: &str,
) -> Result<(String, String, serde_json::Value, String, Option<String>), AppError> {
    // Returns: (id, api_client_id, private_jwk, thumbprint, pkce_challenge)
    let sql = adapt_sql(
        "SELECT id, api_client_id, private_key_enc, jwk_thumbprint, pkce_challenge FROM happyview_dpop_keys WHERE provision_id = ?",
        backend,
    );

    #[allow(clippy::type_complexity)]
    let row: Option<(String, String, Vec<u8>, String, Option<String>)> = sqlx::query_as(&sql)
        .bind(provision_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up DPoP key: {e}")))?;

    let (id, api_client_id, encrypted, thumbprint, pkce_challenge) =
        row.ok_or_else(|| AppError::NotFound("DPoP key not found".into()))?;

    let decrypted = decrypt(encryption_key, &encrypted)
        .map_err(|e| AppError::Internal(format!("failed to decrypt DPoP key: {e}")))?;

    let private_jwk: serde_json::Value = serde_json::from_slice(&decrypted)
        .map_err(|e| AppError::Internal(format!("failed to parse DPoP key: {e}")))?;

    Ok((id, api_client_id, private_jwk, thumbprint, pkce_challenge))
}

/// Retrieve the JWK thumbprint for a DPoP key by its database ID.
pub async fn get_dpop_key_thumbprint(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    key_id: &str,
) -> Result<String, AppError> {
    let sql = adapt_sql(
        "SELECT jwk_thumbprint FROM happyview_dpop_keys WHERE id = ?",
        backend,
    );

    let row: Option<(String,)> = sqlx::query_as(&sql)
        .bind(key_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up DPoP key: {e}")))?;

    row.map(|(t,)| t)
        .ok_or_else(|| AppError::NotFound("DPoP key not found".into()))
}

/// Look up a DPoP key ID by api_client_id and JWK thumbprint.
pub async fn get_dpop_key_id_by_thumbprint(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    api_client_id: &str,
    thumbprint: &str,
) -> Result<String, AppError> {
    let sql = adapt_sql(
        "SELECT id FROM happyview_dpop_keys WHERE api_client_id = ? AND jwk_thumbprint = ?",
        backend,
    );

    let row: Option<(String,)> = sqlx::query_as(&sql)
        .bind(api_client_id)
        .bind(thumbprint)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up DPoP key: {e}")))?;

    row.map(|(id,)| id)
        .ok_or_else(|| AppError::Auth("no DPoP key matching proof thumbprint".into()))
}

/// Delete a DPoP key and its associated session.
pub async fn delete_dpop_key(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    dpop_key_id: &str,
) -> Result<(), AppError> {
    // Session is deleted by CASCADE, but be explicit for clarity
    let session_sql = adapt_sql(
        "DELETE FROM happyview_dpop_sessions WHERE dpop_key_id = ?",
        backend,
    );
    let _ = sqlx::query(&session_sql)
        .bind(dpop_key_id)
        .execute(pool)
        .await;

    let key_sql = adapt_sql("DELETE FROM happyview_dpop_keys WHERE id = ?", backend);
    sqlx::query(&key_sql)
        .bind(dpop_key_id)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete DPoP key: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_keypair_produces_valid_jwk() {
        let keypair = generate_dpop_keypair().unwrap();

        // Private JWK has d parameter
        assert!(keypair.private_jwk["d"].is_string());
        assert_eq!(keypair.private_jwk["kty"], "EC");
        assert_eq!(keypair.private_jwk["crv"], "P-256");

        // Public JWK has no d parameter
        assert!(keypair.public_jwk["d"].is_null());
        assert_eq!(keypair.public_jwk["kty"], "EC");
        assert_eq!(keypair.public_jwk["crv"], "P-256");

        // Thumbprint is a base64url string
        assert!(!keypair.thumbprint.is_empty());
        assert!(!keypair.thumbprint.contains('='));
    }

    #[test]
    fn generate_keypair_produces_unique_keys() {
        let kp1 = generate_dpop_keypair().unwrap();
        let kp2 = generate_dpop_keypair().unwrap();
        assert_ne!(kp1.private_jwk["d"], kp2.private_jwk["d"]);
        assert_ne!(kp1.thumbprint, kp2.thumbprint);
    }

    #[test]
    fn thumbprint_is_deterministic() {
        let keypair = generate_dpop_keypair().unwrap();
        let t1 = compute_jwk_thumbprint(&keypair.public_jwk).unwrap();
        let t2 = compute_jwk_thumbprint(&keypair.public_jwk).unwrap();
        assert_eq!(t1, t2);
        assert_eq!(t1, keypair.thumbprint);
    }

    #[test]
    fn thumbprint_differs_for_different_keys() {
        let kp1 = generate_dpop_keypair().unwrap();
        let kp2 = generate_dpop_keypair().unwrap();
        let t1 = compute_jwk_thumbprint(&kp1.public_jwk).unwrap();
        let t2 = compute_jwk_thumbprint(&kp2.public_jwk).unwrap();
        assert_ne!(t1, t2);
    }
}

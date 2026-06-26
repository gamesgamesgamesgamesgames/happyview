use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::SigningKey;
use rand::RngCore;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::plugin::encryption::{decrypt, encrypt};
use crate::spaces::credential::{
    DEFAULT_CREDENTIAL_TTL_SECS, SpaceCredentialClaims, sign_credential,
};
use crate::spaces::types::{AccessMode, Space};

pub struct IssuedCredential {
    pub token: String,
    pub expires_at: String,
}

pub async fn issue_credential(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    encryption_key: &[u8; 32],
    space: &Space,
    subject_did: &str,
    client_id: Option<&str>,
) -> Result<IssuedCredential, AppError> {
    check_app_access(space, client_id)?;

    let private_jwk = get_or_create_signing_key(pool, backend, encryption_key, space).await?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let exp = now + DEFAULT_CREDENTIAL_TTL_SECS;

    let claims = SpaceCredentialClaims {
        iss: space.did.clone(),
        sub: subject_did.to_string(),
        space: format!("ats://{}/{}/{}", space.did, space.type_nsid, space.skey),
        scope: "read".into(),
        iat: now,
        exp,
    };

    let token = sign_credential(&claims, &private_jwk)?;

    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
    store_credential_record(pool, backend, &space.id, subject_did, &token_hash, exp).await?;

    let expires_at = chrono::DateTime::from_timestamp(exp as i64, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();

    Ok(IssuedCredential { token, expires_at })
}

pub fn check_app_access(space: &Space, client_id: Option<&str>) -> Result<(), AppError> {
    let Some(client_id) = client_id else {
        return Ok(());
    };

    match space.access_mode {
        AccessMode::DefaultDeny => {
            if let Some(ref allowlist) = space.app_allowlist {
                if !allowlist.iter().any(|id| id == client_id) {
                    return Err(AppError::Forbidden(
                        "This app is not authorized to access this space".into(),
                    ));
                }
            } else {
                return Err(AppError::Forbidden(
                    "Space is in default_deny mode with no allowlist".into(),
                ));
            }
        }
        AccessMode::DefaultAllow => {
            if let Some(ref denylist) = space.app_denylist
                && denylist.iter().any(|id| id == client_id)
            {
                return Err(AppError::Forbidden(
                    "This app has been denied access to this space".into(),
                ));
            }
        }
    }

    Ok(())
}

async fn get_or_create_signing_key(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    encryption_key: &[u8; 32],
    space: &Space,
) -> Result<serde_json::Value, AppError> {
    let sql = adapt_sql(
        "SELECT signing_key_enc FROM happyview_space_dids WHERE space_id = ?",
        backend,
    );
    let row: Option<(Vec<u8>,)> = sqlx::query_as(&sql)
        .bind(&space.id)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up space signing key: {e}")))?;

    if let Some((encrypted,)) = row {
        let decrypted = decrypt(encryption_key, &encrypted)
            .map_err(|e| AppError::Internal(format!("failed to decrypt signing key: {e}")))?;
        let jwk: serde_json::Value = serde_json::from_slice(&decrypted)
            .map_err(|e| AppError::Internal(format!("failed to parse signing key: {e}")))?;
        return Ok(jwk);
    }

    let keypair = generate_space_keypair()?;
    let key_bytes = serde_json::to_vec(&keypair.private_jwk)
        .map_err(|e| AppError::Internal(format!("failed to serialize signing key: {e}")))?;
    let encrypted_signing = encrypt(encryption_key, &key_bytes)
        .map_err(|e| AppError::Internal(format!("failed to encrypt signing key: {e}")))?;

    // Rotation key is a separate keypair for recovery
    let rotation_keypair = generate_space_keypair()?;
    let rotation_bytes = serde_json::to_vec(&rotation_keypair.private_jwk)
        .map_err(|e| AppError::Internal(format!("failed to serialize rotation key: {e}")))?;
    let encrypted_rotation = encrypt(encryption_key, &rotation_bytes)
        .map_err(|e| AppError::Internal(format!("failed to encrypt rotation key: {e}")))?;

    let now = now_rfc3339();
    let insert_sql = adapt_sql(
        "INSERT INTO happyview_space_dids (id, did, space_id, signing_key_enc, rotation_key_enc, created_by, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        backend,
    );

    sqlx::query(&insert_sql)
        .bind(Uuid::new_v4().to_string())
        .bind(&space.did)
        .bind(&space.id)
        .bind(&encrypted_signing)
        .bind(&encrypted_rotation)
        .bind(&space.owner_did)
        .bind(&now)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to store space signing key: {e}")))?;

    Ok(keypair.private_jwk)
}

struct SpaceKeypair {
    private_jwk: serde_json::Value,
}

fn generate_space_keypair() -> Result<SpaceKeypair, AppError> {
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

    let x_b64 = URL_SAFE_NO_PAD.encode(x_bytes);
    let y_b64 = URL_SAFE_NO_PAD.encode(y_bytes);
    let d_b64 = URL_SAFE_NO_PAD.encode(rng_bytes);

    let private_jwk = serde_json::json!({
        "kty": "EC",
        "crv": "P-256",
        "x": x_b64,
        "y": y_b64,
        "d": d_b64,
    });

    Ok(SpaceKeypair { private_jwk })
}

async fn store_credential_record(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    space_id: &str,
    issued_to: &str,
    token_hash: &str,
    expires_at_epoch: u64,
) -> Result<(), AppError> {
    let now = now_rfc3339();
    let expires_at = chrono::DateTime::from_timestamp(expires_at_epoch as i64, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();

    let sql = adapt_sql(
        "INSERT INTO happyview_space_credentials (id, space_id, issued_to, token_hash, expires_at, created_at) VALUES (?, ?, ?, ?, ?, ?)",
        backend,
    );

    sqlx::query(&sql)
        .bind(Uuid::new_v4().to_string())
        .bind(space_id)
        .bind(issued_to)
        .bind(token_hash)
        .bind(&expires_at)
        .bind(&now)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to store credential record: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spaces::types::{AccessMode, Space, SpaceConfig};

    fn test_space(access_mode: AccessMode) -> Space {
        Space {
            id: "test-space".into(),
            did: "did:plc:owner".into(),
            owner_did: "did:plc:owner".into(),
            type_nsid: "com.example.forum".into(),
            skey: "main".into(),
            display_name: None,
            description: None,
            access_mode,
            app_allowlist: None,
            app_denylist: None,
            managing_app_did: None,
            config: SpaceConfig::default(),
            revision: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn app_access_default_allow_no_lists() {
        let space = test_space(AccessMode::DefaultAllow);
        assert!(check_app_access(&space, Some("any-app")).is_ok());
    }

    #[test]
    fn app_access_default_allow_denied() {
        let mut space = test_space(AccessMode::DefaultAllow);
        space.app_denylist = Some(vec!["bad-app".into()]);

        assert!(check_app_access(&space, Some("good-app")).is_ok());
        assert!(check_app_access(&space, Some("bad-app")).is_err());
    }

    #[test]
    fn app_access_default_deny_no_allowlist() {
        let space = test_space(AccessMode::DefaultDeny);
        assert!(check_app_access(&space, Some("any-app")).is_err());
    }

    #[test]
    fn app_access_default_deny_allowed() {
        let mut space = test_space(AccessMode::DefaultDeny);
        space.app_allowlist = Some(vec!["good-app".into()]);

        assert!(check_app_access(&space, Some("good-app")).is_ok());
        assert!(check_app_access(&space, Some("other-app")).is_err());
    }

    #[test]
    fn app_access_no_client_id_always_passes() {
        let space = test_space(AccessMode::DefaultDeny);
        assert!(check_app_access(&space, None).is_ok());
    }

    #[test]
    fn generate_keypair_produces_valid_jwk() {
        let kp = generate_space_keypair().unwrap();
        assert_eq!(kp.private_jwk["kty"], "EC");
        assert_eq!(kp.private_jwk["crv"], "P-256");
        assert!(kp.private_jwk["d"].is_string());
        assert!(kp.private_jwk["x"].is_string());
        assert!(kp.private_jwk["y"].is_string());
    }
}

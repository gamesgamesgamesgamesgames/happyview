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
    DEFAULT_CREDENTIAL_TTL_SECS, SpaceCredentialClaims, make_jti, sign_credential,
};
use crate::spaces::types::{AppAccess, MintPolicy, Space};

pub struct IssuedCredential {
    pub token: String,
    pub expires_at: String,
}

#[allow(clippy::too_many_arguments)]
pub async fn issue_credential(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    http: &reqwest::Client,
    encryption_key: &[u8; 32],
    space: &Space,
    subject_did: &str,
    client_id: Option<&str>,
    authority_did: &str,
) -> Result<IssuedCredential, AppError> {
    check_app_access(space, client_id)?;
    check_mint_policy(http, space, subject_did, client_id, authority_did).await?;

    let private_jwk = get_or_create_signing_key(pool, backend, encryption_key, space).await?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let exp = now + DEFAULT_CREDENTIAL_TTL_SECS;

    let claims = SpaceCredentialClaims {
        iss: space.authority_did.clone(),
        sub: format!("ats://{}/{}/{}", space.did, space.type_nsid, space.skey),
        iat: now,
        exp,
        jti: make_jti(),
    };

    let token = sign_credential(&claims, &private_jwk)?;

    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
    store_credential_record(pool, backend, &space.id, subject_did, &token_hash, exp).await?;

    let expires_at = chrono::DateTime::from_timestamp(exp as i64, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();

    Ok(IssuedCredential { token, expires_at })
}

async fn check_mint_policy(
    http: &reqwest::Client,
    space: &Space,
    subject_did: &str,
    client_id: Option<&str>,
    authority_did: &str,
) -> Result<(), AppError> {
    match space.mint_policy {
        MintPolicy::Public => Ok(()),
        MintPolicy::MemberList => {
            // Caller must already be a member; verified upstream by the credential issuance route.
            // We trust that the delegation token proves membership was checked.
            Ok(())
        }
        MintPolicy::ManagingApp => {
            let managing_app = space.managing_app_did.as_deref().ok_or_else(|| {
                AppError::Internal(
                    "space mint_policy is managing-app but managing_app_did is not set".into(),
                )
            })?;
            let space_uri = format!("ats://{}/{}/{}", space.did, space.type_nsid, space.skey);
            let granted = check_user_access_with_managing_app(
                http,
                managing_app,
                &space_uri,
                subject_did,
                client_id,
                authority_did,
            )
            .await?;
            if granted {
                Ok(())
            } else {
                Err(AppError::Forbidden(
                    "managing app denied access to this space".into(),
                ))
            }
        }
    }
}

async fn check_user_access_with_managing_app(
    http: &reqwest::Client,
    managing_app: &str,
    space_uri: &str,
    user_did: &str,
    client_id: Option<&str>,
    authority_did: &str,
) -> Result<bool, AppError> {
    // Parse DID#fragment — the fragment identifies the service endpoint in the DID doc.
    // For outbound callback we derive the endpoint from the DID.
    let (did, _fragment) = if let Some(pos) = managing_app.find('#') {
        (&managing_app[..pos], Some(&managing_app[pos + 1..]))
    } else {
        (managing_app, None)
    };

    // Resolve the managing app's PDS/service endpoint from its DID document.
    let endpoint = resolve_did_service_endpoint(http, did).await?;

    let url = format!(
        "{}/xrpc/com.atproto.simplespace.checkUserAccess",
        endpoint.trim_end_matches('/')
    );

    let mut body = serde_json::json!({
        "space": space_uri,
        "did": user_did,
    });
    if let Some(cid) = client_id {
        body["clientId"] = serde_json::Value::String(cid.to_string());
    }

    // Service auth: iss = authority_did, aud = managing_app DID.
    // We use a simple unsigned assertion here; a full implementation would sign with the space key.
    // For now we send the request without service auth and rely on the managing app to trust HappyView.
    let resp = http
        .post(&url)
        .json(&body)
        .header("X-Authority-Did", authority_did)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("checkUserAccess request failed: {e}")))?;

    if resp.status() == reqwest::StatusCode::FORBIDDEN
        || resp.status() == reqwest::StatusCode::UNAUTHORIZED
    {
        return Ok(false);
    }

    if !resp.status().is_success() {
        return Err(AppError::Internal(format!(
            "checkUserAccess returned unexpected status {}",
            resp.status()
        )));
    }

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("checkUserAccess response parse failed: {e}")))?;

    Ok(json
        .get("granted")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

async fn resolve_did_service_endpoint(
    http: &reqwest::Client,
    did: &str,
) -> Result<String, AppError> {
    let url = if did.starts_with("did:plc:") {
        format!("https://plc.directory/{did}")
    } else if did.starts_with("did:web:") {
        let identifier = did.strip_prefix("did:web:").unwrap();
        let mut segments = identifier.split(':');
        let host = segments.next().unwrap();
        let path_segments: Vec<&str> = segments.collect();
        if path_segments.is_empty() {
            format!("https://{host}/.well-known/did.json")
        } else {
            format!("https://{host}/{}/did.json", path_segments.join("/"))
        }
    } else {
        return Err(AppError::BadRequest(format!(
            "unsupported DID method for managing app: {did}"
        )));
    };

    #[derive(serde::Deserialize)]
    struct DidDoc {
        #[serde(default)]
        service: Vec<DidService>,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct DidService {
        id: String,
        service_endpoint: String,
    }

    let resp = http
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("DID resolution failed for {did}: {e}")))?;

    if !resp.status().is_success() {
        return Err(AppError::Internal(format!(
            "DID resolution returned {} for {did}",
            resp.status()
        )));
    }

    let doc: DidDoc = resp
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("invalid DID document for {did}: {e}")))?;

    doc.service
        .iter()
        .find(|s| s.id == "#atproto_pds" || s.id == format!("{did}#atproto_pds"))
        .map(|s| s.service_endpoint.clone())
        .ok_or_else(|| AppError::Internal(format!("no #atproto_pds service in DID doc for {did}")))
}

pub fn check_app_access(space: &Space, attested_client_id: Option<&str>) -> Result<(), AppError> {
    match &space.app_access {
        AppAccess::Open => Ok(()),
        AppAccess::AllowList { allowed } => {
            let client_id = attested_client_id
                .ok_or_else(|| AppError::Auth("space requires client attestation".into()))?;
            if allowed.iter().any(|id| id == client_id) {
                Ok(())
            } else {
                Err(AppError::Forbidden(
                    "this app is not authorized to access this space".into(),
                ))
            }
        }
    }
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
        .bind(&space.authority_did)
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
    use crate::spaces::types::{AppAccess, MintPolicy, Space, SpaceConfig};

    fn test_space(app_access: AppAccess) -> Space {
        Space {
            id: "test-space".into(),
            did: "did:plc:owner".into(),
            authority_did: "did:plc:owner".into(),
            creator_did: "did:plc:owner".into(),
            type_nsid: "com.example.forum".into(),
            skey: "main".into(),
            display_name: None,
            description: None,
            mint_policy: MintPolicy::MemberList,
            app_access,
            managing_app_did: None,
            config: SpaceConfig::default(),
            revision: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn app_access_open_allows_any() {
        let space = test_space(AppAccess::Open);
        assert!(check_app_access(&space, Some("any-app")).is_ok());
    }

    #[test]
    fn app_access_allowlist_permits_listed() {
        let space = test_space(AppAccess::AllowList {
            allowed: vec!["good-app".into()],
        });
        assert!(check_app_access(&space, Some("good-app")).is_ok());
        assert!(check_app_access(&space, Some("other-app")).is_err());
    }

    #[test]
    fn app_access_allowlist_requires_client_id() {
        let space = test_space(AppAccess::AllowList { allowed: vec![] });
        assert!(check_app_access(&space, None).is_err());
    }

    #[test]
    fn app_access_open_allows_none_client_id() {
        let space = test_space(AppAccess::Open);
        assert!(check_app_access(&space, None).is_ok());
    }

    #[test]
    fn app_access_empty_allowlist_rejects() {
        let space = test_space(AppAccess::AllowList { allowed: vec![] });
        assert!(check_app_access(&space, Some("any-client")).is_err());
    }

    // resolve_did_service_endpoint is async and makes HTTP calls to resolve DID
    // documents, so it cannot be unit-tested without a mock HTTP server.

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

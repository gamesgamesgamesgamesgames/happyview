use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::SigningKey;
use rand::Rng;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::plugin::encryption::{decrypt, encrypt};
use crate::spaces::credential::{
    DEFAULT_CREDENTIAL_TTL_SECS, SpaceCredentialClaims, make_jti, sign_credential,
};
use crate::spaces::types::{AppAccess, Policy, Space};

pub struct IssuedCredential {
    pub token: String,
    pub expires_at: String,
}

/// What the managing-app call needs in order to authenticate itself.
///
/// Bundled rather than threaded as four more parameters: everything here exists
/// only to mint the service-auth token the managing app requires.
#[derive(Clone, Copy)]
pub struct ServiceAuthCtx<'a> {
    pub pool: &'a sqlx::AnyPool,
    pub backend: DatabaseBackend,
    pub encryption_key: &'a [u8; 32],
    pub public_url: &'a str,
}

#[allow(clippy::too_many_arguments)]
pub async fn issue_credential(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    http: &reqwest::Client,
    encryption_key: &[u8; 32],
    public_url: &str,
    space: &Space,
    subject_did: &str,
    client_id: Option<&str>,
    authority_did: &str,
) -> Result<IssuedCredential, AppError> {
    let auth_ctx = ServiceAuthCtx {
        pool,
        backend,
        encryption_key,
        public_url,
    };
    check_app_access(space, client_id)?;
    check_mint_policy(http, auth_ctx, space, subject_did, client_id, authority_did).await?;

    let private_jwk = get_or_create_signing_key(pool, backend, encryption_key, space).await?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let exp = now + DEFAULT_CREDENTIAL_TTL_SECS;

    let claims = SpaceCredentialClaims {
        iss: space.authority_did.clone(),
        sub: format!(
            "at://{}/space/{}/{}",
            space.did, space.type_nsid, space.skey
        ),
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

/// The method a managing-app service-auth token is scoped to.
const CHECK_USER_ACCESS_LXM: &str = "com.atproto.simplespace.checkUserAccess";

/// Build the query parameters for a `checkUserAccess` call.
///
/// A query, not a procedure: the lexicon defines GET with `space`, `user` and a
/// required `access`. Extracted so the wire shape can be asserted without
/// standing up DID resolution.
fn check_user_access_params(
    space_uri: &str,
    user_did: &str,
    client_id: Option<&str>,
    access: AccessKind,
) -> Vec<(&'static str, String)> {
    let mut params = vec![
        ("space", space_uri.to_string()),
        ("user", user_did.to_string()),
        ("access", access.as_str().to_string()),
    ];

    // Omitted for write checks: notifyWrite does not identify the application
    // that originated the write, so there is no client to name.
    if access == AccessKind::Read
        && let Some(cid) = client_id
    {
        params.push(("clientId", cid.to_string()));
    }

    params
}

/// Which permission a policy check is about.
///
/// `read_policy` and `write_policy` are independent and are never substituted
/// for one another: read gates whether a credential is minted, write gates
/// whether the authority tracks a writer and forwards their notifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessKind {
    Read,
    Write,
}

impl AccessKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AccessKind::Read => "read",
            AccessKind::Write => "write",
        }
    }
}

/// Evaluate one policy for one user.
#[allow(clippy::too_many_arguments)]
async fn policy_allows(
    http: &reqwest::Client,
    auth_ctx: ServiceAuthCtx<'_>,
    policy: &Policy,
    space: &Space,
    subject_did: &str,
    client_id: Option<&str>,
    authority_did: &str,
    access: AccessKind,
) -> Result<bool, AppError> {
    match policy {
        Policy::Public => Ok(true),
        Policy::MemberList => {
            // Caller must already be a member; verified upstream by the credential issuance route.
            // We trust that the delegation token proves membership was checked.
            Ok(true)
        }
        Policy::ManagingApp { managing_app } => {
            let space_uri = format!(
                "at://{}/space/{}/{}",
                space.did, space.type_nsid, space.skey
            );
            check_user_access_with_managing_app(
                http,
                auth_ctx,
                managing_app,
                &space_uri,
                subject_did,
                client_id,
                authority_did,
                access,
            )
            .await
        }
    }
}

/// Whether a space credential may be minted for this user, per `read_policy`.
async fn check_mint_policy(
    http: &reqwest::Client,
    auth_ctx: ServiceAuthCtx<'_>,
    space: &Space,
    subject_did: &str,
    client_id: Option<&str>,
    authority_did: &str,
) -> Result<(), AppError> {
    let granted = policy_allows(
        http,
        auth_ctx,
        &space.read_policy,
        space,
        subject_did,
        client_id,
        authority_did,
        AccessKind::Read,
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

#[allow(clippy::too_many_arguments)]
async fn check_user_access_with_managing_app(
    http: &reqwest::Client,
    auth_ctx: ServiceAuthCtx<'_>,
    managing_app: &str,
    space_uri: &str,
    user_did: &str,
    client_id: Option<&str>,
    authority_did: &str,
    access: AccessKind,
) -> Result<bool, AppError> {
    // Parse DID#fragment — the fragment identifies the service endpoint in the DID doc.
    // For outbound callback we derive the endpoint from the DID.
    let (did, fragment) = if let Some(pos) = managing_app.find('#') {
        (&managing_app[..pos], Some(&managing_app[pos + 1..]))
    } else {
        (managing_app, None)
    };

    if let Some(frag) = fragment
        && frag != "atproto_pds"
    {
        return Err(AppError::BadRequest(format!(
            "unsupported service fragment '#{frag}' for managing app"
        )));
    }

    // Resolve the managing app's PDS/service endpoint from its DID document.
    let endpoint = resolve_did_service_endpoint(http, did).await?;

    let url = format!(
        "{}/xrpc/com.atproto.simplespace.checkUserAccess",
        endpoint.trim_end_matches('/')
    );

    let params = check_user_access_params(space_uri, user_did, client_id, access);

    // The lexicon requires service auth from the authority, and the reference
    // managing app verifies it and rejects anything without one. `aud` is the
    // managing app's service identifier and `lxm` pins the token to this single
    // method.
    let token = crate::auth::service_auth::mint_service_auth(
        auth_ctx.pool,
        auth_ctx.backend,
        auth_ctx.encryption_key,
        auth_ctx.public_url,
        managing_app,
        CHECK_USER_ACCESS_LXM,
    )
    .await?;

    // The managing app expects the space authority as `iss`. When the authority
    // is not this instance we cannot sign for it, so fail here rather than send
    // a token the app will reject as the wrong issuer.
    let instance_did = crate::auth::service_auth::instance_did(
        auth_ctx.pool,
        auth_ctx.backend,
        auth_ctx.public_url,
    )
    .await?;
    if instance_did != authority_did {
        return Err(AppError::Internal(format!(
            "cannot authenticate a managing-app check for authority {authority_did}: \
             this instance signs as {instance_did}"
        )));
    }

    let resp = http
        .get(&url)
        .query(&params)
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("checkUserAccess request failed: {e}")))?;

    // A managing app refusing is a denial, not an error. This also covers our
    // own auth being rejected, hence the warn.
    if resp.status() == reqwest::StatusCode::FORBIDDEN
        || resp.status() == reqwest::StatusCode::UNAUTHORIZED
    {
        tracing::warn!(
            status = %resp.status(),
            managing_app,
            "managing app refused the access check; this is a denial, but check service auth"
        );
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
        .get("authorized")
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
    let row: Option<(Vec<u8>,)> = crate::db::query_as(&sql)
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

    crate::db::query(&insert_sql)
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

    let signing_key = SigningKey::from_slice(&rng_bytes[..])
        .map_err(|e| AppError::Internal(format!("failed to generate signing key: {e}")))?;

    let verifying_key = signing_key.verifying_key();
    let public_point = verifying_key.to_sec1_point(false);

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

    crate::db::query(&sql)
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
    use crate::spaces::types::{AppAccess, Policy, Space, SpaceConfig};

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
            read_policy: Policy::MemberList,
            write_policy: Policy::MemberList,
            app_access,
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

#[cfg(test)]
mod managing_app_tests {
    use super::*;

    fn get(params: &[(&str, String)], key: &str) -> Option<String> {
        params
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.clone())
    }

    const SPACE: &str = "at://did:plc:auth/space/com.example.forum/main";

    #[test]
    fn read_checks_carry_space_user_access_and_client() {
        let params =
            check_user_access_params(SPACE, "did:plc:user", Some("https://app"), AccessKind::Read);

        assert_eq!(get(&params, "space").as_deref(), Some(SPACE));
        // The lexicon names this parameter `user`, and the reference managing
        // app reads that key.
        assert_eq!(get(&params, "user").as_deref(), Some("did:plc:user"));
        assert_eq!(get(&params, "access").as_deref(), Some("read"));
        assert_eq!(get(&params, "clientId").as_deref(), Some("https://app"));
        assert!(
            get(&params, "did").is_none(),
            "the lexicon parameter is `user`, not `did`"
        );
    }

    #[test]
    fn access_is_always_present() {
        // Required by the lexicon: bulletin 400s without it, so omitting it
        // makes every managing-app check fail regardless of policy.
        for access in [AccessKind::Read, AccessKind::Write] {
            let params = check_user_access_params(SPACE, "did:plc:user", None, access);
            assert!(get(&params, "access").is_some());
        }
    }

    #[test]
    fn write_checks_omit_the_client_id() {
        let params = check_user_access_params(
            SPACE,
            "did:plc:user",
            Some("https://app"),
            AccessKind::Write,
        );

        assert_eq!(get(&params, "access").as_deref(), Some("write"));
        assert!(get(&params, "clientId").is_none());
    }

    #[test]
    fn access_kind_uses_the_lexicon_known_values() {
        assert_eq!(AccessKind::Read.as_str(), "read");
        assert_eq!(AccessKind::Write.as_str(), "write");
    }
}

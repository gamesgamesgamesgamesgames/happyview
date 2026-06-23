use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum_extra::extract::cookie::{Key, SignedCookieJar};

use crate::AppState;
use crate::auth::COOKIE_NAME;
use crate::error::AppError;

/// Authenticated user identity.
///
/// Tries two auth paths in order:
/// 1. Signed cookie (web UI sessions via OAuth)
/// 2. Bearer token starting with `hv_` (API key — handled downstream by UserAuth)
/// 3. Bearer service auth JWT (AT Protocol inter-service calls)
#[derive(Debug, Clone)]
pub struct Claims {
    did: String,
    /// The API client key (e.g. "hvc_...") if the user authenticated via an API client.
    client_key: Option<String>,
    /// The DPoP key ID identifying the specific device session.
    dpop_key_id: Option<String>,
}

/// Separator used to encode `did` and `client_key` in a single cookie value.
/// Newlines cannot appear in DIDs or client keys, so this is safe.
const COOKIE_SEP: char = '\n';

impl Claims {
    /// The authenticated user's DID.
    pub fn did(&self) -> &str {
        &self.did
    }

    /// The API client key, if the user logged in via an API client.
    pub fn client_key(&self) -> Option<&str> {
        self.client_key.as_deref()
    }

    /// The DPoP key ID, if the user authenticated via a DPoP session.
    pub fn dpop_key_id(&self) -> Option<&str> {
        self.dpop_key_id.as_deref()
    }

    /// Create claims for an internal call (e.g. Lua xrpc lib) with no client key.
    pub fn internal(did: String) -> Self {
        Self {
            did,
            client_key: None,
            dpop_key_id: None,
        }
    }

    /// Test-only constructor.
    #[cfg(test)]
    pub fn new_for_test(did: String) -> Self {
        Self::internal(did)
    }

    #[cfg(test)]
    pub fn with_client_key(did: String, client_key: String) -> Self {
        Self {
            did,
            client_key: Some(client_key),
            dpop_key_id: None,
        }
    }
}

impl FromRequestParts<AppState> for Claims {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // Path 1: Cookie auth (web UI)
        let jar: SignedCookieJar<Key> = SignedCookieJar::from_request_parts(parts, state)
            .await
            .map_err(|_| AppError::Auth("failed to read cookies".into()))?;

        if let Some(cookie) = jar.get(COOKIE_NAME) {
            let value = cookie.value().to_string();
            let (did, client_key) = if let Some((d, k)) = value.split_once(COOKIE_SEP) {
                (d.to_string(), Some(k.to_string()))
            } else {
                (value, None)
            };
            return Ok(Claims {
                did,
                client_key,
                dpop_key_id: None,
            });
        }

        // Path 2: Authorization header
        let header = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                AppError::Auth("missing Authorization header or session cookie".into())
            })?;

        if let Some(token) = header.strip_prefix("Bearer ") {
            // API key tokens start with hv_ — let them through with a placeholder DID.
            // The admin middleware (UserAuth) will resolve the actual DID from the API key.
            if token.starts_with("hv_") {
                // API key auth is handled by UserAuth extractor which looks up the key.
                // We need to extract the DID from the api_keys table.
                let did = resolve_api_key_did(state, token).await?;
                return Ok(Claims {
                    did,
                    client_key: None,
                    dpop_key_id: None,
                });
            }

            // Otherwise, try service auth JWT
            let service_auth = super::service_auth::ServiceAuth::from_bearer(token, state).await?;
            return Ok(Claims {
                did: service_auth.did,
                client_key: None,
                dpop_key_id: None,
            });
        }

        if let Some(token) = header.strip_prefix("DPoP ") {
            return resolve_dpop_claims(state, parts, token).await;
        }

        Err(AppError::Auth("invalid Authorization scheme".into()))
    }
}

/// Look up the DID associated with an API key.
async fn resolve_api_key_did(state: &AppState, token: &str) -> Result<String, AppError> {
    use crate::db::adapt_sql;
    use sha2::{Digest, Sha256};

    let hash = hex::encode(Sha256::digest(token.as_bytes()));
    let sql = adapt_sql(
        "SELECT u.did FROM api_keys k JOIN users u ON k.user_id = u.id WHERE k.key_hash = ? AND k.revoked_at IS NULL",
        state.db_backend,
    );
    let row: Option<(String,)> = sqlx::query_as(&sql)
        .bind(&hash)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("API key lookup failed: {e}")))?;

    row.map(|(did,)| did)
        .ok_or_else(|| AppError::Auth("invalid API key".into()))
}

/// Resolve claims from a DPoP-authenticated request.
///
/// Expects:
/// - `Authorization: DPoP <access_token>`
/// - `DPoP: <proof_jwt>` header
/// - `X-Client-Key: <client_key>` header
pub async fn resolve_dpop_claims(
    state: &AppState,
    parts: &Parts,
    access_token: &str,
) -> Result<Claims, AppError> {
    let client_key = parts
        .headers
        .get("x-client-key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::Auth("DPoP auth requires X-Client-Key header".into()))?;

    let dpop_proof = parts
        .headers
        .get("dpop")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::Auth("DPoP auth requires DPoP header".into()))?;

    let encryption_key = state
        .config
        .token_encryption_key
        .as_ref()
        .ok_or_else(|| AppError::Internal("TOKEN_ENCRYPTION_KEY not configured".into()))?;

    // Resolve the API client
    let client =
        crate::oauth::client_auth::resolve_client_by_key(&state.db, state.db_backend, client_key)
            .await?;

    // Extract JWK thumbprint from the DPoP proof and resolve the key ID
    let thumbprint = crate::oauth::dpop_proof::extract_proof_thumbprint(dpop_proof)?;
    let dpop_key_id = crate::oauth::keys::get_dpop_key_id_by_thumbprint(
        &state.db,
        state.db_backend,
        &client.id,
        &thumbprint,
    )
    .await?;

    // Look up the session by key ID (stable across token rotations)
    let session = crate::oauth::sessions::get_dpop_session_by_key_id(
        &state.db,
        state.db_backend,
        encryption_key,
        &client.id,
        &dpop_key_id,
    )
    .await?;

    // Build the request URL for htu validation
    let scheme = if state.config.public_url.starts_with("https") {
        "https"
    } else {
        "http"
    };
    let host = parts
        .headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    let request_url = format!("{}://{}{}", scheme, host, parts.uri.path());
    let method = parts.method.as_str();

    // Validate the DPoP proof
    crate::oauth::dpop_proof::validate_dpop_proof(
        dpop_proof,
        method,
        &request_url,
        access_token,
        &thumbprint,
    )?;

    Ok(Claims {
        did: session.user_did,
        client_key: Some(client_key.to_string()),
        dpop_key_id: Some(dpop_key_id),
    })
}

/// XRPC-specific claims extractor.
///
/// Accepts DPoP auth (`Authorization: DPoP <token>`), Bearer space credential
/// JWTs (`Authorization: Bearer <space_credential>`), or Bearer service auth
/// JWTs (`Authorization: Bearer <service_jwt>`). Cookie auth and Bearer API keys
/// are rejected on XRPC routes.
#[derive(Debug, Clone)]
pub struct XrpcClaims {
    pub identity: Option<Claims>,
    pub space_credential: Option<String>,
    pub service_auth: Option<ServiceAuthClaims>,
}

#[derive(Debug, Clone)]
pub struct ServiceAuthClaims {
    pub did: String,
    pub aud_fragment: String,
}

impl FromRequestParts<AppState> for XrpcClaims {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok());

        match header {
            Some(h) if h.starts_with("DPoP ") => {
                let token = &h[5..];
                let claims = resolve_dpop_claims(state, parts, token).await?;
                Ok(XrpcClaims {
                    identity: Some(claims),
                    space_credential: None,
                    service_auth: None,
                })
            }
            Some(h) if h.starts_with("Bearer ") => {
                let token = &h[7..];
                let path = parts.uri.path();
                let is_space_route = path.contains("/dev.happyview.space.");

                // Try service auth first
                let host = parts
                    .headers
                    .get(axum::http::header::HOST)
                    .and_then(|v| v.to_str().ok());
                if let Ok(service_claims) = try_parse_service_auth(token, state, host).await {
                    return Ok(XrpcClaims {
                        identity: None,
                        space_credential: None,
                        service_auth: Some(service_claims),
                    });
                }

                // Existing space credential logic
                match crate::spaces::credential::peek_jwt_typ(token) {
                    Some(typ) if typ == "space_credential" && is_space_route => {
                        Ok(XrpcClaims {
                            identity: None,
                            space_credential: Some(token.to_string()),
                            service_auth: None,
                        })
                    }
                    Some(typ) if typ == "space_credential" => Err(AppError::Auth(
                        "space credentials are only accepted on space routes".into(),
                    )),
                    _ => Err(AppError::Auth(
                        "XRPC routes do not accept Bearer auth. Use DPoP auth, a space credential, or omit the Authorization header for anonymous access.".into(),
                    )),
                }
            }
            Some(_) => Err(AppError::Auth("invalid Authorization scheme".into())),
            None => {
                // No auth header — anonymous access (client-key only)
                Ok(XrpcClaims {
                    identity: None,
                    space_credential: None,
                    service_auth: None,
                })
            }
        }
    }
}

async fn try_parse_service_auth(
    token: &str,
    state: &AppState,
    host: Option<&str>,
) -> Result<ServiceAuthClaims, AppError> {
    // 1. Check if service identity is configured and not "not_exposed"
    let identity = crate::service_identity::get_identity(&state.db, state.db_backend).await?;
    let identity =
        identity.ok_or_else(|| AppError::Auth("no service identity configured".into()))?;

    if identity.mode == crate::service_identity::IdentityMode::NotExposed {
        return Err(AppError::Auth("service auth disabled".into()));
    }

    let instance_did = match &identity.mode {
        crate::service_identity::IdentityMode::DidWeb => {
            let h = host.ok_or_else(|| AppError::Auth("missing Host header for did:web".into()))?;
            format!("did:web:{h}")
        }
        _ => identity
            .did
            .clone()
            .ok_or_else(|| AppError::Auth("no DID configured".into()))?,
    };

    // 2. Verify the JWT (this resolves the issuer's DID doc and checks signature)
    let service_auth = crate::auth::service_auth::ServiceAuth::from_bearer(token, state)
        .await
        .map_err(|_| AppError::Auth("invalid service auth token".into()))?;

    // 3. Decode payload to extract aud
    let payload = crate::auth::service_auth::decode_jwt_payload(token)
        .map_err(|_| AppError::Auth("failed to decode JWT".into()))?;

    let aud = payload
        .aud
        .ok_or_else(|| AppError::Auth("JWT missing aud field".into()))?;

    // 4. Verify aud starts with instance DID and extract fragment
    if !aud.starts_with(&*instance_did) {
        return Err(AppError::Auth(format!(
            "JWT aud '{}' does not match instance DID '{}'",
            aud, instance_did
        )));
    }

    let fragment = aud.strip_prefix(&*instance_did).unwrap_or("").to_string();
    if fragment.is_empty() || !fragment.starts_with('#') {
        return Err(AppError::Auth(
            "JWT aud must include a service fragment".into(),
        ));
    }

    Ok(ServiceAuthClaims {
        did: service_auth.did,
        aud_fragment: fragment,
    })
}

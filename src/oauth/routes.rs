use axum::extract::{FromRequest, OriginalUri, Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};

use super::client_auth;
use super::keys;
use super::sessions;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/dpop-keys", post(provision_dpop_key))
        .route("/sessions", post(register_session))
        .route("/sessions/{did}", get(get_session).delete(delete_session))
        .route("/sessions/{did}/devices", get(list_device_sessions))
        .route(
            "/sessions/{did}/devices/{session_id}",
            axum::routing::delete(delete_device_session),
        )
}

/// The request path exactly as the client sent it.
///
/// Two things make this different from `req.uri().path()`. This router is
/// nested under `/oauth`, so the handler's own URI has that prefix stripped;
/// and percent-encoding must survive, because a DPoP `htu` is whatever the
/// client signed, which is whatever it put on the wire. Rebuilding the path
/// from an already-decoded `Path` segment silently rejects any client that
/// encodes the DID — a mismatch the caller has no way to fix from their side.
fn original_request_path(req: &axum::extract::Request) -> String {
    req.extensions()
        .get::<OriginalUri>()
        .map(|uri| uri.path().to_string())
        .unwrap_or_else(|| req.uri().path().to_string())
}

/// Build the `htu` a DPoP proof is validated against.
fn dpop_htu(state: &AppState, host: &str, path: &str) -> String {
    let scheme = if state.config.public_url.starts_with("https") {
        "https"
    } else {
        "http"
    };
    format!("{}://{}{}", scheme, host, path)
}

// --- Request / response types ---

#[derive(Deserialize)]
struct ProvisionKeyBody {
    pkce_challenge: Option<String>,
}

#[derive(Serialize)]
struct ProvisionKeyResponse {
    provision_id: String,
    dpop_key: serde_json::Value,
}

#[derive(Deserialize)]
struct RegisterSessionBody {
    provision_id: String,
    pkce_verifier: Option<String>,
    did: String,
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<String>,
    scopes: String,
    pds_url: Option<String>,
    issuer: Option<String>,
}

#[derive(Serialize)]
struct RegisterSessionResponse {
    session_id: String,
    did: String,
    scopes: Vec<String>,
}

#[derive(Serialize)]
struct GetSessionResponse {
    did: String,
    scopes: Vec<String>,
}

#[derive(Serialize)]
struct DeviceSessionInfo {
    id: String,
    dpop_key_id: String,
    scopes: Vec<String>,
    created_at: String,
    updated_at: String,
}

// --- Handlers ---

/// POST /oauth/dpop-keys — provision a new DPoP keypair.
///
/// Client credentials come from `X-Client-Key` and `X-Client-Secret` headers.
async fn provision_dpop_key(
    State(state): State<AppState>,
    req: axum::extract::Request,
) -> Result<(StatusCode, Json<ProvisionKeyResponse>), AppError> {
    let client_key = req
        .headers()
        .get("x-client-key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::Auth("X-Client-Key header required".into()))?
        .to_string();

    let client_secret = req
        .headers()
        .get("x-client-secret")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let origin = req
        .headers()
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let body: ProvisionKeyBody = Json::<ProvisionKeyBody>::from_request(req, &state)
        .await
        .map_err(|e| AppError::BadRequest(format!("invalid request body: {e}")))?
        .0;

    let encryption_key = state
        .config
        .token_encryption_key
        .as_ref()
        .ok_or_else(|| AppError::Internal("TOKEN_ENCRYPTION_KEY not configured".into()))?;

    // Authenticate the client
    let client = if let Some(ref secret) = client_secret {
        client_auth::authenticate_confidential(&state.db, state.db_backend, &client_key, secret)
            .await?
    } else {
        // Public client — must provide PKCE challenge
        if body.pkce_challenge.is_none() {
            return Err(AppError::BadRequest(
                "public clients must provide pkce_challenge".into(),
            ));
        }
        client_auth::authenticate_public(
            &state.db,
            state.db_backend,
            &client_key,
            origin.as_deref(),
        )
        .await?
    };

    // Generate keypair
    let keypair = keys::generate_dpop_keypair()?;
    let id = Uuid::new_v4().to_string();
    let provision_id = format!("hvp_{}", hex::encode(rand::random::<[u8; 16]>()));

    // Store encrypted key
    keys::store_dpop_key(
        &state.db,
        state.db_backend,
        encryption_key,
        &id,
        &provision_id,
        &client.id,
        &keypair,
        body.pkce_challenge.as_deref(),
    )
    .await?;

    log_event(
        &state.db,
        EventLog {
            event_type: "dpop_key.provisioned".to_string(),
            severity: Severity::Info,
            actor_did: None,
            subject: Some(provision_id.clone()),
            detail: serde_json::json!({
                "client_key": client.client_key,
                "thumbprint": keypair.thumbprint,
            }),
        },
        state.db_backend,
    )
    .await;

    Ok((
        StatusCode::CREATED,
        Json(ProvisionKeyResponse {
            provision_id,
            dpop_key: keypair.private_jwk,
        }),
    ))
}

/// POST /oauth/sessions — register a token set after OAuth callback.
///
/// Client credentials come from `X-Client-Key` and `X-Client-Secret` headers.
async fn register_session(
    State(state): State<AppState>,
    req: axum::extract::Request,
) -> Result<(StatusCode, Json<RegisterSessionResponse>), AppError> {
    let client_key = req
        .headers()
        .get("x-client-key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::Auth("X-Client-Key header required".into()))?
        .to_string();

    let client_secret = req
        .headers()
        .get("x-client-secret")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let body: RegisterSessionBody = Json::<RegisterSessionBody>::from_request(req, &state)
        .await
        .map_err(|e| AppError::BadRequest(format!("invalid request body: {e}")))?
        .0;

    let encryption_key = state
        .config
        .token_encryption_key
        .as_ref()
        .ok_or_else(|| AppError::Internal("TOKEN_ENCRYPTION_KEY not configured".into()))?;

    // Look up the DPoP key by provision_id
    let (dpop_key_id, dpop_client_id, private_jwk, _thumbprint, pkce_challenge) =
        keys::get_dpop_key(
            &state.db,
            state.db_backend,
            encryption_key,
            &body.provision_id,
        )
        .await?;

    // Authenticate the client and verify it matches the key's client
    let client = if let Some(ref secret) = client_secret {
        client_auth::authenticate_confidential(&state.db, state.db_backend, &client_key, secret)
            .await?
    } else {
        // Public client — verify PKCE
        let verifier = body.pkce_verifier.as_deref().ok_or_else(|| {
            AppError::BadRequest("public clients must provide pkce_verifier".into())
        })?;

        let challenge = pkce_challenge.as_deref().ok_or_else(|| {
            AppError::BadRequest("no PKCE challenge found for this provision".into())
        })?;

        if !client_auth::verify_pkce(challenge, verifier) {
            return Err(AppError::Auth("PKCE verification failed".into()));
        }

        client_auth::resolve_client_by_key(&state.db, state.db_backend, &client_key).await?
    };

    // Verify client_key matches the key's owning client
    if client.id != dpop_client_id {
        return Err(AppError::Auth(
            "provision_id does not belong to this client".into(),
        ));
    }

    // Validate scopes
    if let Err(e) =
        client_auth::validate_scopes(&body.scopes, &client.scopes, &state.lexicons).await
    {
        tracing::warn!(
            client_key = %client_key,
            did = %body.did,
            token_scopes = %body.scopes,
            client_scopes = %client.scopes,
            "session registration scope validation failed"
        );
        return Err(e);
    }

    // Verify the access token actually belongs to the claimed DID. The `did` in
    // the request body is client-supplied and untrusted; without this check any
    // holder of a provisioned DPoP key could register a session for an arbitrary
    // victim DID and be authenticated as them on every DPoP-accepting route.
    let verified_did = super::pds_write::verify_access_token_did(
        &state.http,
        &state.config.plc_url,
        &private_jwk,
        &body.did,
        &body.access_token,
    )
    .await?;

    if verified_did != body.did {
        tracing::warn!(
            client_key = %client_key,
            claimed_did = %body.did,
            verified_did = %verified_did,
            "session registration rejected: access token does not belong to claimed DID"
        );
        return Err(AppError::Auth(
            "access token does not belong to the claimed DID".into(),
        ));
    }

    // Store the session
    let session_id = Uuid::new_v4().to_string();
    sessions::store_dpop_session(
        &state.db,
        state.db_backend,
        encryption_key,
        &session_id,
        &client.id,
        &dpop_key_id,
        &body.did,
        &body.access_token,
        body.refresh_token.as_deref(),
        body.expires_at.as_deref(),
        &body.scopes,
        body.pds_url.as_deref(),
        body.issuer.as_deref(),
    )
    .await?;

    // ⚠ A SESSION WITH NO REFRESH TOKEN CANNOT OUTLIVE ITS ACCESS TOKEN, AND IT
    // USED TO REGISTER IN COMPLETE SILENCE. Everything works until the access
    // token expires; from then on every PDS write fails in `pds_write.rs` with
    // "token expired and no refresh_token available" — hours later, surfacing
    // out of Lua, with nothing tying it back to the registration that doomed it.
    // The client cannot see it either: browsers persist only the access token,
    // so `restore()` keeps reporting a healthy signed-in user. Observed live as
    // a player who looked logged in and could not write anything.
    //
    // Still accepted rather than rejected: an access-token-only session is
    // genuinely usable until it expires, and refusing it would turn a
    // degraded login into no login at all for any authorization server that
    // does not issue refresh tokens. It only has to stop being invisible.
    let refreshable = body.refresh_token.is_some();
    if !refreshable {
        tracing::warn!(
            client_key = %client_key,
            did = %body.did,
            "registered a DPoP session with no refresh token; it will stop working when the access token expires"
        );
    }

    log_event(
        &state.db,
        EventLog {
            event_type: "dpop_session.created".to_string(),
            severity: if refreshable {
                Severity::Info
            } else {
                Severity::Warn
            },
            actor_did: Some(body.did.clone()),
            subject: Some(client.client_key.clone()),
            detail: serde_json::json!({
                "scopes": body.scopes,
                "refreshable": refreshable,
            }),
        },
        state.db_backend,
    )
    .await;

    let scopes: Vec<String> = body.scopes.split_whitespace().map(String::from).collect();

    Ok((
        StatusCode::CREATED,
        Json(RegisterSessionResponse {
            session_id,
            did: body.did,
            scopes,
        }),
    ))
}

/// GET /oauth/sessions/:did — retrieve session info (scopes).
///
/// Same auth as DELETE: `X-Client-Key` + `X-Client-Secret` looks the session up
/// by (client, user); otherwise `X-Client-Key` + `Authorization: DPoP <token>` +
/// a `DPoP` proof identifies the specific device session.
async fn get_session(
    State(state): State<AppState>,
    Path(did): Path<String>,
    req: axum::extract::Request,
) -> Result<Json<GetSessionResponse>, AppError> {
    let client_key = req
        .headers()
        .get("x-client-key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::Auth("X-Client-Key header required".into()))?
        .to_string();

    let client_secret = req
        .headers()
        .get("x-client-secret")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let encryption_key = state
        .config
        .token_encryption_key
        .as_ref()
        .ok_or_else(|| AppError::Internal("TOKEN_ENCRYPTION_KEY not configured".into()))?;

    let session = if let Some(ref secret) = client_secret {
        let c = client_auth::authenticate_confidential(
            &state.db,
            state.db_backend,
            &client_key,
            secret,
        )
        .await?;
        // Confidential clients: look up by (client, user) — no DPoP proof needed
        sessions::get_dpop_session_for_user(
            &state.db,
            state.db_backend,
            encryption_key,
            &c.id,
            &did,
        )
        .await?
    } else {
        let resolved =
            client_auth::resolve_client_by_key(&state.db, state.db_backend, &client_key).await?;

        let auth_header = req
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                AppError::Auth("DPoP auth requires Authorization: DPoP <token>".into())
            })?;
        let access_token = auth_header.strip_prefix("DPoP ").ok_or_else(|| {
            AppError::Auth("DPoP auth requires the DPoP authorization scheme".into())
        })?;
        let dpop_proof = req
            .headers()
            .get("dpop")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| AppError::Auth("DPoP auth requires a DPoP proof header".into()))?;

        let thumbprint = crate::oauth::dpop_proof::extract_proof_thumbprint(dpop_proof)?;
        let dpop_key_id = keys::get_dpop_key_id_by_thumbprint(
            &state.db,
            state.db_backend,
            &resolved.id,
            &thumbprint,
        )
        .await?;

        let host = req
            .headers()
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("localhost");
        let request_url = dpop_htu(&state, host, &original_request_path(&req));

        crate::oauth::dpop_proof::validate_dpop_proof(
            dpop_proof,
            "GET",
            &request_url,
            access_token,
            &thumbprint,
        )?;

        sessions::get_dpop_session_by_key_id(
            &state.db,
            state.db_backend,
            encryption_key,
            &resolved.id,
            &dpop_key_id,
        )
        .await?
    };

    let scopes: Vec<String> = session
        .scopes
        .split_whitespace()
        .map(String::from)
        .collect();

    Ok(Json(GetSessionResponse { did, scopes }))
}

/// DELETE /oauth/sessions/:did — logout / revoke a session.
///
/// With `X-Client-Secret`, every session for this user+client is revoked.
/// Otherwise the caller authenticates with `X-Client-Key` + `Authorization:
/// DPoP <token>` + a `DPoP` proof, and only that device's session is revoked.
///
/// DPoP auth is accepted regardless of `client_type`, matching what `/xrpc/*`
/// already accepts. Requiring the client secret here — but not for the calls
/// the same credentials make everywhere else — meant a confidential client
/// using DPoP could never log out: the 401 left the session in local storage,
/// the next restore signed the user back in, and the next logout failed the
/// same way. The DPoP proof is possession of the session key, which is the
/// same thing that authorises using the session; revoking it is strictly less
/// dangerous than continuing to use it.
async fn delete_session(
    State(state): State<AppState>,
    Path(did): Path<String>,
    req: axum::extract::Request,
) -> Result<StatusCode, AppError> {
    let client_key = req
        .headers()
        .get("x-client-key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::Auth("X-Client-Key header required".into()))?
        .to_string();

    let client_secret = req
        .headers()
        .get("x-client-secret")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    if let Some(ref secret) = client_secret {
        let client = client_auth::authenticate_confidential(
            &state.db,
            state.db_backend,
            &client_key,
            secret,
        )
        .await?;
        // Confidential clients: delete all sessions for this user+client
        sessions::delete_all_dpop_sessions(&state.db, state.db_backend, &client.id, &did).await?;

        log_event(
            &state.db,
            EventLog {
                event_type: "dpop_session.deleted".to_string(),
                severity: Severity::Info,
                actor_did: Some(did),
                subject: Some(client.client_key),
                detail: serde_json::json!({}),
            },
            state.db_backend,
        )
        .await;
    } else {
        let resolved =
            client_auth::resolve_client_by_key(&state.db, state.db_backend, &client_key).await?;

        let auth_header = req
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                AppError::Auth("DPoP auth requires Authorization: DPoP <token>".into())
            })?;
        let access_token = auth_header.strip_prefix("DPoP ").ok_or_else(|| {
            AppError::Auth("DPoP auth requires the DPoP authorization scheme".into())
        })?;
        let dpop_proof = req
            .headers()
            .get("dpop")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| AppError::Auth("DPoP auth requires a DPoP proof header".into()))?;

        let thumbprint = crate::oauth::dpop_proof::extract_proof_thumbprint(dpop_proof)?;
        let dpop_key_id = keys::get_dpop_key_id_by_thumbprint(
            &state.db,
            state.db_backend,
            &resolved.id,
            &thumbprint,
        )
        .await?;

        let host = req
            .headers()
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("localhost");
        let request_url = dpop_htu(&state, host, &original_request_path(&req));

        crate::oauth::dpop_proof::validate_dpop_proof(
            dpop_proof,
            "DELETE",
            &request_url,
            access_token,
            &thumbprint,
        )?;

        sessions::delete_dpop_session(
            &state.db,
            state.db_backend,
            &resolved.id,
            &did,
            &dpop_key_id,
        )
        .await?;

        log_event(
            &state.db,
            EventLog {
                event_type: "dpop_session.deleted".to_string(),
                severity: Severity::Info,
                actor_did: Some(did),
                subject: Some(resolved.client_key),
                detail: serde_json::json!({}),
            },
            state.db_backend,
        )
        .await;
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Extracted headers for session endpoint authentication.
struct SessionAuthHeaders {
    client_key: String,
    client_secret: Option<String>,
    auth_header: Option<String>,
    dpop_proof: Option<String>,
    host: String,
}

impl SessionAuthHeaders {
    fn from_request(req: &axum::extract::Request) -> Self {
        Self {
            client_key: req
                .headers()
                .get("x-client-key")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string(),
            client_secret: req
                .headers()
                .get("x-client-secret")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string()),
            auth_header: req
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string()),
            dpop_proof: req
                .headers()
                .get("dpop")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string()),
            host: req
                .headers()
                .get("host")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("localhost")
                .to_string(),
        }
    }
}

/// GET /oauth/sessions/:did/devices — list all device sessions for a user.
async fn list_device_sessions(
    State(state): State<AppState>,
    Path(did): Path<String>,
    req: axum::extract::Request,
) -> Result<Json<Vec<DeviceSessionInfo>>, AppError> {
    let request_path = original_request_path(&req);
    let headers = SessionAuthHeaders::from_request(&req);
    let client = resolve_session_client(&state, &headers, &request_path, "GET").await?;

    let sessions =
        sessions::list_dpop_sessions(&state.db, state.db_backend, &client.id, &did).await?;

    let result: Vec<DeviceSessionInfo> = sessions
        .into_iter()
        .map(|s| DeviceSessionInfo {
            id: s.id,
            dpop_key_id: s.dpop_key_id,
            scopes: s.scopes.split_whitespace().map(String::from).collect(),
            created_at: s.created_at,
            updated_at: s.updated_at,
        })
        .collect();

    Ok(Json(result))
}

/// DELETE /oauth/sessions/:did/devices/:session_id — revoke a specific device session.
async fn delete_device_session(
    State(state): State<AppState>,
    Path((did, session_id)): Path<(String, String)>,
    req: axum::extract::Request,
) -> Result<StatusCode, AppError> {
    let request_path = original_request_path(&req);
    let headers = SessionAuthHeaders::from_request(&req);
    let client = resolve_session_client(&state, &headers, &request_path, "DELETE").await?;

    sessions::delete_dpop_session_by_id(&state.db, state.db_backend, &session_id, &client.id, &did)
        .await?;

    log_event(
        &state.db,
        EventLog {
            event_type: "dpop_session.device_deleted".to_string(),
            severity: Severity::Info,
            actor_did: Some(did),
            subject: Some(client.client_key),
            detail: serde_json::json!({ "session_id": session_id }),
        },
        state.db_backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

/// Shared client authentication for session endpoints.
async fn resolve_session_client(
    state: &AppState,
    headers: &SessionAuthHeaders,
    request_path: &str,
    method: &str,
) -> Result<client_auth::ResolvedClient, AppError> {
    if headers.client_key.is_empty() {
        return Err(AppError::Auth("X-Client-Key header required".into()));
    }

    if let Some(ref secret) = headers.client_secret {
        return client_auth::authenticate_confidential(
            &state.db,
            state.db_backend,
            &headers.client_key,
            secret,
        )
        .await;
    }

    let resolved =
        client_auth::resolve_client_by_key(&state.db, state.db_backend, &headers.client_key)
            .await?;

    let auth_header = headers
        .auth_header
        .as_deref()
        .ok_or_else(|| AppError::Auth("DPoP auth requires Authorization: DPoP <token>".into()))?;
    let access_token = auth_header
        .strip_prefix("DPoP ")
        .ok_or_else(|| AppError::Auth("DPoP auth requires the DPoP authorization scheme".into()))?;
    let dpop_proof = headers
        .dpop_proof
        .as_deref()
        .ok_or_else(|| AppError::Auth("DPoP auth requires a DPoP proof header".into()))?;

    let thumbprint = crate::oauth::dpop_proof::extract_proof_thumbprint(dpop_proof)?;
    let _dpop_key_id =
        keys::get_dpop_key_id_by_thumbprint(&state.db, state.db_backend, &resolved.id, &thumbprint)
            .await?;

    let request_url = dpop_htu(state, &headers.host, request_path);

    crate::oauth::dpop_proof::validate_dpop_proof(
        dpop_proof,
        method,
        &request_url,
        access_token,
        &thumbprint,
    )?;

    Ok(resolved)
}

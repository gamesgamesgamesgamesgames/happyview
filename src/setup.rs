use atrium_api::agent::Agent;
use atrium_api::types::Unknown;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use axum_extra::extract::cookie::{Cookie, Key, SignedCookieJar};
use rand::RngCore;
use serde::Deserialize;

use crate::admin::auth::UserAuth;
use crate::auth::COOKIE_NAME;
use crate::auth::middleware::Claims;
use crate::event_log::{EventLog, Severity, log_event};
use crate::service_identity::{self, IdentityMode};
use crate::{AppState, error::AppError};

fn is_pds_session_expired(err: &impl std::fmt::Display) -> bool {
    let msg = err.to_string();
    msg.contains("invalid_token") || msg.contains("expired") || msg.contains("revoked")
}

fn pds_reauth_error() -> AppError {
    AppError::Auth(
        "Your PDS session has expired or been revoked. \
         Use the Re-authenticate button on the Service Identity page to sign in again."
            .into(),
    )
}

async fn require_setup_incomplete(state: &AppState) -> Result<(), AppError> {
    let status = service_identity::get_setup_status(&state.db, state.db_backend).await?;
    if status.setup_complete {
        return Err(AppError::Forbidden("setup is already complete".into()));
    }
    Ok(())
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/status", get(status))
        .route("/identity", post(set_identity))
        .route("/plc/register", post(plc_register))
        .route("/plc/request", post(plc_request))
        .route("/plc/submit", post(plc_submit))
        .route("/complete", post(complete))
        .route("/rotation-key", get(export_rotation_key))
        .route("/resolve", get(resolve_identity))
        .route("/attach-auth/confirm", post(attach_auth_confirm))
}

async fn status(
    _auth: Claims,
    State(state): State<AppState>,
) -> Result<Json<service_identity::SetupStatus>, AppError> {
    let status = service_identity::get_setup_status(&state.db, state.db_backend).await?;
    Ok(Json(status))
}

#[derive(Debug, Deserialize)]
struct SetIdentityRequest {
    mode: String,
    attached_account_did: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PlcSubmitBody {
    token: String,
}

async fn set_identity(
    _auth: UserAuth,
    State(state): State<AppState>,
    Json(body): Json<SetIdentityRequest>,
) -> Result<StatusCode, AppError> {
    require_setup_incomplete(&state).await?;
    let mode = IdentityMode::parse(&body.mode)
        .ok_or_else(|| AppError::BadRequest(format!("invalid identity mode: {}", body.mode)))?;

    let (did, signing_key_enc, rotation_key_enc, attached_account_did) = match &mode {
        IdentityMode::DidWeb => {
            let signing_key_enc = generate_encrypted_signing_key(&state)?;

            (None::<String>, Some(signing_key_enc), None, None)
        }

        IdentityMode::DidPlc => {
            let signing_key_enc = generate_encrypted_signing_key(&state)?;
            let rotation_key_enc = generate_encrypted_signing_key(&state)?;

            (None, Some(signing_key_enc), Some(rotation_key_enc), None)
        }

        IdentityMode::AttachAccount => {
            let attached = body.attached_account_did.clone();
            (None, None, None, attached)
        }

        IdentityMode::NotExposed => (None, None, None, None),
    };

    service_identity::upsert_identity(
        &state.db,
        state.db_backend,
        &mode,
        did.as_deref(),
        signing_key_enc.as_deref(),
        rotation_key_enc.as_deref(),
        attached_account_did.as_deref(),
    )
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

fn generate_encrypted_signing_key(state: &AppState) -> Result<String, AppError> {
    use base64::Engine;
    use p256::ecdsa::SigningKey;

    let mut rng_bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut rng_bytes);

    // Validate the key bytes produce a valid signing key
    SigningKey::from_bytes((&rng_bytes[..]).into())
        .map_err(|e| AppError::Internal(format!("failed to generate signing key: {e}")))?;

    let encryption_key = state
        .config
        .token_encryption_key
        .as_ref()
        .ok_or_else(|| AppError::Internal("TOKEN_ENCRYPTION_KEY not configured".into()))?;

    let encrypted = crate::plugin::encryption::encrypt(encryption_key, &rng_bytes)
        .map_err(|e| AppError::Internal(format!("failed to encrypt signing key: {e}")))?;

    Ok(base64::engine::general_purpose::STANDARD.encode(&encrypted))
}

#[derive(Debug, serde::Serialize)]
struct PlcRegisterResponse {
    did: String,
}

/// Register a new did:plc identity by creating and submitting a genesis operation
/// to the PLC directory.
///
/// This endpoint:
/// 1. Validates the identity mode is did_plc
/// 2. Decrypts the signing and rotation keys from the database
/// 3. Builds a PLC genesis operation with service entries
/// 4. Signs the operation with the rotation key
/// 5. Derives the DID from the signed operation
/// 6. Submits the signed operation to the PLC directory
/// 7. Updates the service_identity row with the new DID
async fn plc_register(
    _auth: UserAuth,
    State(state): State<AppState>,
) -> Result<Json<PlcRegisterResponse>, AppError> {
    require_setup_incomplete(&state).await?;
    let identity = service_identity::get_identity(&state.db, state.db_backend).await?;
    let identity = identity.ok_or_else(|| AppError::BadRequest("no identity configured".into()))?;

    if identity.mode != IdentityMode::DidPlc {
        return Err(AppError::BadRequest(
            "PLC registration only supported for did_plc mode".into(),
        ));
    }

    if identity.did.is_some() {
        return Err(AppError::Conflict(
            "DID already registered for this identity".into(),
        ));
    }

    let encryption_key = state
        .config
        .token_encryption_key
        .as_ref()
        .ok_or_else(|| AppError::Internal("TOKEN_ENCRYPTION_KEY not configured".into()))?;

    // Decrypt signing key
    let signing_key_enc = identity
        .signing_key_enc
        .as_ref()
        .ok_or_else(|| AppError::Internal("no signing key stored".into()))?;
    let signing_key_bytes = crate::plc::decrypt_key(signing_key_enc, encryption_key)?;
    let signing_key_did = crate::plc::private_key_to_did_key(&signing_key_bytes)?;

    // Decrypt rotation key
    let sql = crate::db::adapt_sql(
        "SELECT rotation_key_enc FROM happyview_service_identity WHERE id = 1",
        state.db_backend,
    );
    let row: Option<(Option<String>,)> = sqlx::query_as(&sql)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to fetch rotation key: {e}")))?;
    let rotation_key_enc = row
        .and_then(|(k,)| k)
        .ok_or_else(|| AppError::Internal("no rotation key stored".into()))?;
    let rotation_key_bytes = crate::plc::decrypt_key(&rotation_key_enc, encryption_key)?;
    let rotation_key_did = crate::plc::private_key_to_did_key(&rotation_key_bytes)?;

    let rotation_signing_key =
        p256::ecdsa::SigningKey::from_bytes(rotation_key_bytes.as_slice().into())
            .map_err(|e| AppError::Internal(format!("invalid rotation key: {e}")))?;

    // Build service entries from the database
    let entries = crate::service_entries::list_entries(&state.db, state.db_backend).await?;
    let public_url = &state.config.public_url;
    let service_entries: Vec<(String, String, String)> = entries
        .iter()
        .map(|e| {
            let key = e.fragment_id.trim_start_matches('#').to_string();
            (key, e.service_type.clone(), public_url.clone())
        })
        .collect();

    let params = crate::plc::PlcGenesisParams {
        rotation_key_did_key: rotation_key_did,
        signing_key_did_key: signing_key_did,
        service_entries,
    };

    // Build, sign, derive DID, and submit
    let unsigned = crate::plc::build_unsigned_genesis(&params);
    let signed = crate::plc::sign_operation(&unsigned, &rotation_signing_key)?;
    let did = crate::plc::derive_did(&signed)?;

    crate::plc::submit_genesis(&state.http, &state.config.plc_url, &did, &signed).await?;

    // Update service_identity with the newly registered DID
    service_identity::upsert_identity(
        &state.db,
        state.db_backend,
        &IdentityMode::DidPlc,
        Some(&did),
        Some(signing_key_enc),
        Some(&rotation_key_enc),
        None,
    )
    .await?;

    tracing::info!(did = %did, "PLC identity registered");

    Ok(Json(PlcRegisterResponse { did }))
}

async fn plc_request(
    _auth: UserAuth,
    State(state): State<AppState>,
) -> Result<StatusCode, AppError> {
    require_setup_incomplete(&state).await?;
    let identity = service_identity::get_identity(&state.db, state.db_backend).await?;
    let identity = identity.ok_or_else(|| AppError::BadRequest("no identity configured".into()))?;

    let account_did = match identity.mode {
        IdentityMode::AttachAccount => {
            let sql = crate::db::adapt_sql(
                "SELECT attached_account_did FROM happyview_service_identity WHERE id = 1",
                state.db_backend,
            );
            let row: Option<(Option<String>,)> = sqlx::query_as(&sql)
                .fetch_optional(&state.db)
                .await
                .map_err(|e| AppError::Internal(format!("failed to fetch identity: {e}")))?;
            row.and_then(|(did,)| did)
                .ok_or_else(|| AppError::BadRequest("no attached account DID configured".into()))?
        }
        _ => {
            return Err(AppError::BadRequest(
                "PLC flow only supported for attach_account mode".into(),
            ));
        }
    };

    // Restore OAuth session for the attached account
    let session = crate::repo::session::get_oauth_session(&state, &account_did)
        .await
        .map_err(|e| {
            if is_pds_session_expired(&e) {
                return pds_reauth_error();
            }
            e
        })?;
    let agent = Agent::new(session);

    // Request PLC operation signature — sends confirmation code to account's email
    agent
        .api
        .com
        .atproto
        .identity
        .request_plc_operation_signature()
        .await
        .map_err(|e| {
            if is_pds_session_expired(&e) {
                return pds_reauth_error();
            }
            AppError::Internal(format!("requestPlcOperationSignature failed: {e}"))
        })?;

    Ok(StatusCode::NO_CONTENT)
}

async fn plc_submit(
    _auth: UserAuth,
    State(state): State<AppState>,
    Json(body): Json<PlcSubmitBody>,
) -> Result<StatusCode, AppError> {
    require_setup_incomplete(&state).await?;
    let identity = service_identity::get_identity(&state.db, state.db_backend).await?;
    let identity = identity.ok_or_else(|| AppError::BadRequest("no identity configured".into()))?;

    let account_did = match identity.mode {
        IdentityMode::AttachAccount => {
            let sql = crate::db::adapt_sql(
                "SELECT attached_account_did FROM happyview_service_identity WHERE id = 1",
                state.db_backend,
            );
            let row: Option<(Option<String>,)> = sqlx::query_as(&sql)
                .fetch_optional(&state.db)
                .await
                .map_err(|e| AppError::Internal(format!("failed to fetch identity: {e}")))?;
            row.and_then(|(did,)| did)
                .ok_or_else(|| AppError::BadRequest("no attached account DID configured".into()))?
        }
        _ => {
            return Err(AppError::BadRequest(
                "PLC flow only supported for attach_account mode".into(),
            ));
        }
    };

    let session = crate::repo::session::get_oauth_session(&state, &account_did)
        .await
        .map_err(|e| {
            if is_pds_session_expired(&e) {
                return pds_reauth_error();
            }
            e
        })?;
    let agent = Agent::new(session);

    // Fetch current PLC operation state
    let plc_url = state.config.plc_url.trim_end_matches('/');
    let last_op = state
        .http
        .get(format!("{}/{}/log/last", plc_url, account_did))
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("failed to fetch PLC log: {e}")))?
        .json::<serde_json::Value>()
        .await
        .map_err(|e| AppError::Internal(format!("failed to parse PLC log: {e}")))?;

    // Preserve existing fields
    let rotation_keys: Vec<String> = last_op["rotationKeys"]
        .as_array()
        .ok_or_else(|| AppError::Internal("no rotationKeys in PLC operation".into()))?
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();

    let also_known_as: Vec<String> = last_op["alsoKnownAs"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();

    // Build services: merge existing + add our service entries
    let mut services_map = last_op["services"].as_object().cloned().unwrap_or_default();

    let entries = crate::service_entries::list_entries(&state.db, state.db_backend).await?;
    let public_url = &state.config.public_url;
    for entry in &entries {
        let key = entry.fragment_id.trim_start_matches('#').to_string();
        services_map.insert(
            key,
            serde_json::json!({
                "type": entry.service_type,
                "endpoint": public_url
            }),
        );
    }

    let services: Unknown = serde_json::from_value(serde_json::Value::Object(services_map))
        .map_err(|e| AppError::Internal(format!("failed to build services Unknown: {e}")))?;

    // Preserve existing verification methods
    let vm_map = last_op["verificationMethods"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    let verification_methods: Unknown = serde_json::from_value(serde_json::Value::Object(vm_map))
        .map_err(|e| {
        AppError::Internal(format!("failed to build verification methods Unknown: {e}"))
    })?;

    // Sign the PLC operation via the user's PDS
    use atrium_api::com::atproto::identity::sign_plc_operation;
    let sign_result = agent
        .api
        .com
        .atproto
        .identity
        .sign_plc_operation(
            sign_plc_operation::InputData {
                token: Some(body.token),
                services: Some(services),
                verification_methods: Some(verification_methods),
                also_known_as: Some(also_known_as),
                rotation_keys: Some(rotation_keys),
            }
            .into(),
        )
        .await
        .map_err(|e| {
            if is_pds_session_expired(&e) {
                return pds_reauth_error();
            }
            AppError::Internal(format!("signPlcOperation failed: {e}"))
        })?;

    // Submit the signed operation
    use atrium_api::com::atproto::identity::submit_plc_operation;
    agent
        .api
        .com
        .atproto
        .identity
        .submit_plc_operation(
            submit_plc_operation::InputData {
                operation: sign_result.operation.clone(),
            }
            .into(),
        )
        .await
        .map_err(|e| {
            if is_pds_session_expired(&e) {
                return pds_reauth_error();
            }
            AppError::Internal(format!("submitPlcOperation failed: {e}"))
        })?;

    // Update service_identity with the account's DID
    service_identity::upsert_identity(
        &state.db,
        state.db_backend,
        &IdentityMode::AttachAccount,
        Some(&account_did),
        None,
        None,
        Some(&account_did),
    )
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct AttachAuthConfirmBody {
    original_did: String,
}

/// Restore the admin's session cookie after the attach-account OAuth flow.
///
/// After the admin authenticates as the attached account (via the regular
/// `/auth/login` flow), the session cookie holds the attached account's DID.
/// This endpoint:
/// 1. Reads the current session (attached account DID) and verifies it matches
///    the `attached_account_did` stored in service_identity.
/// 2. Restores the admin's cookie to `original_did`.
///
/// The attached account's OAuth session remains stored in the database for
/// use by the subsequent PLC request/submit flow.
async fn attach_auth_confirm(
    State(state): State<AppState>,
    jar: SignedCookieJar<Key>,
    Json(body): Json<AttachAuthConfirmBody>,
) -> Result<(SignedCookieJar<Key>, StatusCode), AppError> {
    // Verify the current session is for the attached account
    let current_cookie = jar
        .get(COOKIE_NAME)
        .ok_or_else(|| AppError::Auth("no session cookie present".into()))?;
    let raw = current_cookie.value().to_string();
    let current_did = raw.split('\n').next().unwrap_or(&raw).to_string();

    // Verify the current DID matches the configured attached_account_did
    let sql = crate::db::adapt_sql(
        "SELECT attached_account_did FROM happyview_service_identity WHERE id = 1",
        state.db_backend,
    );
    let row: Option<(Option<String>,)> = sqlx::query_as(&sql)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to fetch identity: {e}")))?;
    let attached_did = row
        .and_then(|(did,)| did)
        .ok_or_else(|| AppError::BadRequest("no attached account configured".into()))?;

    if current_did != attached_did {
        return Err(AppError::Auth(format!(
            "current session DID '{}' does not match attached account DID '{}'",
            current_did, attached_did
        )));
    }

    // Restore the admin's cookie
    let original_did = body.original_did.trim().to_string();
    if original_did.is_empty() || !original_did.starts_with("did:") {
        return Err(AppError::BadRequest("invalid original_did".into()));
    }

    // Verify the original DID is a known admin user
    let user_exists: Option<(i32,)> = sqlx::query_as(&crate::db::adapt_sql(
        "SELECT 1 FROM happyview_users WHERE did = ?",
        state.db_backend,
    ))
    .bind(&original_did)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("user lookup failed: {e}")))?;

    if user_exists.is_none() {
        return Err(AppError::Auth("original_did is not a known user".into()));
    }

    let secure = state.config.public_url.starts_with("https://");
    let same_site = if secure {
        axum_extra::extract::cookie::SameSite::None
    } else {
        axum_extra::extract::cookie::SameSite::Lax
    };
    let mut session_cookie = Cookie::new(COOKIE_NAME, original_did);
    session_cookie.set_path("/");
    session_cookie.set_http_only(true);
    session_cookie.set_same_site(same_site);
    session_cookie.set_secure(secure);

    let jar = jar.add(session_cookie);

    Ok((jar, StatusCode::NO_CONTENT))
}

async fn export_rotation_key(
    _auth: UserAuth,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    require_setup_incomplete(&state).await?;
    use base64::Engine;

    let identity = service_identity::get_identity(&state.db, state.db_backend).await?;
    let identity = identity.ok_or_else(|| AppError::BadRequest("no identity configured".into()))?;

    if identity.mode != IdentityMode::DidPlc {
        return Err(AppError::BadRequest(
            "rotation key export only supported for did_plc mode".into(),
        ));
    }

    let sql = crate::db::adapt_sql(
        "SELECT rotation_key_enc FROM happyview_service_identity WHERE id = 1",
        state.db_backend,
    );
    let row: Option<(Option<String>,)> = sqlx::query_as(&sql)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to fetch rotation key: {e}")))?;

    let enc_b64 = row
        .and_then(|(k,)| k)
        .ok_or_else(|| AppError::Internal("no rotation key stored".into()))?;

    let encrypted = base64::engine::general_purpose::STANDARD
        .decode(&enc_b64)
        .map_err(|e| AppError::Internal(format!("failed to decode rotation key: {e}")))?;

    let encryption_key = state
        .config
        .token_encryption_key
        .as_ref()
        .ok_or_else(|| AppError::Internal("TOKEN_ENCRYPTION_KEY not configured".into()))?;

    let key_bytes = crate::plugin::encryption::decrypt(encryption_key, &encrypted)
        .map_err(|e| AppError::Internal(format!("failed to decrypt rotation key: {e}")))?;

    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"rotation-key.bin\"",
            ),
        ],
        key_bytes,
    ))
}

async fn complete(_auth: UserAuth, State(state): State<AppState>) -> Result<StatusCode, AppError> {
    require_setup_incomplete(&state).await?;
    service_identity::mark_setup_complete(&state.db, state.db_backend).await?;

    log_event(
        &state.db,
        EventLog {
            event_type: "setup.completed".to_string(),
            severity: Severity::Info,
            actor_did: None,
            subject: None,
            detail: serde_json::json!({}),
        },
        state.db_backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct ResolveQuery {
    q: String,
}

#[derive(Debug, serde::Serialize)]
struct ResolveResult {
    did: String,
    handle: Option<String>,
    display_name: Option<String>,
    avatar: Option<String>,
}

async fn resolve_identity(
    _auth: UserAuth,
    State(state): State<AppState>,
    Query(query): Query<ResolveQuery>,
) -> Result<Json<Vec<ResolveResult>>, AppError> {
    let q = query.q.trim().to_string();
    if q.is_empty() {
        return Ok(Json(vec![]));
    }

    // If it's already a DID, resolve the profile directly
    if q.starts_with("did:") {
        match crate::profile::resolve_profile(&state.http, &state.config.plc_url, &q).await {
            Ok(profile) => {
                return Ok(Json(vec![ResolveResult {
                    did: profile.did,
                    handle: Some(profile.handle),
                    display_name: profile.display_name,
                    avatar: profile.avatar_url,
                }]));
            }
            Err(_) => {
                // Return the DID as-is if profile resolution fails
                return Ok(Json(vec![ResolveResult {
                    did: q.to_string(),
                    handle: None,
                    display_name: None,
                    avatar: None,
                }]));
            }
        }
    }

    // Try to resolve the handle to a DID, then fetch the profile.
    // AT Protocol handle resolution: check DNS TXT `_atproto.<handle>` for `did=<DID>`,
    // or fall back to `https://<handle>/.well-known/atproto-did`.
    let handle = q.trim_start_matches('@').to_string();
    let did = resolve_handle_to_did(&state.http, &handle).await;

    match did {
        Some(did) => {
            match crate::profile::resolve_profile(&state.http, &state.config.plc_url, &did).await {
                Ok(profile) => Ok(Json(vec![ResolveResult {
                    did: profile.did,
                    handle: Some(profile.handle),
                    display_name: profile.display_name,
                    avatar: profile.avatar_url,
                }])),
                Err(_) => Ok(Json(vec![ResolveResult {
                    did,
                    handle: Some(handle),
                    display_name: None,
                    avatar: None,
                }])),
            }
        }
        None => Ok(Json(vec![])),
    }
}

/// Resolve an AT Protocol handle to a DID.
/// Tries HTTPS well-known first, then DNS TXT `_atproto.<handle>` fallback.
async fn resolve_handle_to_did(http: &reqwest::Client, handle: &str) -> Option<String> {
    // Try HTTPS well-known first (simpler, no DNS library needed here)
    let url = format!("https://{}/.well-known/atproto-did", handle);
    if let Ok(resp) = http.get(&url).send().await
        && resp.status().is_success()
        && let Ok(text) = resp.text().await
    {
        let did = text.trim().to_string();
        if did.starts_with("did:") {
            return Some(did);
        }
    }

    // Try DNS TXT record `_atproto.<handle>`
    use hickory_resolver::Resolver;
    let lookup_name = format!("_atproto.{}.", handle);
    if let Ok(resolver) = Resolver::builder_tokio().map(|b| b.build())
        && let Ok(txt_lookup) = resolver.txt_lookup(&lookup_name).await
    {
        let did = txt_lookup
            .iter()
            .flat_map(|txt| txt.txt_data().iter())
            .filter_map(|data| {
                let s = std::str::from_utf8(data).ok()?;
                s.strip_prefix("did=")
            })
            .next()
            .map(|s| s.to_string());
        if did.is_some() {
            return did;
        }
    }

    None
}

use atrium_api::agent::Agent;
use atrium_api::types::Unknown;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;

use crate::AppState;
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};
use crate::service_entries::{
    CreateServiceEntry, ServiceEntry, UpdateServiceEntry, add_entry_xrpcs, create_entry,
    delete_entry, list_entries, list_entry_xrpcs, remove_entry_xrpcs, services_for_lexicon,
    update_entry,
};
use crate::service_identity::IdentityMode;

use super::auth::UserAuth;
use super::permissions::Permission;

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

/// GET /admin/service-entries — list all service entries.
pub(super) async fn list(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<Vec<ServiceEntry>>, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let entries = list_entries(&state.db, state.db_backend).await?;
    Ok(Json(entries))
}

/// POST /admin/service-entries — create a new service entry.
pub(super) async fn create(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<CreateServiceEntry>,
) -> Result<(StatusCode, Json<ServiceEntry>), AppError> {
    auth.require(Permission::SettingsManage).await?;

    let entry = create_entry(&state.db, state.db_backend, &body).await?;
    Ok((StatusCode::CREATED, Json(entry)))
}

/// PUT /admin/service-entries/{id} — update a service entry.
pub(super) async fn update(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<i64>,
    Json(body): Json<UpdateServiceEntry>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::SettingsManage).await?;

    update_entry(&state.db, state.db_backend, id, &body).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// DELETE /admin/service-entries/{id} — delete a service entry.
pub(super) async fn delete(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let deleted = delete_entry(&state.db, state.db_backend, id).await?;
    if !deleted {
        return Err(AppError::NotFound(format!("service entry {id} not found")));
    }

    log_event(
        &state.db,
        EventLog {
            event_type: "service_entry.deleted".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(id.to_string()),
            detail: serde_json::json!({}),
        },
        state.db_backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

/// GET /admin/service-entries/{id}/xrpcs — list lexicon IDs for a service entry.
pub(super) async fn list_xrpcs(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<i64>,
) -> Result<Json<Vec<String>>, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let xrpcs = list_entry_xrpcs(&state.db, state.db_backend, id).await?;
    Ok(Json(xrpcs))
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct XrpcListBody {
    pub lexicon_ids: Vec<String>,
}

/// POST /admin/service-entries/{id}/xrpcs — add lexicon IDs to a service entry.
pub(super) async fn add_xrpcs(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<i64>,
    Json(body): Json<XrpcListBody>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::SettingsManage).await?;

    add_entry_xrpcs(&state.db, state.db_backend, id, &body.lexicon_ids).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// DELETE /admin/service-entries/{id}/xrpcs — remove lexicon IDs from a service entry.
pub(super) async fn remove_xrpcs(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<i64>,
    Json(body): Json<XrpcListBody>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::SettingsManage).await?;

    remove_entry_xrpcs(&state.db, state.db_backend, id, &body.lexicon_ids).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// GET /admin/lexicons/{id}/services — list service entries that grant access to a lexicon.
pub(super) async fn lexicon_services(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(lexicon_id): Path<String>,
) -> Result<Json<Vec<ServiceEntry>>, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let entries = services_for_lexicon(&state.db, state.db_backend, &lexicon_id).await?;
    Ok(Json(entries))
}

// ---------------------------------------------------------------------------
// PLC sync endpoints
// ---------------------------------------------------------------------------

/// POST /admin/service-entries/sync-plc — one-click PLC sync for did_plc mode.
///
/// Signs and submits a PLC update operation directly using the rotation key.
pub(super) async fn sync_plc(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let identity = crate::service_identity::get_identity(&state.db, state.db_backend).await?;
    let identity = identity.ok_or_else(|| AppError::BadRequest("no identity configured".into()))?;

    if identity.mode != IdentityMode::DidPlc {
        return Err(AppError::BadRequest(
            "PLC sync only supported for did_plc mode".into(),
        ));
    }

    let did = identity
        .did
        .as_ref()
        .ok_or_else(|| AppError::BadRequest("no DID registered yet".into()))?;

    let encryption_key = state
        .config
        .token_encryption_key
        .as_ref()
        .ok_or_else(|| AppError::Internal("TOKEN_ENCRYPTION_KEY not configured".into()))?;

    // Fetch last PLC operation to get prev CID and preserve existing fields
    let plc_url = &state.config.plc_url;
    let last_op = crate::plc::fetch_last_operation(&state.http, plc_url, did).await?;
    let prev_cid = crate::plc::extract_prev_cid(&last_op)?;

    // Preserve existing fields from the current DID document
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

    let mut verification_methods = last_op["verificationMethods"]
        .as_object()
        .cloned()
        .unwrap_or_default();

    // Merge verification methods from the table
    let vm_entries = crate::verification_methods::list_methods(&state.db, state.db_backend).await?;
    for vm in &vm_entries {
        let key = vm.fragment_id.trim_start_matches('#').to_string();
        verification_methods.insert(key, serde_json::json!(vm.public_key_multibase));
    }

    // Build services: start from existing, then merge our service entries
    let mut services_map = last_op["services"].as_object().cloned().unwrap_or_default();

    let entries = list_entries(&state.db, state.db_backend).await?;
    let public_url = &state.config.public_url;

    // Collect the fragment keys we manage so we can remove stale entries
    let managed_keys: std::collections::HashSet<String> = entries
        .iter()
        .map(|e| e.fragment_id.trim_start_matches('#').to_string())
        .collect();

    // Remove any services that were previously managed but are no longer present
    // (We only remove keys that look like they could be ours — those that were in
    // the DB before. We detect "ours" by checking endpoint == public_url.)
    services_map.retain(|key, val| {
        if managed_keys.contains(key) {
            return true; // will be overwritten below
        }
        // Keep services whose endpoint differs from ours (they belong to the account)
        val["endpoint"].as_str() != Some(public_url)
    });

    for entry in &entries {
        let key = entry.fragment_id.trim_start_matches('#').to_string();
        services_map.insert(
            key,
            serde_json::json!({
                "type": entry.service_type,
                "endpoint": public_url,
            }),
        );
    }

    // Build, sign, and submit the update operation
    let unsigned = crate::plc::build_update_operation(
        &prev_cid,
        rotation_keys,
        verification_methods,
        also_known_as,
        services_map,
    );

    // Decrypt the rotation key for signing
    let rotation_key_enc_sql = crate::db::adapt_sql(
        "SELECT rotation_key_enc FROM happyview_service_identity WHERE id = 1",
        state.db_backend,
    );
    let row: Option<(Option<String>,)> = sqlx::query_as(&rotation_key_enc_sql)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to fetch rotation key: {e}")))?;
    let rotation_key_enc = row
        .and_then(|(k,)| k)
        .ok_or_else(|| AppError::Internal("no rotation key stored".into()))?;
    let rotation_key_bytes = crate::plc::decrypt_key(&rotation_key_enc, encryption_key)?;
    let rotation_signing_key =
        p256::ecdsa::SigningKey::from_bytes(rotation_key_bytes.as_slice().into())
            .map_err(|e| AppError::Internal(format!("invalid rotation key: {e}")))?;

    let signed = crate::plc::sign_operation(&unsigned, &rotation_signing_key)?;
    crate::plc::submit_operation(&state.http, plc_url, did, &signed).await?;

    log_event(
        &state.db,
        EventLog {
            event_type: "service_entry.plc_synced".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: None,
            detail: serde_json::json!({ "mode": "did_plc" }),
        },
        state.db_backend,
    )
    .await;

    tracing::info!(did = %did, "PLC DID document synced (did_plc mode)");

    Ok(StatusCode::NO_CONTENT)
}

/// POST /admin/service-entries/sync-plc/request — request PLC operation signature
/// for attach_account mode (sends email confirmation code).
pub(super) async fn sync_plc_request(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let identity = crate::service_identity::get_identity(&state.db, state.db_backend).await?;
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
                "PLC sync request only supported for attach_account mode".into(),
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

#[derive(Debug, serde::Deserialize)]
pub(super) struct SyncPlcSubmitBody {
    token: String,
}

/// POST /admin/service-entries/sync-plc/submit — submit PLC operation with email token
/// for attach_account mode.
pub(super) async fn sync_plc_submit(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<SyncPlcSubmitBody>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let identity = crate::service_identity::get_identity(&state.db, state.db_backend).await?;
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
                "PLC sync submit only supported for attach_account mode".into(),
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
    let last_op = crate::plc::fetch_last_operation(&state.http, plc_url, &account_did).await?;

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

    let entries = list_entries(&state.db, state.db_backend).await?;
    let public_url = &state.config.public_url;

    // Remove services whose endpoint matches ours that are no longer in the DB
    let managed_keys: std::collections::HashSet<String> = entries
        .iter()
        .map(|e| e.fragment_id.trim_start_matches('#').to_string())
        .collect();

    services_map.retain(|key, val| {
        if managed_keys.contains(key) {
            return true;
        }
        val["endpoint"].as_str() != Some(public_url)
    });

    for entry in &entries {
        let key = entry.fragment_id.trim_start_matches('#').to_string();
        services_map.insert(
            key,
            serde_json::json!({
                "type": entry.service_type,
                "endpoint": public_url,
            }),
        );
    }

    let services: Unknown = serde_json::from_value(serde_json::Value::Object(services_map))
        .map_err(|e| AppError::Internal(format!("failed to build services Unknown: {e}")))?;

    // Merge verification methods from the table into existing
    let mut vm_map = last_op["verificationMethods"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    let vm_entries = crate::verification_methods::list_methods(&state.db, state.db_backend).await?;
    for vm in &vm_entries {
        let key = vm.fragment_id.trim_start_matches('#').to_string();
        vm_map.insert(key, serde_json::json!(vm.public_key_multibase));
    }
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

    log_event(
        &state.db,
        EventLog {
            event_type: "service_entry.plc_synced".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: None,
            detail: serde_json::json!({ "mode": "attach_account" }),
        },
        state.db_backend,
    )
    .await;

    tracing::info!(did = %account_did, "PLC DID document synced (attach_account mode)");

    Ok(StatusCode::NO_CONTENT)
}

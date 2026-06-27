use axum::{Json, extract::Path, extract::State, http::StatusCode};

use crate::AppState;
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};
use crate::verification_methods::{VerificationMethod, create_method, delete_method, list_methods};

use super::auth::UserAuth;
use super::permissions::Permission;

/// GET /admin/verification-methods — list all verification methods.
pub(super) async fn list(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<Vec<VerificationMethod>>, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let methods = list_methods(&state.db, state.db_backend).await?;
    Ok(Json(methods))
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct CreateVerificationMethodBody {
    pub fragment_id: String,
}

/// POST /admin/verification-methods — create a new verification method (generates P-256 keypair).
pub(super) async fn create(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<CreateVerificationMethodBody>,
) -> Result<(StatusCode, Json<VerificationMethod>), AppError> {
    auth.require(Permission::SettingsManage).await?;

    let encryption_key = state
        .config
        .token_encryption_key
        .as_ref()
        .ok_or_else(|| AppError::Internal("TOKEN_ENCRYPTION_KEY not configured".into()))?;

    let method = create_method(
        &state.db,
        state.db_backend,
        &body.fragment_id,
        encryption_key,
    )
    .await?;

    log_event(
        &state.db,
        EventLog {
            event_type: "verification_method.created".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(method.fragment_id.clone()),
            detail: serde_json::json!({ "fragment_id": &method.fragment_id }),
        },
        state.db_backend,
    )
    .await;

    Ok((StatusCode::CREATED, Json(method)))
}

/// DELETE /admin/verification-methods/{fragment_id} — delete a verification method.
pub(super) async fn delete(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(fragment_id): Path<String>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let deleted = delete_method(&state.db, state.db_backend, &fragment_id).await?;
    if !deleted {
        return Err(AppError::NotFound(format!(
            "verification method '{}' not found",
            fragment_id
        )));
    }

    log_event(
        &state.db,
        EventLog {
            event_type: "verification_method.deleted".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(fragment_id.clone()),
            detail: serde_json::json!({ "fragment_id": &fragment_id }),
        },
        state.db_backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

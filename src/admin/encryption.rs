//! `/admin/encryption/rotation`: what a `TOKEN_ENCRYPTION_KEY` rotation still
//! has to re-seal, and starting the job that does it.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde_json::json;

use crate::AppState;
use crate::error::AppError;
use crate::jobs::native::reseal;

use super::auth::UserAuth;
use super::permissions::Permission;

/// `GET /admin/encryption/rotation`
///
/// `remaining` counts the rows only `TOKEN_ENCRYPTION_KEY_PREVIOUS` opens. It
/// is read from the stored values on every request, so `previous_key_can_go`
/// is never true while one still needs the previous key.
pub(super) async fn rotation_status(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::SettingsManage).await?;
    let Some(keys) = state.config.keyring() else {
        return Ok(Json(json!({
            "key_configured": false,
            "previous_key_configured": false,
            "previous_key_can_go": false,
            "remaining": 0,
            "stores": [],
            "job": null,
        })));
    };
    let stores = crate::reseal::status(&state.db, state.db_backend, &keys).await?;
    let remaining: u64 = stores.iter().map(|s| s.previous).sum();
    let job = reseal::active(&state)
        .await?
        .map(|job| json!({ "id": job.id, "status": job.status, "progress": job.progress }));
    Ok(Json(json!({
        "key_configured": true,
        "previous_key_configured": keys.has_previous(),
        "previous_key_can_go": keys.has_previous() && remaining == 0,
        "remaining": remaining,
        "unsealed": stores.iter().map(|s| s.unsealed).sum::<u64>(),
        "unreadable": stores.iter().map(|s| s.unreadable).sum::<u64>(),
        "stores": stores,
        "job": job,
    })))
}

/// `POST /admin/encryption/rotation`: start a re-seal, or answer the one
/// already under way.
pub(super) async fn start_reseal(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    auth.require(Permission::SettingsManage).await?;
    if state.config.keyring().is_none() {
        return Err(AppError::BadRequest(
            "TOKEN_ENCRYPTION_KEY is not set, so there is nothing to re-seal with".into(),
        ));
    }
    let job_id = reseal::start(&state, &auth.did).await?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "job_id": job_id }))))
}

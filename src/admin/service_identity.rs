use axum::{Json, extract::State, http::StatusCode};

use crate::AppState;
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};
use crate::service_identity::{IdentityMode, get_identity, upsert_identity};

use super::auth::UserAuth;
use super::permissions::Permission;

/// GET /admin/service-identity — return current identity config (or null).
pub(super) async fn get(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let identity = get_identity(&state.db, state.db_backend).await?;

    Ok(Json(match identity {
        Some(id) => serde_json::to_value(id)
            .map_err(|e| AppError::Internal(format!("failed to serialize identity: {e}")))?,
        None => serde_json::Value::Null,
    }))
}

async fn stored_rotation_key(state: &AppState) -> Result<Option<String>, AppError> {
    let sql = crate::db::adapt_sql(
        "SELECT rotation_key_enc FROM happyview_service_identity WHERE id = 1",
        state.db_backend,
    );
    let row: Option<(Option<String>,)> = crate::db::query_as(&sql)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to read service identity: {e}")))?;
    Ok(row.and_then(|(key,)| key))
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct UpdateIdentityBody {
    pub mode: String,
    pub did: Option<String>,
    pub signing_key_enc: Option<String>,
    pub rotation_key_enc: Option<String>,
    pub attached_account_did: Option<String>,
}

/// PUT /admin/service-identity — update identity config.
pub(super) async fn update(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<UpdateIdentityBody>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::SettingsManage).await?;

    let mode = IdentityMode::parse(&body.mode)
        .ok_or_else(|| AppError::BadRequest(format!("invalid identity mode: {}", body.mode)))?;

    // Keys aren't returned by GET, so a caller changing something else can't
    // send them back. An update within a mode keeps the ones it omits; a
    // change of mode leaves the old mode's keys behind.
    let (signing_key_enc, rotation_key_enc) =
        match get_identity(&state.db, state.db_backend).await? {
            Some(current) if current.mode.as_str() == mode.as_str() => (
                body.signing_key_enc.or(current.signing_key_enc),
                match body.rotation_key_enc {
                    Some(key) => Some(key),
                    None => stored_rotation_key(&state).await?,
                },
            ),
            _ => (body.signing_key_enc, body.rotation_key_enc),
        };

    upsert_identity(
        &state.db,
        state.db_backend,
        &mode,
        body.did.as_deref(),
        signing_key_enc.as_deref(),
        rotation_key_enc.as_deref(),
        body.attached_account_did.as_deref(),
    )
    .await?;

    log_event(
        &state.db,
        EventLog {
            event_type: "service_identity.updated".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: None,
            detail: serde_json::json!({ "mode": body.mode }),
        },
        state.db_backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

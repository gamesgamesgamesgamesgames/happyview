use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use crate::auth::XrpcClaims;
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};

use super::DelegateRole;
use super::db;

#[derive(Deserialize)]
pub struct LinkAccountInput {
    pub did: String,
}

pub async fn link_account(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<LinkAccountInput>,
) -> Result<Response, AppError> {
    let claims = xrpc_claims
        .identity
        .ok_or_else(|| AppError::Auth("linkAccount requires authentication".into()))?;

    let caller_did = claims.did().to_string();
    let account_did = &input.did;

    if caller_did == *account_did {
        return Err(AppError::BadRequest(
            "cannot link your own account as a delegate".into(),
        ));
    }

    if db::is_account_linked(&state.db, state.db_backend, account_did).await? {
        return Err(AppError::Conflict("account is already linked".into()));
    }

    // Verify a DPoP session exists for the target DID
    let client_key = claims
        .client_key()
        .ok_or_else(|| AppError::Auth("linkAccount requires DPoP authentication".into()))?;
    let api_client_id = crate::repo::get_dpop_client_id(&state, client_key).await?;

    let session_check_sql = crate::db::adapt_sql(
        "SELECT id FROM happyview_dpop_sessions WHERE api_client_id = ? AND user_did = ?",
        state.db_backend,
    );
    let session_exists: Option<(String,)> = sqlx::query_as(&session_check_sql)
        .bind(&api_client_id)
        .bind(account_did)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to check session: {e}")))?;

    if session_exists.is_none() {
        return Err(AppError::BadRequest(
            "no DPoP session found for the target account — complete OAuth first".into(),
        ));
    }

    db::create_delegated_account(
        &state.db,
        state.db_backend,
        account_did,
        &caller_did,
        &api_client_id,
    )
    .await?;
    db::add_delegate(
        &state.db,
        state.db_backend,
        account_did,
        &caller_did,
        DelegateRole::Owner,
        &caller_did,
    )
    .await?;

    log_event(
        &state.db,
        EventLog {
            event_type: "delegation.account_linked".to_string(),
            severity: Severity::Info,
            actor_did: Some(caller_did),
            subject: Some(account_did.clone()),
            detail: json!({}),
        },
        state.db_backend,
    )
    .await;

    Ok((StatusCode::CREATED, Json(json!({ "did": account_did }))).into_response())
}

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
pub struct UnlinkAccountInput {
    pub did: String,
}

pub async fn unlink_account(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<UnlinkAccountInput>,
) -> Result<Response, AppError> {
    let claims = xrpc_claims
        .identity
        .ok_or_else(|| AppError::Auth("unlinkAccount requires authentication".into()))?;

    let caller_did = claims.did().to_string();
    let account_did = &input.did;

    super::verify_client_scope(&state, &claims, account_did).await?;

    let is_linked = db::is_account_linked(&state.db, state.db_backend, account_did).await?;
    if !is_linked {
        return Err(AppError::NotFound("delegated account not found".into()));
    }

    let role = db::get_delegate_role(&state.db, state.db_backend, account_did, &caller_did)
        .await?
        .ok_or_else(|| AppError::Forbidden("you are not a delegate of this account".into()))?;

    if role != DelegateRole::Owner {
        return Err(AppError::Forbidden(
            "only the owner can unlink an account".into(),
        ));
    }

    // Look up the stored api_client_id before deleting the account
    let stored_api_client_id =
        db::get_api_client_id(&state.db, state.db_backend, account_did).await?;

    // Delete delegated account (CASCADE deletes all delegates)
    db::delete_delegated_account(&state.db, state.db_backend, account_did).await?;

    // Delete all DPoP sessions for the target account using the stored
    // api_client_id, revoking each at the user's authorization server first —
    // unlinking that left the grants live on their PDS was the same leak as a
    // logout that never told anyone.
    if let Some(api_client_id) = stored_api_client_id {
        if let Some(encryption_key) = state.config.token_encryption_key.as_ref() {
            let key_ids = crate::oauth::sessions::dpop_key_ids_for_user(
                &state.db,
                state.db_backend,
                &api_client_id,
                account_did,
            )
            .await
            .unwrap_or_default();

            for dpop_key_id in &key_ids {
                if let Err(e) = crate::oauth::pds_write::revoke_dpop_session_at_as(
                    &state.http,
                    &state.db,
                    state.db_backend,
                    encryption_key,
                    &state.oauth,
                    &api_client_id,
                    account_did,
                    dpop_key_id,
                )
                .await
                {
                    tracing::warn!(
                        account_did,
                        %e,
                        "failed to revoke session at authorization server on unlink"
                    );
                }
            }
        }

        if let Err(e) = crate::oauth::sessions::delete_all_dpop_sessions(
            &state.db,
            state.db_backend,
            &api_client_id,
            account_did,
        )
        .await
        {
            tracing::warn!(account_did, %e, "failed to clean up DPoP sessions on unlink");
        }
    }

    log_event(
        &state.db,
        EventLog {
            event_type: "delegation.account_unlinked".to_string(),
            severity: Severity::Info,
            actor_did: Some(caller_did),
            subject: Some(account_did.clone()),
            detail: json!({}),
        },
        state.db_backend,
    )
    .await;

    Ok((StatusCode::OK, Json(json!({}))).into_response())
}

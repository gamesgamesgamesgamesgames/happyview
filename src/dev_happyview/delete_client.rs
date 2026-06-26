use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};

use super::DeleteApiClientInput;
use crate::AppState;
use crate::auth::XrpcClaims;
use crate::db::adapt_sql;
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};
use crate::rate_limit::CheckResult;

pub async fn delete_api_client(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<DeleteApiClientInput>,
) -> Result<Response, AppError> {
    // 1. Require DPoP auth
    let claims = xrpc_claims
        .identity
        .ok_or_else(|| AppError::Auth("deleteApiClient requires DPoP authentication".into()))?;

    // 2. Rate-limit the request (procedure type)
    let check = if let Some(client_key) = claims.client_key() {
        let cost = state
            .rate_limiter
            .default_cost_for_type(client_key, "procedure");
        Some(state.rate_limiter.check(client_key, cost))
    } else {
        None
    };

    if let Some(CheckResult::Limited {
        retry_after,
        limit,
        reset,
    }) = check
    {
        return Err(AppError::RateLimited {
            retry_after,
            limit,
            reset,
        });
    }

    let user_did = claims.did().to_string();
    let id = input.id;

    // 3. Look up client_id_url and client_key before deleting (scoped to owner)
    let lookup_sql = adapt_sql(
        "SELECT client_id_url, client_key FROM happyview_api_clients WHERE id = ? AND owner_did = ?",
        state.db_backend,
    );
    let client_info: Option<(String, String)> = sqlx::query_as(&lookup_sql)
        .bind(&id)
        .bind(&user_did)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up api client: {e}")))?;

    // 4. Look up child clients before deleting (ON DELETE CASCADE will remove DB rows)
    let children_sql = adapt_sql(
        "SELECT client_id_url, client_key FROM happyview_api_clients WHERE parent_client_id = ?",
        state.db_backend,
    );
    let children: Vec<(String, String)> = sqlx::query_as(&children_sql)
        .bind(&id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();

    // 5. Delete from DB — scoped to owner_did so users cannot delete others' clients
    let delete_sql = adapt_sql(
        "DELETE FROM happyview_api_clients WHERE id = ? AND owner_did = ?",
        state.db_backend,
    );
    let result = sqlx::query(&delete_sql)
        .bind(&id)
        .bind(&user_did)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to delete api client: {e}")))?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("api client '{id}' not found")));
    }

    // 6. Remove parent from OAuth registry, rate limiter, and client identities
    if let Some((url, key)) = client_info {
        state.oauth.remove(&url);
        state.rate_limiter.remove_client_config(&key);
        state.rate_limiter.remove_client_identity(&key);
    }

    // 7. Remove child clients from in-memory registries (DB rows already cascaded)
    for (child_url, child_key) in &children {
        state.oauth.remove(child_url);
        state.rate_limiter.remove_client_config(child_key);
        state.rate_limiter.remove_client_identity(child_key);
    }

    // 8. Log event
    log_event(
        &state.db,
        EventLog {
            event_type: "api_client.deleted".to_string(),
            severity: Severity::Info,
            actor_did: Some(user_did.clone()),
            subject: Some(id),
            detail: serde_json::json!({}),
        },
        state.db_backend,
    )
    .await;

    // 9. Return `{}`
    let mut response = Json(serde_json::json!({})).into_response();

    if let Some(CheckResult::Allowed {
        remaining,
        limit,
        reset,
    }) = check
    {
        let h = response.headers_mut();
        h.insert("RateLimit-Limit", limit.into());
        h.insert("RateLimit-Remaining", remaining.into());
        h.insert("RateLimit-Reset", reset.into());
    }

    Ok(response)
}

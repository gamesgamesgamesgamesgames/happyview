use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};

use super::row_to_view;
use crate::AppState;
use crate::auth::XrpcClaims;
use crate::db::adapt_sql;
use crate::error::AppError;
use crate::rate_limit::CheckResult;

pub async fn list_api_clients(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
) -> Result<Response, AppError> {
    let claims = xrpc_claims
        .identity
        .ok_or_else(|| AppError::Auth("listApiClients requires DPoP authentication".into()))?;

    let check = if let Some(client_key) = claims.client_key() {
        let cost = state
            .rate_limiter
            .default_cost_for_type(client_key, "query");
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

    let did = claims.did();

    // Reject requests from child clients
    if let Some(client_key) = claims.client_key() {
        let sql = adapt_sql(
            "SELECT parent_client_id FROM happyview_api_clients WHERE client_key = $1",
            state.db_backend,
        );
        let parent_check: Option<Option<String>> = sqlx::query_scalar(&sql)
            .bind(client_key)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("client lookup failed: {e}")))?;

        if let Some(Some(_)) = parent_check {
            return Err(AppError::Auth(
                "child clients cannot manage API clients".into(),
            ));
        }
    }

    // Fetch all clients owned by the authenticated user
    let sql = adapt_sql(
        "SELECT id, client_key, name, client_id_url, client_uri, redirect_uris, scopes, client_type, allowed_origins, is_active, created_at \
         FROM happyview_api_clients \
         WHERE owner_did = $1 \
         ORDER BY created_at DESC",
        state.db_backend,
    );

    let rows = sqlx::query(&sql)
        .bind(did)
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list clients: {e}")))?;

    let clients: Vec<_> = rows
        .iter()
        .filter_map(|row| row_to_view(row).ok())
        .collect();

    let mut response = Json(serde_json::json!({ "clients": clients })).into_response();

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

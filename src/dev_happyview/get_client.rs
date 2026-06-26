use axum::Json;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use super::row_to_view;
use crate::AppState;
use crate::auth::XrpcClaims;
use crate::db::adapt_sql;
use crate::error::AppError;
use crate::rate_limit::CheckResult;

#[derive(Debug, Deserialize)]
pub struct GetApiClientParams {
    pub id: String,
}

pub async fn get_api_client(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Query(params): Query<GetApiClientParams>,
) -> Result<Response, AppError> {
    let claims = xrpc_claims
        .identity
        .ok_or_else(|| AppError::Auth("getApiClient requires DPoP authentication".into()))?;

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

    let sql = adapt_sql(
        "SELECT id, client_key, name, client_id_url, client_uri, redirect_uris, scopes, client_type, allowed_origins, is_active, created_at \
         FROM happyview_api_clients \
         WHERE id = $1 AND owner_did = $2",
        state.db_backend,
    );

    let row = sqlx::query(&sql)
        .bind(&params.id)
        .bind(did)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to get client: {e}")))?
        .ok_or_else(|| AppError::NotFound("API client not found".into()))?;

    let client =
        row_to_view(&row).map_err(|e| AppError::Internal(format!("failed to read client: {e}")))?;

    let mut response = Json(serde_json::json!({ "client": client })).into_response();

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

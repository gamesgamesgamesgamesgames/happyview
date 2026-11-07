use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};

use super::{Manager, row_to_view};
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
        .ok_or_else(|| AppError::Auth("listApiClients requires DPoP authentication, or the session cookie on a request from this instance".into()))?;

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

    let (filter, app_id) = Manager::of(&state, &claims).await?.filter();

    let sql = adapt_sql(
        &format!(
            "SELECT id, client_key, name, client_id_url, client_uri, redirect_uris, scopes, client_type, allowed_origins, is_active, created_at \
             FROM happyview_api_clients \
             WHERE owner_did = ?{filter} \
             ORDER BY created_at DESC"
        ),
        state.db_backend,
    );

    let mut query = crate::db::query(&sql).bind(did);
    if let Some(app_id) = app_id {
        query = query.bind(app_id);
    }
    let rows = query
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

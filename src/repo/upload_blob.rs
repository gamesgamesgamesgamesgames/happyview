use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;

use crate::AppState;
use crate::auth::XrpcClaims;
use crate::error::AppError;
use crate::rate_limit::CheckResult;

use super::pds::pds_post_blob;

pub async fn upload_blob(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let claims = xrpc_claims
        .identity
        .ok_or_else(|| AppError::Auth("uploadBlob requires DPoP authentication".into()))?;
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

    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream");

    // The auth branch is the shared one, so this route and the service-proxy
    // path cannot drift apart in how they resolve credentials.
    let auth = crate::repo::pds_auth_for_claims(&state, &claims).await?;

    // A blob scope is a MIME pattern, so the check is on the declared
    // content-type. Parameters are stripped and case folded first — that is
    // HTTP's business, not the scope grammar's, which matches only a clean
    // type/subtype.
    if let Some(scopes) = auth.granted_scopes(&state).await? {
        let mime = content_type
            .split(';')
            .next()
            .unwrap_or(content_type)
            .trim()
            .to_ascii_lowercase();
        let granted = happyview_scopes::ScopePermissions::parse(&scopes);
        crate::xrpc::scope_check::check(
            &granted,
            &[crate::xrpc::scope_check::Required::Blob { mime }],
            "com.atproto.repo.uploadBlob",
        )?;
    }

    let mut response = match &auth {
        crate::repo::PdsAuth::Dpop {
            api_client_id,
            dpop_key_id,
            encryption_key,
        } => {
            // Blobs are raw bytes with a caller-chosen content type, so they do
            // not go through the JSON forward helper.
            let resp = crate::oauth::pds_write::dpop_pds_post_blob(
                &state.http,
                &state.db,
                state.db_backend,
                encryption_key,
                &state.oauth,
                &state.config.plc_url,
                api_client_id,
                claims.did(),
                dpop_key_id,
                content_type,
                body,
            )
            .await?;

            crate::repo::forward_pds_response(resp).await?
        }
        crate::repo::PdsAuth::OAuth(session) => {
            pds_post_blob(&state, session, content_type, body).await?
        }
    };

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

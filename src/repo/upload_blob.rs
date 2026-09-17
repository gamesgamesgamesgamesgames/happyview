use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;

use crate::AppState;
use crate::auth::XrpcClaims;
use crate::error::AppError;
use crate::rate_limit::CheckResult;

use super::pds::{PdsAuth, pds_post_blob};

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

/// Upload a blob to the caller's PDS through whichever credential the caller
/// holds, returning the PDS's blob reference verbatim.
pub(crate) async fn upload_blob_to_pds(
    state: &AppState,
    caller_did: &str,
    pds_auth: &PdsAuth,
    content_type: &str,
    blob_bytes: Bytes,
) -> Result<serde_json::Value, AppError> {
    match pds_auth {
        PdsAuth::OAuth(session) => {
            use atrium_xrpc::{
                InputDataOrBytes, OutputDataOrBytes, XrpcClient, XrpcRequest, http::Method,
            };

            let request = XrpcRequest {
                method: Method::POST,
                nsid: "com.atproto.repo.uploadBlob".to_string(),
                parameters: None::<()>,
                input: Some(InputDataOrBytes::<()>::Bytes(blob_bytes.to_vec())),
                encoding: Some(content_type.to_string()),
            };

            let result: Result<
                OutputDataOrBytes<serde_json::Value>,
                atrium_xrpc::Error<serde_json::Value>,
            > = session.send_xrpc(&request).await;

            match result {
                Ok(OutputDataOrBytes::Data(data)) => Ok(data),
                Ok(OutputDataOrBytes::Bytes(bytes)) => serde_json::from_slice(&bytes)
                    .map_err(|e| AppError::Internal(format!("invalid uploadBlob response: {e}"))),
                Err(atrium_xrpc::Error::XrpcResponse(xrpc_err)) => {
                    let status = axum::http::StatusCode::from_u16(xrpc_err.status.as_u16())
                        .unwrap_or(axum::http::StatusCode::BAD_GATEWAY);
                    let body = xrpc_err
                        .error
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    Err(AppError::PdsError(status, Bytes::from(body)))
                }
                Err(e) => Err(AppError::Internal(format!("PDS uploadBlob failed: {e}"))),
            }
        }
        PdsAuth::Dpop {
            api_client_id,
            dpop_key_id,
            encryption_key,
        } => {
            let resp = crate::oauth::pds_write::dpop_pds_post_blob(
                &state.http,
                &state.db,
                state.db_backend,
                encryption_key,
                &state.oauth,
                &state.config.plc_url,
                api_client_id,
                caller_did,
                dpop_key_id,
                content_type,
                blob_bytes,
            )
            .await?;

            let status = resp.status();
            let body = resp
                .bytes()
                .await
                .map_err(|e| AppError::Internal(format!("failed to read upload response: {e}")))?;

            if !status.is_success() {
                let axum_status = axum::http::StatusCode::from_u16(status.as_u16())
                    .unwrap_or(axum::http::StatusCode::BAD_GATEWAY);
                return Err(AppError::PdsError(axum_status, body));
            }

            serde_json::from_slice(&body)
                .map_err(|e| AppError::Internal(format!("invalid uploadBlob response: {e}")))
        }
    }
}

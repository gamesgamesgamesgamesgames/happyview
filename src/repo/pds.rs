use std::sync::Arc;

use atrium_xrpc::{InputDataOrBytes, OutputDataOrBytes, XrpcClient, XrpcRequest, http::Method};
use axum::body::Bytes;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::AppState;
use crate::HappyViewOAuthSession;
use crate::error::AppError;

/// Abstraction over the two PDS authentication paths.
///
/// - `OAuth`: uses atrium's OAuthSession (dashboard cookie auth)
/// - `Dpop`: uses the manual DPoP session from `dpop_sessions` table (third-party apps)
#[derive(Clone)]
pub(crate) enum PdsAuth {
    OAuth(Arc<HappyViewOAuthSession>),
    Dpop {
        api_client_id: String,
        dpop_key_id: String,
        encryption_key: [u8; 32],
    },
}

impl PdsAuth {
    pub async fn post_json(
        &self,
        state: &AppState,
        user_did: &str,
        xrpc_method: &str,
        body: &Value,
    ) -> Result<reqwest::Response, AppError> {
        self.post_json_with_headers(state, user_did, xrpc_method, body, &[])
            .await
    }

    /// POST JSON, additionally forwarding `headers` to the PDS.
    ///
    /// Used for service proxying, where `atproto-proxy` selects a destination
    /// the PDS is responsible for resolving.
    ///
    /// The two auth paths carry the headers differently. The DPoP path builds
    /// its own request and sets them directly. The OAuth path goes through
    /// atrium, whose `XrpcRequest` has no header field — but atrium models
    /// these two headers as *client configuration* rather than request data,
    /// so they are applied to the session instead. See
    /// [`apply_forwarded_headers`] for why that is safe here.
    pub async fn post_json_with_headers(
        &self,
        state: &AppState,
        user_did: &str,
        xrpc_method: &str,
        body: &Value,
        headers: &[(String, String)],
    ) -> Result<reqwest::Response, AppError> {
        match self {
            PdsAuth::OAuth(session) => {
                apply_forwarded_headers(session, headers)?;
                pds_post_json_raw(state, session, xrpc_method, body).await
            }
            PdsAuth::Dpop {
                api_client_id,
                dpop_key_id,
                encryption_key,
            } => {
                crate::oauth::pds_write::dpop_pds_post_with_headers(
                    &state.http,
                    &state.db,
                    state.db_backend,
                    encryption_key,
                    &state.oauth,
                    &state.config.plc_url,
                    api_client_id,
                    user_did,
                    dpop_key_id,
                    xrpc_method,
                    body,
                    headers,
                )
                .await
            }
        }
    }

    /// The scopes this session was granted, when they are knowable.
    ///
    /// `None` means "not knowable here", not "no scopes". The DPoP path stores
    /// the granted scope string alongside the session; the OAuth path's session
    /// is atrium's and does not expose one. Callers must treat `None` as
    /// *skip the local check and let the PDS decide* — inferring "no scopes"
    /// would refuse every dashboard request.
    pub async fn granted_scopes(&self, state: &AppState) -> Result<Option<String>, AppError> {
        match self {
            PdsAuth::OAuth(_) => Ok(None),
            PdsAuth::Dpop {
                api_client_id,
                dpop_key_id,
                ..
            } => {
                crate::oauth::sessions::get_dpop_session_scopes(
                    &state.db,
                    state.db_backend,
                    api_client_id,
                    dpop_key_id,
                )
                .await
            }
        }
    }

    /// GET an XRPC endpoint, forwarding `query` verbatim and applying any
    /// service-proxy headers.
    pub async fn get_with_headers(
        &self,
        state: &AppState,
        user_did: &str,
        xrpc_method: &str,
        query: &str,
        headers: &[(String, String)],
    ) -> Result<reqwest::Response, AppError> {
        match self {
            PdsAuth::OAuth(session) => {
                apply_forwarded_headers(session, headers)?;
                pds_get_raw(session, xrpc_method, query).await
            }
            PdsAuth::Dpop {
                api_client_id,
                dpop_key_id,
                encryption_key,
            } => {
                crate::oauth::pds_write::dpop_pds_get(
                    &state.http,
                    &state.db,
                    state.db_backend,
                    encryption_key,
                    &state.oauth,
                    &state.config.plc_url,
                    api_client_id,
                    user_did,
                    dpop_key_id,
                    xrpc_method,
                    query,
                    headers,
                )
                .await
            }
        }
    }
}

/// GET an XRPC endpoint using the OAuth session.
///
/// Parameters are passed as ordered pairs rather than a map so repeated keys
/// survive: `serde_html_form` renders `[("a","1"),("a","2")]` back to
/// `a=1&a=2`, where a map would keep only one.
async fn pds_get_raw(
    session: &HappyViewOAuthSession,
    xrpc_method: &str,
    query: &str,
) -> Result<reqwest::Response, AppError> {
    let parameters: Vec<(String, String)> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (decode(k), decode(v)),
            None => (decode(pair), String::new()),
        })
        .collect();

    let request = XrpcRequest {
        method: Method::GET,
        nsid: xrpc_method.to_string(),
        parameters: if parameters.is_empty() {
            None
        } else {
            Some(parameters)
        },
        input: None::<InputDataOrBytes<()>>,
        encoding: None,
    };

    let result: Result<OutputDataOrBytes<Value>, atrium_xrpc::Error<Value>> =
        session.send_xrpc(&request).await;

    let bytes = match result {
        Ok(OutputDataOrBytes::Data(data)) => serde_json::to_vec(&data)
            .map_err(|e| AppError::Internal(format!("failed to serialize response: {e}")))?,
        Ok(OutputDataOrBytes::Bytes(bytes)) => bytes,
        Err(e) => return Err(AppError::Internal(format!("PDS request failed: {e}"))),
    };

    let http_resp = atrium_xrpc::http::Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(bytes)
        .map_err(|e| AppError::Internal(format!("failed to build response: {e}")))?;
    Ok(reqwest::Response::from(http_resp))
}

fn decode(value: &str) -> String {
    let plus_decoded = value.replace('+', " ");
    urlencoding::decode(&plus_decoded)
        .map(|c| c.into_owned())
        .unwrap_or(plus_decoded)
}

/// Apply forwarded service-proxy headers to an OAuth session.
///
/// atrium models these two headers as *client configuration* rather than
/// request data, which is why they cannot be passed through `XrpcRequest`.
/// Configuring goes through `&self`, so this is only safe because the session
/// is built per request — `pds_auth_for_claims` calls `get_oauth_session`,
/// which restores a fresh one each time. It must not be applied to a session
/// shared across requests, or a proxy target would leak onto the next one and
/// send it somewhere nobody asked for.
///
/// A header that cannot be parsed is an error rather than a silent drop:
/// dropping `atproto-proxy` turns "relay this onwards" into "handle this
/// yourself", producing a plausible wrong answer instead of a failure.
fn apply_forwarded_headers(
    session: &HappyViewOAuthSession,
    headers: &[(String, String)],
) -> Result<(), AppError> {
    use atrium_api::agent::Configure;

    for (name, value) in headers {
        match name.as_str() {
            "atproto-proxy" => {
                let (did, service_type) = parse_proxy_header(value)?;
                session.configure_proxy_header(did, service_type);
            }
            "atproto-accept-labelers" => {
                let labelers = parse_labelers_header(value)?;
                if !labelers.is_empty() {
                    session.configure_labelers_header(Some(labelers));
                }
            }
            other => {
                return Err(AppError::Internal(format!(
                    "unexpected forwarded header: {other}"
                )));
            }
        }
    }

    Ok(())
}

/// Split `atproto-proxy` into its DID and service-type halves.
///
/// The value is `<did>#<service-type>`, e.g.
/// `did:web:api.bsky.app#bsky_appview`. Split from the right so a DID
/// containing no `#` of its own cannot be mis-parsed, and refuse anything
/// without one rather than guessing a service type.
fn parse_proxy_header(value: &str) -> Result<(atrium_api::types::string::Did, &str), AppError> {
    use atrium_api::types::string::Did;

    let (did, service_type) = value.rsplit_once('#').ok_or_else(|| {
        AppError::BadRequest(format!(
            "atproto-proxy must be <did>#<service-type>, got: {value}"
        ))
    })?;
    if service_type.is_empty() {
        return Err(AppError::BadRequest(format!(
            "atproto-proxy is missing a service type after '#': {value}"
        )));
    }
    let did = Did::new(did.to_string())
        .map_err(|_| AppError::BadRequest(format!("atproto-proxy has an invalid DID: {did}")))?;
    Ok((did, service_type))
}

/// Parse `atproto-accept-labelers`: comma-separated DIDs, each optionally
/// suffixed `;redact`.
fn parse_labelers_header(
    value: &str,
) -> Result<Vec<(atrium_api::types::string::Did, bool)>, AppError> {
    use atrium_api::types::string::Did;

    let mut labelers = Vec::new();
    for entry in value.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (did, redact) = match entry.split_once(';') {
            Some((did, rest)) => (did.trim(), rest.trim() == "redact"),
            None => (entry, false),
        };
        let did = Did::new(did.to_string()).map_err(|_| {
            AppError::BadRequest(format!("atproto-accept-labelers has an invalid DID: {did}"))
        })?;
        labelers.push((did, redact));
    }
    Ok(labelers)
}

/// Build the PDS credentials a request should act with, from its claims.
///
/// Third-party apps present a client key and authenticate with DPoP; the
/// dashboard presents its own OAuth session. Both act strictly as the caller.
pub(crate) async fn pds_auth_for_claims(
    state: &AppState,
    claims: &crate::auth::Claims,
) -> Result<PdsAuth, AppError> {
    let Some(client_key) = claims.client_key() else {
        return Ok(PdsAuth::OAuth(std::sync::Arc::new(
            super::session::get_oauth_session(state, claims.did()).await?,
        )));
    };

    let encryption_key = state
        .config
        .token_encryption_key
        .as_ref()
        .ok_or_else(|| AppError::Internal("TOKEN_ENCRYPTION_KEY not configured".into()))?;

    let api_client_id = super::session::get_dpop_client_id(state, client_key).await?;
    let dpop_key_id = claims
        .dpop_key_id()
        .ok_or_else(|| AppError::Internal("DPoP key ID not available in claims".into()))?
        .to_string();

    Ok(PdsAuth::Dpop {
        api_client_id,
        dpop_key_id,
        encryption_key: *encryption_key,
    })
}

/// Forward a PDS response back to the client, preserving status and body.
pub(crate) async fn forward_pds_response(resp: reqwest::Response) -> Result<Response, AppError> {
    let status = resp.status();
    let body = resp
        .bytes()
        .await
        .map_err(|e| AppError::Internal(format!("failed to read PDS response: {e}")))?;

    let axum_status = StatusCode::from_u16(status.as_u16()).unwrap();

    if status.is_success() {
        Ok((
            axum_status,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response())
    } else {
        let body_str = String::from_utf8_lossy(&body);
        tracing::warn!(status = %axum_status, body = %body_str, "PDS returned error");
        Err(AppError::PdsError(axum_status, body))
    }
}

/// POST JSON to a PDS XRPC endpoint using the OAuth session.
/// Uses `send_xrpc` so the OAuthSession attaches DPoP proof and Bearer token.
pub(crate) async fn pds_post_json_raw(
    _state: &AppState,
    session: &HappyViewOAuthSession,
    xrpc_method: &str,
    body: &Value,
) -> Result<reqwest::Response, AppError> {
    let request = XrpcRequest {
        method: Method::POST,
        nsid: xrpc_method.to_string(),
        parameters: None::<()>,
        input: Some(InputDataOrBytes::Data(body.clone())),
        encoding: Some("application/json".to_string()),
    };

    let result: Result<OutputDataOrBytes<Value>, atrium_xrpc::Error<Value>> =
        session.send_xrpc(&request).await;

    match result {
        Ok(OutputDataOrBytes::Data(data)) => {
            let body_bytes = serde_json::to_vec(&data)
                .map_err(|e| AppError::Internal(format!("failed to serialize response: {e}")))?;
            let http_resp = atrium_xrpc::http::Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(body_bytes)
                .map_err(|e| AppError::Internal(format!("failed to build response: {e}")))?;
            Ok(reqwest::Response::from(http_resp))
        }
        Ok(OutputDataOrBytes::Bytes(bytes)) => {
            let http_resp = atrium_xrpc::http::Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(bytes)
                .map_err(|e| AppError::Internal(format!("failed to build response: {e}")))?;
            Ok(reqwest::Response::from(http_resp))
        }
        Err(e) => Err(AppError::Internal(format!("PDS request failed: {e}"))),
    }
}

/// POST a binary blob to the PDS with OAuth session auth.
pub(super) async fn pds_post_blob(
    _state: &AppState,
    session: &HappyViewOAuthSession,
    content_type: &str,
    blob: Bytes,
) -> Result<Response, AppError> {
    let request = XrpcRequest {
        method: Method::POST,
        nsid: "com.atproto.repo.uploadBlob".to_string(),
        parameters: None::<()>,
        input: Some(InputDataOrBytes::<()>::Bytes(blob.to_vec())),
        encoding: Some(content_type.to_string()),
    };

    let result: Result<OutputDataOrBytes<Value>, atrium_xrpc::Error<Value>> =
        session.send_xrpc(&request).await;

    match result {
        Ok(OutputDataOrBytes::Data(data)) => {
            let body_bytes = serde_json::to_vec(&data)
                .map_err(|e| AppError::Internal(format!("failed to serialize response: {e}")))?;
            Ok((
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                Bytes::from(body_bytes),
            )
                .into_response())
        }
        Ok(OutputDataOrBytes::Bytes(bytes)) => Ok((
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            Bytes::from(bytes),
        )
            .into_response()),
        Err(e) => Err(AppError::Internal(format!("PDS uploadBlob failed: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_header_splits_did_and_service_type() {
        let (did, service) = parse_proxy_header("did:web:api.bsky.app#bsky_appview").unwrap();
        assert_eq!(did.as_str(), "did:web:api.bsky.app");
        assert_eq!(service, "bsky_appview");
    }

    #[test]
    fn proxy_header_accepts_a_plc_did() {
        let (did, service) =
            parse_proxy_header("did:plc:6msi3pj7krzih5qxqtryxlzw#atproto_pds").unwrap();
        assert_eq!(did.as_str(), "did:plc:6msi3pj7krzih5qxqtryxlzw");
        assert_eq!(service, "atproto_pds");
    }

    /// Refusing is the point: a dropped `atproto-proxy` would be handled by the
    /// PDS itself, answering a different question than the caller asked.
    #[test]
    fn proxy_header_without_a_fragment_is_refused() {
        assert!(parse_proxy_header("did:web:api.bsky.app").is_err());
        assert!(parse_proxy_header("did:web:api.bsky.app#").is_err());
        assert!(parse_proxy_header("not-a-did#svc").is_err());
        assert!(parse_proxy_header("").is_err());
    }

    #[test]
    fn labelers_header_parses_dids_and_redact_flags() {
        let out = parse_labelers_header(
            "did:plc:6msi3pj7krzih5qxqtryxlzw, did:web:labeler.example.com;redact",
        )
        .unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0.as_str(), "did:plc:6msi3pj7krzih5qxqtryxlzw");
        assert!(!out[0].1);
        assert_eq!(out[1].0.as_str(), "did:web:labeler.example.com");
        assert!(out[1].1);
    }

    #[test]
    fn labelers_header_ignores_empty_entries() {
        let out = parse_labelers_header("did:web:a.example.com,,  ,").unwrap();
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn labelers_header_rejects_an_invalid_did() {
        assert!(parse_labelers_header("did:web:a.example.com,nonsense").is_err());
    }

    #[test]
    fn labelers_header_empty_is_empty_not_an_error() {
        assert!(parse_labelers_header("").unwrap().is_empty());
    }
}

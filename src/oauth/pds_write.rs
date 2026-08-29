use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::{SigningKey, signature::Signer};
use sha2::{Digest, Sha256};

use std::sync::Arc;

use crate::auth::OAuthClientRegistry;
use crate::db::DatabaseBackend;
use crate::error::AppError;
use crate::plugin::encryption::decrypt;

use super::sessions::DpopSession;

/// Resolved DPoP credentials needed to make authenticated PDS requests.
struct DpopCredentials {
    session: DpopSession,
    pds_url: String,
    private_jwk: serde_json::Value,
}

/// Resolve DPoP credentials: session, PDS URL, and decrypted private key.
#[allow(clippy::too_many_arguments)]
async fn resolve_credentials(
    http: &reqwest::Client,
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    encryption_key: &[u8; 32],
    plc_url: &str,
    api_client_id: &str,
    user_did: &str,
    dpop_key_id: &str,
) -> Result<DpopCredentials, AppError> {
    let session = super::sessions::get_dpop_session(
        pool,
        backend,
        encryption_key,
        api_client_id,
        user_did,
        dpop_key_id,
    )
    .await?;

    let pds_url = match session.pds_url {
        Some(ref url) => url.clone(),
        None => resolve_pds_from_did(http, plc_url, user_did).await?,
    };

    let key_sql = crate::db::adapt_sql(
        "SELECT private_key_enc FROM happyview_dpop_keys WHERE id = ?",
        backend,
    );
    let row: Option<(Vec<u8>,)> = crate::db::query_as(&key_sql)
        .bind(&session.dpop_key_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up DPoP key: {e}")))?;

    let (encrypted_key,) = row.ok_or_else(|| AppError::Internal("DPoP key not found".into()))?;

    let key_bytes = decrypt(encryption_key, &encrypted_key)
        .map_err(|e| AppError::Internal(format!("failed to decrypt DPoP key: {e}")))?;

    let private_jwk: serde_json::Value = serde_json::from_slice(&key_bytes)
        .map_err(|e| AppError::Internal(format!("failed to parse DPoP key: {e}")))?;

    Ok(DpopCredentials {
        session,
        pds_url,
        private_jwk,
    })
}

/// A response whose body has been read.
///
/// Deciding whether to retry means inspecting the body, and reading a
/// `reqwest::Response` consumes it. Buffering first is what lets the retry
/// logic distinguish "the server wants a nonce" from "the record you sent is
/// invalid" — both of which can arrive as a 4xx carrying a `dpop-nonce` header.
struct BufferedResponse {
    status: reqwest::StatusCode,
    headers: reqwest::header::HeaderMap,
    body: bytes::Bytes,
}

impl BufferedResponse {
    async fn read(resp: reqwest::Response) -> Result<Self, AppError> {
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = resp
            .bytes()
            .await
            .map_err(|e| AppError::Internal(format!("failed to read PDS response: {e}")))?;
        Ok(Self {
            status,
            headers,
            body,
        })
    }

    fn dpop_nonce(&self) -> Option<String> {
        self.headers
            .get("dpop-nonce")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    }

    fn www_authenticate(&self) -> Option<&str> {
        self.headers
            .get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
    }

    /// The `error` field of a JSON error body, if there is one.
    fn body_error(&self) -> Option<String> {
        serde_json::from_slice::<serde_json::Value>(&self.body)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
    }

    /// Is the server asking us to retry with a (new) nonce?
    ///
    /// A `dpop-nonce` header alone is not the signal: PDS implementations
    /// attach one to *every* error response, so keying off it meant a 400 for a
    /// malformed record was read as a nonce challenge and the write was resent
    /// unchanged.
    ///
    /// The header is necessary but not sufficient. Beyond it we accept an
    /// explicit `use_dpop_nonce` in `WWW-Authenticate` or the body — and, for a
    /// 401 only, a response that names no error we recognise. That last case is
    /// deliberate leniency: a 401 with a fresh nonce and no other explanation is
    /// what an unadorned nonce challenge looks like, and refusing to retry it
    /// would break a PDS that is merely terse. A 400 gets no such benefit,
    /// which is what fixes the resent-write bug.
    fn wants_dpop_nonce(&self) -> bool {
        if self.dpop_nonce().is_none() {
            return false;
        }

        let explicit = self
            .www_authenticate()
            .is_some_and(|h| h.contains("use_dpop_nonce"))
            || self.body_error().as_deref() == Some("use_dpop_nonce");

        if explicit {
            return true;
        }

        self.status == reqwest::StatusCode::UNAUTHORIZED && self.body_error().is_none()
    }

    /// Would refreshing the access token plausibly help?
    ///
    /// Only an invalid or expired *token* is worth a refresh. Treating every
    /// 401 as expiry — which is what `is_expired_token` used to do — meant an
    /// insufficient-scope or revoked-grant response triggered a refresh, and a
    /// refresh that came back `invalid_grant` **deleted the user's session**.
    /// An unrelated 401 could therefore destroy a working login.
    ///
    /// Both spellings are accepted: OAuth uses `invalid_token` in
    /// `WWW-Authenticate`, while an atproto XRPC error body names
    /// `InvalidToken` or `ExpiredToken`.
    fn indicates_invalid_token(&self) -> bool {
        if self.status != reqwest::StatusCode::UNAUTHORIZED {
            return false;
        }
        if self
            .www_authenticate()
            .is_some_and(|h| h.contains("invalid_token"))
        {
            return true;
        }
        matches!(
            self.body_error().as_deref(),
            Some("invalid_token" | "InvalidToken" | "ExpiredToken")
        )
    }

    /// Rebuild a `reqwest::Response` for the caller. Status, headers and body
    /// are preserved so downstream relaying is unchanged.
    fn into_response(self) -> Result<reqwest::Response, AppError> {
        let mut builder = atrium_xrpc::http::Response::builder().status(self.status);
        if let Some(headers) = builder.headers_mut() {
            *headers = self.headers;
        }
        let resp = builder
            .body(self.body)
            .map_err(|e| AppError::Internal(format!("failed to rebuild PDS response: {e}")))?;
        Ok(reqwest::Response::from(resp))
    }
}

/// How many times a single request may be re-sent to negotiate a nonce.
///
/// Nonces rotate, so one retry is not always enough: a server may hand back a
/// fresh nonce with the retried request's own rejection. The old code tried
/// exactly once and then fell through to a token refresh, which could not help.
const MAX_NONCE_ATTEMPTS: usize = 3;

/// Make an authenticated request, handling DPoP nonce negotiation and token
/// refresh.
#[allow(clippy::too_many_arguments)]
async fn dpop_request_with_retry(
    http: &reqwest::Client,
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    encryption_key: &[u8; 32],
    oauth_registry: &Arc<OAuthClientRegistry>,
    creds: &mut DpopCredentials,
    http_method: &str,
    target_url: &str,
    request_builder: impl Fn(&reqwest::Client, &str, &str) -> reqwest::RequestBuilder,
) -> Result<reqwest::Response, AppError> {
    let mut nonce: Option<String> = None;

    for attempt in 0..MAX_NONCE_ATTEMPTS {
        let buffered = send_once(
            http,
            creds,
            http_method,
            target_url,
            nonce.as_deref(),
            &request_builder,
        )
        .await?;

        if buffered.wants_dpop_nonce() {
            let fresh = buffered.dpop_nonce();
            // Only worth retrying if the nonce actually changed; otherwise the
            // server is rejecting the one it just gave us and looping would
            // send the same request repeatedly.
            if fresh.is_some() && fresh != nonce && attempt + 1 < MAX_NONCE_ATTEMPTS {
                nonce = fresh;
                continue;
            }
        }

        if buffered.indicates_invalid_token() {
            return retry_after_refresh(
                http,
                pool,
                backend,
                encryption_key,
                oauth_registry,
                creds,
                http_method,
                target_url,
                nonce.as_deref(),
                &request_builder,
            )
            .await;
        }

        return buffered.into_response();
    }

    Err(AppError::Auth(
        "PDS kept requesting a new DPoP nonce".into(),
    ))
}

/// Send one attempt with a freshly generated proof.
async fn send_once(
    http: &reqwest::Client,
    creds: &DpopCredentials,
    http_method: &str,
    target_url: &str,
    nonce: Option<&str>,
    request_builder: &impl Fn(&reqwest::Client, &str, &str) -> reqwest::RequestBuilder,
) -> Result<BufferedResponse, AppError> {
    let proof = generate_dpop_proof(
        &creds.private_jwk,
        http_method,
        target_url,
        &creds.session.access_token,
        nonce,
    )?;

    let resp = request_builder(http, &creds.session.access_token, &proof)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("PDS request failed: {e}")))?;

    BufferedResponse::read(resp).await
}

/// Refresh the access token and retry the PDS request.
///
/// If the refresh fails with `invalid_grant`, re-reads the session from the
/// database — a concurrent request may have already refreshed the token.
#[allow(clippy::too_many_arguments)]
async fn retry_after_refresh(
    http: &reqwest::Client,
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    encryption_key: &[u8; 32],
    oauth_registry: &Arc<OAuthClientRegistry>,
    creds: &mut DpopCredentials,
    http_method: &str,
    target_url: &str,
    nonce: Option<&str>,
    request_builder: &impl Fn(&reqwest::Client, &str, &str) -> reqwest::RequestBuilder,
) -> Result<reqwest::Response, AppError> {
    let stale_access_token = creds.session.access_token.clone();

    if let Err(e) =
        refresh_access_token(http, pool, backend, encryption_key, oauth_registry, creds).await
    {
        // If the refresh token was rejected, check whether a concurrent request
        // already refreshed the session. Re-read from the database and compare
        // the access token — if it changed, another refresh succeeded.
        if is_invalid_grant_error(&e) {
            let fresh_session = super::sessions::get_dpop_session(
                pool,
                backend,
                encryption_key,
                &creds.session.api_client_id,
                &creds.session.user_did,
                &creds.session.dpop_key_id,
            )
            .await?;

            if fresh_session.access_token != stale_access_token {
                tracing::info!(
                    user_did = %creds.session.user_did,
                    api_client_id = %creds.session.api_client_id,
                    "concurrent refresh detected, using updated token"
                );
                creds.session = fresh_session;
            } else {
                // Session is unrecoverable — clean it up so future requests
                // fail fast instead of repeating the same doomed refresh.
                tracing::warn!(
                    user_did = %creds.session.user_did,
                    api_client_id = %creds.session.api_client_id,
                    "refresh token permanently invalid, deleting broken session"
                );
                if let Err(del_err) = super::sessions::delete_dpop_session(
                    pool,
                    backend,
                    &creds.session.api_client_id,
                    &creds.session.user_did,
                    &creds.session.dpop_key_id,
                )
                .await
                {
                    tracing::error!(%del_err, "failed to delete broken DPoP session");
                }
                return Err(AppError::Auth(
                    "session expired, please re-authenticate".into(),
                ));
            }
        } else {
            return Err(e);
        }
    }

    // The refreshed token needs its own nonce negotiation: the nonce is bound
    // to the proof, not the token, but a server may rotate it on the way past.
    let mut nonce = nonce.map(str::to_string);

    for attempt in 0..MAX_NONCE_ATTEMPTS {
        let buffered = send_once(
            http,
            creds,
            http_method,
            target_url,
            nonce.as_deref(),
            request_builder,
        )
        .await?;

        if buffered.wants_dpop_nonce() {
            let fresh = buffered.dpop_nonce();
            if fresh.is_some() && fresh != nonce && attempt + 1 < MAX_NONCE_ATTEMPTS {
                nonce = fresh;
                continue;
            }
        }

        return buffered.into_response();
    }

    Err(AppError::Auth(
        "PDS kept requesting a new DPoP nonce after token refresh".into(),
    ))
}

fn is_invalid_grant_error(e: &AppError) -> bool {
    matches!(e, AppError::Auth(msg) if msg.contains("invalid_grant"))
}

/// Make an authenticated POST to a PDS XRPC endpoint using a DPoP session.
#[allow(clippy::too_many_arguments)]
pub async fn dpop_pds_post(
    http: &reqwest::Client,
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    encryption_key: &[u8; 32],
    oauth_registry: &Arc<OAuthClientRegistry>,
    plc_url: &str,
    api_client_id: &str,
    user_did: &str,
    dpop_key_id: &str,
    xrpc_method: &str,
    body: &serde_json::Value,
) -> Result<reqwest::Response, AppError> {
    let mut creds = resolve_credentials(
        http,
        pool,
        backend,
        encryption_key,
        plc_url,
        api_client_id,
        user_did,
        dpop_key_id,
    )
    .await?;

    let target_url = format!(
        "{}/xrpc/{}",
        creds.pds_url.trim_end_matches('/'),
        xrpc_method
    );

    let body = body.clone();
    let target = target_url.clone();
    dpop_request_with_retry(
        http,
        pool,
        backend,
        encryption_key,
        oauth_registry,
        &mut creds,
        "POST",
        &target_url,
        |http, access_token, proof| {
            http.post(&target)
                .header("Authorization", format!("DPoP {access_token}"))
                .header("DPoP", proof)
                .header("Content-Type", "application/json")
                .json(&body)
        },
    )
    .await
}

/// As [`dpop_pds_post`], additionally forwarding `extra_headers` verbatim.
///
/// The headers are attached on every attempt, including nonce retries and the
/// retry after a token refresh — dropping them on a retry would send a
/// materially different request than the one that was retried.
#[allow(clippy::too_many_arguments)]
pub async fn dpop_pds_post_with_headers(
    http: &reqwest::Client,
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    encryption_key: &[u8; 32],
    oauth_registry: &Arc<OAuthClientRegistry>,
    plc_url: &str,
    api_client_id: &str,
    user_did: &str,
    dpop_key_id: &str,
    xrpc_method: &str,
    body: &serde_json::Value,
    extra_headers: &[(String, String)],
) -> Result<reqwest::Response, AppError> {
    let mut creds = resolve_credentials(
        http,
        pool,
        backend,
        encryption_key,
        plc_url,
        api_client_id,
        user_did,
        dpop_key_id,
    )
    .await?;

    let target_url = format!(
        "{}/xrpc/{}",
        creds.pds_url.trim_end_matches('/'),
        xrpc_method
    );

    let body = body.clone();
    let target = target_url.clone();
    let extra: Vec<(String, String)> = extra_headers.to_vec();
    dpop_request_with_retry(
        http,
        pool,
        backend,
        encryption_key,
        oauth_registry,
        &mut creds,
        "POST",
        &target_url,
        move |http, access_token, proof| {
            let mut request = http
                .post(&target)
                .header("Authorization", format!("DPoP {access_token}"))
                .header("DPoP", proof)
                .header("Content-Type", "application/json");
            for (name, value) in &extra {
                request = request.header(name, value);
            }
            request.json(&body)
        },
    )
    .await
}

/// Make an authenticated GET to a PDS XRPC endpoint using a DPoP session.
///
/// `query` is the raw query string, forwarded verbatim so repeated parameters
/// survive — re-encoding through a map would collapse `?a=1&a=2`.
#[allow(clippy::too_many_arguments)]
pub async fn dpop_pds_get(
    http: &reqwest::Client,
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    encryption_key: &[u8; 32],
    oauth_registry: &Arc<OAuthClientRegistry>,
    plc_url: &str,
    api_client_id: &str,
    user_did: &str,
    dpop_key_id: &str,
    xrpc_method: &str,
    query: &str,
    extra_headers: &[(String, String)],
) -> Result<reqwest::Response, AppError> {
    let mut creds = resolve_credentials(
        http,
        pool,
        backend,
        encryption_key,
        plc_url,
        api_client_id,
        user_did,
        dpop_key_id,
    )
    .await?;

    // The proof's `htu` is the target URI **without** query or fragment
    // (RFC 9449 §4.2), so the proof is generated against this bare URL while
    // the request itself carries the query. For POST the two were always the
    // same, which is why this distinction has not come up before.
    let proof_url = format!(
        "{}/xrpc/{}",
        creds.pds_url.trim_end_matches('/'),
        xrpc_method
    );

    let mut request_url = proof_url.clone();
    if !query.is_empty() {
        request_url.push('?');
        request_url.push_str(query);
    }

    let extra: Vec<(String, String)> = extra_headers.to_vec();
    dpop_request_with_retry(
        http,
        pool,
        backend,
        encryption_key,
        oauth_registry,
        &mut creds,
        "GET",
        &proof_url,
        move |http, access_token, proof| {
            let mut request = http
                .get(&request_url)
                .header("Authorization", format!("DPoP {access_token}"))
                .header("DPoP", proof);
            for (name, value) in &extra {
                request = request.header(name, value);
            }
            request
        },
    )
    .await
}

/// Make an authenticated blob upload to a PDS using a DPoP session.
#[allow(clippy::too_many_arguments)]
pub async fn dpop_pds_post_blob(
    http: &reqwest::Client,
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    encryption_key: &[u8; 32],
    oauth_registry: &Arc<OAuthClientRegistry>,
    plc_url: &str,
    api_client_id: &str,
    user_did: &str,
    dpop_key_id: &str,
    content_type: &str,
    blob: bytes::Bytes,
) -> Result<reqwest::Response, AppError> {
    let mut creds = resolve_credentials(
        http,
        pool,
        backend,
        encryption_key,
        plc_url,
        api_client_id,
        user_did,
        dpop_key_id,
    )
    .await?;

    let target_url = format!(
        "{}/xrpc/com.atproto.repo.uploadBlob",
        creds.pds_url.trim_end_matches('/')
    );

    let content_type = content_type.to_string();
    let target = target_url.clone();
    dpop_request_with_retry(
        http,
        pool,
        backend,
        encryption_key,
        oauth_registry,
        &mut creds,
        "POST",
        &target_url,
        |http, access_token, proof| {
            http.post(&target)
                .header("Authorization", format!("DPoP {access_token}"))
                .header("DPoP", proof)
                .header("Content-Type", &content_type)
                .body(blob.clone())
        },
    )
    .await
}

/// Build the token-endpoint form for a refresh, with client authentication
/// when the client is confidential.
///
/// Used at both the initial attempt and the nonce-retry call site, so the two
/// cannot drift: a confidential client's assertion must be present either
/// way, or a nonce retry would silently downgrade to an unauthenticated
/// refresh.
fn build_refresh_form(
    refresh_token: &str,
    client_id: &str,
    assertion: Option<String>,
) -> Vec<(&'static str, String)> {
    let mut form = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.to_string()),
        ("client_id", client_id.to_string()),
    ];
    if let Some(assertion) = assertion {
        form.push((
            "client_assertion_type",
            super::client_assertion::CLIENT_ASSERTION_TYPE.to_string(),
        ));
        form.push(("client_assertion", assertion));
    }
    form
}

/// Refresh an expired access token using the session's refresh_token.
///
/// Discovers the token endpoint from the issuer's OAuth metadata, sends a
/// `grant_type=refresh_token` request with a DPoP proof, and updates the
/// stored session with the new tokens.
///
/// Unlike PDS resource endpoints, the token endpoint response body is read
/// before deciding whether a `dpop-nonce` header indicates a retry — this
/// avoids misinterpreting `invalid_grant` as a nonce requirement when the
/// PDS includes `dpop-nonce` on all error responses.
async fn refresh_access_token(
    http: &reqwest::Client,
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    encryption_key: &[u8; 32],
    oauth_registry: &Arc<OAuthClientRegistry>,
    creds: &mut DpopCredentials,
) -> Result<(), AppError> {
    let refresh_token = creds
        .session
        .refresh_token
        .as_deref()
        .ok_or_else(|| AppError::Auth("token expired and no refresh_token available".into()))?;

    let issuer = creds
        .session
        .issuer
        .as_deref()
        .ok_or_else(|| AppError::Auth("token expired and no issuer URL stored".into()))?;

    let token_endpoint = discover_token_endpoint(http, issuer).await?;

    // Get the resolved client_id from the OAuth registry. For loopback clients
    // this returns `http://localhost?scope=...` which auth servers handle inline,
    // rather than the `client_id_url` from the DB which they'd try to fetch.
    let client_id_url = lookup_client_id_url(pool, backend, &creds.session.api_client_id).await?;
    let client_id = oauth_registry
        .get_resolved_client_id(&client_id_url)
        .unwrap_or(client_id_url);

    let keys = super::client_keys::load_keys(
        pool,
        backend,
        Some(encryption_key),
        &creds.session.api_client_id,
    )
    .await?;
    let resolved =
        super::client_keys::resolve_signing_key(&keys, creds.session.signing_kid.as_deref())?;

    if creds.session.signing_kid.is_none()
        && let Some(key) = resolved
        && let Err(e) = crate::auth::oauth_store::stamp_signing_kid_if_unset(
            pool,
            backend,
            "happyview_dpop_sessions",
            "id",
            &creds.session.id,
            &key.kid,
        )
        .await
    {
        tracing::warn!(
            error = %e,
            session_id = %creds.session.id,
            "failed to lazily stamp DPoP session signing_kid"
        );
    }

    let assertion = match resolved {
        Some(key) => Some(super::client_assertion::build(
            &key.private_jwk,
            &key.kid,
            &client_id,
            issuer,
        )?),
        None => None,
    };

    let proof = generate_dpop_proof_no_ath(&creds.private_jwk, "POST", &token_endpoint, None)?;

    let resp = http
        .post(&token_endpoint)
        .header("DPoP", &proof)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&build_refresh_form(
            refresh_token,
            &client_id,
            assertion.clone(),
        ))
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("token refresh request failed: {e}")))?;

    let status = resp.status();
    let dpop_nonce = resp
        .headers()
        .get("dpop-nonce")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let body = resp.text().await.unwrap_or_default();

    if status.is_success() {
        return apply_refresh_response(pool, backend, encryption_key, creds, &body).await;
    }

    // Only retry with a nonce if the error is actually `use_dpop_nonce`.
    // PDS implementations include `dpop-nonce` on all responses, so checking
    // the header alone would misinterpret `invalid_grant` as a nonce issue.
    if let Some(nonce) = dpop_nonce
        && is_use_dpop_nonce_error(&body)
    {
        let proof =
            generate_dpop_proof_no_ath(&creds.private_jwk, "POST", &token_endpoint, Some(&nonce))?;

        let retry_resp = http
            .post(&token_endpoint)
            .header("DPoP", &proof)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&build_refresh_form(refresh_token, &client_id, assertion))
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("token refresh request failed: {e}")))?;

        let retry_status = retry_resp.status();
        let retry_body = retry_resp.text().await.unwrap_or_default();

        if !retry_status.is_success() {
            return Err(AppError::Auth(format!(
                "token refresh failed ({retry_status}): {retry_body}"
            )));
        }

        return apply_refresh_response(pool, backend, encryption_key, creds, &retry_body).await;
    }

    Err(AppError::Auth(format!(
        "token refresh failed ({status}): {body}"
    )))
}

/// Check if a token endpoint error body indicates a `use_dpop_nonce` error.
fn is_use_dpop_nonce_error(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"].as_str().map(|s| s == "use_dpop_nonce"))
        .unwrap_or(false)
}

/// Parse a successful token refresh response body and update the stored session.
async fn apply_refresh_response(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    encryption_key: &[u8; 32],
    creds: &mut DpopCredentials,
    body: &str,
) -> Result<(), AppError> {
    let token_resp: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| AppError::Internal(format!("invalid token refresh response: {e}")))?;

    let new_access_token = token_resp["access_token"]
        .as_str()
        .ok_or_else(|| AppError::Internal("refresh response missing access_token".into()))?;

    let new_refresh_token = token_resp["refresh_token"].as_str();

    let expires_in = token_resp["expires_in"].as_u64();
    let new_expires_at = expires_in
        .map(|secs| (chrono::Utc::now() + chrono::Duration::seconds(secs as i64)).to_rfc3339());

    super::sessions::store_dpop_session(
        pool,
        backend,
        encryption_key,
        &creds.session.id,
        &creds.session.api_client_id,
        &creds.session.dpop_key_id,
        &creds.session.user_did,
        new_access_token,
        new_refresh_token.or(creds.session.refresh_token.as_deref()),
        new_expires_at
            .as_deref()
            .or(creds.session.token_expires_at.as_deref()),
        &creds.session.scopes,
        creds.session.pds_url.as_deref(),
        creds.session.issuer.as_deref(),
        creds.session.signing_kid.as_deref(),
    )
    .await?;

    // Update the in-memory credentials
    creds.session.access_token = new_access_token.to_string();
    if let Some(rt) = new_refresh_token {
        creds.session.refresh_token = Some(rt.to_string());
    }
    if let Some(ref exp) = new_expires_at {
        creds.session.token_expires_at = Some(exp.clone());
    }

    tracing::info!(
        user_did = %creds.session.user_did,
        api_client_id = %creds.session.api_client_id,
        "refreshed DPoP access token"
    );

    Ok(())
}

/// Discover the token endpoint from an OAuth authorization server's metadata.
async fn discover_token_endpoint(http: &reqwest::Client, issuer: &str) -> Result<String, AppError> {
    let metadata_url = format!(
        "{}/.well-known/oauth-authorization-server",
        issuer.trim_end_matches('/')
    );

    let resp =
        http.get(&metadata_url).send().await.map_err(|e| {
            AppError::Internal(format!("failed to fetch auth server metadata: {e}"))
        })?;

    if !resp.status().is_success() {
        return Err(AppError::Internal(format!(
            "auth server metadata returned {}",
            resp.status()
        )));
    }

    let metadata: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("invalid auth server metadata: {e}")))?;

    metadata["token_endpoint"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| AppError::Internal("auth server metadata missing token_endpoint".into()))
}

/// Look up the client_id_url for an API client by its internal ID.
pub(crate) async fn lookup_client_id_url(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    api_client_id: &str,
) -> Result<String, AppError> {
    let sql = crate::db::adapt_sql(
        "SELECT client_id_url FROM happyview_api_clients WHERE id = ?",
        backend,
    );
    let row: Option<(String,)> = crate::db::query_as(&sql)
        .bind(api_client_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up API client: {e}")))?;

    row.map(|(url,)| url)
        .ok_or_else(|| AppError::Internal("API client not found".into()))
}

/// Generate a DPoP proof JWT for a PDS request.
pub fn generate_dpop_proof(
    private_jwk: &serde_json::Value,
    method: &str,
    url: &str,
    access_token: &str,
    nonce: Option<&str>,
) -> Result<String, AppError> {
    let ath = URL_SAFE_NO_PAD.encode(Sha256::digest(access_token.as_bytes()));
    generate_dpop_proof_inner(private_jwk, method, url, Some(&ath), nonce)
}

/// Generate a DPoP proof JWT without an `ath` claim (for token endpoint requests).
fn generate_dpop_proof_no_ath(
    private_jwk: &serde_json::Value,
    method: &str,
    url: &str,
    nonce: Option<&str>,
) -> Result<String, AppError> {
    generate_dpop_proof_inner(private_jwk, method, url, None, nonce)
}

fn generate_dpop_proof_inner(
    private_jwk: &serde_json::Value,
    method: &str,
    url: &str,
    ath: Option<&str>,
    nonce: Option<&str>,
) -> Result<String, AppError> {
    let d_b64 = private_jwk["d"]
        .as_str()
        .ok_or_else(|| AppError::Internal("DPoP key missing d parameter".into()))?;
    let x_b64 = private_jwk["x"]
        .as_str()
        .ok_or_else(|| AppError::Internal("DPoP key missing x parameter".into()))?;
    let y_b64 = private_jwk["y"]
        .as_str()
        .ok_or_else(|| AppError::Internal("DPoP key missing y parameter".into()))?;

    let d_bytes = URL_SAFE_NO_PAD
        .decode(d_b64)
        .map_err(|_| AppError::Internal("invalid DPoP key d parameter".into()))?;

    let signing_key = SigningKey::from_slice(&d_bytes[..])
        .map_err(|e| AppError::Internal(format!("invalid DPoP signing key: {e}")))?;

    let public_jwk = serde_json::json!({
        "kty": "EC",
        "crv": "P-256",
        "x": x_b64,
        "y": y_b64,
    });

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let header = serde_json::json!({
        "alg": "ES256",
        "typ": "dpop+jwt",
        "jwk": public_jwk,
    });

    let mut payload = serde_json::json!({
        "htm": method,
        "htu": url,
        "iat": now,
        "jti": format!("{:x}", rand::random::<u64>()),
    });
    if let Some(ath) = ath {
        payload["ath"] = serde_json::json!(ath);
    }
    if let Some(nonce) = nonce {
        payload["nonce"] = serde_json::json!(nonce);
    }

    let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
    let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());

    let message = format!("{}.{}", header_b64, payload_b64);
    let signature: p256::ecdsa::Signature = signing_key.sign(message.as_bytes());
    let sig_b64 = URL_SAFE_NO_PAD.encode(signature.to_bytes());

    Ok(format!("{}.{}.{}", header_b64, payload_b64, sig_b64))
}

/// Verify that a DPoP access token belongs to `did` by calling
/// `com.atproto.server.getSession` on the DID's own PDS.
///
/// This is the trust anchor for session registration: the caller supplies a
/// `did` and an `access_token`, but nothing proves the token was issued for
/// that DID. We resolve the PDS **authoritatively from the DID document** (never
/// from a client-supplied PDS URL, which an attacker could point at a server
/// that lies), then present the token with a DPoP proof signed by the
/// provisioned key the token is bound to. The PDS reports which DID the token
/// actually belongs to; the caller compares it against the claimed `did`.
///
/// Returns the DID the PDS reports for the token, or an auth error if the token
/// is rejected.
pub async fn verify_access_token_did(
    http: &reqwest::Client,
    plc_url: &str,
    private_jwk: &serde_json::Value,
    did: &str,
    access_token: &str,
) -> Result<String, AppError> {
    let pds_url = resolve_pds_from_did(http, plc_url, did).await?;
    let target_url = format!(
        "{}/xrpc/com.atproto.server.getSession",
        pds_url.trim_end_matches('/')
    );

    // The PDS may demand a DPoP nonce, and may rotate it. The previous
    // `nonce.is_none()` guard meant only the *first* challenge was honoured, so
    // a rotation on the retry could never be satisfied.
    let mut nonce: Option<String> = None;

    for attempt in 0..MAX_NONCE_ATTEMPTS {
        let proof = generate_dpop_proof(
            private_jwk,
            "GET",
            &target_url,
            access_token,
            nonce.as_deref(),
        )?;
        let resp = http
            .get(&target_url)
            .header("Authorization", format!("DPoP {access_token}"))
            .header("DPoP", proof)
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("getSession request failed: {e}")))?;

        let status = resp.status();
        let buffered = BufferedResponse::read(resp).await?;

        if status.is_success() {
            let body: serde_json::Value = serde_json::from_slice(&buffered.body)
                .map_err(|e| AppError::Internal(format!("invalid getSession response: {e}")))?;
            return body["did"]
                .as_str()
                .map(|s| s.to_string())
                .ok_or_else(|| AppError::Auth("getSession response missing did".into()));
        }

        if buffered.wants_dpop_nonce() {
            let fresh = buffered.dpop_nonce();
            if fresh.is_some() && fresh != nonce && attempt + 1 < MAX_NONCE_ATTEMPTS {
                nonce = fresh;
                continue;
            }
        }

        return Err(AppError::Auth(format!(
            "access token verification failed ({status})"
        )));
    }

    Err(AppError::Auth("access token verification failed".into()))
}

/// Resolve a user's PDS URL from their DID document.
async fn resolve_pds_from_did(
    http: &reqwest::Client,
    plc_url: &str,
    did: &str,
) -> Result<String, AppError> {
    let url = if did.starts_with("did:plc:") {
        format!("{}/{}", plc_url.trim_end_matches('/'), did)
    } else if did.starts_with("did:web:") {
        let host = did.strip_prefix("did:web:").unwrap();
        format!("https://{}/.well-known/did.json", host)
    } else {
        return Err(AppError::Internal(format!("unsupported DID method: {did}")));
    };

    let resp = http
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("failed to resolve DID: {e}")))?;

    let doc: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("failed to parse DID document: {e}")))?;

    let services = doc["service"]
        .as_array()
        .ok_or_else(|| AppError::Internal("DID document missing service array".into()))?;

    for service in services {
        let id = service["id"].as_str().unwrap_or("");
        if (id == "#atproto_pds" || id.ends_with("#atproto_pds"))
            && let Some(endpoint) = service["serviceEndpoint"].as_str()
        {
            return Ok(endpoint.to_string());
        }
    }

    Err(AppError::Internal(format!(
        "no #atproto_pds service found in DID document for {did}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_form_is_unauthenticated_for_a_public_client() {
        let form = build_refresh_form("tok", "https://app.example.com/meta.json", None);
        assert_eq!(form.len(), 3);
        assert!(form.iter().all(|(k, _)| *k != "client_assertion"));
    }

    #[test]
    fn refresh_form_carries_the_assertion_for_a_confidential_client() {
        let form = build_refresh_form(
            "tok",
            "https://app.example.com/meta.json",
            Some("signed.jwt.here".to_string()),
        );
        let get = |name: &str| {
            form.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(get("grant_type"), "refresh_token");
        assert_eq!(get("client_assertion"), "signed.jwt.here");
        assert_eq!(
            get("client_assertion_type"),
            "urn:ietf:params:oauth:client-assertion-type:jwt-bearer"
        );
    }

    #[test]
    fn generate_dpop_proof_produces_valid_jwt() {
        let keypair = super::super::keys::generate_dpop_keypair().unwrap();

        let proof = generate_dpop_proof(
            &keypair.private_jwk,
            "POST",
            "https://pds.example.com/xrpc/com.atproto.repo.createRecord",
            "test-access-token",
            None,
        )
        .unwrap();

        let parts: Vec<&str> = proof.split('.').collect();
        assert_eq!(parts.len(), 3);

        let header_bytes = URL_SAFE_NO_PAD.decode(parts[0]).unwrap();
        let header: serde_json::Value = serde_json::from_slice(&header_bytes).unwrap();
        assert_eq!(header["alg"], "ES256");
        assert_eq!(header["typ"], "dpop+jwt");
        assert_eq!(header["jwk"]["kty"], "EC");

        let payload_bytes = URL_SAFE_NO_PAD.decode(parts[1]).unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&payload_bytes).unwrap();
        assert_eq!(payload["htm"], "POST");
        assert_eq!(
            payload["htu"],
            "https://pds.example.com/xrpc/com.atproto.repo.createRecord"
        );
        assert!(payload["iat"].is_number());
        assert!(payload["ath"].is_string());
        assert!(payload["jti"].is_string());
    }

    #[test]
    fn generate_dpop_proof_includes_nonce() {
        let keypair = super::super::keys::generate_dpop_keypair().unwrap();

        let proof = generate_dpop_proof(
            &keypair.private_jwk,
            "POST",
            "https://pds.example.com/xrpc/test",
            "token",
            Some("server-nonce-123"),
        )
        .unwrap();

        let parts: Vec<&str> = proof.split('.').collect();
        let payload_bytes = URL_SAFE_NO_PAD.decode(parts[1]).unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&payload_bytes).unwrap();
        assert_eq!(payload["nonce"], "server-nonce-123");
    }

    #[test]
    fn generate_dpop_proof_no_ath_omits_ath() {
        let keypair = super::super::keys::generate_dpop_keypair().unwrap();

        let proof = generate_dpop_proof_no_ath(
            &keypair.private_jwk,
            "POST",
            "https://auth.example.com/oauth/token",
            None,
        )
        .unwrap();

        let parts: Vec<&str> = proof.split('.').collect();
        let payload_bytes = URL_SAFE_NO_PAD.decode(parts[1]).unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&payload_bytes).unwrap();
        assert!(payload.get("ath").is_none());
        assert!(payload["htm"].is_string());
        assert!(payload["htu"].is_string());
    }

    #[test]
    fn generated_proof_validates_against_own_key() {
        let keypair = super::super::keys::generate_dpop_keypair().unwrap();
        let url = "https://pds.example.com/xrpc/test.method";
        let token = "my-access-token";

        let proof = generate_dpop_proof(&keypair.private_jwk, "POST", url, token, None).unwrap();

        let result = super::super::dpop_proof::validate_dpop_proof(
            &proof,
            "POST",
            url,
            token,
            &keypair.thumbprint,
        );
        assert!(result.is_ok(), "validation failed: {:?}", result.err());
    }

    fn buffered(status: u16, headers: &[(&str, &str)], body: &str) -> BufferedResponse {
        let mut map = reqwest::header::HeaderMap::new();
        for (name, value) in headers {
            map.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                reqwest::header::HeaderValue::from_str(value).unwrap(),
            );
        }
        BufferedResponse {
            status: reqwest::StatusCode::from_u16(status).unwrap(),
            headers: map,
            body: bytes::Bytes::from(body.to_string()),
        }
    }

    // -----------------------------------------------------------------------
    // nonce detection
    // -----------------------------------------------------------------------

    #[test]
    fn nonce_wanted_when_www_authenticate_says_so() {
        let r = buffered(
            401,
            &[
                ("dpop-nonce", "abc"),
                ("www-authenticate", r#"DPoP error="use_dpop_nonce""#),
            ],
            "",
        );
        assert!(r.wants_dpop_nonce());
    }

    #[test]
    fn nonce_wanted_when_body_says_so() {
        let r = buffered(
            400,
            &[("dpop-nonce", "abc")],
            r#"{"error":"use_dpop_nonce"}"#,
        );
        assert!(r.wants_dpop_nonce());
    }

    /// The bug this fixes: PDS implementations attach `dpop-nonce` to every
    /// error response, so a validation failure looked like a nonce challenge
    /// and the write was resent unchanged.
    #[test]
    fn a_validation_error_carrying_a_nonce_is_not_a_nonce_challenge() {
        let r = buffered(
            400,
            &[("dpop-nonce", "abc")],
            r#"{"error":"InvalidRequest","message":"Invalid record"}"#,
        );
        assert!(!r.wants_dpop_nonce());
    }

    /// Deliberate leniency: a bare 401 with a fresh nonce and no other
    /// explanation is what a terse nonce challenge looks like, and refusing it
    /// would break a PDS that simply does not elaborate.
    #[test]
    fn a_bare_401_with_a_nonce_is_treated_as_a_challenge() {
        let r = buffered(401, &[("dpop-nonce", "abc")], "");
        assert!(r.wants_dpop_nonce());
    }

    #[test]
    fn a_401_naming_another_error_is_not_a_nonce_challenge() {
        let r = buffered(401, &[("dpop-nonce", "abc")], r#"{"error":"InvalidToken"}"#);
        assert!(!r.wants_dpop_nonce());
    }

    #[test]
    fn no_nonce_header_means_no_challenge() {
        let r = buffered(401, &[], r#"{"error":"use_dpop_nonce"}"#);
        assert!(!r.wants_dpop_nonce());
    }

    // -----------------------------------------------------------------------
    // token-expiry detection
    // -----------------------------------------------------------------------

    #[test]
    fn invalid_token_detected_from_www_authenticate() {
        let r = buffered(
            401,
            &[("www-authenticate", r#"DPoP error="invalid_token""#)],
            "",
        );
        assert!(r.indicates_invalid_token());
    }

    #[test]
    fn invalid_token_detected_from_atproto_error_names() {
        for name in ["InvalidToken", "ExpiredToken", "invalid_token"] {
            let body = format!(r#"{{"error":"{name}"}}"#);
            assert!(
                buffered(401, &[], &body).indicates_invalid_token(),
                "{name} should be treated as a token problem"
            );
        }
    }

    /// The bug this fixes: every 401 was treated as expiry, so an unrelated
    /// one triggered a refresh — and a refresh returning `invalid_grant`
    /// deletes the session. An authorization failure could destroy a login.
    #[test]
    fn an_unrelated_401_does_not_look_like_an_expired_token() {
        for body in [
            r#"{"error":"AuthMissing","message":"Authentication Required"}"#,
            r#"{"error":"InsufficientScope"}"#,
            r#"{"error":"AccountTakedown"}"#,
        ] {
            assert!(
                !buffered(401, &[], body).indicates_invalid_token(),
                "{body} should not trigger a token refresh"
            );
        }
    }

    #[test]
    fn a_non_401_is_never_an_expired_token() {
        assert!(
            !buffered(400, &[], r#"{"error":"ExpiredToken"}"#).indicates_invalid_token(),
            "only a 401 can mean the token is the problem"
        );
        assert!(!buffered(200, &[], "{}").indicates_invalid_token());
    }

    #[test]
    fn buffered_response_round_trips_status_and_body() {
        let r = buffered(409, &[("content-type", "application/json")], r#"{"a":1}"#);
        let resp = r.into_response().unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );
    }

    #[test]
    fn is_use_dpop_nonce_error_detects_nonce_error() {
        assert!(is_use_dpop_nonce_error(r#"{"error":"use_dpop_nonce"}"#));
    }

    #[test]
    fn is_use_dpop_nonce_error_rejects_invalid_grant() {
        assert!(!is_use_dpop_nonce_error(
            r#"{"error":"invalid_grant","error_description":"Invalid refresh token"}"#
        ));
    }

    #[test]
    fn is_use_dpop_nonce_error_rejects_garbage() {
        assert!(!is_use_dpop_nonce_error("not json"));
        assert!(!is_use_dpop_nonce_error(""));
        assert!(!is_use_dpop_nonce_error("{}"));
    }

    #[test]
    fn is_invalid_grant_error_matches() {
        let e = AppError::Auth("token refresh failed (400 Bad Request): {\"error\":\"invalid_grant\",\"error_description\":\"Invalid refresh token\"}".into());
        assert!(is_invalid_grant_error(&e));
    }

    #[test]
    fn is_invalid_grant_error_ignores_other_errors() {
        assert!(!is_invalid_grant_error(&AppError::Auth(
            "token expired".into()
        )));
        assert!(!is_invalid_grant_error(&AppError::Internal(
            "something broke".into()
        )));
    }
}

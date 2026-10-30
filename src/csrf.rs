//! Cross-site request forgery protection for cookie-authenticated requests.
//!
//! On an https instance the session cookie is `SameSite=None`, so a browser
//! sends it with a request another site starts. CORS keeps that site from
//! reading the response, not the request from running, and a form POST needs
//! no preflight. So an unsafe request carrying the session cookie runs only
//! when the browser says it came from this instance (`Sec-Fetch-Site`), or its
//! `Origin` is one of the instance's domains or an integration origin an admin
//! listed in [`INTEGRATION_ORIGINS_SETTING`]. A request with neither header did
//! not come from a browser, and has no ambient cookie to abuse: a browser that
//! predates `Sec-Fetch-Site` still sends `Origin` on every unsafe request, so
//! "neither" is reachable only from outside one.
//!
//! A listed origin is also trusted by CORS with credentials (`server::cors`),
//! or its JSON requests would fail their preflight and it could not read a
//! response.
//!
//! Bearer, DPoP and API-key requests are untouched: none of them is a
//! credential a browser attaches by itself.

use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::AppState;
use crate::error::AppError;

/// The instance setting listing the origins of integrations that call the
/// instance with the session cookie, separated by commas or whitespace.
pub const INTEGRATION_ORIGINS_SETTING: &str = "cookie_integration_origins";

/// Refuse a cross-site unsafe request that carries the session cookie.
pub async fn guard(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if is_safe(req.method()) || !carries_session_cookie(&req) {
        return next.run(req).await;
    }
    if cookie_request_is_first_party(&state, req.headers()).await {
        return next.run(req).await;
    }

    tracing::warn!(
        method = %req.method(),
        path = req.uri().path(),
        origin = origin_of(req.headers()).unwrap_or(""),
        "refused a cross-site request carrying the session cookie"
    );
    AppError::Forbidden(format!(
        "a cross-site request with the session cookie is refused; an admin can list this \
         integration's origin in the {INTEGRATION_ORIGINS_SETTING} setting"
    ))
    .into_response()
}

/// Whether a request carrying the session cookie may use it: the browser says
/// it came from this instance, or its `Origin` is one of the instance's
/// domains or a listed integration, or it sent neither header and so did not
/// come from a browser. The one rule behind [`guard`] and the cookie identity
/// XRPC accepts (`auth::XrpcClaims`).
pub async fn cookie_request_is_first_party(state: &AppState, headers: &HeaderMap) -> bool {
    let fetch_site = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok());
    match (fetch_site, origin_of(headers)) {
        (Some("same-origin" | "none"), _) => true,
        (None, None) => true,
        (_, Some(origin)) => origin_allowed(state, origin).await,
        (Some(_), None) => false,
    }
}

fn origin_of(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::ORIGIN).and_then(|v| v.to_str().ok())
}

fn is_safe(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn carries_session_cookie(req: &Request) -> bool {
    let prefix = format!("{}=", crate::auth::COOKIE_NAME);
    req.headers()
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .any(|pair| pair.trim_start().starts_with(&prefix))
}

async fn origin_allowed(state: &AppState, origin: &str) -> bool {
    state.domain_cache.is_allowed_origin(origin).await
        || state.integration_origins.contains(state, origin).await
}

/// How long [`IntegrationOrigins`] trusts what it read. A change made through
/// the settings API lands at once; this bounds a change made elsewhere, such
/// as by another instance on the same database.
const INTEGRATION_ORIGINS_TTL: Duration = Duration::from_secs(10);

/// The listed integration origins, read from the setting and kept briefly,
/// because CORS asks on every cross-origin request.
#[derive(Default)]
pub struct IntegrationOrigins {
    cached: tokio::sync::RwLock<Option<(Instant, Arc<Vec<String>>)>>,
}

impl IntegrationOrigins {
    /// Whether `origin` is listed.
    pub async fn contains(&self, state: &AppState, origin: &str) -> bool {
        let origin = normalize(origin);
        self.get(state).await.contains(&origin)
    }

    /// Drop what was read, so the next request reads the setting again.
    pub async fn forget(&self) {
        *self.cached.write().await = None;
    }

    async fn get(&self, state: &AppState) -> Arc<Vec<String>> {
        if let Some((read_at, origins)) = &*self.cached.read().await
            && read_at.elapsed() < INTEGRATION_ORIGINS_TTL
        {
            return origins.clone();
        }
        let value = crate::admin::settings::get_setting(
            &state.db,
            INTEGRATION_ORIGINS_SETTING,
            state.db_backend,
        )
        .await
        .unwrap_or_default();
        let origins = Arc::new(parse_origins(&value).collect::<Vec<_>>());
        *self.cached.write().await = Some((Instant::now(), origins.clone()));
        origins
    }
}

/// The origins in a setting value, normalized for comparison.
pub fn parse_origins(value: &str) -> impl Iterator<Item = String> + '_ {
    value
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(normalize)
}

fn normalize(origin: &str) -> String {
    origin.trim().trim_end_matches('/').to_ascii_lowercase()
}

/// Refuse a setting value whose entries are not origins: a scheme, a host,
/// an optional port, and nothing after it.
pub fn validate_origins(value: &str) -> Result<(), String> {
    for entry in parse_origins(value) {
        let url = reqwest::Url::parse(&entry).map_err(|_| format!("'{entry}' is not an origin"))?;
        let is_origin = matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.origin().ascii_serialization() == entry;
        if !is_origin {
            return Err(format!(
                "'{entry}' is not an origin; give the scheme, host and port only, as in https://app.example"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_parse_from_commas_and_whitespace() {
        let parsed: Vec<String> =
            parse_origins("https://A.example/, http://b.example:8080\nhttps://c.example").collect();
        assert_eq!(
            parsed,
            [
                "https://a.example",
                "http://b.example:8080",
                "https://c.example"
            ]
        );
    }

    #[test]
    fn only_origins_validate() {
        assert!(validate_origins("https://app.example, http://localhost:3000").is_ok());
        assert!(validate_origins("").is_ok());
        for bad in [
            "app.example",
            "https://app.example/path",
            "ftp://app.example",
            "https://app.example?x=1",
            "null",
        ] {
            assert!(validate_origins(bad).is_err(), "{bad}");
        }
    }
}

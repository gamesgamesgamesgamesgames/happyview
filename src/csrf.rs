//! Cross-site request forgery protection for cookie-authenticated requests.
//!
//! On an https instance the session cookie is `SameSite=None`, so a browser
//! sends it with a request another site starts. CORS keeps that site from
//! reading the response, not the request from running, and a form POST needs
//! no preflight. So an unsafe request carrying the session cookie runs only
//! when the browser says it came from this instance (`Sec-Fetch-Site`), or its
//! `Origin` is one of the instance's domains or an integration origin an admin
//! listed in [`INTEGRATION_ORIGINS_SETTING`]. A request with neither header did
//! not come from a browser, and has no ambient cookie to abuse.
//!
//! Bearer, DPoP and API-key requests are untouched: none of them is a
//! credential a browser attaches by itself.

use axum::extract::{Request, State};
use axum::http::{Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

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
    let headers = req.headers();
    let fetch_site = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok());
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let allowed = match (fetch_site, origin.as_deref()) {
        (Some("same-origin" | "none"), _) => true,
        (None, None) => true,
        (_, Some(origin)) => origin_allowed(&state, origin).await,
        (Some(_), None) => false,
    };
    if allowed {
        return next.run(req).await;
    }

    tracing::warn!(
        method = %req.method(),
        path = req.uri().path(),
        origin = origin.as_deref().unwrap_or(""),
        "refused a cross-site request carrying the session cookie"
    );
    AppError::Forbidden(format!(
        "a cross-site request with the session cookie is refused; an admin can list this \
         integration's origin in the {INTEGRATION_ORIGINS_SETTING} setting"
    ))
    .into_response()
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
    if state.domain_cache.is_allowed_origin(origin).await {
        return true;
    }
    let listed = crate::admin::settings::get_setting(
        &state.db,
        INTEGRATION_ORIGINS_SETTING,
        state.db_backend,
    )
    .await
    .unwrap_or_default();
    let origin = normalize(origin);
    parse_origins(&listed).any(|listed| listed == origin)
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

//! Cross-site request forgery protection for cookie-authenticated requests.
//!
//! The session cookie is `SameSite=Lax`, so a browser sends it with a
//! top-level `GET` navigation another site starts and with any request from a
//! sibling on the same site, and an older browser may not honour the
//! attribute at all. CORS keeps a page from reading the response, not the
//! request from running. So an unsafe request carrying the session cookie runs only
//! when the browser says it came from this instance (`Sec-Fetch-Site`), or its
//! `Origin` is one of the instance's domains. A request with neither header did
//! not come from a browser, and has no ambient cookie to abuse: a browser that
//! predates `Sec-Fetch-Site` still sends `Origin` on every unsafe request, so
//! "neither" is reachable only from outside one.
//!
//! Outside `/admin` the cookie is held to the stricter
//! [`cookie_request_is_same_origin`]: it counts only on a request from the
//! instance's own pages, and any other request runs as anonymous.
//!
//! Bearer, DPoP and API-key requests are untouched: none of them is a
//! credential a browser attaches by itself.

use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::AppState;
use crate::error::AppError;

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
    AppError::Forbidden(
        "a cross-site request with the session cookie is refused; an integration \
         authenticates with OAuth (DPoP) or an API key instead"
            .into(),
    )
    .into_response()
}

/// Whether a request carrying the session cookie may use it: the browser says
/// it came from this instance, or its `Origin` is one of the instance's
/// domains, or it sent neither header and so did not
/// come from a browser. [`guard`]'s rule for an unsafe request; outside
/// `/admin` the cookie is held to [`cookie_request_is_same_origin`].
async fn cookie_request_is_first_party(state: &AppState, headers: &HeaderMap) -> bool {
    let fetch_site = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok());
    match (fetch_site, origin_of(headers)) {
        (Some("same-origin" | "none"), _) => true,
        (None, None) => true,
        (_, Some(origin)) => origin_allowed(state, origin).await,
        (Some(_), None) => false,
    }
}

/// Whether a request came from one of this instance's own pages, the only
/// place the session cookie counts outside `/admin`: the browser says so
/// (`Sec-Fetch-Site: same-origin`), or, from a browser too old to send that,
/// its `Origin` is one of the instance's domains. Anything else, including a
/// request with neither header, is not.
pub async fn cookie_request_is_same_origin(state: &AppState, headers: &HeaderMap) -> bool {
    match headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        Some(site) => site == "same-origin",
        None => match origin_of(headers) {
            Some(origin) => state.domain_cache.is_allowed_origin(origin).await,
            None => false,
        },
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
}

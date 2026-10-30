use axum::http::{HeaderName, HeaderValue};

/// A Cookie header carrying a signed session cookie for the given DID: a
/// user who has completed OAuth.
pub use happyview::testkit::app::admin_cookie_header;

/// Build an Authorization header for API key endpoints.
pub fn admin_auth_header(token: &str) -> (HeaderName, HeaderValue) {
    (
        HeaderName::from_static("authorization"),
        HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
    )
}

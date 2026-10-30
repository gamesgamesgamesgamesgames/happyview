pub mod admin_scope;
pub mod cli_client;
pub mod client_registry;
pub mod middleware;
pub mod oauth_store;
pub mod routes;
pub mod service_auth;
pub mod state_gc;

pub use client_registry::OAuthClientRegistry;
pub use middleware::Claims;
pub use middleware::ServiceAuthClaims;
pub use middleware::XrpcClaims;
pub use routes::parse_scope_string;
pub use service_auth::ServiceAuth;

pub const COOKIE_NAME: &str = "happyview_session";

/// The session cookie holding `value`, or with an empty value the cookie that
/// removes it. `SameSite=Lax`: the session is the dashboard's own, so a browser
/// sends it only with requests the instance's pages make and with top-level
/// navigations to it (the OAuth callbacks, which arrive from the PDS), never
/// with a request another site starts in the background.
pub fn session_cookie(
    value: String,
    public_url: &str,
) -> axum_extra::extract::cookie::Cookie<'static> {
    let mut cookie = axum_extra::extract::cookie::Cookie::new(COOKIE_NAME, value);
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_same_site(axum_extra::extract::cookie::SameSite::Lax);
    cookie.set_secure(public_url.starts_with("https://"));
    cookie
}

/// Error message returned when cookie-based auth (dashboard login) is disabled
/// because `SESSION_SECRET` is not configured securely. Other auth mechanisms
/// (DPoP, service auth, API keys) are unaffected.
pub const COOKIE_AUTH_DISABLED_MSG: &str = "Cookie-based login is disabled because SESSION_SECRET is not configured securely. \
     Set SESSION_SECRET to a random value of at least 32 bytes and restart the server.";

#[cfg(test)]
mod tests {
    use super::*;
    use axum_extra::extract::cookie::SameSite;

    /// `SameSite=Lax` on both schemes: a browser sends the session with the
    /// instance's own requests and the OAuth callbacks' top-level navigations,
    /// and with nothing another site starts in the background.
    #[test]
    fn the_session_cookie_is_lax_http_only_and_secure_on_https() {
        for (url, secure) in [
            ("https://happyview.example", true),
            ("http://127.0.0.1:3000", false),
        ] {
            for cookie in [
                session_cookie("did:plc:a".into(), url),
                session_cookie(String::new(), url),
            ] {
                assert_eq!(cookie.name(), COOKIE_NAME);
                assert_eq!(cookie.same_site(), Some(SameSite::Lax), "{url}");
                assert_eq!(cookie.http_only(), Some(true), "{url}");
                assert_eq!(cookie.secure(), Some(secure), "{url}");
                assert_eq!(cookie.path(), Some("/"), "{url}");
            }
        }
    }
}

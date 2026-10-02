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

/// Error message returned when cookie-based auth (dashboard login) is disabled
/// because `SESSION_SECRET` is not configured securely. Other auth mechanisms
/// (DPoP, service auth, API keys) are unaffected.
pub const COOKIE_AUTH_DISABLED_MSG: &str = "Cookie-based login is disabled because SESSION_SECRET is not configured securely. \
     Set SESSION_SECRET to a random value of at least 32 bytes and restart the server.";

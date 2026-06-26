pub mod client_registry;
pub mod middleware;
pub mod oauth_store;
pub mod routes;
pub mod service_auth;

pub use client_registry::OAuthClientRegistry;
pub use middleware::Claims;
pub use middleware::ServiceAuthClaims;
pub use middleware::XrpcClaims;
pub use routes::parse_scope_string;
pub use service_auth::ServiceAuth;

pub const COOKIE_NAME: &str = "happyview_session";

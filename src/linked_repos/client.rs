use std::sync::Arc;

use atrium_identity::did::{CommonDidResolver, CommonDidResolverConfig};
use atrium_identity::handle::{AtprotoHandleResolver, AtprotoHandleResolverConfig};
use atrium_oauth::{
    AtprotoClientMetadata, AtprotoLocalhostClientMetadata, AuthMethod, DefaultHttpClient,
    GrantType, OAuthClientConfig, OAuthResolverConfig, Scope,
};

use crate::HappyViewOAuthClient;
use crate::auth::oauth_store::{DbSessionStore, DbStateStore};
use crate::db::DatabaseBackend;
use crate::dns::NativeDnsResolver;

pub const LINKED_SESSIONS_TABLE: &str = "happyview_linked_repo_sessions";

#[allow(clippy::too_many_arguments)]
pub fn build(
    plc_url: &str,
    client_id: &str,
    client_uri: &str,
    redirect_uri: String,
    is_loopback: bool,
    scopes: Vec<Scope>,
    state_store: DbStateStore,
    pool: sqlx::AnyPool,
    backend: DatabaseBackend,
) -> Result<HappyViewOAuthClient, String> {
    let http = Arc::new(DefaultHttpClient::default());
    let resolver = OAuthResolverConfig {
        did_resolver: CommonDidResolver::new(CommonDidResolverConfig {
            plc_directory_url: plc_url.to_string(),
            http_client: Arc::clone(&http),
        }),
        handle_resolver: AtprotoHandleResolver::new(AtprotoHandleResolverConfig {
            dns_txt_resolver: NativeDnsResolver::new(),
            http_client: Arc::clone(&http),
        }),
        authorization_server_metadata: Default::default(),
        protected_resource_metadata: Default::default(),
    };

    let session_store = DbSessionStore::new_with_table(pool, backend, LINKED_SESSIONS_TABLE);

    let client = if is_loopback {
        atrium_oauth::OAuthClient::new(OAuthClientConfig {
            client_metadata: AtprotoLocalhostClientMetadata {
                redirect_uris: Some(vec![redirect_uri]),
                scopes: Some(scopes),
            },
            keys: None,
            state_store,
            session_store,
            resolver,
        })
    } else {
        atrium_oauth::OAuthClient::new(OAuthClientConfig {
            client_metadata: AtprotoClientMetadata {
                client_id: client_id.to_string(),
                client_uri: Some(client_uri.to_string()),
                redirect_uris: vec![redirect_uri],
                token_endpoint_auth_method: AuthMethod::None,
                grant_types: vec![GrantType::AuthorizationCode, GrantType::RefreshToken],
                scopes,
                jwks_uri: None,
                token_endpoint_auth_signing_alg: None,
            },
            keys: None,
            state_store,
            session_store,
            resolver,
        })
    };

    client.map_err(|e| format!("failed to create linked-repo OAuth client: {e}"))
}

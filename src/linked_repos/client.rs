use std::sync::Arc;

use atrium_identity::did::{CommonDidResolver, CommonDidResolverConfig};
use atrium_identity::handle::{AtprotoHandleResolver, AtprotoHandleResolverConfig};
use atrium_oauth::{
    AtprotoClientMetadata, AtprotoLocalhostClientMetadata, AuthMethod, GrantType,
    OAuthClientConfig, OAuthResolverConfig, Scope,
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
    client_keys: Option<Vec<jose_jwk::Jwk>>,
) -> Result<HappyViewOAuthClient, String> {
    let http = Arc::new(crate::http_retry::HappyViewHttpClient::new(
        crate::http_retry::shared_client().clone(),
    ));
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

    let signing_kid = if is_loopback {
        None
    } else {
        client_keys
            .as_ref()
            .and_then(|keys| keys.first())
            .and_then(|jwk| jwk.prm.kid.clone())
    };
    let session_store = DbSessionStore::new_with_table(pool, backend, LINKED_SESSIONS_TABLE)
        .with_signing_kid(signing_kid);

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
            http_client: crate::http_retry::HappyViewHttpClient::new(
                crate::http_retry::shared_client().clone(),
            ),
        })
    } else {
        atrium_oauth::OAuthClient::new(OAuthClientConfig {
            client_metadata: AtprotoClientMetadata {
                client_id: client_id.to_string(),
                client_uri: Some(client_uri.to_string()),
                redirect_uris: vec![redirect_uri],
                token_endpoint_auth_method: AuthMethod::PrivateKeyJwt,
                grant_types: vec![GrantType::AuthorizationCode, GrantType::RefreshToken],
                scopes,
                jwks_uri: None,
                token_endpoint_auth_signing_alg: Some("ES256".to_string()),
            },
            keys: client_keys,
            state_store,
            session_store,
            resolver,
            http_client: crate::http_retry::HappyViewHttpClient::new(
                crate::http_retry::shared_client().clone(),
            ),
        })
    };

    client.map_err(|e| format!("failed to create linked-repo OAuth client: {e}"))
}

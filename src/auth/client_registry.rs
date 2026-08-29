use arc_swap::{ArcSwap, ArcSwapOption};
use dashmap::DashMap;
use std::sync::Arc;

use atrium_identity::did::{CommonDidResolver, CommonDidResolverConfig};
use atrium_identity::handle::{AtprotoHandleResolver, AtprotoHandleResolverConfig};
use atrium_oauth::{
    AtprotoClientMetadata, AtprotoLocalhostClientMetadata, AuthMethod, GrantType,
    OAuthClientConfig, OAuthResolverConfig, Scope,
};

use crate::AppState;
use crate::HappyViewOAuthClient;
use crate::auth::oauth_store::{DbSessionStore, DbStateStore};
use crate::db::{DatabaseBackend, adapt_sql};
use crate::dns::NativeDnsResolver;
use crate::error::AppError;
use crate::oauth::client_keys::ClientKey;

#[allow(clippy::too_many_arguments)]
pub fn build_instance_client(
    plc_url: &str,
    client_id_url: &str,
    client_uri: &str,
    redirect_uris: Vec<String>,
    is_loopback: bool,
    scopes: Vec<Scope>,
    state_store: DbStateStore,
    session_store_pool: sqlx::AnyPool,
    db_backend: DatabaseBackend,
    key: &ClientKey,
) -> Result<HappyViewOAuthClient, AppError> {
    let jwk = crate::oauth::client_keys::to_atrium_jwk(key)?;

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

    // A loopback client never signs at all — the branch below ignores
    // `key` entirely — so it gets no pin regardless.
    let signing_kid = if is_loopback {
        None
    } else {
        Some(key.kid.clone())
    };
    let session_store =
        DbSessionStore::new(session_store_pool, db_backend).with_signing_kid(signing_kid);

    let client = if is_loopback {
        atrium_oauth::OAuthClient::new(OAuthClientConfig {
            client_metadata: AtprotoLocalhostClientMetadata {
                redirect_uris: Some(redirect_uris),
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
                client_id: client_id_url.to_string(),
                client_uri: Some(client_uri.to_string()),
                redirect_uris,
                token_endpoint_auth_method: AuthMethod::PrivateKeyJwt,
                grant_types: vec![GrantType::AuthorizationCode, GrantType::RefreshToken],
                scopes,
                jwks_uri: None,
                token_endpoint_auth_signing_alg: Some("ES256".to_string()),
            },
            keys: Some(vec![jwk]),
            state_store,
            session_store,
            resolver,
            http_client: crate::http_retry::HappyViewHttpClient::new(
                crate::http_retry::shared_client().clone(),
            ),
        })
    };

    client.map_err(|e| AppError::Internal(format!("failed to create OAuth client: {e}")))
}

pub fn is_loopback_url(url: &str) -> bool {
    match reqwest::Url::parse(url) {
        Ok(parsed) => matches!(
            parsed.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        ),
        Err(_) => false,
    }
}

/// Parameters needed to build an OAuth client for an API client registration.
pub struct ApiClientOAuthParams {
    pub plc_url: String,
    pub state_store: DbStateStore,
    pub session_store_pool: sqlx::AnyPool,
    pub db_backend: DatabaseBackend,
    pub client_keys: Option<Vec<jose_jwk::Jwk>>,
    pub signing_kid: Option<String>,
}

struct LinkedReposEntry {
    client: Arc<HappyViewOAuthClient>,
    kid: Option<String>,
}

struct ClientEntry {
    client: Arc<HappyViewOAuthClient>,
    kid: Option<String>,
}

struct PrimaryEntry {
    client: Arc<HappyViewOAuthClient>,
    kid: Option<String>,
}

/// Registry of OAuth clients, keyed by `client_id_url`.
pub struct OAuthClientRegistry {
    primary: ArcSwap<PrimaryEntry>,
    domain_clients: DashMap<String, Arc<HappyViewOAuthClient>>,
    clients: DashMap<String, ClientEntry>,
    by_kid: DashMap<(String, String), Arc<HappyViewOAuthClient>>,
    linked_repos_by_kid: DashMap<String, Arc<HappyViewOAuthClient>>,
    linked_repos_primary: ArcSwapOption<LinkedReposEntry>,
}

impl OAuthClientRegistry {
    pub fn new(primary_client: Arc<HappyViewOAuthClient>) -> Self {
        Self::new_with_kid(primary_client, None)
    }

    pub fn new_with_kid(primary_client: Arc<HappyViewOAuthClient>, kid: Option<String>) -> Self {
        Self {
            primary: ArcSwap::new(Arc::new(PrimaryEntry {
                client: primary_client,
                kid,
            })),
            domain_clients: DashMap::new(),
            clients: DashMap::new(),
            by_kid: DashMap::new(),
            linked_repos_by_kid: DashMap::new(),
            linked_repos_primary: ArcSwapOption::empty(),
        }
    }

    /// Register a client for a specific `(client_id_url, kid)` pair.
    pub fn register_for_kid(
        &self,
        client_id_url: &str,
        kid: &str,
        client: Arc<HappyViewOAuthClient>,
    ) {
        self.by_kid
            .insert((client_id_url.to_string(), kid.to_string()), client);
    }

    pub fn get_for_kid(&self, client_id_url: &str, kid: &str) -> Option<Arc<HappyViewOAuthClient>> {
        self.by_kid
            .get(&(client_id_url.to_string(), kid.to_string()))
            .map(|r| r.value().clone())
    }

    /// Register the linked-repos client for a specific `kid`. See the
    /// `linked_repos_by_kid` field doc for why this is separate from
    /// [`Self::register_for_kid`]. `client` must be built with a single-key
    /// keyset containing exactly the key named by `kid`, same as `by_kid`.
    pub fn register_linked_repos_for_kid(&self, kid: &str, client: Arc<HappyViewOAuthClient>) {
        self.linked_repos_by_kid.insert(kid.to_string(), client);
    }

    /// Look up the linked-repos client for a `kid`. Returns `None` if no
    /// such entry was registered (e.g. the key has since been revoked and
    /// removed).
    pub fn get_linked_repos_for_kid(&self, kid: &str) -> Option<Arc<HappyViewOAuthClient>> {
        self.linked_repos_by_kid.get(kid).map(|r| r.value().clone())
    }

    /// Remove every `by_kid` and `linked_repos_by_kid` entry for `kid`, under
    /// any `client_id_url`.
    pub fn evict_kid(&self, kid: &str) {
        self.by_kid.retain(|(_, k), _| k != kid);
        self.linked_repos_by_kid.remove(kid);
    }

    /// Register an API client's OAuth client, keyed by its `client_id_url`.
    pub fn register(
        &self,
        client_id_url: String,
        client: Arc<HappyViewOAuthClient>,
        kid: Option<String>,
    ) {
        self.clients
            .insert(client_id_url, ClientEntry { client, kid });
    }

    /// Remove an API client's OAuth client.
    pub fn remove(&self, client_id_url: &str) {
        self.clients.remove(client_id_url);
    }

    /// Look up a client by `client_id_url`.
    pub fn get(&self, client_id_url: &str) -> Option<Arc<HappyViewOAuthClient>> {
        self.clients
            .get(client_id_url)
            .map(|r| r.value().client.clone())
    }

    pub fn get_with_kid(
        &self,
        client_id_url: &str,
    ) -> Option<(Arc<HappyViewOAuthClient>, Option<String>)> {
        self.clients
            .get(client_id_url)
            .map(|r| (r.value().client.clone(), r.value().kid.clone()))
    }

    /// Get the resolved OAuth `client_id` for a registered client.
    ///
    /// For loopback clients this returns `http://localhost?scope=...` (the format
    /// auth servers expect), not the original `client_id_url` key.
    pub fn get_resolved_client_id(&self, client_id_url: &str) -> Option<String> {
        self.clients
            .get(client_id_url)
            .map(|r| r.value().client.client_metadata.client_id.clone())
    }

    /// Look up a client by `client_id_url`, falling back to the primary client.
    pub fn get_or_default(&self, client_id_url: Option<&str>) -> Arc<HappyViewOAuthClient> {
        if let Some(url) = client_id_url {
            self.clients
                .get(url)
                .map(|r| r.value().client.clone())
                .unwrap_or_else(|| self.primary_client())
        } else {
            self.primary_client()
        }
    }

    /// Get the primary (HappyView dashboard) client.
    pub fn primary_client(&self) -> Arc<HappyViewOAuthClient> {
        self.primary.load_full().client.clone()
    }

    /// Atomically read the primary client together with the kid it actually
    /// signs with, if known.
    pub fn primary_client_and_kid(&self) -> (Arc<HappyViewOAuthClient>, Option<String>) {
        let entry = self.primary.load_full();
        (entry.client.clone(), entry.kid.clone())
    }

    pub fn set_linked_repos_primary(&self, client: Arc<HappyViewOAuthClient>, kid: Option<String>) {
        self.linked_repos_primary
            .store(Some(Arc::new(LinkedReposEntry { client, kid })));
    }

    pub fn linked_repos_primary(&self) -> Option<(Arc<HappyViewOAuthClient>, Option<String>)> {
        self.linked_repos_primary
            .load_full()
            .map(|e| (e.client.clone(), e.kid.clone()))
    }

    pub fn get_or_default_with_kid(
        &self,
        client_id_url: Option<&str>,
    ) -> (Arc<HappyViewOAuthClient>, Option<String>) {
        if let Some(url) = client_id_url
            && let Some(found) = self.get_with_kid(url)
        {
            return found;
        }
        self.primary_client_and_kid()
    }

    /// Register a domain-specific OAuth client.
    /// Inserts into both `domain_clients` (keyed by domain URL, for `get_for_domain`)
    /// and `clients` (keyed by client_id_url, for `get_or_default`).
    ///
    /// `client_id_url` must be the base-path-aware client ID
    /// (e.g. `{domain_url}{base_path}/oauth-client-metadata.json`).
    pub fn register_domain_client(
        &self,
        domain_url: String,
        client_id_url: String,
        client: Arc<HappyViewOAuthClient>,
        kid: Option<String>,
    ) {
        self.domain_clients.insert(domain_url, Arc::clone(&client));
        self.clients
            .insert(client_id_url, ClientEntry { client, kid });
    }

    /// Remove a domain-specific OAuth client from both maps.
    ///
    /// `client_id_url` must be the same base-path-aware client ID that was
    /// passed to `register_domain_client`.
    pub fn remove_domain_client(&self, domain_url: &str, client_id_url: &str) {
        self.domain_clients.remove(domain_url);
        self.clients.remove(client_id_url);
    }

    /// Look up a domain-specific OAuth client.
    pub fn get_domain_client(&self, domain_url: &str) -> Option<Arc<HappyViewOAuthClient>> {
        self.domain_clients
            .get(domain_url)
            .map(|r| r.value().clone())
    }

    /// Get the OAuth client for a domain, falling back to the primary client.
    pub fn get_for_domain(&self, domain_url: &str) -> Arc<HappyViewOAuthClient> {
        self.domain_clients
            .get(domain_url)
            .map(|r| r.value().clone())
            .unwrap_or_else(|| self.primary_client())
    }

    /// Like [`Self::set_primary_client`], but atomically records the kid
    /// `client` actually signs with. `oauth::rotation::rotate_instance_key`
    /// uses this — getting the client and its kid to move together, in one
    /// store, is the entire point of `PrimaryEntry` existing.
    pub fn set_primary_client_with_kid(
        &self,
        client: Arc<HappyViewOAuthClient>,
        kid: Option<String>,
    ) {
        self.primary.store(Arc::new(PrimaryEntry { client, kid }));
    }

    /// Returns true if the given `client_id_url` is already claimed by a domain
    /// client. Checks by comparing against the actual client instances stored by
    /// domain registrations, so it works correctly regardless of `BASE_PATH`.
    pub fn is_domain_client_id(&self, client_id_url: &str) -> bool {
        // A client_id_url belongs to a domain client if any domain_clients entry
        // has the same Arc as the one stored in `clients` under that key.
        if let Some(candidate) = self.clients.get(client_id_url) {
            self.domain_clients
                .iter()
                .any(|entry| Arc::ptr_eq(entry.value(), &candidate.value().client))
        } else {
            false
        }
    }

    /// Build and register a single OAuth client from API client metadata.
    /// Used when creating or updating an API client via the admin UI.
    pub fn register_api_client(
        &self,
        client_id_url: &str,
        client_uri: &str,
        redirect_uris: Vec<String>,
        scopes_str: &str,
        params: &ApiClientOAuthParams,
    ) -> Result<(), String> {
        if self.is_domain_client_id(client_id_url) {
            return Err(format!(
                "client_id_url '{}' conflicts with a registered domain's OAuth client",
                client_id_url
            ));
        }
        let ApiClientOAuthParams {
            plc_url,
            state_store,
            session_store_pool,
            db_backend,
            client_keys,
            signing_kid,
        } = params;
        let scopes = crate::auth::parse_scope_string(scopes_str);
        let scopes = if scopes.is_empty() {
            vec![atrium_oauth::Scope::Known(
                atrium_oauth::KnownScope::Atproto,
            )]
        } else {
            scopes
        };

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

        let client = if is_loopback_url(client_id_url) {
            atrium_oauth::OAuthClient::new(OAuthClientConfig {
                client_metadata: AtprotoLocalhostClientMetadata {
                    redirect_uris: None,
                    scopes: Some(scopes),
                },
                keys: None,
                state_store: state_store.clone(),
                session_store: DbSessionStore::new(session_store_pool.clone(), *db_backend),
                resolver,
                http_client: crate::http_retry::HappyViewHttpClient::new(
                    crate::http_retry::shared_client().clone(),
                ),
            })
        } else {
            atrium_oauth::OAuthClient::new(OAuthClientConfig {
                client_metadata: AtprotoClientMetadata {
                    client_id: client_id_url.to_string(),
                    client_uri: Some(client_uri.to_string()),
                    redirect_uris,
                    token_endpoint_auth_method: if client_keys.is_some() {
                        AuthMethod::PrivateKeyJwt
                    } else {
                        AuthMethod::None
                    },
                    grant_types: vec![GrantType::AuthorizationCode, GrantType::RefreshToken],
                    scopes,
                    jwks_uri: None,
                    token_endpoint_auth_signing_alg: client_keys
                        .as_ref()
                        .map(|_| "ES256".to_string()),
                },
                keys: client_keys.clone(),
                state_store: state_store.clone(),
                session_store: DbSessionStore::new(session_store_pool.clone(), *db_backend)
                    .with_signing_kid(signing_kid.clone()),
                resolver,
                http_client: crate::http_retry::HappyViewHttpClient::new(
                    crate::http_retry::shared_client().clone(),
                ),
            })
        };

        match client {
            Ok(client) => {
                self.register(
                    client_id_url.to_string(),
                    Arc::new(client),
                    signing_kid.clone(),
                );
                Ok(())
            }
            Err(e) => Err(format!("failed to create OAuth client: {e}")),
        }
    }

    /// Re-probe an API client's published metadata document and, if its
    /// confidentiality verdict differs from what is currently registered,
    /// re-register it.
    pub async fn refresh_client_confidentiality(
        &self,
        state: &AppState,
        api_client_id: &str,
        client_id_url: &str,
    ) -> Result<bool, AppError> {
        if is_loopback_url(client_id_url) {
            return Ok(false);
        }

        let probe = crate::oauth::client_probe::cached(state, api_client_id, client_id_url).await?;

        let currently_confidential = self.get(client_id_url).is_some_and(|c| {
            c.client_metadata.token_endpoint_auth_method.as_deref() == Some("private_key_jwt")
        });

        if probe.confidential == currently_confidential {
            return Ok(currently_confidential);
        }

        let sql = adapt_sql(
            "SELECT client_uri, redirect_uris, scopes FROM happyview_api_clients WHERE id = ?",
            state.db_backend,
        );
        let row: Option<(String, String, String)> = crate::db::query_as(&sql)
            .bind(api_client_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("failed to look up API client: {e}")))?;
        let Some((client_uri, redirect_uris_json, scopes)) = row else {
            // The client row is gone (e.g. deleted concurrently) — nothing to
            // re-register against. Report what is actually registered, not
            // what the probe wanted.
            return Ok(currently_confidential);
        };
        let redirect_uris: Vec<String> =
            serde_json::from_str(&redirect_uris_json).unwrap_or_default();

        let (client_keys, signing_kid) = if probe.confidential {
            let keys = crate::oauth::client_keys::load_keys(
                &state.db,
                state.db_backend,
                state.config.token_encryption_key.as_ref(),
                api_client_id,
            )
            .await?;
            match keys
                .into_iter()
                .find(|k| k.status == crate::oauth::client_keys::KeyStatus::Current)
            {
                Some(k) => {
                    let jwk = crate::oauth::client_keys::to_atrium_jwk(&k)?;
                    (Some(vec![jwk]), Some(k.kid))
                }
                None => (None, None),
            }
        } else {
            (None, None)
        };

        let params = ApiClientOAuthParams {
            plc_url: state.config.plc_url.clone(),
            state_store: state.oauth_state_store.clone(),
            session_store_pool: state.db.clone(),
            db_backend: state.db_backend,
            client_keys,
            signing_kid,
        };

        self.register_api_client(client_id_url, &client_uri, redirect_uris, &scopes, &params)
            .map_err(|e| AppError::Internal(format!("failed to re-register API client: {e}")))?;

        let now_confidential = self.get(client_id_url).is_some_and(|c| {
            c.client_metadata.token_endpoint_auth_method.as_deref() == Some("private_key_jwt")
        });
        if now_confidential != probe.confidential {
            tracing::warn!(
                client_id = %api_client_id,
                client_id_url = %client_id_url,
                probe_confidential = probe.confidential,
                registered_confidential = now_confidential,
                "probe verdict and actual registration disagree after re-registration"
            );
        }

        Ok(now_confidential)
    }

    /// Load all active API clients from the database and register OAuth clients for each.
    pub async fn load_from_db(
        &self,
        db: &sqlx::AnyPool,
        db_backend: DatabaseBackend,
        plc_url: &str,
        state_store: DbStateStore,
        session_store_pool: sqlx::AnyPool,
    ) {
        let sql = adapt_sql(
            "SELECT client_id_url, client_uri, redirect_uris, scopes FROM happyview_api_clients WHERE is_active = 1",
            db_backend,
        );

        let rows: Vec<(String, String, String, String)> =
            match crate::db::query_as(&sql).fetch_all(db).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("Failed to load API clients from database: {e}");
                    return;
                }
            };

        for (client_id_url, client_uri, redirect_uris_json, scopes_str) in rows {
            if self.is_domain_client_id(&client_id_url) {
                tracing::warn!(
                    client_id = %client_id_url,
                    "Skipping API client that conflicts with a domain OAuth client"
                );
                continue;
            }

            let redirect_uris: Vec<String> =
                serde_json::from_str(&redirect_uris_json).unwrap_or_default();

            let scopes = crate::auth::parse_scope_string(&scopes_str);
            let scopes = if scopes.is_empty() {
                vec![atrium_oauth::Scope::Known(
                    atrium_oauth::KnownScope::Atproto,
                )]
            } else {
                scopes
            };

            // Each OAuthClient needs its own resolver instances (they're not Clone)
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

            let client = if is_loopback_url(&client_id_url) {
                atrium_oauth::OAuthClient::new(OAuthClientConfig {
                    client_metadata: AtprotoLocalhostClientMetadata {
                        redirect_uris: None,
                        scopes: Some(scopes),
                    },
                    keys: None,
                    state_store: state_store.clone(),
                    session_store: DbSessionStore::new(session_store_pool.clone(), db_backend),
                    resolver,
                    http_client: crate::http_retry::HappyViewHttpClient::new(
                        crate::http_retry::shared_client().clone(),
                    ),
                })
            } else {
                atrium_oauth::OAuthClient::new(OAuthClientConfig {
                    client_metadata: AtprotoClientMetadata {
                        client_id: client_id_url.clone(),
                        client_uri: Some(client_uri),
                        redirect_uris,
                        token_endpoint_auth_method: AuthMethod::None,
                        grant_types: vec![GrantType::AuthorizationCode, GrantType::RefreshToken],
                        scopes,
                        jwks_uri: None,
                        token_endpoint_auth_signing_alg: None,
                    },
                    keys: None,
                    state_store: state_store.clone(),
                    session_store: DbSessionStore::new(session_store_pool.clone(), db_backend),
                    resolver,
                    http_client: crate::http_retry::HappyViewHttpClient::new(
                        crate::http_retry::shared_client().clone(),
                    ),
                })
            };

            match client {
                Ok(client) => {
                    tracing::info!(client_id = %client_id_url, "Registered API client OAuth identity");
                    self.register(client_id_url, Arc::new(client), None);
                }
                Err(e) => {
                    tracing::error!(client_id = %client_id_url, error = %e, "Failed to create OAuth client for API client");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn is_loopback_url_matches_the_host_exactly() {
        for url in [
            "http://localhost",
            "http://localhost:3000",
            "http://127.0.0.1:3200",
            "http://127.0.0.1:9999",
            "http://127.0.0.1:0",
            "http://[::1]:8080",
        ] {
            assert!(is_loopback_url(url), "{url} should be loopback");
        }

        for url in [
            "https://happyview.localhost",
            "https://localhost.example.com",
            "https://mylocalhost.com",
            "https://127.0.0.1.example.com",
            "https://example.com",
        ] {
            assert!(!is_loopback_url(url), "{url} should NOT be loopback");
        }

        assert!(!is_loopback_url("not a url"));
        assert!(!is_loopback_url(""));
        assert!(!is_loopback_url("localhost:3000"));
    }

    use super::*;

    // Note: we can't easily construct real OAuthClient instances in unit tests
    // because they require resolvers, stores, etc. The registry logic is simple
    // enough that we test it via integration tests that stand up the full stack.
    // These tests verify the DashMap-based lookup logic using a mock approach.

    #[test]
    fn test_registry_stores_and_retrieves() {
        // We can at least verify the DashMap operations work correctly
        let map: DashMap<String, String> = DashMap::new();
        map.insert("key1".to_string(), "val1".to_string());

        assert!(map.get("key1").is_some());
        assert!(map.get("key2").is_none());

        map.remove("key1");
        assert!(map.get("key1").is_none());
    }

    #[test]
    fn test_registry_overwrite() {
        let map: DashMap<String, String> = DashMap::new();
        map.insert("key1".to_string(), "val1".to_string());
        map.insert("key1".to_string(), "val2".to_string());

        assert_eq!(map.get("key1").unwrap().value(), "val2");
    }

    #[test]
    fn test_domain_client_id_collision_detection() {
        // Simulate the is_domain_client_id logic using raw DashMaps and Arc pointer equality,
        // mirroring the real OAuthClientRegistry implementation.
        let domain_clients: DashMap<String, Arc<String>> = DashMap::new();
        let clients: DashMap<String, Arc<String>> = DashMap::new();

        // Register domain "https://example.com" with base-path-aware client_id_url
        let client_a = Arc::new("client_a".to_string());
        domain_clients.insert("https://example.com".to_string(), Arc::clone(&client_a));
        clients.insert(
            "https://example.com/hv/oauth-client-metadata.json".to_string(),
            client_a,
        );

        // Register domain "https://other.example.com" without base path
        let client_b = Arc::new("client_b".to_string());
        domain_clients.insert(
            "https://other.example.com".to_string(),
            Arc::clone(&client_b),
        );
        clients.insert(
            "https://other.example.com/oauth-client-metadata.json".to_string(),
            client_b,
        );

        // Also register a non-domain API client
        let api_client = Arc::new("api_client".to_string());
        clients.insert(
            "https://api.example.com/oauth-client-metadata.json".to_string(),
            api_client,
        );

        let is_domain_client_id = |client_id_url: &str| -> bool {
            if let Some(candidate) = clients.get(client_id_url) {
                domain_clients
                    .iter()
                    .any(|entry| Arc::ptr_eq(entry.value(), candidate.value()))
            } else {
                false
            }
        };

        // Base-path-aware key is detected as a domain client
        assert!(is_domain_client_id(
            "https://example.com/hv/oauth-client-metadata.json"
        ));
        // Non-base-path key is also detected
        assert!(is_domain_client_id(
            "https://other.example.com/oauth-client-metadata.json"
        ));
        // Unrelated URLs are not detected
        assert!(!is_domain_client_id(
            "https://unrelated.com/oauth-client-metadata.json"
        ));
        assert!(!is_domain_client_id("https://example.com/other-path.json"));
        // The old (wrong) key without base path is not detected
        assert!(!is_domain_client_id(
            "https://example.com/oauth-client-metadata.json"
        ));
        // API client is not detected as a domain client
        assert!(!is_domain_client_id(
            "https://api.example.com/oauth-client-metadata.json"
        ));
    }

    #[tokio::test]
    async fn register_api_client_confidential_vs_public() {
        let pool = crate::test_support::memory_pool().await;
        let backend = DatabaseBackend::Sqlite;
        let http = Arc::new(crate::http_retry::HappyViewHttpClient::default());
        let primary = atrium_oauth::OAuthClient::new(OAuthClientConfig {
            client_metadata: AtprotoLocalhostClientMetadata {
                redirect_uris: None,
                scopes: None,
            },
            keys: None,
            state_store: DbStateStore::new(pool.clone(), backend),
            session_store: DbSessionStore::new(pool.clone(), backend),
            resolver: OAuthResolverConfig {
                did_resolver: CommonDidResolver::new(CommonDidResolverConfig {
                    plc_directory_url: "https://plc.directory".into(),
                    http_client: Arc::clone(&http),
                }),
                handle_resolver: AtprotoHandleResolver::new(AtprotoHandleResolverConfig {
                    dns_txt_resolver: NativeDnsResolver::new(),
                    http_client: Arc::clone(&http),
                }),
                authorization_server_metadata: Default::default(),
                protected_resource_metadata: Default::default(),
            },
            http_client: crate::http_retry::HappyViewHttpClient::default(),
        })
        .expect("primary loopback client");
        let registry = OAuthClientRegistry::new(Arc::new(primary));

        let key = crate::oauth::client_keys::generate_client_key("test-owner")
            .expect("generate client key");
        let jwk = crate::oauth::client_keys::to_atrium_jwk(&key).expect("convert to atrium jwk");

        let confidential_url = "https://example.com/oauth-client-metadata.json";
        registry
            .register_api_client(
                confidential_url,
                "https://example.com",
                vec!["https://example.com/auth/callback".to_string()],
                "atproto",
                &ApiClientOAuthParams {
                    plc_url: "https://plc.directory".into(),
                    state_store: DbStateStore::new(pool.clone(), backend),
                    session_store_pool: pool.clone(),
                    db_backend: backend,
                    client_keys: Some(vec![jwk]),
                    signing_kid: Some("test-kid".to_string()),
                },
            )
            .expect("register confidential client");

        let confidential = registry
            .get(confidential_url)
            .expect("confidential client registered");
        assert_eq!(
            confidential.client_metadata.token_endpoint_auth_method,
            Some("private_key_jwt".to_string())
        );
        assert_eq!(
            confidential.client_metadata.token_endpoint_auth_signing_alg,
            Some("ES256".to_string())
        );
        assert!(confidential.client_metadata.jwks.is_some());

        let public_url = "https://other.example.com/oauth-client-metadata.json";
        registry
            .register_api_client(
                public_url,
                "https://other.example.com",
                vec!["https://other.example.com/auth/callback".to_string()],
                "atproto",
                &ApiClientOAuthParams {
                    plc_url: "https://plc.directory".into(),
                    state_store: DbStateStore::new(pool.clone(), backend),
                    session_store_pool: pool.clone(),
                    db_backend: backend,
                    client_keys: None,
                    signing_kid: None,
                },
            )
            .expect("register public client");

        let public = registry.get(public_url).expect("public client registered");
        assert_eq!(
            public.client_metadata.token_endpoint_auth_method,
            Some("none".to_string())
        );
        assert_eq!(public.client_metadata.token_endpoint_auth_signing_alg, None);
        assert!(public.client_metadata.jwks.is_none());
    }

    #[tokio::test]
    async fn get_for_kid_returns_the_registered_client_and_none_otherwise() {
        let pool = crate::test_support::memory_pool().await;
        let backend = DatabaseBackend::Sqlite;
        let http = Arc::new(crate::http_retry::HappyViewHttpClient::default());
        let primary = atrium_oauth::OAuthClient::new(OAuthClientConfig {
            client_metadata: AtprotoLocalhostClientMetadata {
                redirect_uris: None,
                scopes: None,
            },
            keys: None,
            state_store: DbStateStore::new(pool.clone(), backend),
            session_store: DbSessionStore::new(pool.clone(), backend),
            resolver: OAuthResolverConfig {
                did_resolver: CommonDidResolver::new(CommonDidResolverConfig {
                    plc_directory_url: "https://plc.directory".into(),
                    http_client: Arc::clone(&http),
                }),
                handle_resolver: AtprotoHandleResolver::new(AtprotoHandleResolverConfig {
                    dns_txt_resolver: NativeDnsResolver::new(),
                    http_client: Arc::clone(&http),
                }),
                authorization_server_metadata: Default::default(),
                protected_resource_metadata: Default::default(),
            },
            http_client: crate::http_retry::HappyViewHttpClient::default(),
        })
        .expect("primary loopback client");
        let registry = OAuthClientRegistry::new(Arc::new(primary));

        let key_a = crate::oauth::client_keys::generate_client_key("test-owner")
            .expect("generate client key a");
        let jwk_a = crate::oauth::client_keys::to_atrium_jwk(&key_a).expect("convert key a");
        let key_b = crate::oauth::client_keys::generate_client_key("test-owner")
            .expect("generate client key b");
        let jwk_b = crate::oauth::client_keys::to_atrium_jwk(&key_b).expect("convert key b");

        let client_id_url = "https://example.com/oauth-client-metadata.json";

        // Two entries under the same client_id_url, each a single-key client
        // for a different kid.
        for (kid, jwk) in [(&key_a.kid, jwk_a), (&key_b.kid, jwk_b)] {
            let client = atrium_oauth::OAuthClient::new(OAuthClientConfig {
                client_metadata: AtprotoClientMetadata {
                    client_id: client_id_url.to_string(),
                    client_uri: Some("https://example.com".to_string()),
                    redirect_uris: vec!["https://example.com/auth/callback".to_string()],
                    token_endpoint_auth_method: AuthMethod::PrivateKeyJwt,
                    grant_types: vec![GrantType::AuthorizationCode, GrantType::RefreshToken],
                    scopes: vec![atrium_oauth::Scope::Known(
                        atrium_oauth::KnownScope::Atproto,
                    )],
                    jwks_uri: None,
                    token_endpoint_auth_signing_alg: Some("ES256".to_string()),
                },
                keys: Some(vec![jwk]),
                state_store: DbStateStore::new(pool.clone(), backend),
                session_store: DbSessionStore::new(pool.clone(), backend),
                resolver: OAuthResolverConfig {
                    did_resolver: CommonDidResolver::new(CommonDidResolverConfig {
                        plc_directory_url: "https://plc.directory".into(),
                        http_client: Arc::clone(&http),
                    }),
                    handle_resolver: AtprotoHandleResolver::new(AtprotoHandleResolverConfig {
                        dns_txt_resolver: NativeDnsResolver::new(),
                        http_client: Arc::clone(&http),
                    }),
                    authorization_server_metadata: Default::default(),
                    protected_resource_metadata: Default::default(),
                },
                http_client: crate::http_retry::HappyViewHttpClient::default(),
            })
            .expect("single-key client");
            registry.register_for_kid(client_id_url, kid, Arc::new(client));
        }

        let found_a = registry
            .get_for_kid(client_id_url, &key_a.kid)
            .expect("entry for kid a");
        assert!(found_a.client_metadata.jwks.is_some());

        let found_b = registry
            .get_for_kid(client_id_url, &key_b.kid)
            .expect("entry for kid b");
        assert!(!Arc::ptr_eq(&found_a, &found_b));

        assert!(registry.get_for_kid(client_id_url, "gone").is_none());
        assert!(
            registry
                .get_for_kid("https://unregistered.example.com", &key_a.kid)
                .is_none()
        );
    }

    #[tokio::test]
    async fn linked_repos_by_kid_does_not_collide_with_by_kid() {
        let pool = crate::test_support::memory_pool().await;
        let backend = DatabaseBackend::Sqlite;
        let http = Arc::new(crate::http_retry::HappyViewHttpClient::default());
        let primary = atrium_oauth::OAuthClient::new(OAuthClientConfig {
            client_metadata: AtprotoLocalhostClientMetadata {
                redirect_uris: None,
                scopes: None,
            },
            keys: None,
            state_store: DbStateStore::new(pool.clone(), backend),
            session_store: DbSessionStore::new(pool.clone(), backend),
            resolver: OAuthResolverConfig {
                did_resolver: CommonDidResolver::new(CommonDidResolverConfig {
                    plc_directory_url: "https://plc.directory".into(),
                    http_client: Arc::clone(&http),
                }),
                handle_resolver: AtprotoHandleResolver::new(AtprotoHandleResolverConfig {
                    dns_txt_resolver: NativeDnsResolver::new(),
                    http_client: Arc::clone(&http),
                }),
                authorization_server_metadata: Default::default(),
                protected_resource_metadata: Default::default(),
            },
            http_client: crate::http_retry::HappyViewHttpClient::default(),
        })
        .expect("primary loopback client");
        let registry = OAuthClientRegistry::new(Arc::new(primary));

        let key = crate::oauth::client_keys::generate_client_key("instance")
            .expect("generate client key");
        let jwk = crate::oauth::client_keys::to_atrium_jwk(&key).expect("convert to atrium jwk");

        let shared_client_id_url = "https://example.com/oauth-client-metadata.json";

        let build_single_key_client = |jwk: jose_jwk::Jwk| {
            atrium_oauth::OAuthClient::new(OAuthClientConfig {
                client_metadata: AtprotoClientMetadata {
                    client_id: shared_client_id_url.to_string(),
                    client_uri: Some("https://example.com".to_string()),
                    redirect_uris: vec!["https://example.com/auth/callback".to_string()],
                    token_endpoint_auth_method: AuthMethod::PrivateKeyJwt,
                    grant_types: vec![GrantType::AuthorizationCode, GrantType::RefreshToken],
                    scopes: vec![atrium_oauth::Scope::Known(
                        atrium_oauth::KnownScope::Atproto,
                    )],
                    jwks_uri: None,
                    token_endpoint_auth_signing_alg: Some("ES256".to_string()),
                },
                keys: Some(vec![jwk]),
                state_store: DbStateStore::new(pool.clone(), backend),
                session_store: DbSessionStore::new(pool.clone(), backend),
                resolver: OAuthResolverConfig {
                    did_resolver: CommonDidResolver::new(CommonDidResolverConfig {
                        plc_directory_url: "https://plc.directory".into(),
                        http_client: Arc::clone(&http),
                    }),
                    handle_resolver: AtprotoHandleResolver::new(AtprotoHandleResolverConfig {
                        dns_txt_resolver: NativeDnsResolver::new(),
                        http_client: Arc::clone(&http),
                    }),
                    authorization_server_metadata: Default::default(),
                    protected_resource_metadata: Default::default(),
                },
                http_client: crate::http_retry::HappyViewHttpClient::default(),
            })
            .expect("single-key client")
        };

        let instance_client = Arc::new(build_single_key_client(jwk.clone()));
        let linked_repos_client = Arc::new(build_single_key_client(jwk));

        registry.register_for_kid(shared_client_id_url, &key.kid, Arc::clone(&instance_client));
        registry.register_linked_repos_for_kid(&key.kid, Arc::clone(&linked_repos_client));

        let found_instance = registry
            .get_for_kid(shared_client_id_url, &key.kid)
            .expect("instance entry present");
        let found_linked_repos = registry
            .get_linked_repos_for_kid(&key.kid)
            .expect("linked-repos entry present");

        assert!(Arc::ptr_eq(&found_instance, &instance_client));
        assert!(Arc::ptr_eq(&found_linked_repos, &linked_repos_client));
        assert!(!Arc::ptr_eq(&found_instance, &found_linked_repos));

        assert!(registry.get_linked_repos_for_kid("gone").is_none());
    }

    #[tokio::test]
    async fn evict_kid_removes_target_but_spares_other_kids() {
        let pool = crate::test_support::memory_pool().await;
        let backend = DatabaseBackend::Sqlite;
        let http = Arc::new(crate::http_retry::HappyViewHttpClient::default());
        let primary = atrium_oauth::OAuthClient::new(OAuthClientConfig {
            client_metadata: AtprotoLocalhostClientMetadata {
                redirect_uris: None,
                scopes: None,
            },
            keys: None,
            state_store: DbStateStore::new(pool.clone(), backend),
            session_store: DbSessionStore::new(pool.clone(), backend),
            resolver: OAuthResolverConfig {
                did_resolver: CommonDidResolver::new(CommonDidResolverConfig {
                    plc_directory_url: "https://plc.directory".into(),
                    http_client: Arc::clone(&http),
                }),
                handle_resolver: AtprotoHandleResolver::new(AtprotoHandleResolverConfig {
                    dns_txt_resolver: NativeDnsResolver::new(),
                    http_client: Arc::clone(&http),
                }),
                authorization_server_metadata: Default::default(),
                protected_resource_metadata: Default::default(),
            },
            http_client: crate::http_retry::HappyViewHttpClient::default(),
        })
        .expect("primary loopback client");
        let registry = OAuthClientRegistry::new(Arc::new(primary));

        let client_id_url = "https://example.com/oauth-client-metadata.json";

        let build_single_key_client = |jwk: jose_jwk::Jwk| {
            atrium_oauth::OAuthClient::new(OAuthClientConfig {
                client_metadata: AtprotoClientMetadata {
                    client_id: client_id_url.to_string(),
                    client_uri: Some("https://example.com".to_string()),
                    redirect_uris: vec!["https://example.com/auth/callback".to_string()],
                    token_endpoint_auth_method: AuthMethod::PrivateKeyJwt,
                    grant_types: vec![GrantType::AuthorizationCode, GrantType::RefreshToken],
                    scopes: vec![atrium_oauth::Scope::Known(
                        atrium_oauth::KnownScope::Atproto,
                    )],
                    jwks_uri: None,
                    token_endpoint_auth_signing_alg: Some("ES256".to_string()),
                },
                keys: Some(vec![jwk]),
                state_store: DbStateStore::new(pool.clone(), backend),
                session_store: DbSessionStore::new(pool.clone(), backend),
                resolver: OAuthResolverConfig {
                    did_resolver: CommonDidResolver::new(CommonDidResolverConfig {
                        plc_directory_url: "https://plc.directory".into(),
                        http_client: Arc::clone(&http),
                    }),
                    handle_resolver: AtprotoHandleResolver::new(AtprotoHandleResolverConfig {
                        dns_txt_resolver: NativeDnsResolver::new(),
                        http_client: Arc::clone(&http),
                    }),
                    authorization_server_metadata: Default::default(),
                    protected_resource_metadata: Default::default(),
                },
                http_client: crate::http_retry::HappyViewHttpClient::default(),
            })
            .expect("single-key client")
        };

        let key_a = crate::oauth::client_keys::generate_client_key("test-owner")
            .expect("generate client key a");
        let jwk_a = crate::oauth::client_keys::to_atrium_jwk(&key_a).expect("convert key a");
        let key_b = crate::oauth::client_keys::generate_client_key("test-owner")
            .expect("generate client key b");
        let jwk_b = crate::oauth::client_keys::to_atrium_jwk(&key_b).expect("convert key b");

        registry.register_for_kid(
            client_id_url,
            &key_a.kid,
            Arc::new(build_single_key_client(jwk_a)),
        );
        registry.register_for_kid(
            client_id_url,
            &key_b.kid,
            Arc::new(build_single_key_client(jwk_b)),
        );

        assert!(registry.get_for_kid(client_id_url, &key_a.kid).is_some());
        assert!(registry.get_for_kid(client_id_url, &key_b.kid).is_some());

        registry.evict_kid(&key_a.kid);

        assert!(
            registry.get_for_kid(client_id_url, &key_a.kid).is_none(),
            "evicted kid a must be gone"
        );
        assert!(
            registry.get_for_kid(client_id_url, &key_b.kid).is_some(),
            "kid b must survive evicting kid a"
        );
    }

    #[tokio::test]
    async fn evict_kid_removes_entry_under_every_client_id_url() {
        let pool = crate::test_support::memory_pool().await;
        let backend = DatabaseBackend::Sqlite;
        let http = Arc::new(crate::http_retry::HappyViewHttpClient::default());
        let primary = atrium_oauth::OAuthClient::new(OAuthClientConfig {
            client_metadata: AtprotoLocalhostClientMetadata {
                redirect_uris: None,
                scopes: None,
            },
            keys: None,
            state_store: DbStateStore::new(pool.clone(), backend),
            session_store: DbSessionStore::new(pool.clone(), backend),
            resolver: OAuthResolverConfig {
                did_resolver: CommonDidResolver::new(CommonDidResolverConfig {
                    plc_directory_url: "https://plc.directory".into(),
                    http_client: Arc::clone(&http),
                }),
                handle_resolver: AtprotoHandleResolver::new(AtprotoHandleResolverConfig {
                    dns_txt_resolver: NativeDnsResolver::new(),
                    http_client: Arc::clone(&http),
                }),
                authorization_server_metadata: Default::default(),
                protected_resource_metadata: Default::default(),
            },
            http_client: crate::http_retry::HappyViewHttpClient::default(),
        })
        .expect("primary loopback client");
        let registry = OAuthClientRegistry::new(Arc::new(primary));

        let primary_client_id_url = "https://example.com/oauth-client-metadata.json";
        let domain_client_id_url = "https://domain.example.com/oauth-client-metadata.json";

        let build_single_key_client = |client_id_url: &str, jwk: jose_jwk::Jwk| {
            atrium_oauth::OAuthClient::new(OAuthClientConfig {
                client_metadata: AtprotoClientMetadata {
                    client_id: client_id_url.to_string(),
                    client_uri: Some("https://example.com".to_string()),
                    redirect_uris: vec!["https://example.com/auth/callback".to_string()],
                    token_endpoint_auth_method: AuthMethod::PrivateKeyJwt,
                    grant_types: vec![GrantType::AuthorizationCode, GrantType::RefreshToken],
                    scopes: vec![atrium_oauth::Scope::Known(
                        atrium_oauth::KnownScope::Atproto,
                    )],
                    jwks_uri: None,
                    token_endpoint_auth_signing_alg: Some("ES256".to_string()),
                },
                keys: Some(vec![jwk]),
                state_store: DbStateStore::new(pool.clone(), backend),
                session_store: DbSessionStore::new(pool.clone(), backend),
                resolver: OAuthResolverConfig {
                    did_resolver: CommonDidResolver::new(CommonDidResolverConfig {
                        plc_directory_url: "https://plc.directory".into(),
                        http_client: Arc::clone(&http),
                    }),
                    handle_resolver: AtprotoHandleResolver::new(AtprotoHandleResolverConfig {
                        dns_txt_resolver: NativeDnsResolver::new(),
                        http_client: Arc::clone(&http),
                    }),
                    authorization_server_metadata: Default::default(),
                    protected_resource_metadata: Default::default(),
                },
                http_client: crate::http_retry::HappyViewHttpClient::default(),
            })
            .expect("single-key client")
        };

        let key = crate::oauth::client_keys::generate_client_key("test-owner")
            .expect("generate client key");
        let jwk_primary =
            crate::oauth::client_keys::to_atrium_jwk(&key).expect("convert jwk (primary)");
        let jwk_domain =
            crate::oauth::client_keys::to_atrium_jwk(&key).expect("convert jwk (domain)");

        registry.register_for_kid(
            primary_client_id_url,
            &key.kid,
            Arc::new(build_single_key_client(primary_client_id_url, jwk_primary)),
        );
        registry.register_for_kid(
            domain_client_id_url,
            &key.kid,
            Arc::new(build_single_key_client(domain_client_id_url, jwk_domain)),
        );

        assert!(
            registry
                .get_for_kid(primary_client_id_url, &key.kid)
                .is_some()
        );
        assert!(
            registry
                .get_for_kid(domain_client_id_url, &key.kid)
                .is_some()
        );

        registry.evict_kid(&key.kid);

        assert!(
            registry
                .get_for_kid(primary_client_id_url, &key.kid)
                .is_none(),
            "entry under the primary's client_id_url must be evicted"
        );
        assert!(
            registry
                .get_for_kid(domain_client_id_url, &key.kid)
                .is_none(),
            "entry under the domain's client_id_url must also be evicted"
        );
    }

    #[tokio::test]
    async fn evict_kid_clears_linked_repos_by_kid_but_spares_other_kids() {
        let pool = crate::test_support::memory_pool().await;
        let backend = DatabaseBackend::Sqlite;
        let http = Arc::new(crate::http_retry::HappyViewHttpClient::default());
        let primary = atrium_oauth::OAuthClient::new(OAuthClientConfig {
            client_metadata: AtprotoLocalhostClientMetadata {
                redirect_uris: None,
                scopes: None,
            },
            keys: None,
            state_store: DbStateStore::new(pool.clone(), backend),
            session_store: DbSessionStore::new(pool.clone(), backend),
            resolver: OAuthResolverConfig {
                did_resolver: CommonDidResolver::new(CommonDidResolverConfig {
                    plc_directory_url: "https://plc.directory".into(),
                    http_client: Arc::clone(&http),
                }),
                handle_resolver: AtprotoHandleResolver::new(AtprotoHandleResolverConfig {
                    dns_txt_resolver: NativeDnsResolver::new(),
                    http_client: Arc::clone(&http),
                }),
                authorization_server_metadata: Default::default(),
                protected_resource_metadata: Default::default(),
            },
            http_client: crate::http_retry::HappyViewHttpClient::default(),
        })
        .expect("primary loopback client");
        let registry = OAuthClientRegistry::new(Arc::new(primary));

        let client_id_url = "https://example.com/oauth-client-metadata.json";

        let build_single_key_client = |jwk: jose_jwk::Jwk| {
            atrium_oauth::OAuthClient::new(OAuthClientConfig {
                client_metadata: AtprotoClientMetadata {
                    client_id: client_id_url.to_string(),
                    client_uri: Some("https://example.com".to_string()),
                    redirect_uris: vec!["https://example.com/auth/callback".to_string()],
                    token_endpoint_auth_method: AuthMethod::PrivateKeyJwt,
                    grant_types: vec![GrantType::AuthorizationCode, GrantType::RefreshToken],
                    scopes: vec![atrium_oauth::Scope::Known(
                        atrium_oauth::KnownScope::Atproto,
                    )],
                    jwks_uri: None,
                    token_endpoint_auth_signing_alg: Some("ES256".to_string()),
                },
                keys: Some(vec![jwk]),
                state_store: DbStateStore::new(pool.clone(), backend),
                session_store: DbSessionStore::new(pool.clone(), backend),
                resolver: OAuthResolverConfig {
                    did_resolver: CommonDidResolver::new(CommonDidResolverConfig {
                        plc_directory_url: "https://plc.directory".into(),
                        http_client: Arc::clone(&http),
                    }),
                    handle_resolver: AtprotoHandleResolver::new(AtprotoHandleResolverConfig {
                        dns_txt_resolver: NativeDnsResolver::new(),
                        http_client: Arc::clone(&http),
                    }),
                    authorization_server_metadata: Default::default(),
                    protected_resource_metadata: Default::default(),
                },
                http_client: crate::http_retry::HappyViewHttpClient::default(),
            })
            .expect("single-key client")
        };

        let key_x = crate::oauth::client_keys::generate_client_key("test-owner")
            .expect("generate client key x");
        let jwk_x = crate::oauth::client_keys::to_atrium_jwk(&key_x).expect("convert key x");
        let key_y = crate::oauth::client_keys::generate_client_key("test-owner")
            .expect("generate client key y");
        let jwk_y = crate::oauth::client_keys::to_atrium_jwk(&key_y).expect("convert key y");

        registry
            .register_linked_repos_for_kid(&key_x.kid, Arc::new(build_single_key_client(jwk_x)));
        registry
            .register_linked_repos_for_kid(&key_y.kid, Arc::new(build_single_key_client(jwk_y)));

        assert!(registry.get_linked_repos_for_kid(&key_x.kid).is_some());
        assert!(registry.get_linked_repos_for_kid(&key_y.kid).is_some());

        registry.evict_kid(&key_x.kid);

        assert!(
            registry.get_linked_repos_for_kid(&key_x.kid).is_none(),
            "evicted kid x's linked-repos entry must be gone"
        );
        assert!(
            registry.get_linked_repos_for_kid(&key_y.kid).is_some(),
            "kid y's linked-repos entry must survive evicting kid x"
        );
    }
}

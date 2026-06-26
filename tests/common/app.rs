use atrium_identity::did::{CommonDidResolver, CommonDidResolverConfig};
use atrium_identity::handle::{AtprotoHandleResolver, AtprotoHandleResolverConfig};
use atrium_oauth::{
    AtprotoLocalhostClientMetadata, DefaultHttpClient, KnownScope, OAuthClientConfig,
    OAuthResolverConfig, Scope,
};
use axum::Router;
use axum::http::Request;
use base64::Engine as _;
use happyview::config::Config;
use happyview::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use happyview::lexicon::LexiconRegistry;
use happyview::{AppState, server};
use tokio::sync::watch;
use wiremock::MockServer;

use crate::common::db;

pub struct TestApp {
    pub router: Router,
    pub state: AppState,
    pub mock_server: MockServer,
    pub admin_did: String,
    pub admin_token: String,
    _db_lock: Option<sqlx::AnyPool>,
}

impl TestApp {
    pub async fn new() -> Self {
        Self::new_with_registry_config(
            happyview::plugin::official_registry::RegistryConfig::production(),
        )
        .await
    }

    pub async fn new_with_registry_config(
        registry_config: happyview::plugin::official_registry::RegistryConfig,
    ) -> Self {
        let _db_lock = db::acquire_test_lock().await;
        let pool = db::test_pool().await;
        let backend = db::test_backend();
        db::truncate_all(&pool).await;

        let mock_server = MockServer::start().await;
        let mock_url = mock_server.uri();

        let admin_did = "did:plc:testadmin".to_string();
        let admin_token = "test-admin-token".to_string();

        let config = Config {
            host: "127.0.0.1".into(),
            port: 0,
            database_url: String::new(),
            database_backend: backend,
            public_url: "http://127.0.0.1:0".into(),
            session_secret: "test-secret".into(),
            jetstream_url: "wss://jetstream1.us-east.bsky.network".into(),
            relay_url: mock_url.clone(),
            plc_url: mock_url.clone(),
            static_dir: "./web/out".into(),
            base_path: None,
            event_log_retention_days: 30,
            app_name: None,
            logo_uri: None,
            tos_uri: None,
            policy_uri: None,
            token_encryption_key: None,
            default_rate_limit_capacity: 100,
            default_rate_limit_refill_rate: 2.0,
        };

        let sql = adapt_sql(
            "INSERT INTO happyview_users (id, did, is_super, created_at) VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING",
            backend,
        );
        sqlx::query(&sql)
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(&admin_did)
            .bind(1_i32)
            .bind(now_rfc3339())
            .execute(&pool)
            .await
            .expect("failed to seed admin user");

        let lexicons = LexiconRegistry::new();
        lexicons
            .load_from_db(&pool)
            .await
            .expect("failed to load lexicons");

        let initial_collections = lexicons.get_record_collections().await;
        let (collections_tx, _collections_rx) = watch::channel(initial_collections);
        let (labeler_subscriptions_tx, _) = watch::channel(());

        let atrium_http = std::sync::Arc::new(DefaultHttpClient::default());
        let did_resolver = CommonDidResolver::new(CommonDidResolverConfig {
            plc_directory_url: "https://plc.directory".into(),
            http_client: std::sync::Arc::clone(&atrium_http),
        });
        let handle_resolver = AtprotoHandleResolver::new(AtprotoHandleResolverConfig {
            dns_txt_resolver: happyview::dns::NativeDnsResolver::new(),
            http_client: atrium_http,
        });
        let oauth_pool = db::test_pool().await;
        let oauth = atrium_oauth::OAuthClient::new(OAuthClientConfig {
            client_metadata: AtprotoLocalhostClientMetadata {
                redirect_uris: Some(vec!["http://127.0.0.1:0/auth/callback".into()]),
                scopes: Some(vec![Scope::Known(KnownScope::Atproto)]),
            },
            keys: None,
            state_store: happyview::auth::oauth_store::DbStateStore::new(
                oauth_pool.clone(),
                backend,
            ),
            session_store: happyview::auth::oauth_store::DbSessionStore::new(oauth_pool, backend),
            resolver: OAuthResolverConfig {
                did_resolver,
                handle_resolver,
                authorization_server_metadata: Default::default(),
                protected_resource_metadata: Default::default(),
            },
        })
        .expect("Failed to create test OAuth client");

        let domain_cache = happyview::domain::DomainCache::new();
        domain_cache
            .insert(happyview::domain::Domain {
                id: uuid::Uuid::new_v4().to_string(),
                url: "http://127.0.0.1:0".to_string(),
                is_primary: true,
                created_at: now_rfc3339(),
                updated_at: now_rfc3339(),
            })
            .await;

        let state = AppState {
            config,
            http: reqwest::Client::new(),
            db: pool.clone(),
            db_backend: backend,
            domain_cache,
            lexicons,
            collections_tx,
            labeler_subscriptions_tx,
            rate_limiter: happyview::rate_limit::RateLimiter::new(
                happyview::rate_limit::RateLimitDefaults {
                    query_cost: 1,
                    procedure_cost: 1,
                    proxy_cost: 1,
                },
            ),
            oauth: std::sync::Arc::new(happyview::auth::OAuthClientRegistry::new(
                std::sync::Arc::new(oauth),
            )),
            oauth_state_store: happyview::auth::oauth_store::DbStateStore::new(
                pool.clone(),
                backend,
            ),
            cookie_key: axum_extra::extract::cookie::Key::derive_from(
                b"test-secret-that-is-at-least-32-bytes-long",
            ),
            plugin_registry: std::sync::Arc::new(happyview::plugin::PluginRegistry::new()),
            wasm_runtime: std::sync::Arc::new(
                happyview::plugin::WasmRuntime::new().expect("wasm runtime"),
            ),
            attestation_signer: None,
            official_registry: std::sync::Arc::new(tokio::sync::RwLock::new(
                happyview::plugin::official_registry::OfficialRegistryState::default(),
            )),
            official_registry_config: registry_config,
            proxy_config: std::sync::Arc::new(arc_swap::ArcSwap::new(std::sync::Arc::new(
                happyview::proxy_config::ProxyConfig::default(),
            ))),
            backfill_db: pool.clone(),
            backfill_events_tx: tokio::sync::broadcast::channel(16).0,
            verbose_event_logging: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };

        let router = Self::build_router(&state);

        Self {
            router,
            state,
            mock_server,
            admin_did,
            _db_lock,
            admin_token,
        }
    }

    fn build_router(state: &AppState) -> axum::Router {
        server::router(state.clone()).layer(axum::middleware::from_fn(
            |mut req: axum::extract::Request, next: axum::middleware::Next| async move {
                if !req.headers().contains_key("host") {
                    req.headers_mut()
                        .insert("host", axum::http::HeaderValue::from_static("127.0.0.1"));
                }
                next.run(req).await
            },
        ))
    }

    pub fn rebuild_router(&mut self) {
        self.router = Self::build_router(&self.state);
    }

    pub async fn new_with_base_path(base_path: &str) -> Self {
        let mut app = Self::new().await;
        app.state.config.base_path = Some(base_path.to_string());
        app.rebuild_router();
        app
    }

    pub async fn new_with_encryption() -> Self {
        let mut app = Self::new().await;
        app.state.config.token_encryption_key = Some([0x42u8; 32]);
        app.rebuild_router();
        app
    }

    /// Create an API client in the database for testing.
    /// Returns (client_key, client_secret, api_client_id).
    pub async fn create_api_client(
        &self,
        client_type: &str,
        allowed_origins: Option<Vec<String>>,
    ) -> (String, String, String) {
        use happyview::db::{adapt_sql, now_rfc3339};
        use rand::RngCore;
        use sha2::{Digest, Sha256};

        let mut key_bytes = [0u8; 16];
        rand::rng().fill_bytes(&mut key_bytes);
        let client_key = format!("hvc_{}", hex::encode(key_bytes));

        let mut secret_bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut secret_bytes);
        let client_secret = format!("hvs_{}", hex::encode(secret_bytes));
        let secret_hash = hex::encode(Sha256::digest(client_secret.as_bytes()));

        let id = uuid::Uuid::new_v4().to_string();
        let now = now_rfc3339();
        let origins_json = allowed_origins
            .as_ref()
            .map(|o| serde_json::to_string(o).unwrap_or_else(|_| "[]".to_string()));

        let sql = adapt_sql(
            "INSERT INTO happyview_api_clients (id, client_key, client_secret_hash, name, client_id_url, client_uri, redirect_uris, scopes, client_type, allowed_origins, is_active, created_by, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?)",
            self.state.db_backend,
        );

        sqlx::query(&sql)
            .bind(&id)
            .bind(&client_key)
            .bind(&secret_hash)
            .bind("test-client")
            .bind(format!("https://test.example.com/oauth/{}", &id[..8]))
            .bind("https://test.example.com")
            .bind("[]")
            .bind("atproto")
            .bind(client_type)
            .bind(&origins_json)
            .bind(&self.admin_did)
            .bind(&now)
            .bind(&now)
            .execute(&self.state.db)
            .await
            .expect("failed to create test API client");

        (client_key, client_secret, id)
    }

    /// Build a Cookie header that authenticates as the admin user.
    pub fn admin_cookie(&self) -> (axum::http::HeaderName, axum::http::HeaderValue) {
        crate::common::auth::admin_cookie_header(&self.admin_did, &self.state.cookie_key)
    }

    /// Return a `Request::builder()` pre-configured with the admin auth cookie.
    pub fn authed_request(&self) -> axum::http::request::Builder {
        let cookie = self.admin_cookie();
        Request::builder().header(cookie.0, cookie.1)
    }

    pub async fn setup_did_web(&mut self) -> String {
        use p256::ecdsa::SigningKey;
        use rand::RngCore;

        let encryption_key = [0x42u8; 32];
        self.state.config.token_encryption_key = Some(encryption_key);

        let mut key_bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut key_bytes);
        let signing_key = SigningKey::from_bytes((&key_bytes[..]).into()).unwrap();
        let private_bytes = signing_key.to_bytes();
        let encrypted = happyview::plugin::encryption::encrypt(&encryption_key, &private_bytes)
            .expect("encryption failed");
        let enc_b64 = base64::engine::general_purpose::STANDARD.encode(encrypted);

        let url = &self.state.config.public_url;
        let host = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .unwrap_or(url);
        let did = format!("did:web:{}", host.replace(':', "%3A"));

        happyview::service_identity::upsert_identity(
            &self.state.db,
            self.state.db_backend,
            &happyview::service_identity::IdentityMode::DidWeb,
            None,
            Some(&enc_b64),
            None,
            None,
        )
        .await
        .expect("failed to upsert service identity");

        happyview::service_identity::mark_setup_complete(&self.state.db, self.state.db_backend)
            .await
            .expect("failed to mark setup complete");

        self.rebuild_router();

        did
    }

    pub async fn create_service_entry(
        &self,
        fragment_id: &str,
        service_type: &str,
        access_mode: &str,
    ) -> i64 {
        let entry = happyview::service_entries::create_entry(
            &self.state.db,
            self.state.db_backend,
            &happyview::service_entries::CreateServiceEntry {
                fragment_id: fragment_id.to_string(),
                service_type: service_type.to_string(),
            },
        )
        .await
        .expect("failed to create service entry");

        if access_mode != "all" {
            happyview::service_entries::update_entry(
                &self.state.db,
                self.state.db_backend,
                entry.id,
                &happyview::service_entries::UpdateServiceEntry {
                    fragment_id: None,
                    service_type: None,
                    access_mode: Some(access_mode.to_string()),
                },
            )
            .await
            .expect("failed to update service entry access mode");
        }

        entry.id
    }

    pub async fn add_entry_xrpcs(&self, entry_id: i64, xrpcs: &[&str]) {
        let xrpc_strings: Vec<String> = xrpcs.iter().map(|s| s.to_string()).collect();
        happyview::service_entries::add_entry_xrpcs(
            &self.state.db,
            self.state.db_backend,
            entry_id,
            &xrpc_strings,
        )
        .await
        .expect("failed to add entry xrpcs");
    }

    pub async fn service_auth_jwt(
        &self,
        plc_store: &crate::common::plc::PlcStore,
        issuer_did: &str,
        instance_did: &str,
        aud_fragment: &str,
    ) -> String {
        use base64::Engine;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use p256::ecdsa::{SigningKey, signature::Signer};
        use rand::RngCore;

        let mut key_bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut key_bytes);
        let signing_key = SigningKey::from_bytes((&key_bytes[..]).into()).unwrap();
        let public_key = signing_key.verifying_key();
        let compressed = public_key.to_encoded_point(true);

        let did_doc = crate::common::plc::test_did_document(issuer_did, compressed.as_bytes());
        plc_store
            .write()
            .await
            .insert(issuer_did.to_string(), did_doc);

        let header = serde_json::json!({"alg": "ES256"});
        let payload = serde_json::json!({
            "iss": issuer_did,
            "aud": format!("{}{}", instance_did, aud_fragment),
            "exp": chrono::Utc::now().timestamp() as u64 + 60,
        });

        let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
        let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
        let message = format!("{}.{}", header_b64, payload_b64);

        let signature: p256::ecdsa::Signature = signing_key.sign(message.as_bytes());
        let sig_b64 = URL_SAFE_NO_PAD.encode(signature.to_bytes());

        format!("Bearer {}.{}.{}", header_b64, payload_b64, sig_b64)
    }

    pub async fn setup_not_exposed(&mut self) {
        happyview::service_identity::upsert_identity(
            &self.state.db,
            self.state.db_backend,
            &happyview::service_identity::IdentityMode::NotExposed,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("failed to upsert not_exposed identity");

        happyview::service_identity::mark_setup_complete(&self.state.db, self.state.db_backend)
            .await
            .expect("failed to mark setup complete");

        self.rebuild_router();
    }

    pub async fn setup_did_plc(&mut self) -> String {
        use p256::ecdsa::SigningKey;
        use rand::RngCore;

        let encryption_key = [0x42u8; 32];
        self.state.config.token_encryption_key = Some(encryption_key);

        let mut key_bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut key_bytes);
        let signing_key = SigningKey::from_bytes((&key_bytes[..]).into()).unwrap();
        let private_bytes = signing_key.to_bytes();
        let encrypted = happyview::plugin::encryption::encrypt(&encryption_key, &private_bytes)
            .expect("encryption failed");
        let enc_b64 = base64::engine::general_purpose::STANDARD.encode(encrypted);

        let did = "did:plc:testinstance".to_string();

        happyview::service_identity::upsert_identity(
            &self.state.db,
            self.state.db_backend,
            &happyview::service_identity::IdentityMode::DidPlc,
            Some(&did),
            Some(&enc_b64),
            None,
            None,
        )
        .await
        .expect("failed to upsert did:plc identity");

        happyview::service_identity::mark_setup_complete(&self.state.db, self.state.db_backend)
            .await
            .expect("failed to mark setup complete");

        self.rebuild_router();

        did
    }

    pub async fn raw_service_auth_jwt(
        &self,
        plc_store: &crate::common::plc::PlcStore,
        issuer_did: &str,
        aud: &str,
        exp: u64,
    ) -> String {
        use base64::Engine;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use p256::ecdsa::{SigningKey, signature::Signer};
        use rand::RngCore;

        let mut key_bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut key_bytes);
        let signing_key = SigningKey::from_bytes((&key_bytes[..]).into()).unwrap();
        let public_key = signing_key.verifying_key();
        let compressed = public_key.to_encoded_point(true);

        let did_doc = crate::common::plc::test_did_document(issuer_did, compressed.as_bytes());
        plc_store
            .write()
            .await
            .insert(issuer_did.to_string(), did_doc);

        let header = serde_json::json!({"alg": "ES256"});
        let payload = serde_json::json!({
            "iss": issuer_did,
            "aud": aud,
            "exp": exp,
        });

        let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
        let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
        let message = format!("{}.{}", header_b64, payload_b64);

        let signature: p256::ecdsa::Signature = signing_key.sign(message.as_bytes());
        let sig_b64 = URL_SAFE_NO_PAD.encode(signature.to_bytes());

        format!("Bearer {}.{}.{}", header_b64, payload_b64, sig_b64)
    }

    pub async fn custom_service_auth_jwt(
        &self,
        plc_store: &crate::common::plc::PlcStore,
        issuer_did: &str,
        header: serde_json::Value,
        payload: serde_json::Value,
    ) -> String {
        use base64::Engine;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use p256::ecdsa::{SigningKey, signature::Signer};
        use rand::RngCore;

        let mut key_bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut key_bytes);
        let signing_key = SigningKey::from_bytes((&key_bytes[..]).into()).unwrap();
        let public_key = signing_key.verifying_key();
        let compressed = public_key.to_encoded_point(true);

        let did_doc = crate::common::plc::test_did_document(issuer_did, compressed.as_bytes());
        plc_store
            .write()
            .await
            .insert(issuer_did.to_string(), did_doc);

        let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
        let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
        let message = format!("{}.{}", header_b64, payload_b64);

        let signature: p256::ecdsa::Signature = signing_key.sign(message.as_bytes());
        let sig_b64 = URL_SAFE_NO_PAD.encode(signature.to_bytes());

        format!("Bearer {}.{}.{}", header_b64, payload_b64, sig_b64)
    }

    pub fn use_permissive_http_client(&mut self) {
        self.state.http = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .expect("failed to build permissive http client");
        self.rebuild_router();
    }

    pub fn did_web_service_auth_jwt(
        &self,
        signing_key: &p256::ecdsa::SigningKey,
        issuer_did: &str,
        instance_did: &str,
        aud_fragment: &str,
    ) -> String {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use p256::ecdsa::signature::Signer;

        let header = serde_json::json!({"alg": "ES256"});
        let payload = serde_json::json!({
            "iss": issuer_did,
            "aud": format!("{}{}", instance_did, aud_fragment),
            "exp": chrono::Utc::now().timestamp() as u64 + 60,
        });

        let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
        let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
        let message = format!("{}.{}", header_b64, payload_b64);

        let signature: p256::ecdsa::Signature = signing_key.sign(message.as_bytes());
        let sig_b64 = URL_SAFE_NO_PAD.encode(signature.to_bytes());

        format!("Bearer {}.{}.{}", header_b64, payload_b64, sig_b64)
    }

    /// Install a fake plugin directly into the registry at the given version.
    pub async fn install_fake_plugin(&self, id: &str, version: &str) {
        use happyview::plugin::{LoadedPlugin, PluginInfo, PluginSource};

        let plugin = LoadedPlugin {
            info: PluginInfo {
                id: id.to_string(),
                name: id.to_string(),
                version: version.to_string(),
                api_version: "1".to_string(),
                icon_url: None,
                required_secrets: vec![],
                auth_type: "openid".to_string(),
                config_schema: None,
            },
            source: PluginSource::Url {
                url: format!("https://example.com/{id}.wasm"),
                sha256: None,
            },
            wasm_bytes: vec![],
            manifest: None,
        };
        self.state.plugin_registry.register(plugin).await;
    }
}

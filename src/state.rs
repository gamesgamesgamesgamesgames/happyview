//! Building an [`AppState`] without the full server boot.

use std::sync::Arc;

use tokio::sync::watch;

use crate::AppState;
use crate::catalog::LexiconCatalog;
use crate::config::Config;

/// An [`AppState`] over `pool`, with no network dependency reachable and
/// every field at its inert default.
///
/// The `happyview test` runner and the test suites build their states here
/// and then assign the fields that differ, so a change to `AppState` is made
/// in this one place. The addresses are taken from `config` when it names
/// them and fall back to unroutable local ones when it does not.
pub fn state_with_pool_on(
    config: Config,
    pool: sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
) -> AppState {
    let plc_url = if config.plc_url.is_empty() {
        "https://plc.directory".to_string()
    } else {
        config.plc_url.clone()
    };
    let public_url = if config.public_url.is_empty() {
        "http://127.0.0.1:0".to_string()
    } else {
        config.public_url.trim_end_matches('/').to_string()
    };
    let metadata_url = format!("{public_url}/oauth-client-metadata.json");
    let callback = format!("{public_url}/auth/callback");
    let (tx, _) = watch::channel(vec![]);
    let (labeler_tx, _) = watch::channel(());
    let atrium_http = Arc::new(crate::http_retry::HappyViewHttpClient::default());
    let did_resolver = atrium_identity::did::CommonDidResolver::new(
        atrium_identity::did::CommonDidResolverConfig {
            plc_directory_url: plc_url.clone(),
            http_client: Arc::clone(&atrium_http),
        },
    );
    let handle_resolver = atrium_identity::handle::AtprotoHandleResolver::new(
        atrium_identity::handle::AtprotoHandleResolverConfig {
            dns_txt_resolver: crate::dns::NativeDnsResolver::new(),
            http_client: atrium_http,
        },
    );
    let scopes = vec![atrium_oauth::Scope::Known(
        atrium_oauth::KnownScope::Atproto,
    )];
    let oauth = atrium_oauth::OAuthClient::new(atrium_oauth::OAuthClientConfig {
        client_metadata: atrium_oauth::AtprotoLocalhostClientMetadata {
            redirect_uris: Some(vec![callback.clone()]),
            scopes: Some(scopes.clone()),
        },
        keys: None,
        state_store: crate::auth::oauth_store::DbStateStore::new(pool.clone(), backend),
        session_store: crate::auth::oauth_store::DbSessionStore::new(pool.clone(), backend),
        resolver: atrium_oauth::OAuthResolverConfig {
            did_resolver,
            handle_resolver,
            authorization_server_metadata: Default::default(),
            protected_resource_metadata: Default::default(),
        },
        http_client: crate::http_retry::HappyViewHttpClient::default(),
    })
    .expect("test OAuth client");
    let project = crate::project::shared_empty();
    let source = Arc::new(crate::builtin_source::BuiltinSource::detached());
    let state = AppState {
        config,
        http: reqwest::Client::new(),
        db: pool.clone(),
        backfill_db: pool.clone(),
        db_backend: backend,
        records: crate::record_store::RecordStore::new(pool.clone(), backend),
        source: source.clone(),
        archive: Default::default(),
        domain_cache: crate::domain::DomainCache::new(),
        lexicons: LexiconCatalog::new(project.clone()),
        scripts: crate::catalog::ScriptCatalog::new(pool.clone(), backend, project.clone()),
        project,
        collections_tx: tx,
        labeler_subscriptions_tx: labeler_tx,
        labeler_subscription_cache: Arc::new(crate::labeler::SubscriptionCache::default()),
        rate_limiter: crate::rate_limit::RateLimiter::new(crate::rate_limit::RateLimitDefaults {
            query_cost: 1,
            procedure_cost: 1,
            proxy_cost: 1,
        }),
        oauth: Arc::new(crate::auth::OAuthClientRegistry::new(Arc::new(oauth))),
        oauth_state_store: crate::auth::oauth_store::DbStateStore::new(pool.clone(), backend),
        linked_repos_client: Arc::new(
            crate::linked_repos::client::build(
                plc_url.as_str(),
                metadata_url.as_str(),
                public_url.as_str(),
                callback.clone(),
                true,
                scopes,
                crate::auth::oauth_store::DbStateStore::new(pool.clone(), backend),
                pool.clone(),
                backend,
                None,
            )
            .expect("test linked-repo OAuth client"),
        ),
        linked_repos_client_kid: None,
        cookie_key: axum_extra::extract::cookie::Key::derive_from(
            b"test-secret-that-is-at-least-32-bytes-long",
        ),
        plugin_registry: Arc::new(crate::plugin::PluginRegistry::new()),
        wasm_runtime: Arc::new(crate::plugin::WasmRuntime::new().expect("wasm runtime")),
        attestation_signer: None,
        official_registry: Arc::new(tokio::sync::RwLock::new(
            crate::plugin::official_registry::OfficialRegistryState::default(),
        )),
        official_registry_config: crate::plugin::official_registry::RegistryConfig::production(),
        proxy_config: Arc::new(arc_swap::ArcSwap::new(Arc::new(
            crate::proxy_config::ProxyConfig::default(),
        ))),
        backfill_events_tx: tokio::sync::broadcast::channel(16).0,
        verbose_event_logging: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        client_jwks: Vec::new(),
        telemetry_counters: Arc::new(crate::telemetry::counters::Counters::new()),
        script_limits: Arc::new(crate::lua::limits::ScriptLimits::default()),
        test: None,
    };
    // Backfills and their admin routes work on this state; nothing is started.
    source.attach(&state, None);
    state
}

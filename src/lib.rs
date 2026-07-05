pub mod admin;
pub mod auth;
pub mod config;
pub mod db;
pub mod delegation;
pub mod dev_happyview;
pub mod dns;
pub mod domain;
pub mod domain_middleware;
pub mod error;
pub mod event_log;
pub mod external_auth;
pub mod feature_flags;
pub mod feature_middleware;
pub mod http_retry;
pub mod jetstream;
pub mod jobs;
pub mod labeler;
pub mod lexicon;
pub mod lua;
pub mod lua_analysis;
pub mod oauth;
pub mod plc;
pub mod plugin;
pub mod profile;
pub mod proxy_config;
pub mod rate_limit;
pub mod record_handler;
pub mod record_refs;
pub mod repo;
pub mod resolve;
pub mod server;
pub mod service_entries;
pub mod service_identity;
pub mod setup;
pub mod spaces;
pub mod verification_methods;
pub mod xrpc;

use auth::oauth_store::{DbSessionStore, DbStateStore};
use config::Config;
use db::DatabaseBackend;
use dns::NativeDnsResolver;
use lexicon::LexiconRegistry;
use plugin::official_registry::{RegistryConfig, SharedRegistry};
use rate_limit::RateLimiter;
use std::sync::Arc;
use tokio::sync::watch;

use atrium_identity::did::CommonDidResolver;
use atrium_identity::handle::AtprotoHandleResolver;
use atrium_oauth::DefaultHttpClient;

pub type HappyViewOAuthClient = atrium_oauth::OAuthClient<
    DbStateStore,
    DbSessionStore,
    CommonDidResolver<DefaultHttpClient>,
    AtprotoHandleResolver<NativeDnsResolver, DefaultHttpClient>,
>;

pub type HappyViewOAuthSession = atrium_oauth::OAuthSession<
    DefaultHttpClient,
    CommonDidResolver<DefaultHttpClient>,
    AtprotoHandleResolver<NativeDnsResolver, DefaultHttpClient>,
    DbSessionStore,
>;

#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub http: reqwest::Client,
    pub db: sqlx::AnyPool,
    pub backfill_db: sqlx::AnyPool,
    pub db_backend: DatabaseBackend,
    pub domain_cache: domain::DomainCache,
    pub lexicons: LexiconRegistry,
    pub collections_tx: watch::Sender<Vec<String>>,
    pub labeler_subscriptions_tx: watch::Sender<()>,
    pub rate_limiter: Arc<RateLimiter>,
    pub oauth: Arc<auth::OAuthClientRegistry>,
    pub oauth_state_store: DbStateStore,
    pub cookie_key: axum_extra::extract::cookie::Key,
    pub plugin_registry: Arc<plugin::PluginRegistry>,
    pub wasm_runtime: Arc<plugin::WasmRuntime>,
    pub attestation_signer: Option<Arc<plugin::attestation::AttestationSigner>>,
    pub official_registry: SharedRegistry,
    pub official_registry_config: RegistryConfig,
    pub proxy_config: Arc<arc_swap::ArcSwap<proxy_config::ProxyConfig>>,
    pub backfill_events_tx: tokio::sync::broadcast::Sender<crate::admin::types::BackfillEvent>,
    pub verbose_event_logging: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl axum::extract::FromRef<AppState> for axum_extra::extract::cookie::Key {
    fn from_ref(state: &AppState) -> Self {
        state.cookie_key.clone()
    }
}

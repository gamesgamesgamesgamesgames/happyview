//! Shared fixtures for `cargo test --lib`.
//!
//! Building an `AppState` requires a fully-constructed OAuth client, which is
//! ~100 lines of boilerplate that several test modules had each copied. This
//! module exists so new tests can take a pool and get a usable state back.

use std::sync::Arc;

use tokio::sync::watch;

use crate::AppState;
use crate::config::Config;
use crate::lexicon::LexiconRegistry;

/// An in-memory SQLite pool. `max_connections(1)` is required: every
/// connection to `sqlite::memory:` gets its own private database, so a larger
/// pool would hand later queries an empty schema.
pub async fn memory_pool() -> sqlx::AnyPool {
    sqlx::any::install_default_drivers();
    sqlx::pool::PoolOptions::<sqlx::Any>::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("connect to in-memory sqlite")
}

/// A [`memory_pool`] with every SQLite migration applied. A hand-written
/// `CREATE TABLE` in a test drifts silently from the real schema — a
/// migration can add a column the copy never gets, and a test against the
/// stale copy keeps passing while the code it exercises breaks against a real
/// database. The path is anchored to the crate manifest so the helper works
/// from any working directory.
pub async fn migrated_memory_pool() -> sqlx::AnyPool {
    let pool = memory_pool().await;
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations/sqlite");
    sqlx::migrate::Migrator::new(dir)
        .await
        .expect("load sqlite migrations")
        .run(&pool)
        .await
        .expect("run migrations");
    pool
}

/// The encryption key every test uses.
///
/// One value across the whole suite: DB-backed tests share a database, and a
/// `#atproto_space` key written under one encryption key cannot be decrypted
/// under another. Differing keys surface later as an opaque "failed to decrypt
/// verification key" on the first space write.
pub const TEST_ENCRYPTION_KEY: [u8; 32] = [0x42u8; 32];

/// Provision the `#atproto_space` signing key for a test.
///
/// Idempotent, and safe to race: every test uses [`TEST_ENCRYPTION_KEY`], so an
/// existing row is always one this state can decrypt.
pub async fn provision_space_signing_key(state: &AppState) {
    let Some(key) = state.config.token_encryption_key.as_ref() else {
        return;
    };

    // A row written under a different encryption key cannot be decrypted, and
    // `ensure_atproto_space_method` returns an existing row as is. Replace it
    // so space writes do not fail with a decryption error.
    let usable = crate::verification_methods::get_private_key_bytes(
        &state.db,
        state.db_backend,
        "#atproto_space",
        key,
    )
    .await
    .ok()
    .flatten()
    .is_some();

    if !usable {
        let sql = crate::db::adapt_sql(
            "DELETE FROM happyview_verification_methods WHERE fragment_id = ?",
            state.db_backend,
        );
        let _ = crate::db::query(&sql)
            .bind("#atproto_space")
            .execute(&state.db)
            .await;
    }

    crate::verification_methods::ensure_atproto_space_method(&state.db, state.db_backend, key)
        .await
        .expect("provision #atproto_space key");
}

/// Build an `AppState` backed by `pool`, wired for SQLite with no network
/// dependencies reachable (PLC and OAuth point at unroutable local ports).
pub fn test_state_with_pool(pool: sqlx::AnyPool) -> AppState {
    let config = Config {
        host: "127.0.0.1".into(),
        port: 3000,
        database_url: String::new(),
        database_backend: crate::db::DatabaseBackend::Sqlite,
        sqlite_journal_size_limit: crate::db::DEFAULT_JOURNAL_SIZE_LIMIT,
        public_url: String::new(),
        user_agent: String::new(),
        session_secret: "test-secret".into(),
        jetstream_url: String::new(),
        relay_url: String::new(),
        plc_url: String::new(),
        static_dir: String::new(),
        base_path: None,
        event_log_retention_days: 30,
        app_name: None,
        logo_uri: None,
        tos_uri: None,
        policy_uri: None,
        // Spaces sign every commit with the `#atproto_space` key, which is
        // stored encrypted, so space writes need this set.
        token_encryption_key: Some(TEST_ENCRYPTION_KEY),
        default_rate_limit_capacity: 100,
        default_rate_limit_refill_rate: 2.0,
        telemetry_collector_url: String::new(),
    };
    let (tx, _) = watch::channel(vec![]);
    let (labeler_tx, _) = watch::channel(());
    let backend = crate::db::DatabaseBackend::Sqlite;
    let atrium_http = Arc::new(crate::http_retry::HappyViewHttpClient::default());
    let did_resolver = atrium_identity::did::CommonDidResolver::new(
        atrium_identity::did::CommonDidResolverConfig {
            plc_directory_url: "https://plc.directory".into(),
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
            redirect_uris: Some(vec!["http://127.0.0.1:0/auth/callback".into()]),
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
    AppState {
        config,
        http: reqwest::Client::new(),
        db: pool.clone(),
        backfill_db: pool.clone(),
        db_backend: backend,
        domain_cache: crate::domain::DomainCache::new(),
        lexicons: LexiconRegistry::new(),
        collections_tx: tx,
        labeler_subscriptions_tx: labeler_tx,
        rate_limiter: crate::rate_limit::RateLimiter::new(crate::rate_limit::RateLimitDefaults {
            query_cost: 1,
            procedure_cost: 1,
            proxy_cost: 1,
        }),
        oauth: Arc::new(crate::auth::OAuthClientRegistry::new(Arc::new(oauth))),
        oauth_state_store: crate::auth::oauth_store::DbStateStore::new(pool.clone(), backend),
        linked_repos_client: Arc::new(
            crate::linked_repos::client::build(
                "https://plc.directory",
                "http://127.0.0.1:0/oauth-client-metadata.json",
                "http://127.0.0.1:0",
                "http://127.0.0.1:0/auth/callback".into(),
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
    }
}

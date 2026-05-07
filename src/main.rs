use std::sync::Arc;

use happyview::auth::oauth_store::{DbSessionStore, DbStateStore};
use happyview::config::Config;
use happyview::db;
use happyview::dns::NativeDnsResolver;
use happyview::lexicon::{LexiconRegistry, ParsedLexicon, ProcedureAction};
use happyview::rate_limit::{RateLimitDefaults, RateLimiter};
use happyview::resolve::{fetch_lexicon_from_pds, resolve_nsid_authority};
use happyview::{AppState, jetstream, labeler, server};
use sqlx::Row;
use tokio::sync::watch;
use tracing::{info, warn};

use atrium_identity::did::{CommonDidResolver, CommonDidResolverConfig};
use atrium_identity::handle::{AtprotoHandleResolver, AtprotoHandleResolverConfig};
use atrium_oauth::{
    AtprotoClientMetadata, AtprotoLocalhostClientMetadata, AuthMethod, DefaultHttpClient,
    GrantType, KnownScope, OAuthClientConfig, OAuthResolverConfig, Scope,
};

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    // Install rustls crypto provider early so all TLS users (jetstream, labeler, etc.) can find it.
    let _ = rustls::crypto::ring::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "happyview=debug,tower_http=debug".parse().unwrap()),
        )
        .init();

    let config = Config::from_env();
    let db_backend = config.database_backend;

    // Connect to database and run migrations.
    let db_pool = db::connect(&config.database_url, db_backend).await;

    info!(
        backend = ?db_backend,
        "connected to database"
    );

    // Backfill record_refs in the background (first run after upgrade)
    {
        let db_bg = db_pool.clone();
        let backend = db_backend;
        tokio::spawn(async move {
            let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM record_refs")
                .fetch_one(&db_bg)
                .await
                .expect("failed to count record_refs");

            if count.0 == 0 {
                info!("backfilling record_refs table in background...");
                let total: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM records")
                    .fetch_one(&db_bg)
                    .await
                    .expect("failed to count records");
                let total = total.0 as usize;

                let batch_size = 1000i64;
                let mut offset = 0i64;
                let mut processed = 0usize;

                let query = db::adapt_sql(
                    "SELECT uri, collection, record FROM records ORDER BY uri LIMIT ? OFFSET ?",
                    backend,
                );

                loop {
                    let batch: Vec<(String, String, String)> = sqlx::query_as(&query)
                        .bind(batch_size)
                        .bind(offset)
                        .fetch_all(&db_bg)
                        .await
                        .expect("failed to fetch records for backfill");

                    if batch.is_empty() {
                        break;
                    }

                    for (uri, collection, record_str) in &batch {
                        let record: serde_json::Value =
                            serde_json::from_str(record_str).unwrap_or(serde_json::Value::Null);
                        if let Err(e) = happyview::record_refs::sync_refs(
                            &db_bg, uri, collection, &record, backend,
                        )
                        .await
                        {
                            warn!(uri = uri.as_str(), "failed to backfill refs: {e}");
                        }
                    }

                    processed += batch.len();
                    offset += batch_size;

                    if processed.is_multiple_of(10000) || processed == total {
                        info!("backfill progress: {processed}/{total}");
                    }
                }

                info!("backfill complete: processed {processed} records");
            }
        });
    }

    let lexicons = LexiconRegistry::new();
    lexicons
        .load_from_db(&db_pool)
        .await
        .expect("failed to load lexicons");

    // Re-fetch all network lexicons from their respective PDSes.
    let http = reqwest::Client::new();
    let network_rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, authority_did, target_collection FROM lexicons WHERE source = 'network'",
    )
    .fetch_all(&db_pool)
    .await
    .unwrap_or_default();

    for (nsid, _authority_did, target_collection) in &network_rows {
        match resolve_nsid_authority(&http, &config.plc_url, nsid).await {
            Ok((did, pds_endpoint)) => {
                match fetch_lexicon_from_pds(&http, &pds_endpoint, &did, nsid).await {
                    Ok(lexicon_json) => {
                        match ParsedLexicon::parse(
                            lexicon_json.clone(),
                            1,
                            target_collection.clone(),
                            ProcedureAction::Upsert,
                            None,
                        ) {
                            Ok(parsed) => {
                                let now = db::now_rfc3339();
                                let update_sql = db::adapt_sql(
                                    "UPDATE lexicons SET lexicon_json = ?, last_fetched_at = ?, revision = revision + 1, updated_at = ? WHERE id = ? AND source = 'network'",
                                    db_backend,
                                );
                                let lexicon_json_str =
                                    serde_json::to_string(&lexicon_json).unwrap_or_default();
                                if let Err(e) = sqlx::query(&update_sql)
                                    .bind(&lexicon_json_str)
                                    .bind(&now)
                                    .bind(&now)
                                    .bind(nsid)
                                    .execute(&db_pool)
                                    .await
                                {
                                    warn!(nsid, "failed to update network lexicon in DB: {e}");
                                    continue;
                                }

                                lexicons.upsert(parsed).await;
                                info!(nsid, "refreshed network lexicon");
                            }
                            Err(e) => warn!(nsid, "failed to parse network lexicon: {e}"),
                        }
                    }
                    Err(e) => warn!(nsid, "failed to fetch network lexicon from PDS: {e}"),
                }
            }
            Err(e) => warn!(nsid, "failed to resolve network lexicon authority: {e}"),
        }
    }

    if !network_rows.is_empty() {
        info!(
            count = network_rows.len(),
            "processed network lexicons on startup"
        );
    }

    // Initialize plugin registry (with DB for persistence)
    let plugin_registry = Arc::new(happyview::plugin::PluginRegistry::with_db(
        db_pool.clone(),
        db_backend,
    ));

    // Initialize WASM runtime
    let wasm_runtime =
        Arc::new(happyview::plugin::WasmRuntime::new().expect("Failed to create WASM runtime"));

    // Initialize attestation signer (auto-generates key if none exists)
    let attestation_signer = match happyview::plugin::attestation::load_or_generate(
        &db_pool,
        db_backend,
        &config.public_url,
    )
    .await
    {
        Ok(signer) => {
            tracing::info!("Attestation signing enabled");
            Some(Arc::new(signer))
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to initialize attestation signer");
            None
        }
    };

    // Load plugins from PLUGIN_URLS env var
    if let Ok(urls) = std::env::var("PLUGIN_URLS") {
        for (id, url, sha256) in happyview::plugin::loader::parse_plugin_urls(&urls) {
            match happyview::plugin::loader::fetch_manifest(&http, &url).await {
                Ok(preview) => {
                    match happyview::plugin::loader::load_from_manifest(
                        &http,
                        &preview,
                        sha256.as_deref(),
                    )
                    .await
                    {
                        Ok(plugin) => {
                            tracing::info!(id = %id, "Loaded plugin from URL");
                            plugin_registry.register(plugin).await;
                        }
                        Err(e) => {
                            tracing::error!(id = %id, error = %e, "Failed to load plugin WASM");
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(id = %id, error = %e, "Failed to fetch plugin manifest");
                }
            }
        }
    }

    // Load plugins from directory
    let plugin_dir = std::path::Path::new("./plugins");
    if plugin_dir.exists()
        && let Ok(entries) = std::fs::read_dir(plugin_dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                match happyview::plugin::loader::load_from_file(&path).await {
                    Ok(plugin) => {
                        tracing::info!(id = %plugin.info.id, "Loaded plugin from file");
                        plugin_registry.register(plugin).await;
                    }
                    Err(e) => {
                        tracing::error!(path = %path.display(), error = %e, "Failed to load plugin");
                    }
                }
            }
        }
    }

    // Load plugins from database (added via admin UI)
    match plugin_registry.load_from_db(&http).await {
        Ok(count) if count > 0 => {
            tracing::info!(count = count, "Loaded plugins from database");
        }
        Ok(_) => {}
        Err(e) => {
            tracing::error!(error = %e, "Failed to load plugins from database");
        }
    }

    // Seed and load per-instance default token costs from instance_settings.
    let defaults = seed_and_load_rate_limit_defaults(&db_pool, db_backend).await;
    let rate_limiter = RateLimiter::new(defaults);
    tokio::spawn(rate_limiter.clone().spawn_cleanup());

    // Load per-client rate limit configs and identities from api_clients table.
    {
        type ClientRow = (
            String,
            String,
            String,
            Option<i32>,
            Option<f64>,
            Option<String>,
        );
        let client_rows: Vec<ClientRow> = sqlx::query_as(
            "SELECT client_key, client_secret_hash, client_uri, rate_limit_capacity, rate_limit_refill_rate, parent_client_id FROM api_clients WHERE is_active = 1",
        )
        .fetch_all(&db_pool)
        .await
        .unwrap_or_default();

        for (client_key, secret_hash, client_uri, capacity, refill_rate, _) in &client_rows {
            rate_limiter.register_client_identity(
                client_key.clone(),
                happyview::rate_limit::ClientIdentity {
                    secret_hash: secret_hash.clone(),
                    client_uri: client_uri.clone(),
                },
            );
            if let (Some(cap), Some(refill)) = (capacity, refill_rate) {
                rate_limiter.register_client_config(
                    client_key.clone(),
                    happyview::rate_limit::RateLimitConfig {
                        capacity: *cap as u32,
                        refill_rate: *refill,
                        default_query_cost: defaults.query_cost,
                        default_procedure_cost: defaults.procedure_cost,
                        default_proxy_cost: defaults.proxy_cost,
                    },
                );
            } else {
                rate_limiter.register_client_config(
                    client_key.clone(),
                    happyview::rate_limit::RateLimitConfig {
                        capacity: config.default_rate_limit_capacity,
                        refill_rate: config.default_rate_limit_refill_rate,
                        default_query_cost: defaults.query_cost,
                        default_procedure_cost: defaults.procedure_cost,
                        default_proxy_cost: defaults.proxy_cost,
                    },
                );
            }
        }
    }

    // Seed and load domain cache
    let domain_cache = happyview::domain::DomainCache::new();
    {
        let count_sql = happyview::db::adapt_sql("SELECT COUNT(*) FROM domains", db_backend);
        let row = sqlx::query(&count_sql)
            .fetch_one(&db_pool)
            .await
            .expect("Failed to count domains");
        let count: i64 = row.try_get(0).unwrap_or(0);

        if count == 0 {
            let id = uuid::Uuid::new_v4().to_string();
            let now = happyview::db::now_rfc3339();
            let insert_sql = happyview::db::adapt_sql(
                "INSERT INTO domains (id, url, is_primary, created_at, updated_at) VALUES (?, ?, 1, ?, ?)",
                db_backend,
            );
            sqlx::query(&insert_sql)
                .bind(&id)
                .bind(&config.public_url)
                .bind(&now)
                .bind(&now)
                .execute(&db_pool)
                .await
                .expect("Failed to insert primary domain");
            info!("Seeded primary domain: {}", config.public_url);
        } else {
            // Sync the primary domain URL with PUBLIC_URL if it changed
            let primary_sql = happyview::db::adapt_sql(
                "SELECT id, url FROM domains WHERE is_primary = 1",
                db_backend,
            );
            if let Some(row) = sqlx::query(&primary_sql)
                .fetch_optional(&db_pool)
                .await
                .expect("Failed to check primary domain")
            {
                let primary_url: String = row.try_get("url").unwrap_or_default();
                if primary_url != config.public_url {
                    let primary_id: String = row.try_get("id").unwrap_or_default();
                    let now = happyview::db::now_rfc3339();
                    let update_sql = happyview::db::adapt_sql(
                        "UPDATE domains SET url = ?, updated_at = ? WHERE id = ?",
                        db_backend,
                    );
                    sqlx::query(&update_sql)
                        .bind(&config.public_url)
                        .bind(&now)
                        .bind(&primary_id)
                        .execute(&db_pool)
                        .await
                        .expect("Failed to update primary domain URL");
                    info!(
                        "Updated primary domain URL from {} to {}",
                        primary_url, config.public_url
                    );
                }
            }
        }

        let select_sql = happyview::db::adapt_sql(
            "SELECT id, url, is_primary, created_at, updated_at FROM domains",
            db_backend,
        );
        let rows = sqlx::query(&select_sql)
            .fetch_all(&db_pool)
            .await
            .expect("Failed to load domains");

        let domains: Vec<happyview::domain::Domain> = rows
            .into_iter()
            .map(|row| {
                let is_primary_int: i32 = row.try_get("is_primary").unwrap_or(0);
                happyview::domain::Domain {
                    id: row.try_get("id").unwrap_or_default(),
                    url: row.try_get("url").unwrap_or_default(),
                    is_primary: is_primary_int != 0,
                    created_at: row.try_get("created_at").unwrap_or_default(),
                    updated_at: row.try_get("updated_at").unwrap_or_default(),
                }
            })
            .collect();

        let domain_count = domains.len();
        domain_cache.load(domains).await;
        info!("Loaded {} domain(s) into cache", domain_count);
    }

    // Build atrium-oauth client
    let dns = NativeDnsResolver::new();
    let callback_url = format!("{}/auth/callback", config.public_url.trim_end_matches('/'));
    let atrium_http = Arc::new(DefaultHttpClient::default());

    let did_resolver = CommonDidResolver::new(CommonDidResolverConfig {
        plc_directory_url: config.plc_url.clone(),
        http_client: Arc::clone(&atrium_http),
    });

    let handle_resolver = AtprotoHandleResolver::new(AtprotoHandleResolverConfig {
        dns_txt_resolver: dns,
        http_client: Arc::clone(&atrium_http),
    });

    let is_loopback = config.public_url.contains("127.0.0.1")
        || config.public_url.contains("[::1]")
        || config.public_url.contains("localhost");

    let resolver_config = OAuthResolverConfig {
        did_resolver,
        handle_resolver,
        authorization_server_metadata: Default::default(),
        protected_resource_metadata: Default::default(),
    };

    let oauth_state_store = DbStateStore::new(db_pool.clone(), db_backend);

    // HappyView's own default OAuth client always uses the `atproto` scope.
    // API clients configure their own scopes via the API Clients settings page.
    let oauth_scopes = vec![Scope::Known(KnownScope::Atproto)];

    let oauth_client = if is_loopback {
        info!("Using loopback OAuth client metadata (local development)");
        atrium_oauth::OAuthClient::new(OAuthClientConfig {
            client_metadata: AtprotoLocalhostClientMetadata {
                redirect_uris: Some(vec![callback_url]),
                scopes: Some(oauth_scopes),
            },
            keys: None,
            state_store: oauth_state_store.clone(),
            session_store: DbSessionStore::new(db_pool.clone(), db_backend),
            resolver: resolver_config,
        })
        .expect("Failed to create OAuth client")
    } else {
        atrium_oauth::OAuthClient::new(OAuthClientConfig {
            client_metadata: AtprotoClientMetadata {
                client_id: format!(
                    "{}/oauth-client-metadata.json",
                    config.public_url.trim_end_matches('/')
                ),
                client_uri: Some(config.public_url.clone()),
                redirect_uris: vec![callback_url],
                token_endpoint_auth_method: AuthMethod::None,
                grant_types: vec![GrantType::AuthorizationCode, GrantType::RefreshToken],
                scopes: oauth_scopes,
                jwks_uri: None,
                token_endpoint_auth_signing_alg: None,
            },
            keys: None,
            state_store: oauth_state_store.clone(),
            session_store: DbSessionStore::new(db_pool.clone(), db_backend),
            resolver: resolver_config,
        })
        .expect("Failed to create OAuth client")
    };

    if config.session_secret == "change-me-in-production-not-secure" {
        if db_backend == happyview::db::DatabaseBackend::Postgres {
            tracing::error!(
                "INSECURE SESSION SECRET — You are using the default session secret with a \
                 Postgres backend, which likely indicates a production deployment. \
                 Set SESSION_SECRET to a random string of at least 64 characters."
            );
        } else {
            warn!(
                "Using the default session secret. Set SESSION_SECRET to a random \
                 string in production."
            );
        }
    }

    let cookie_key =
        axum_extra::extract::cookie::Key::derive_from(config.session_secret.as_bytes());

    let initial_collections = lexicons.get_record_collections().await;
    let (collections_tx, collections_rx) = watch::channel(initial_collections);
    let (labeler_subscriptions_tx, labeler_subscriptions_rx) = watch::channel(());

    // Build the OAuth client registry and load API clients from DB
    let oauth_client_arc = Arc::new(oauth_client);
    let oauth_registry = Arc::new(happyview::auth::OAuthClientRegistry::new(Arc::clone(
        &oauth_client_arc,
    )));
    oauth_registry
        .load_from_db(
            &db_pool,
            db_backend,
            &config.plc_url,
            oauth_state_store.clone(),
            db_pool.clone(),
        )
        .await;

    // Register the primary domain's OAuth client in domain_clients
    if let Some(ref pd) = domain_cache.primary().await {
        oauth_registry.register_domain_client(pd.url.clone(), Arc::clone(&oauth_client_arc));
    }

    // Build OAuth clients for all non-primary domains
    let all_domains = domain_cache.all().await;
    for domain in &all_domains {
        if domain.is_primary {
            continue; // Already registered above
        }

        let domain_callback_url = format!("{}/auth/callback", domain.url.trim_end_matches('/'));
        let domain_client_id = format!(
            "{}/oauth-client-metadata.json",
            domain.url.trim_end_matches('/')
        );

        let domain_http = Arc::new(DefaultHttpClient::default());
        let domain_resolver = OAuthResolverConfig {
            did_resolver: CommonDidResolver::new(CommonDidResolverConfig {
                plc_directory_url: config.plc_url.clone(),
                http_client: Arc::clone(&domain_http),
            }),
            handle_resolver: AtprotoHandleResolver::new(AtprotoHandleResolverConfig {
                dns_txt_resolver: NativeDnsResolver::new(),
                http_client: Arc::clone(&domain_http),
            }),
            authorization_server_metadata: Default::default(),
            protected_resource_metadata: Default::default(),
        };

        match atrium_oauth::OAuthClient::new(OAuthClientConfig {
            client_metadata: AtprotoClientMetadata {
                client_id: domain_client_id,
                client_uri: Some(domain.url.clone()),
                redirect_uris: vec![domain_callback_url],
                token_endpoint_auth_method: AuthMethod::None,
                grant_types: vec![GrantType::AuthorizationCode, GrantType::RefreshToken],
                scopes: vec![Scope::Known(KnownScope::Atproto)],
                jwks_uri: None,
                token_endpoint_auth_signing_alg: None,
            },
            keys: None,
            state_store: oauth_state_store.clone(),
            session_store: DbSessionStore::new(db_pool.clone(), db_backend),
            resolver: domain_resolver,
        }) {
            Ok(client) => {
                info!(domain = %domain.url, "Registered domain OAuth client");
                oauth_registry.register_domain_client(domain.url.clone(), Arc::new(client));
            }
            Err(e) => {
                tracing::error!(domain = %domain.url, error = %e, "Failed to create domain OAuth client");
            }
        }
    }

    let official_registry: happyview::plugin::official_registry::SharedRegistry =
        std::sync::Arc::new(tokio::sync::RwLock::new(
            happyview::plugin::official_registry::OfficialRegistryState::default(),
        ));
    let official_registry_config =
        happyview::plugin::official_registry::RegistryConfig::production();
    happyview::plugin::official_registry::spawn_refresh_task(
        http.clone(),
        official_registry_config.clone(),
        official_registry.clone(),
    );

    let proxy_config = {
        let json_str =
            happyview::admin::settings::get_setting(&db_pool, "xrpc_proxy_config", db_backend)
                .await;
        let config = json_str
            .and_then(|s| serde_json::from_str::<happyview::proxy_config::ProxyConfig>(&s).ok())
            .unwrap_or_default();
        info!(mode = ?config.mode, nsid_count = config.nsids.len(), "Loaded XRPC proxy config");
        std::sync::Arc::new(arc_swap::ArcSwap::new(std::sync::Arc::new(config)))
    };

    let state = AppState {
        config: config.clone(),
        http,
        db: db_pool,
        db_backend,
        domain_cache: domain_cache.clone(),
        lexicons,
        collections_tx,
        labeler_subscriptions_tx,
        rate_limiter,
        oauth: oauth_registry,
        oauth_state_store,
        cookie_key,
        plugin_registry,
        wasm_runtime,
        attestation_signer,
        official_registry,
        official_registry_config,
        proxy_config,
    };

    jetstream::spawn(state.clone(), collections_rx);

    labeler::spawn(state.clone(), labeler_subscriptions_rx);
    tokio::spawn(labeler::spawn_label_gc(state.db.clone(), state.db_backend));

    tokio::spawn(happyview::event_log::spawn_retention_cleanup(
        state.db.clone(),
        state.config.event_log_retention_days,
        state.db_backend,
    ));

    let app = server::router(state);
    let addr = config.listen_addr();

    info!(%addr, "HappyView is listening");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind");

    axum::serve(listener, app).await.expect("server error");
}

async fn seed_and_load_rate_limit_defaults(
    pool: &sqlx::AnyPool,
    backend: happyview::db::DatabaseBackend,
) -> RateLimitDefaults {
    use happyview::rate_limit::{
        SEED_DEFAULT_PROCEDURE_COST, SEED_DEFAULT_PROXY_COST, SEED_DEFAULT_QUERY_COST,
        SETTING_DEFAULT_PROCEDURE_COST, SETTING_DEFAULT_PROXY_COST, SETTING_DEFAULT_QUERY_COST,
    };

    async fn seed_and_read(
        pool: &sqlx::AnyPool,
        backend: happyview::db::DatabaseBackend,
        key: &str,
        seed: u32,
    ) -> u32 {
        if happyview::admin::settings::get_setting(pool, key, backend)
            .await
            .is_none()
        {
            let now = happyview::db::now_rfc3339();
            let sql = happyview::db::adapt_sql(
                "INSERT INTO instance_settings (key, value, updated_at) VALUES (?, ?, ?) ON CONFLICT (key) DO NOTHING",
                backend,
            );
            if let Err(e) = sqlx::query(&sql)
                .bind(key)
                .bind(seed.to_string())
                .bind(&now)
                .execute(pool)
                .await
            {
                warn!(error = %e, key = key, "failed to seed rate-limit default");
            }
        }
        happyview::admin::settings::get_setting(pool, key, backend)
            .await
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(seed)
    }

    RateLimitDefaults {
        query_cost: seed_and_read(
            pool,
            backend,
            SETTING_DEFAULT_QUERY_COST,
            SEED_DEFAULT_QUERY_COST,
        )
        .await,
        procedure_cost: seed_and_read(
            pool,
            backend,
            SETTING_DEFAULT_PROCEDURE_COST,
            SEED_DEFAULT_PROCEDURE_COST,
        )
        .await,
        proxy_cost: seed_and_read(
            pool,
            backend,
            SETTING_DEFAULT_PROXY_COST,
            SEED_DEFAULT_PROXY_COST,
        )
        .await,
    }
}

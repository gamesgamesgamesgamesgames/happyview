//! `happyview::run`: the process HappyView's binaries run, given the record
//! source and storage to run with.

use std::sync::Arc;

use crate::auth::oauth_store::{DbSessionStore, DbStateStore};
use crate::catalog::LexiconCatalog;
use crate::config::Config;
use crate::db;
use crate::dns::NativeDnsResolver;
use crate::rate_limit::{RateLimitDefaults, RateLimiter};
use crate::resolve::{fetch_lexicon_from_pds, resolve_nsid_authority};
use crate::{AppState, labeler, server};
use happyview_provider::{RecordSource, RecordStorage};
use sqlx::Row;
use tokio::sync::watch;
use tracing::{info, warn};

use atrium_identity::did::{CommonDidResolver, CommonDidResolverConfig};
use atrium_identity::handle::{AtprotoHandleResolver, AtprotoHandleResolverConfig};
use atrium_oauth::{
    AtprotoClientMetadata, AtprotoLocalhostClientMetadata, AuthMethod, GrantType, KnownScope,
    OAuthClientConfig, OAuthResolverConfig, Scope,
};

/// Where records come from and, optionally, where some collections' records
/// are kept instead of the instance database.
pub struct Providers {
    pub source: Arc<dyn RecordSource>,
    /// Claims collections whose records it keeps; `None` keeps every
    /// collection in the instance database.
    pub storage: Option<Arc<dyn RecordStorage>>,
}

impl Default for Providers {
    /// The in-tree Jetstream source and no external storage.
    fn default() -> Self {
        Self {
            source: happyview_source_jetstream::JetstreamSource::builder().build(),
            storage: None,
        }
    }
}

/// Refuses a source built for another contract version.
fn check_contract_version(providers: &Providers) -> Result<(), happyview_provider::ProviderError> {
    let found = providers.source.contract_version();
    if found == happyview_provider::CONTRACT_VERSION {
        return Ok(());
    }
    Err(happyview_provider::ProviderError::Version {
        found,
        expected: happyview_provider::CONTRACT_VERSION,
    })
}

/// Serve HappyView, or run one of its other commands, with `providers` as the
/// record source and storage.
pub async fn run(command: crate::cli::Command, providers: Providers) -> std::process::ExitCode {
    // Install rustls crypto provider early so all TLS users (jetstream, labeler, etc.) can find it.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let dev_root = match &command {
        crate::cli::Command::Dev { root } => Some(root.clone()),
        _ => None,
    };
    match command {
        crate::cli::Command::Check(args) => {
            // Logs go to stderr, so stdout holds only what the build found.
            tracing_subscriber::fmt()
                .with_writer(std::io::stderr)
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "warn".parse().unwrap()),
                )
                .init();
            let code = crate::project::check::run(&args, &mut std::io::stdout()).await;
            return std::process::ExitCode::from(code);
        }
        crate::cli::Command::Test(args) => {
            // Logs go to stderr at `error` unless asked for: the run's own
            // output is the report.
            tracing_subscriber::fmt()
                .with_writer(std::io::stderr)
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "error".parse().unwrap()),
                )
                .init();
            let code = crate::testing::runner::run(&args, &mut std::io::stdout()).await;
            return std::process::ExitCode::from(code);
        }
        crate::cli::Command::Types(args) => {
            // As `check`: logs to stderr, stdout for what was written.
            tracing_subscriber::fmt()
                .with_writer(std::io::stderr)
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "warn".parse().unwrap()),
                )
                .init();
            let code =
                crate::project::declarations::command::run(&args, &mut std::io::stdout()).await;
            return std::process::ExitCode::from(code);
        }
        crate::cli::Command::Help => {
            println!("{}", crate::cli::USAGE);
            return std::process::ExitCode::SUCCESS;
        }
        crate::cli::Command::Serve | crate::cli::Command::Dev { .. } => {}
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                if dev_root.is_some() {
                    crate::dev::env::DEV_LOG_FILTER
                } else {
                    "happyview=debug,tower_http=debug,sqlx=warn"
                }
                .parse()
                .unwrap()
            }),
        )
        .init();

    if let Err(e) = check_contract_version(&providers) {
        let id = providers.source.id();
        tracing::error!("refusing to start: record source `{id}`: {e}");
        eprintln!("refusing to start: record source `{id}`: {e}");
        return std::process::ExitCode::FAILURE;
    }

    let (dev, config) = match &dev_root {
        Some(root) => match crate::dev::prepare(root) {
            Ok((session, config)) => (Some(session), config),
            Err(e) => {
                eprintln!("happyview dev: {e}");
                return std::process::ExitCode::FAILURE;
            }
        },
        None => match Config::load() {
            Ok(config) => (None, config),
            Err(e) => {
                tracing::error!("{e}");
                std::process::exit(1);
            }
        },
    };
    let Booted {
        state,
        labeler_subscriptions_rx,
    } = match boot(config.clone(), dev.is_some(), providers).await {
        Ok(booted) => booted,
        Err(code) => return code,
    };

    labeler::spawn(state.clone(), labeler_subscriptions_rx);
    tokio::spawn(labeler::spawn_label_gc(state.records.clone()));

    {
        let gc_db = state.db.clone();
        let gc_backend = state.db_backend;
        tokio::spawn(async move {
            crate::auth::state_gc::run_expired_state_gc(gc_db, gc_backend).await;
        });
    }

    {
        let telemetry_state = state.clone();
        tokio::spawn(async move {
            crate::telemetry::reporter::run_reporter(telemetry_state).await;
        });
    }

    tokio::spawn(crate::event_log::spawn_retention_cleanup(
        state.db.clone(),
        state.db_backend,
        crate::event_log::parse_dead_letter_retention_days(
            crate::config::vars::var("DEAD_LETTER_RETENTION_DAYS").as_deref(),
        ),
        crate::tombstones::retention_hours(),
    ));

    {
        let interval = crate::maintenance::sqlite::checkpoint_interval_secs();
        let checkpoint_every = (interval > 0).then(|| std::time::Duration::from_secs(interval));
        tokio::spawn(crate::maintenance::sqlite::run(
            state.db.clone(),
            state.db_backend,
            checkpoint_every,
            crate::maintenance::sqlite::wal_limit(&state.config.database_url),
        ));
    }

    tokio::spawn(crate::record_store::compression::run(
        state.records.clone(),
        crate::record_store::compression::train_at_from_env(),
        crate::record_store::compression::interval_from_env(),
    ));

    {
        let db = state.db.clone();
        let flag = state.verbose_event_logging.clone();
        let backend = state.db_backend;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                let enabled =
                    crate::admin::settings::get_setting(&db, "verbose_event_logging", backend)
                        .await
                        .map(|v| v.eq_ignore_ascii_case("true"))
                        .unwrap_or(false);
                flag.store(enabled, std::sync::atomic::Ordering::Relaxed);
            }
        });
    }

    // Resume interrupted jobs and start the job worker
    crate::jobs::worker::spawn_recovery(state.clone());
    crate::jobs::worker::spawn_workers(state.clone(), state.config.job_worker_concurrency);

    {
        let keepalive_state = state.clone();
        tokio::spawn(async move {
            crate::linked_repos::worker::run_keepalive(keepalive_state).await;
        });
    }

    {
        let sweeper_state = state.clone();
        tokio::spawn(async move {
            crate::spaces::native_sync::run_sweeper(sweeper_state).await;
        });
    }

    crate::lexicon_publishing::spawn_reconcile(&state);

    let dev_mode = dev.is_some();
    let _watch = match dev {
        Some(session) => {
            println!("{}", crate::dev::report::banner(&session));
            if let Some(warning) = crate::dev::report::network_warning(
                crate::config::vars::var("HAPPYVIEW_NETWORK").as_deref(),
            ) {
                println!("{warning}");
            }
            match crate::dev::watch::spawn(state.clone(), session) {
                Ok(handle) => {
                    if let crate::dev::watch::WatchMode::Poll(every) = handle.mode() {
                        println!(
                            "watching by polling every {} ms (HAPPYVIEW_WATCH_POLL_MS)",
                            every.as_millis()
                        );
                    }
                    Some(handle)
                }
                Err(e) => {
                    eprintln!("happyview dev: {e}");
                    return std::process::ExitCode::FAILURE;
                }
            }
        }
        None => None,
    };

    let app = server::router(state);
    let addr = config.listen_addr();

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(e) if dev_mode => {
            eprintln!(
                "happyview dev: cannot listen on {addr}: {e}; set [dev] port in happyview.toml, or PORT"
            );
            return std::process::ExitCode::FAILURE;
        }
        Err(e) => panic!("failed to bind {addr}: {e}"),
    };
    info!(%addr, "HappyView is listening");

    axum::serve(listener, app).await.expect("server error");
    std::process::ExitCode::SUCCESS
}

/// What [`boot`] hands the rest of `run`.
struct Booted {
    state: AppState,
    labeler_subscriptions_rx: watch::Receiver<()>,
}

/// `run` from connecting to the database through building the state and
/// starting the record source: everything a served instance has before its
/// background tasks start and it listens. An `Err` is the exit code to stop
/// with.
/// Logs every configuration problem first, before anything slow can push it
/// out of view. A bad `SESSION_SECRET` does not stop boot, because API-key,
/// DPoP and service-auth callers need no cookie, but it leaves the dashboard
/// with no way to sign in, so the error says that and names the fix.
fn report_config_errors(config: &Config) {
    let errors = config.config_errors();
    for err in &errors {
        tracing::error!("INSECURE CONFIGURATION: {err}");
    }
    if !config.session_secret_secure() {
        tracing::error!(
            "Dashboard sign-in is DISABLED: nobody can sign in with a cookie until \
             SESSION_SECRET is set to a random value of at least 32 bytes \
             (`openssl rand -base64 48`) and the server is restarted. API keys, \
             DPoP and service auth keep working."
        );
    }
}

async fn boot(
    config: Config,
    dev_mode: bool,
    providers: Providers,
) -> Result<Booted, std::process::ExitCode> {
    let db_backend = config.database_backend;

    report_config_errors(&config);

    // Connect to database and run migrations.
    let db_pool = db::connect(&config.database_url, db_backend).await;
    let backfill_db_pool =
        db::connect_backfill_pool(&config.database_url, db_backend, &db_pool).await;

    // The source's own schema, after HappyView's own migrations and before
    // the source starts.
    if let Err(e) =
        crate::provider_migrations::run(&db_pool, db_backend, providers.source.as_ref()).await
    {
        eprintln!(
            "happyview: could not migrate the record source {}: {e}",
            providers.source.id()
        );
        return Err(std::process::ExitCode::FAILURE);
    }

    if dev_mode && let Err(e) = crate::dev::instance::seed(&db_pool, db_backend).await {
        eprintln!("happyview dev: could not prepare the instance: {e}");
        return Err(std::process::ExitCode::FAILURE);
    }

    info!(
        backend = ?db_backend,
        "connected to database"
    );

    // The backfill re-mints commits, so it needs the space signing key.
    let space_signing_key = match config.token_encryption_key.as_ref() {
        Some(key) => crate::spaces::service::signing_key_from_pool(&db_pool, db_backend, key)
            .await
            .ok(),
        None => None,
    };

    match match space_signing_key.as_ref() {
        Some(key) => crate::spaces::cid_backfill::run_if_needed(&db_pool, db_backend, key).await,
        None => Ok(None),
    } {
        Ok(Some(report)) if report.is_noop() => {
            info!("space CID backfill: nothing to repair");
        }
        Ok(Some(report)) => {
            info!(
                records_updated = report.records_updated,
                oplog_rows_remapped = report.oplog_rows_remapped,
                repos_rebuilt = report.repos_rebuilt,
                records_unencodable = report.records_unencodable,
                "space CID backfill applied"
            );
            if report.records_unencodable > 0 {
                tracing::warn!(
                    count = report.records_unencodable,
                    "some space records could not be encoded as DAG-CBOR and kept their existing CID"
                );
            }
        }
        Ok(None) => { /* already completed; skipped without scanning */ }
        Err(e) => tracing::error!(
            error = %e,
            "space CID backfill failed; will retry on next startup"
        ),
    }

    // One-time re-mint of every existing commit in the current commit format
    // (HKDF-Expand-only MAC derivation, plus a signature). Commits are derived
    // from records, which are not touched, so this recomputes rather than
    // migrates. Each repo keeps its revision, since the record set is unchanged.
    match match space_signing_key.as_ref() {
        Some(key) => {
            crate::spaces::rebuild::run_commit_format_rebuild(&db_pool, db_backend, key).await
        }
        None => Ok(None),
    } {
        Ok(Some(rebuilt)) => info!(
            repos = rebuilt,
            "re-minted space commits in the current format"
        ),
        Ok(None) => { /* already completed */ }
        Err(e) => tracing::error!(
            error = %e,
            "space commit format rebuild failed; will retry on next startup"
        ),
    }

    match crate::maintenance::vacuum::run_if_requested(&db_pool, db_backend, &config.database_url)
        .await
    {
        Ok(Some(result)) if result.status == "ok" => {
            info!(
                reclaimed = %crate::maintenance::vacuum::human_bytes(result.reclaimed_bytes),
                "scheduled vacuum complete"
            );
        }
        Ok(Some(result)) => {
            tracing::error!(
                error = %result.error.clone().unwrap_or_default(),
                "scheduled vacuum failed"
            );
        }
        Ok(None) => { /* not scheduled */ }
        Err(e) => tracing::error!(error = %e, "scheduled vacuum could not be evaluated"),
    }

    crate::telemetry::collect::health::note_restart(&db_pool, db_backend).await;
    crate::telemetry::collect::health::note_version(
        &db_pool,
        db_backend,
        crate::version::version(),
    )
    .await;
    crate::maintenance::nsid_audit::run(&db_pool, db_backend).await;
    crate::maintenance::lexicon_ids::run(&db_pool, db_backend).await;

    let project = crate::project::shared_empty();
    let lexicons = LexiconCatalog::new(project.clone());
    let scripts = crate::catalog::ScriptCatalog::new(db_pool.clone(), db_backend, project.clone());
    lexicons
        .load_from_db(&db_pool)
        .await
        .expect("failed to load lexicons");

    // Re-fetch all network lexicons from their respective PDSes.
    let http = crate::http_retry::init_shared_client(&config.user_agent);
    // From the table rather than memory, so a row that failed to parse at load
    // is fetched again and repaired.
    let network: Vec<String> = match lexicons.list_network(&db_pool, db_backend).await {
        Ok(rows) => rows.into_iter().map(|l| l.id).collect(),
        Err(e) => {
            warn!("failed to list network lexicons for refresh: {e}");
            Vec::new()
        }
    };
    for nsid in &network {
        let nsid = nsid.as_str();
        match resolve_nsid_authority(&http, &config.plc_url, nsid).await {
            Ok((did, pds_endpoint)) => {
                match fetch_lexicon_from_pds(&http, &pds_endpoint, &did, nsid).await {
                    Ok(lexicon_json) => {
                        match lexicons
                            .refresh_network(&db_pool, db_backend, nsid, lexicon_json)
                            .await
                        {
                            Ok(Some(_)) => info!(nsid, "refreshed network lexicon"),
                            Ok(None) => {
                                info!(nsid, "network lexicon was removed before it was refreshed")
                            }
                            Err(e) => warn!(nsid, "failed to refresh network lexicon: {e}"),
                        }
                    }
                    Err(e) => warn!(nsid, "failed to fetch network lexicon from PDS: {e}"),
                }
            }
            Err(e) => warn!(nsid, "failed to resolve network lexicon authority: {e}"),
        }
    }

    if !network.is_empty() {
        info!(
            count = network.len(),
            "processed network lexicons on startup"
        );
    }

    crate::config::declarations::apply_at_boot(&config, &db_pool, db_backend, &http, &lexicons)
        .await;

    // Initialize WASM runtime
    if config.plugin_cache_dir.is_none() {
        tracing::info!(
            "compiled plugin modules are cached in memory only; set PLUGIN_CACHE_DIR to a directory owned by this process to keep them across restarts"
        );
    }
    let wasm_runtime = Arc::new(
        crate::plugin::WasmRuntime::with_cache_dir(config.plugin_cache_dir.clone())
            .expect("Failed to create WASM runtime"),
    );

    // Initialize plugin registry (with DB for persistence)
    let plugin_registry = Arc::new(
        crate::plugin::PluginRegistry::with_db(db_pool.clone(), db_backend)
            .with_runtime(wasm_runtime.clone()),
    );

    // Initialize attestation signer (auto-generates key if none exists)
    let attestation_signer = match crate::plugin::attestation::load_or_generate(
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

    // Load plugins from PLUGIN_URLS env var and from the local plugin
    // directory, collecting both into a batch installed in dependency
    // order below — a library must land before anything that depends on it,
    // regardless of which of these two sources it came from.
    let mut boot_plugins: Vec<crate::plugin::LoadedPlugin> = Vec::new();

    if let Some(urls) = crate::config::vars::var("PLUGIN_URLS") {
        for (id, url, sha256) in crate::plugin::loader::parse_plugin_urls(&urls) {
            match crate::plugin::loader::fetch_manifest(&http, &url).await {
                Ok(preview) => {
                    match crate::plugin::loader::load_from_manifest(
                        &http,
                        &preview,
                        sha256.as_deref(),
                    )
                    .await
                    {
                        Ok(plugin) => {
                            tracing::info!(id = %id, "Loaded plugin from URL");
                            boot_plugins.push(plugin);
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
    for path in crate::plugin::loader::plugin_dirs_in(&config.plugin_dir) {
        match crate::plugin::loader::load_from_file(&path).await {
            Ok(plugin) => {
                tracing::info!(id = %plugin.info.id, "Loaded plugin from file");
                boot_plugins.push(plugin);
            }
            Err(e) => {
                tracing::error!(path = %path.display(), error = %e, "Failed to load plugin");
            }
        }
    }

    for (id, err) in plugin_registry.register_all(boot_plugins).await {
        tracing::error!(plugin_id = %id, error = %err, "Failed to install plugin at boot");
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

    // Said at boot because the refusals an operator would otherwise meet
    // first — on a save, on a request — name a language rather than the gap.
    if plugin_registry
        .list_by_type(crate::plugin::PluginType::Interpreter)
        .await
        .is_empty()
    {
        tracing::warn!(
            "no interpreter plugin is installed, so no script can be saved or run; \
             install one from the plugins page in the dashboard"
        );
    }

    // Before the API clients are loaded below, so hpy is among them.
    if let Err(e) = crate::auth::cli_client::seed(&db_pool, db_backend).await {
        tracing::error!(error = %e, "hpy cannot sign in to this instance");
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
        let client_rows: Vec<ClientRow> = crate::db::query_as(
            "SELECT id, client_key, client_uri, rate_limit_capacity, rate_limit_refill_rate, parent_client_id FROM happyview_api_clients WHERE is_active = 1",
        )
        .fetch_all(&db_pool)
        .await
        .unwrap_or_default();

        for (client_id, client_key, client_uri, capacity, refill_rate, _) in &client_rows {
            if let Err(e) = crate::oauth::client_secrets::register_identity(
                &rate_limiter,
                &db_pool,
                db_backend,
                client_id,
                client_key,
                client_uri,
            )
            .await
            {
                tracing::error!(client_key, error = %e, "could not load an API client's secrets; it is registered with none");
                rate_limiter.register_client_identity(
                    client_key.clone(),
                    crate::rate_limit::ClientIdentity {
                        secrets: Vec::new(),
                        client_uri: client_uri.clone(),
                    },
                );
            }
            if let (Some(cap), Some(refill)) = (capacity, refill_rate) {
                rate_limiter.register_client_config(
                    client_key.clone(),
                    crate::rate_limit::RateLimitConfig {
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
                    crate::rate_limit::RateLimitConfig {
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
    let domain_cache = crate::domain::DomainCache::new();
    {
        // Not fatal: booting with the domains as they are beats a crash loop,
        // and they can be fixed through the admin API once the instance is up.
        if let Err(e) = crate::domain::sync_primary(&db_pool, db_backend, &config.public_url).await
        {
            tracing::error!(error = %e, public_url = %config.public_url, "failed to sync the primary domain with PUBLIC_URL");
        }

        let select_sql = crate::db::adapt_sql(
            "SELECT id, url, is_primary, created_at, updated_at FROM happyview_domains",
            db_backend,
        );
        let rows = crate::db::query(&select_sql)
            .fetch_all(&db_pool)
            .await
            .expect("Failed to load domains");

        let domains: Vec<crate::domain::Domain> = rows
            .into_iter()
            .map(|row| {
                let is_primary_int: i32 = row.try_get("is_primary").unwrap_or(0);
                crate::domain::Domain {
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
    let callback_url = format!(
        "{}/auth/callback",
        config.effective_public_url().trim_end_matches('/')
    );
    // Use our UA-and-timeout-configured client for OAuth traffic. atrium-oauth's
    // `default-client` feature (which we no longer enable — see Cargo.toml)
    // used to build its own unconfigured `reqwest::Client::new()` here, with no
    // seam for headers or timeouts; this identifies OAuth requests and gives
    // them the shared connect/read timeouts instead.
    let atrium_http = Arc::new(crate::http_retry::HappyViewHttpClient::new(http.clone()));

    let did_resolver = CommonDidResolver::new(CommonDidResolverConfig {
        plc_directory_url: config.plc_url.clone(),
        http_client: Arc::clone(&atrium_http),
    });

    let handle_resolver = AtprotoHandleResolver::new(AtprotoHandleResolverConfig {
        dns_txt_resolver: dns,
        http_client: Arc::clone(&atrium_http),
    });

    let is_loopback = crate::auth::client_registry::is_loopback_url(&config.public_url);

    let resolver_config = OAuthResolverConfig {
        did_resolver,
        handle_resolver,
        authorization_server_metadata: Default::default(),
        protected_resource_metadata: Default::default(),
    };

    let oauth_state_store = DbStateStore::new(db_pool.clone(), db_backend);

    let oauth_scopes = crate::oauth::rotation::instance_scopes();

    let linked_repos_scopes = oauth_scopes.clone();
    let oauth_scopes_for_by_kid = oauth_scopes.clone();

    // A staged key that came due while the instance was down takes over
    // before the clients are built around the current one.
    if let Err(e) = crate::oauth::client_keys::promote_due_keys(&db_pool, db_backend).await {
        warn!(error = %e, "failed to promote staged OAuth client keys");
    }

    let instance_key = crate::oauth::client_keys::ensure_instance_key(
        &db_pool,
        db_backend,
        config.token_encryption_key.as_ref(),
    )
    .await
    .expect("Failed to load OAuth client authentication key");

    let client_jwks = vec![
        crate::oauth::client_keys::to_atrium_jwk(&instance_key)
            .expect("Failed to convert client key to JWK"),
    ];
    info!(kid = %instance_key.kid, "confidential OAuth client key ready");

    let instance_client_id_url = config.instance_client_id_url();

    let oauth_client = if is_loopback {
        info!("Using loopback OAuth client metadata (local development)");
        atrium_oauth::OAuthClient::new(OAuthClientConfig {
            client_metadata: AtprotoLocalhostClientMetadata {
                redirect_uris: Some(vec![callback_url.clone()]),
                scopes: Some(oauth_scopes),
            },
            keys: None,
            state_store: oauth_state_store.clone(),
            session_store: DbSessionStore::new(db_pool.clone(), db_backend),
            resolver: resolver_config,
            http_client: crate::http_retry::HappyViewHttpClient::new(http.clone()),
        })
        .expect("Failed to create OAuth client")
    } else {
        atrium_oauth::OAuthClient::new(OAuthClientConfig {
            client_metadata: AtprotoClientMetadata {
                client_id: instance_client_id_url.clone(),
                client_uri: Some(config.effective_public_url()),
                redirect_uris: vec![callback_url.clone()],
                token_endpoint_auth_method: AuthMethod::PrivateKeyJwt,
                grant_types: vec![GrantType::AuthorizationCode, GrantType::RefreshToken],
                scopes: oauth_scopes,
                jwks_uri: None,
                token_endpoint_auth_signing_alg: Some("ES256".to_string()),
            },
            keys: Some(client_jwks.clone()),
            state_store: oauth_state_store.clone(),
            session_store: DbSessionStore::new(db_pool.clone(), db_backend)
                .with_signing_kid(Some(instance_key.kid.clone())),
            resolver: resolver_config,
            http_client: crate::http_retry::HappyViewHttpClient::new(http.clone()),
        })
        .expect("Failed to create OAuth client")
    };

    // A loopback client never signs at all, so it gets no pin regardless —
    // same reasoning as `build_instance_client`'s and `linked_repos::client::
    // build`'s own `signing_kid` computation, which this must keep matching.
    // Fixed here, once, alongside `linked_repos_client` itself: that client
    // is never rebuilt live on rotation (see `oauth::rotation`'s module
    // doc), so this is the one place its actual kid is ever set.
    let instance_signing_kid = if is_loopback {
        None
    } else {
        Some(instance_key.kid.clone())
    };

    let linked_repos_client = Arc::new(
        crate::linked_repos::client::build(
            &config.plc_url,
            &format!(
                "{}/oauth-client-metadata.json",
                config.effective_public_url().trim_end_matches('/')
            ),
            &config.effective_public_url(),
            callback_url.clone(),
            is_loopback,
            linked_repos_scopes,
            oauth_state_store.clone(),
            db_pool.clone(),
            db_backend,
            Some(client_jwks.clone()),
        )
        .expect("Failed to create linked-repo OAuth client"),
    );

    let cookie_key = if config.session_secret_secure() {
        axum_extra::extract::cookie::Key::derive_from(config.session_secret.as_bytes())
    } else {
        axum_extra::extract::cookie::Key::generate()
    };

    let initial_collections = lexicons.get_record_collections().await;
    let (collections_tx, collections_rx) = watch::channel(initial_collections);
    let (labeler_subscriptions_tx, labeler_subscriptions_rx) = watch::channel(());

    // Build the OAuth client registry and load API clients from DB
    let oauth_client_arc = Arc::new(oauth_client);
    let oauth_registry = Arc::new(crate::auth::OAuthClientRegistry::new_with_kid(
        Arc::clone(&oauth_client_arc),
        instance_signing_kid.clone(),
    ));
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
        let primary_client_id_url = format!(
            "{}/oauth-client-metadata.json",
            config.url_with_base_path(&pd.url).trim_end_matches('/')
        );
        oauth_registry.register_domain_client(
            pd.url.clone(),
            primary_client_id_url,
            Arc::clone(&oauth_client_arc),
            instance_signing_kid.clone(),
        );
    }

    // Build OAuth clients for all non-primary domains
    let all_domains = domain_cache.all().await;
    for domain in &all_domains {
        if domain.is_primary {
            continue; // Already registered above
        }

        let domain_base_url = config.url_with_base_path(&domain.url);
        let domain_callback_url =
            format!("{}/auth/callback", domain_base_url.trim_end_matches('/'));
        let domain_client_id = format!(
            "{}/oauth-client-metadata.json",
            domain_base_url.trim_end_matches('/')
        );

        // Same reasoning as `atrium_http` above.
        let domain_http = Arc::new(crate::http_retry::HappyViewHttpClient::new(http.clone()));
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
                client_id: domain_client_id.clone(),
                client_uri: Some(domain_base_url.clone()),
                redirect_uris: vec![domain_callback_url],
                token_endpoint_auth_method: AuthMethod::PrivateKeyJwt,
                grant_types: vec![GrantType::AuthorizationCode, GrantType::RefreshToken],
                scopes: vec![Scope::Known(KnownScope::Atproto)],
                jwks_uri: None,
                token_endpoint_auth_signing_alg: Some("ES256".to_string()),
            },
            keys: Some(client_jwks.clone()),
            state_store: oauth_state_store.clone(),
            session_store: DbSessionStore::new(db_pool.clone(), db_backend)
                .with_signing_kid(Some(instance_key.kid.clone())),
            resolver: domain_resolver,
            http_client: crate::http_retry::HappyViewHttpClient::new(http.clone()),
        }) {
            Ok(client) => {
                info!(domain = %domain.url, "Registered domain OAuth client");
                oauth_registry.register_domain_client(
                    domain.url.clone(),
                    domain_client_id,
                    Arc::new(client),
                    Some(instance_key.kid.clone()),
                );
            }
            Err(e) => {
                tracing::error!(domain = %domain.url, error = %e, "Failed to create domain OAuth client");
            }
        }
    }

    // Index every non-revoked instance key by (client_id_url, kid) so a
    // session pinned to a specific key can be resolved unambiguously — see
    // `OAuthClientRegistry::register_for_kid`. Each key gets its own,
    // genuinely single-key `OAuthClient` built fresh here: reusing
    // `oauth_client_arc` (built from only the *current* key) across every
    // kid is correct only while exactly one key is ever live. The moment a
    // `retiring` key coexists with `current` — which rotation makes routine
    // — that reuse would map the retiring kid to a client that can only
    // sign with the wrong (current) key, silently, since the lookup itself
    // succeeds.
    let instance_keys = crate::oauth::client_keys::load_keys(
        &db_pool,
        db_backend,
        config.token_encryption_key.as_ref(),
        crate::oauth::client_keys::INSTANCE_OWNER,
    )
    .await
    .unwrap_or_default();
    for key in &instance_keys {
        match crate::auth::client_registry::build_instance_client(
            &config.plc_url,
            &instance_client_id_url,
            &config.effective_public_url(),
            vec![callback_url.clone()],
            is_loopback,
            oauth_scopes_for_by_kid.clone(),
            oauth_state_store.clone(),
            db_pool.clone(),
            db_backend,
            key,
        ) {
            Ok(client) => {
                oauth_registry.register_for_kid(
                    &instance_client_id_url,
                    &key.kid,
                    Arc::new(client),
                );
            }
            Err(e) => {
                warn!(kid = %key.kid, error = %e, "failed to build per-kid instance OAuth client");
            }
        }

        // Same reasoning, nested: each domain's `by_kid` entry must also be
        // a distinct single-key client, not a reused Arc built for a
        // different kid.
        for domain in &all_domains {
            let domain_base_url = config.url_with_base_path(&domain.url);
            let domain_callback_url =
                format!("{}/auth/callback", domain_base_url.trim_end_matches('/'));
            let domain_client_id = format!(
                "{}/oauth-client-metadata.json",
                domain_base_url.trim_end_matches('/')
            );
            match crate::auth::client_registry::build_instance_client(
                &config.plc_url,
                &domain_client_id,
                &domain_base_url,
                vec![domain_callback_url],
                false,
                vec![Scope::Known(KnownScope::Atproto)],
                oauth_state_store.clone(),
                db_pool.clone(),
                db_backend,
                key,
            ) {
                Ok(client) => {
                    oauth_registry.register_for_kid(&domain_client_id, &key.kid, Arc::new(client));
                }
                Err(e) => {
                    warn!(domain = %domain.url, kid = %key.kid, error = %e, "failed to build per-kid domain OAuth client");
                }
            }
        }
    }

    // Index the linked-repos client by kid too, in its own namespace kept
    // separate from the loop above — see `OAuthClientRegistry`'s
    // `linked_repos_by_kid` field doc for why it cannot share a
    // `(client_id_url, kid)` entry with the instance/domain clients despite
    // publishing the same `client_id` text. Same distinct-client-per-kid
    // reasoning applies here too.
    for key in &instance_keys {
        match crate::oauth::client_keys::to_atrium_jwk(key) {
            Ok(jwk) => match crate::linked_repos::client::build(
                &config.plc_url,
                &format!(
                    "{}/oauth-client-metadata.json",
                    config.effective_public_url().trim_end_matches('/')
                ),
                &config.effective_public_url(),
                callback_url.clone(),
                is_loopback,
                oauth_scopes_for_by_kid.clone(),
                oauth_state_store.clone(),
                db_pool.clone(),
                db_backend,
                Some(vec![jwk]),
            ) {
                Ok(client) => {
                    oauth_registry.register_linked_repos_for_kid(&key.kid, Arc::new(client));
                }
                Err(e) => {
                    warn!(kid = %key.kid, error = %e, "failed to build per-kid linked-repos OAuth client");
                }
            },
            Err(e) => {
                warn!(kid = %key.kid, error = %e, "failed to convert instance key to JWK for linked-repos registration");
            }
        }
    }

    let official_registry: crate::plugin::official_registry::SharedRegistry =
        std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::plugin::official_registry::OfficialRegistryState::default(),
        ));
    let official_registry_config =
        crate::plugin::official_registry::RegistryConfig::production().with_registry_from_env();
    crate::plugin::official_registry::spawn_refresh_task(
        http.clone(),
        official_registry_config.clone(),
        official_registry.clone(),
        db_pool.clone(),
        db_backend,
    );

    let proxy_config = {
        let json_str =
            crate::admin::settings::get_setting(&db_pool, "xrpc_proxy_config", db_backend).await;
        let config = json_str
            .and_then(|s| serde_json::from_str::<crate::proxy_config::ProxyConfig>(&s).ok())
            .unwrap_or_default();
        info!(mode = ?config.mode, nsid_count = config.nsids.len(), "Loaded XRPC proxy config");
        std::sync::Arc::new(arc_swap::ArcSwap::new(std::sync::Arc::new(config)))
    };

    let verbose_event_logging = {
        let enabled =
            crate::admin::settings::get_setting(&db_pool, "verbose_event_logging", db_backend)
                .await
                .map(|v| v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(enabled))
    };

    let telemetry_counters = std::sync::Arc::new(crate::telemetry::counters::Counters::new());

    let script_limits =
        std::sync::Arc::new(crate::lua::limits::ScriptLimits::load(&db_pool, db_backend).await);
    let metrics = std::sync::Arc::new(crate::metrics::ScriptMetrics::new());
    metrics.refresh(&db_pool, db_backend).await;

    let mut records = crate::record_store::RecordStore::shared(db_pool.clone(), db_backend)
        .await
        .expect("failed to load record compression dictionaries");
    if let Some(storage) = providers.storage {
        records = records.with_external(storage);
    }

    let state = AppState {
        config: config.clone(),
        http,
        records,
        source: providers.source,
        db: db_pool,
        backfill_db: backfill_db_pool,
        db_backend,
        domain_cache: domain_cache.clone(),
        lexicons,
        scripts,
        project,
        collections_tx,
        labeler_subscriptions_tx,
        labeler_subscription_cache: Arc::new(crate::labeler::SubscriptionCache::default()),
        rate_limiter,
        oauth: oauth_registry,
        oauth_state_store,
        linked_repos_client,
        linked_repos_client_kid: instance_signing_kid,
        cookie_key,
        plugin_registry,
        wasm_runtime,
        attestation_signer,
        official_registry,
        official_registry_config,
        proxy_config,
        verbose_event_logging,
        ignored_keeps: Arc::default(),
        client_jwks,
        telemetry_counters,
        script_limits,
        metrics,
        integration_origins: Arc::new(crate::csrf::IntegrationOrigins::default()),
        test: None,
    };

    // Populate record_refs once, for records indexed before the table existed.
    // Spawned before anything that writes records starts, as it was before
    // it took the shared store.
    {
        let records = state.records.clone();
        tokio::spawn(async move {
            match crate::record_refs::rebuild_once(&records).await {
                Ok(crate::record_refs::RebuildOutcome::Rebuilt { records }) => {
                    info!(records, "record_refs rebuild complete");
                }
                Ok(_) => {}
                Err(e) => {
                    warn!(error = %e, "record_refs rebuild failed; will resume on next startup")
                }
            }
        });
    }

    // Before anything that runs a script starts: a worker that came first
    // would run against no project at all, and serving without the project
    // would answer its routes with nothing.
    if let Some(root) = state.config.project_root.clone() {
        let started = std::time::Instant::now();
        let mode = if dev_mode {
            crate::project::boot::Mode::Dev
        } else {
            crate::project::boot::Mode::Serve
        };
        match crate::project::boot::start(&state, &root, mode).await {
            Ok(installed) if dev_mode => {
                println!(
                    "{}",
                    crate::dev::report::booted(&installed.warnings, started.elapsed())
                );
            }
            Ok(installed) => {
                for warning in &installed.warnings {
                    eprintln!("{warning}");
                }
            }
            // A dev server waits for the fix; production refuses to serve a
            // project it could not build.
            Err(diagnostics) if dev_mode => {
                println!("{}", crate::dev::report::failed(&diagnostics));
            }
            Err(diagnostics) => {
                for diagnostic in &diagnostics {
                    eprintln!("{diagnostic}");
                }
                eprintln!(
                    "{}: the project did not build, so HappyView is not starting",
                    root.display()
                );
                return Err(std::process::ExitCode::from(1));
            }
        }
    } else {
        // A deployed project, if one was. Its build failing is logged and
        // reported by `GET /admin/project`, never fatal: the fix is a deploy,
        // and that needs a running instance.
        if let Some(installed) = crate::project::deploy::serve_stored(&state).await {
            for warning in &installed.warnings {
                eprintln!("{warning}");
            }
        }
        // A deploy can arrive at any time, so the listener runs whether or
        // not one has; it rebuilds whatever the snapshot names as its root.
        crate::project::prepare::spawn_rebuild_on_interpreter_change(state.clone());
    }

    // Every plugin and project module this boot will run is known by now, so
    // anything else in the cache directory is a module nothing will load. In
    // dev, a first build that failed leaves no modules in the snapshot, so
    // theirs are swept and the next successful build compiles them again. A
    // dev rebuild does not sweep, so a long dev session leaves its replaced
    // builds behind until the next boot.
    let live = crate::project::script_module::boot_live_hashes(
        &state.plugin_registry,
        &state.project.load_full(),
    )
    .await;
    state.wasm_runtime.sweep_cache_dir(&live);

    tokio::spawn(crate::lua::limits::run_refresh_loop(
        state.script_limits.clone(),
        state.db.clone(),
        state.db_backend,
    ));
    tokio::spawn(crate::metrics::run_refresh_loop(
        state.metrics.clone(),
        state.db.clone(),
        state.db_backend,
    ));

    // Staged OAuth client keys take over when they come due. In the boot,
    // rather than in `run`, so `boot_for_test` exercises it too.
    tokio::spawn(crate::oauth::rotation::run_key_promotion(state.clone()));

    // Where `jetstream::spawn` stood: live ingest starts after the project
    // is installed and before anything else is spawned.
    if let Err(e) = state
        .source
        .clone()
        .start(crate::host::AppHost::new(state.clone()))
        .await
    {
        tracing::error!(source = state.source.id(), error = %e, "the record source did not start");
        return Err(std::process::ExitCode::FAILURE);
    }
    // After the source resumed the jobs that were running, which would
    // otherwise run one started here twice.
    if state.project.load().root.is_some() {
        crate::project::boot::start_declared_backfills(&state).await;
    }
    // `HAPPYVIEW_DEFAULT_PLUGINS`, in a task of its own: its sources are
    // fetched over the network, and none of them may hold up serving.
    crate::admin::default_plugins::spawn(&state);
    drop(collections_rx);

    Ok(Booted {
        state,
        labeler_subscriptions_rx,
    })
}

/// `run`'s boot alone, up to and including building the state and starting
/// `providers.source`, for tests: no background task besides those the boot
/// itself spawns, and nothing listens.
pub async fn boot_for_test(config: Config, providers: Providers) -> AppState {
    let _ = rustls::crypto::ring::default_provider().install_default();
    match boot(config, false, providers).await {
        Ok(booted) => booted.state,
        Err(code) => panic!("boot failed: {code:?}"),
    }
}

async fn seed_and_load_rate_limit_defaults(
    pool: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
) -> RateLimitDefaults {
    use crate::rate_limit::{
        SEED_DEFAULT_PROCEDURE_COST, SEED_DEFAULT_PROXY_COST, SEED_DEFAULT_QUERY_COST,
        SETTING_DEFAULT_PROCEDURE_COST, SETTING_DEFAULT_PROXY_COST, SETTING_DEFAULT_QUERY_COST,
    };

    async fn seed_and_read(
        pool: &sqlx::AnyPool,
        backend: crate::db::DatabaseBackend,
        key: &str,
        seed: u32,
    ) -> u32 {
        if crate::admin::settings::get_setting(pool, key, backend)
            .await
            .is_none()
        {
            let now = crate::db::now_rfc3339();
            let sql = crate::db::adapt_sql(
                "INSERT INTO happyview_instance_settings (key, value, updated_at) VALUES (?, ?, ?) ON CONFLICT (key) DO NOTHING",
                backend,
            );
            if let Err(e) = crate::db::query(&sql)
                .bind(key)
                .bind(seed.to_string())
                .bind(&now)
                .execute(pool)
                .await
            {
                warn!(error = %e, key = key, "failed to seed rate-limit default");
            }
        }
        crate::admin::settings::get_setting(pool, key, backend)
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

#[cfg(test)]
mod contract_version_tests {
    use super::*;
    use happyview_provider::{
        BackfillError, BackfillRequest, CONTRACT_VERSION, Host, ProviderError,
    };

    /// A source that reports the contract version it was built for.
    struct Versioned(u32);

    #[async_trait::async_trait]
    impl RecordSource for Versioned {
        fn id(&self) -> &'static str {
            "versioned"
        }
        fn contract_version(&self) -> u32 {
            self.0
        }
        fn migrations(
            &self,
            _: happyview_provider::db::DatabaseBackend,
        ) -> Option<&'static sqlx::migrate::Migrator> {
            None
        }
        async fn start(self: Arc<Self>, _: Arc<dyn Host>) -> Result<(), ProviderError> {
            Ok(())
        }
        fn admin_routes(&self) -> axum::Router {
            axum::Router::new()
        }
        async fn start_backfill(
            &self,
            _: BackfillRequest,
        ) -> Result<Option<String>, BackfillError> {
            Ok(None)
        }
        async fn has_backfill(&self, _: &str) -> bool {
            false
        }
        async fn backfill_running(&self, _: &str) -> bool {
            false
        }
        async fn status(&self) -> serde_json::Value {
            serde_json::Value::Null
        }
        async fn metrics(&self) -> Vec<(String, i64)> {
            Vec::new()
        }
    }

    fn providers(version: u32) -> Providers {
        Providers {
            source: Arc::new(Versioned(version)),
            storage: None,
        }
    }

    #[test]
    fn a_source_built_for_this_contract_version_is_accepted() {
        assert!(check_contract_version(&providers(CONTRACT_VERSION)).is_ok());
    }

    #[test]
    fn a_source_built_for_another_contract_version_is_refused() {
        let err = check_contract_version(&providers(CONTRACT_VERSION + 1)).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "contract version {} is not {CONTRACT_VERSION}",
                CONTRACT_VERSION + 1
            )
        );
    }

    #[tokio::test]
    async fn run_exits_non_zero_for_a_source_of_another_version() {
        let code = run(crate::cli::Command::Serve, providers(CONTRACT_VERSION + 1)).await;
        assert_eq!(code, std::process::ExitCode::FAILURE);
    }
}

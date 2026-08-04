use mlua::{Lua, LuaSerdeExt, Result as LuaResult};
use regex::Regex;
use std::sync::{Arc, LazyLock};

use crate::AppState;
use crate::jobs;

static JOB_TYPE_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9._-]*$").unwrap());

/// Caller identity for the jobs API — carries the DID and optional DPoP
/// session identifiers so `inherit_auth` jobs can restore PDS credentials.
#[derive(Clone, Debug)]
pub struct JobsCaller {
    pub did: String,
    pub api_client_id: Option<String>,
    pub dpop_key_id: Option<String>,
}

/// Register the `jobs` table for queuing jobs from scripts.
/// Available in all script contexts (procedure, query, record-event).
pub fn register_jobs_api(
    lua: &Lua,
    state: Arc<AppState>,
    caller: Option<JobsCaller>,
) -> LuaResult<()> {
    let jobs_table = lua.create_table()?;

    // jobs.create(job_type, input[, opts]) -> job_id string
    // opts.auth: boolean (default false) — inherit caller's PDS auth
    {
        let state = state.clone();
        let caller = caller.clone();
        let create_fn = lua.create_async_function(
            move |lua, (job_type, input, opts): (String, mlua::Value, Option<mlua::Table>)| {
                let state = state.clone();
                let caller = caller.clone();

                let input_json: serde_json::Value =
                    lua.from_value(input).unwrap_or(serde_json::json!({}));

                let inherit_auth = opts
                    .and_then(|t| t.get::<bool>("auth").ok())
                    .unwrap_or(false);

                async move {
                    if job_type.is_empty()
                        || job_type.len() > 128
                        || !JOB_TYPE_PATTERN.is_match(&job_type)
                    {
                        return Err(mlua::Error::runtime(
                            "job_type must be 1-128 characters matching /^[a-z0-9][a-z0-9._-]*$/",
                        ));
                    }

                    if crate::jobs::native::is_reserved(&job_type) {
                        return Err(mlua::Error::runtime(format!(
                            "job_type prefix '{}' is reserved for built-in jobs",
                            crate::jobs::native::RESERVED_PREFIX
                        )));
                    }

                    let caller = caller.as_ref().ok_or_else(|| {
                        mlua::Error::runtime("jobs.create requires an authenticated caller")
                    })?;

                    let (api_client_id, dpop_key_id) = if inherit_auth {
                        (
                            caller.api_client_id.as_deref(),
                            caller.dpop_key_id.as_deref(),
                        )
                    } else {
                        (None, None)
                    };

                    let job_id = jobs::db::create_job(
                        &state,
                        &job_type,
                        &input_json,
                        &caller.did,
                        inherit_auth,
                        api_client_id,
                        dpop_key_id,
                    )
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("jobs.create failed: {e}")))?;

                    Ok(job_id)
                }
            },
        )?;
        jobs_table.set("create", create_fn)?;
    }

    lua.globals().set("jobs", jobs_table)?;
    Ok(())
}

/// Register the `job` context table for use inside job scripts.
/// Provides access to job input, progress reporting, cooperative
/// cancellation, and sleep/wait.
///
/// Called by the job worker, not by the normal script execution path.
pub fn register_job_context(
    lua: &Lua,
    state: Arc<AppState>,
    job_id: String,
    input: serde_json::Value,
) -> LuaResult<()> {
    let job_table = lua.create_table()?;

    // job.input — the JSONB input passed to jobs.create()
    let input_value = lua.to_value(&input)?;
    job_table.set("input", input_value)?;

    // job.id — the job's UUID
    job_table.set("id", job_id.clone())?;

    // job.progress(data) — persist progress to DB
    {
        let state = state.clone();
        let job_id = job_id.clone();
        let progress_fn = lua.create_async_function(move |lua, data: mlua::Value| {
            let state = state.clone();
            let job_id = job_id.clone();
            let json_data: serde_json::Value =
                lua.from_value(data).unwrap_or(serde_json::json!({}));
            async move {
                jobs::db::update_progress(&state, &job_id, &json_data)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("job.progress failed: {e}")))?;
                Ok(())
            }
        })?;
        job_table.set("progress", progress_fn)?;
    }

    // job.should_stop() -> boolean
    {
        let state = state.clone();
        let job_id = job_id.clone();
        let should_stop_fn = lua.create_async_function(move |_lua, ()| {
            let state = state.clone();
            let job_id = job_id.clone();
            async move {
                let result = jobs::db::should_stop(&state, &job_id).await;
                Ok(result.is_some())
            }
        })?;
        job_table.set("should_stop", should_stop_fn)?;
    }

    // job.wait(seconds) — yield execution for the given duration
    {
        let wait_fn = lua.create_async_function(move |_lua, seconds: f64| async move {
            let duration = std::time::Duration::from_secs_f64(seconds.clamp(0.0, 3600.0));
            tokio::time::sleep(duration).await;
            Ok(())
        })?;
        job_table.set("wait", wait_fn)?;
    }

    // job.log(message)
    {
        let state = state.clone();
        let job_id = job_id.clone();
        let log_fn = lua.create_async_function(move |_lua, msg: String| {
            let state = state.clone();
            let job_id = job_id.clone();
            async move {
                if let Err(e) = crate::jobs::logs::insert_log(
                    &state.db,
                    state.db_backend,
                    &job_id,
                    "info",
                    &msg,
                )
                .await
                {
                    tracing::warn!(job_id = %job_id, error = %e, "job.log insert failed");
                }
                Ok(())
            }
        })?;
        job_table.set("log", log_fn)?;
    }

    // job.warn(message)
    {
        let state = state.clone();
        let job_id = job_id.clone();
        let warn_fn = lua.create_async_function(move |_lua, msg: String| {
            let state = state.clone();
            let job_id = job_id.clone();
            async move {
                if let Err(e) = crate::jobs::logs::insert_log(
                    &state.db,
                    state.db_backend,
                    &job_id,
                    "warn",
                    &msg,
                )
                .await
                {
                    tracing::warn!(job_id = %job_id, error = %e, "job.warn insert failed");
                }
                Ok(())
            }
        })?;
        job_table.set("warn", warn_fn)?;
    }

    lua.globals().set("job", job_table)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db::DatabaseBackend;
    use crate::lexicon::LexiconRegistry;
    use tokio::sync::watch;

    fn test_state() -> AppState {
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
            token_encryption_key: None,
            default_rate_limit_capacity: 100,
            default_rate_limit_refill_rate: 2.0,
        };
        let (tx, _) = watch::channel(vec![]);
        let (labeler_tx, _) = watch::channel(());
        sqlx::any::install_default_drivers();
        let test_db = sqlx::AnyPool::connect_lazy("sqlite::memory:").unwrap();
        let atrium_http = std::sync::Arc::new(crate::http_retry::HappyViewHttpClient::default());
        let did_resolver = atrium_identity::did::CommonDidResolver::new(
            atrium_identity::did::CommonDidResolverConfig {
                plc_directory_url: "https://plc.directory".into(),
                http_client: std::sync::Arc::clone(&atrium_http),
            },
        );
        let handle_resolver = atrium_identity::handle::AtprotoHandleResolver::new(
            atrium_identity::handle::AtprotoHandleResolverConfig {
                dns_txt_resolver: crate::dns::NativeDnsResolver::new(),
                http_client: atrium_http,
            },
        );
        let oauth = atrium_oauth::OAuthClient::new(atrium_oauth::OAuthClientConfig {
            client_metadata: atrium_oauth::AtprotoLocalhostClientMetadata {
                redirect_uris: Some(vec!["http://127.0.0.1:0/auth/callback".into()]),
                scopes: Some(vec![atrium_oauth::Scope::Known(
                    atrium_oauth::KnownScope::Atproto,
                )]),
            },
            keys: None,
            state_store: crate::auth::oauth_store::DbStateStore::new(
                test_db.clone(),
                crate::db::DatabaseBackend::Sqlite,
            ),
            session_store: crate::auth::oauth_store::DbSessionStore::new(
                test_db.clone(),
                crate::db::DatabaseBackend::Sqlite,
            ),
            resolver: atrium_oauth::OAuthResolverConfig {
                did_resolver,
                handle_resolver,
                authorization_server_metadata: Default::default(),
                protected_resource_metadata: Default::default(),
            },
            http_client: crate::http_retry::HappyViewHttpClient::default(),
        })
        .expect("Failed to create test OAuth client");
        AppState {
            config,
            http: reqwest::Client::new(),
            db: test_db.clone(),
            backfill_db: test_db.clone(),
            db_backend: DatabaseBackend::Sqlite,
            domain_cache: crate::domain::DomainCache::new(),
            lexicons: LexiconRegistry::new(),
            collections_tx: tx,
            labeler_subscriptions_tx: labeler_tx,
            rate_limiter: crate::rate_limit::RateLimiter::new(
                crate::rate_limit::RateLimitDefaults {
                    query_cost: 1,
                    procedure_cost: 1,
                    proxy_cost: 1,
                },
            ),
            oauth: std::sync::Arc::new(crate::auth::OAuthClientRegistry::new(std::sync::Arc::new(
                oauth,
            ))),
            oauth_state_store: crate::auth::oauth_store::DbStateStore::new(
                test_db.clone(),
                crate::db::DatabaseBackend::Sqlite,
            ),
            linked_repos_client: std::sync::Arc::new(
                crate::linked_repos::client::build(
                    "https://plc.directory",
                    "http://127.0.0.1:0/oauth-client-metadata.json",
                    "http://127.0.0.1:0",
                    "http://127.0.0.1:0/auth/callback".into(),
                    true,
                    vec![atrium_oauth::Scope::Known(
                        atrium_oauth::KnownScope::Atproto,
                    )],
                    crate::auth::oauth_store::DbStateStore::new(
                        test_db.clone(),
                        crate::db::DatabaseBackend::Sqlite,
                    ),
                    test_db.clone(),
                    crate::db::DatabaseBackend::Sqlite,
                )
                .expect("Failed to create test linked-repo OAuth client"),
            ),
            cookie_key: axum_extra::extract::cookie::Key::derive_from(
                b"test-secret-for-tests-only-not-production",
            ),
            plugin_registry: std::sync::Arc::new(crate::plugin::PluginRegistry::new()),
            wasm_runtime: std::sync::Arc::new(
                crate::plugin::WasmRuntime::new().expect("wasm runtime"),
            ),
            attestation_signer: None,
            official_registry: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::plugin::official_registry::OfficialRegistryState::default(),
            )),
            official_registry_config: crate::plugin::official_registry::RegistryConfig::production(
            ),
            proxy_config: std::sync::Arc::new(arc_swap::ArcSwap::new(std::sync::Arc::new(
                crate::proxy_config::ProxyConfig::default(),
            ))),
            backfill_events_tx: tokio::sync::broadcast::channel(16).0,
            verbose_event_logging: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    #[tokio::test]
    async fn jobs_api_is_registered() {
        let lua = crate::lua::sandbox::create_sandbox().unwrap();
        let state = test_state();
        let caller = JobsCaller {
            did: "did:plc:test".into(),
            api_client_id: None,
            dpop_key_id: None,
        };
        register_jobs_api(&lua, Arc::new(state), Some(caller)).unwrap();

        let has_create: bool = lua
            .load("return type(jobs.create) == 'function'")
            .eval_async()
            .await
            .unwrap();
        assert!(has_create);
    }

    #[tokio::test]
    async fn job_context_exposes_input() {
        let lua = crate::lua::sandbox::create_sandbox().unwrap();
        let state = test_state();
        let input = serde_json::json!({ "game_uri": "at://did:plc:test/game/123" });
        register_job_context(&lua, Arc::new(state), "test-job-id".into(), input).unwrap();

        let game_uri: String = lua
            .load("return job.input.game_uri")
            .eval_async()
            .await
            .unwrap();
        assert_eq!(game_uri, "at://did:plc:test/game/123");

        let job_id: String = lua.load("return job.id").eval_async().await.unwrap();
        assert_eq!(job_id, "test-job-id");
    }

    #[tokio::test]
    async fn job_context_has_required_functions() {
        let lua = crate::lua::sandbox::create_sandbox().unwrap();
        let state = test_state();
        register_job_context(
            &lua,
            Arc::new(state),
            "test-id".into(),
            serde_json::json!({}),
        )
        .unwrap();

        let result: bool = lua
            .load(
                r#"
                return type(job.progress) == 'function'
                    and type(job.should_stop) == 'function'
                    and type(job.wait) == 'function'
                    and type(job.log) == 'function'
                    and type(job.warn) == 'function'
                "#,
            )
            .eval_async()
            .await
            .unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn jobs_create_rejects_invalid_job_type() {
        let lua = crate::lua::sandbox::create_sandbox().unwrap();
        let state = test_state();
        let caller = JobsCaller {
            did: "did:plc:test".into(),
            api_client_id: None,
            dpop_key_id: None,
        };
        register_jobs_api(&lua, Arc::new(state), Some(caller)).unwrap();

        for bad in [
            "",
            "UPPER",
            "has space",
            "has:colon",
            "-leading-dash",
            ".leading-dot",
        ] {
            let script = format!(r#"return jobs.create("{bad}", {{}})"#);
            let result: mlua::Result<String> = lua.load(&script).eval_async().await;
            assert!(result.is_err(), "expected error for job_type={bad:?}");
        }
    }

    async fn migrated_pool() -> sqlx::AnyPool {
        sqlx::any::install_default_drivers();
        let pool = sqlx::pool::PoolOptions::<sqlx::Any>::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("connect to in-memory sqlite");
        crate::db::query(
            "CREATE TABLE happyview_jobs (
                id TEXT PRIMARY KEY,
                job_type TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                input TEXT NOT NULL DEFAULT '{}',
                progress TEXT NOT NULL DEFAULT '{}',
                result TEXT,
                error TEXT,
                created_by TEXT NOT NULL,
                started_at TEXT,
                completed_at TEXT,
                created_at TEXT NOT NULL,
                inherit_auth INTEGER NOT NULL DEFAULT 0,
                api_client_id TEXT,
                dpop_key_id TEXT
            )",
        )
        .execute(&pool)
        .await
        .expect("create happyview_jobs table");
        crate::db::query(
            "CREATE TABLE happyview_job_logs (
                id TEXT PRIMARY KEY,
                job_id TEXT NOT NULL,
                level TEXT NOT NULL DEFAULT 'info',
                message TEXT NOT NULL,
                created_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("create happyview_job_logs table");
        pool
    }

    fn test_state_with_pool(pool: sqlx::AnyPool) -> AppState {
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
            token_encryption_key: None,
            default_rate_limit_capacity: 100,
            default_rate_limit_refill_rate: 2.0,
        };
        let (tx, _) = watch::channel(vec![]);
        let (labeler_tx, _) = watch::channel(());
        let atrium_http = std::sync::Arc::new(crate::http_retry::HappyViewHttpClient::default());
        let did_resolver = atrium_identity::did::CommonDidResolver::new(
            atrium_identity::did::CommonDidResolverConfig {
                plc_directory_url: "https://plc.directory".into(),
                http_client: std::sync::Arc::clone(&atrium_http),
            },
        );
        let handle_resolver = atrium_identity::handle::AtprotoHandleResolver::new(
            atrium_identity::handle::AtprotoHandleResolverConfig {
                dns_txt_resolver: crate::dns::NativeDnsResolver::new(),
                http_client: atrium_http,
            },
        );
        let oauth = atrium_oauth::OAuthClient::new(atrium_oauth::OAuthClientConfig {
            client_metadata: atrium_oauth::AtprotoLocalhostClientMetadata {
                redirect_uris: Some(vec!["http://127.0.0.1:0/auth/callback".into()]),
                scopes: Some(vec![atrium_oauth::Scope::Known(
                    atrium_oauth::KnownScope::Atproto,
                )]),
            },
            keys: None,
            state_store: crate::auth::oauth_store::DbStateStore::new(
                pool.clone(),
                crate::db::DatabaseBackend::Sqlite,
            ),
            session_store: crate::auth::oauth_store::DbSessionStore::new(
                pool.clone(),
                crate::db::DatabaseBackend::Sqlite,
            ),
            resolver: atrium_oauth::OAuthResolverConfig {
                did_resolver,
                handle_resolver,
                authorization_server_metadata: Default::default(),
                protected_resource_metadata: Default::default(),
            },
            http_client: crate::http_retry::HappyViewHttpClient::default(),
        })
        .expect("Failed to create test OAuth client");
        AppState {
            config,
            http: reqwest::Client::new(),
            db: pool.clone(),
            backfill_db: pool.clone(),
            db_backend: crate::db::DatabaseBackend::Sqlite,
            domain_cache: crate::domain::DomainCache::new(),
            lexicons: crate::lexicon::LexiconRegistry::new(),
            collections_tx: tx,
            labeler_subscriptions_tx: labeler_tx,
            rate_limiter: crate::rate_limit::RateLimiter::new(
                crate::rate_limit::RateLimitDefaults {
                    query_cost: 1,
                    procedure_cost: 1,
                    proxy_cost: 1,
                },
            ),
            oauth: std::sync::Arc::new(crate::auth::OAuthClientRegistry::new(std::sync::Arc::new(
                oauth,
            ))),
            oauth_state_store: crate::auth::oauth_store::DbStateStore::new(
                pool.clone(),
                crate::db::DatabaseBackend::Sqlite,
            ),
            linked_repos_client: std::sync::Arc::new(
                crate::linked_repos::client::build(
                    "https://plc.directory",
                    "http://127.0.0.1:0/oauth-client-metadata.json",
                    "http://127.0.0.1:0",
                    "http://127.0.0.1:0/auth/callback".into(),
                    true,
                    vec![atrium_oauth::Scope::Known(
                        atrium_oauth::KnownScope::Atproto,
                    )],
                    crate::auth::oauth_store::DbStateStore::new(
                        pool.clone(),
                        crate::db::DatabaseBackend::Sqlite,
                    ),
                    pool.clone(),
                    crate::db::DatabaseBackend::Sqlite,
                )
                .expect("Failed to create test linked-repo OAuth client"),
            ),
            cookie_key: axum_extra::extract::cookie::Key::derive_from(
                b"test-secret-for-tests-only-not-production",
            ),
            plugin_registry: std::sync::Arc::new(crate::plugin::PluginRegistry::new()),
            wasm_runtime: std::sync::Arc::new(
                crate::plugin::WasmRuntime::new().expect("wasm runtime"),
            ),
            attestation_signer: None,
            official_registry: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::plugin::official_registry::OfficialRegistryState::default(),
            )),
            official_registry_config: crate::plugin::official_registry::RegistryConfig::production(
            ),
            proxy_config: std::sync::Arc::new(arc_swap::ArcSwap::new(std::sync::Arc::new(
                crate::proxy_config::ProxyConfig::default(),
            ))),
            backfill_events_tx: tokio::sync::broadcast::channel(16).0,
            verbose_event_logging: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    #[tokio::test]
    async fn jobs_create_with_auth_persists_dpop_context() {
        let pool = migrated_pool().await;
        let state = test_state_with_pool(pool);
        let lua = crate::lua::sandbox::create_sandbox().unwrap();
        let caller = JobsCaller {
            did: "did:plc:dpopuser".into(),
            api_client_id: Some("test_client_id".into()),
            dpop_key_id: Some("test_dpop_key".into()),
        };
        register_jobs_api(&lua, Arc::new(state.clone()), Some(caller)).unwrap();

        let job_id: String = lua
            .load(r#"return jobs.create("test.dpop-job", { foo = "bar" }, { auth = true })"#)
            .eval_async()
            .await
            .unwrap();

        let row: (String, i32, Option<String>, Option<String>) = crate::db::query_as(
            "SELECT created_by, inherit_auth, api_client_id, dpop_key_id FROM happyview_jobs WHERE id = ?",
        )
        .bind(&job_id)
        .fetch_one(&state.db)
        .await
        .expect("job should exist");

        assert_eq!(row.0, "did:plc:dpopuser");
        assert_eq!(row.1, 1);
        assert_eq!(row.2.as_deref(), Some("test_client_id"));
        assert_eq!(row.3.as_deref(), Some("test_dpop_key"));
    }

    #[tokio::test]
    async fn jobs_create_without_auth_omits_dpop_context() {
        let pool = migrated_pool().await;
        let state = test_state_with_pool(pool);
        let lua = crate::lua::sandbox::create_sandbox().unwrap();
        let caller = JobsCaller {
            did: "did:plc:dpopuser".into(),
            api_client_id: Some("should_not_appear".into()),
            dpop_key_id: Some("should_not_appear".into()),
        };
        register_jobs_api(&lua, Arc::new(state.clone()), Some(caller)).unwrap();

        let job_id: String = lua
            .load(r#"return jobs.create("test.no-auth", { foo = "bar" })"#)
            .eval_async()
            .await
            .unwrap();

        let row: (i32, Option<String>, Option<String>) = crate::db::query_as(
            "SELECT inherit_auth, api_client_id, dpop_key_id FROM happyview_jobs WHERE id = ?",
        )
        .bind(&job_id)
        .fetch_one(&state.db)
        .await
        .expect("job should exist");

        assert_eq!(row.0, 0);
        assert!(row.1.is_none());
        assert!(row.2.is_none());
    }

    #[tokio::test]
    async fn job_log_inserts_info_row() {
        let pool = migrated_pool().await;
        let state = test_state_with_pool(pool);
        let lua = crate::lua::sandbox::create_sandbox().unwrap();
        register_job_context(
            &lua,
            Arc::new(state.clone()),
            "log-test-job".into(),
            serde_json::json!({}),
        )
        .unwrap();

        lua.load(r#"job.log("hello from lua")"#)
            .exec_async()
            .await
            .unwrap();

        let row: (String, String, String) = crate::db::query_as(
            "SELECT job_id, level, message FROM happyview_job_logs WHERE job_id = 'log-test-job'",
        )
        .fetch_one(&state.db)
        .await
        .expect("log row should exist");

        assert_eq!(row.0, "log-test-job");
        assert_eq!(row.1, "info");
        assert_eq!(row.2, "hello from lua");
    }

    #[tokio::test]
    async fn job_warn_inserts_warn_row() {
        let pool = migrated_pool().await;
        let state = test_state_with_pool(pool);
        let lua = crate::lua::sandbox::create_sandbox().unwrap();
        register_job_context(
            &lua,
            Arc::new(state.clone()),
            "warn-test-job".into(),
            serde_json::json!({}),
        )
        .unwrap();

        lua.load(r#"job.warn("something is off")"#)
            .exec_async()
            .await
            .unwrap();

        let row: (String, String, String) = crate::db::query_as(
            "SELECT job_id, level, message FROM happyview_job_logs WHERE job_id = 'warn-test-job'",
        )
        .fetch_one(&state.db)
        .await
        .expect("warn row should exist");

        assert_eq!(row.0, "warn-test-job");
        assert_eq!(row.1, "warn");
        assert_eq!(row.2, "something is off");
    }
}

use mlua::{Lua, Result as LuaResult};
use reqwest::Method;
use std::sync::Arc;

use crate::AppState;

/// Register the `http` table with async HTTP request functions.
pub fn register_http_api(lua: &Lua, state: Arc<AppState>) -> LuaResult<()> {
    let http_table = lua.create_table()?;

    let methods = [
        ("get", Method::GET),
        ("post", Method::POST),
        ("put", Method::PUT),
        ("patch", Method::PATCH),
        ("delete", Method::DELETE),
        ("head", Method::HEAD),
    ];

    for (name, method) in methods {
        let state_clone = state.clone();
        let func =
            lua.create_async_function(move |lua, (url, opts): (String, Option<mlua::Table>)| {
                let state = state_clone.clone();
                let method = method.clone();
                async move {
                    let mut builder = state.http.request(method.clone(), &url);

                    if let Some(ref opts) = opts {
                        if let Ok(headers_table) = opts.get::<mlua::Table>("headers") {
                            for pair in headers_table.pairs::<String, String>() {
                                let (key, value) = pair?;
                                builder = builder.header(key, value);
                            }
                        }

                        if method != Method::GET
                            && method != Method::HEAD
                            && let Ok(body) = opts.get::<String>("body")
                        {
                            builder = builder.body(body);
                        }
                    }

                    let response = builder
                        .send()
                        .await
                        .map_err(|e| mlua::Error::runtime(format!("HTTP request failed: {e}")))?;

                    let status = response.status().as_u16();

                    let headers_table = lua.create_table()?;
                    for (key, value) in response.headers() {
                        if let Ok(v) = value.to_str() {
                            headers_table.set(key.as_str().to_lowercase(), v.to_string())?;
                        }
                    }

                    let body = if method == Method::HEAD {
                        String::new()
                    } else {
                        response.text().await.map_err(|e| {
                            mlua::Error::runtime(format!("HTTP read body failed: {e}"))
                        })?
                    };

                    let result = lua.create_table()?;
                    result.set("status", status)?;
                    result.set("body", body)?;
                    result.set("headers", headers_table)?;

                    Ok(mlua::Value::Table(result))
                }
            })?;
        http_table.set(name, func)?;
    }

    lua.globals().set("http", http_table)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::lexicon::LexiconRegistry;
    use tokio::sync::watch;

    fn test_state() -> AppState {
        let config = Config {
            host: "127.0.0.1".into(),
            port: 3000,
            database_url: String::new(),
            database_backend: crate::db::DatabaseBackend::Sqlite,
            public_url: String::new(),
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
        let atrium_http = std::sync::Arc::new(atrium_oauth::DefaultHttpClient::default());
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
        })
        .expect("Failed to create test OAuth client");
        AppState {
            config,
            http: reqwest::Client::new(),
            db: test_db.clone(),
            backfill_db: test_db.clone(),
            db_backend: crate::db::DatabaseBackend::Sqlite,
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

    fn setup(state: &AppState) -> Lua {
        let lua = Lua::new();
        register_http_api(&lua, Arc::new(state.clone())).unwrap();
        lua
    }

    #[tokio::test]
    async fn get_returns_status_and_body() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/test"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("hello"))
            .mount(&mock)
            .await;

        let state = test_state();
        let lua = setup(&state);
        let chunk = format!(r#"return http.get("{}/test")"#, mock.uri());
        let result: mlua::Table = lua.load(chunk).eval_async().await.unwrap();
        assert_eq!(result.get::<u16>("status").unwrap(), 200);
        assert_eq!(result.get::<String>("body").unwrap(), "hello");
    }

    #[tokio::test]
    async fn get_returns_headers() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/h"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("X-Custom", "test-value")
                    .set_body_string(""),
            )
            .mount(&mock)
            .await;

        let state = test_state();
        let lua = setup(&state);
        let chunk = format!(r#"return http.get("{}/h")"#, mock.uri());
        let result: mlua::Table = lua.load(chunk).eval_async().await.unwrap();
        let headers: mlua::Table = result.get("headers").unwrap();
        assert_eq!(headers.get::<String>("x-custom").unwrap(), "test-value");
    }

    #[tokio::test]
    async fn post_sends_body_and_headers() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/post"))
            .and(wiremock::matchers::header(
                "content-type",
                "application/json",
            ))
            .and(wiremock::matchers::body_string(r#"{"k":"v"}"#))
            .respond_with(wiremock::ResponseTemplate::new(201).set_body_string("created"))
            .mount(&mock)
            .await;

        let state = test_state();
        let lua = setup(&state);
        let chunk = format!(
            r#"return http.post("{}/post", {{
                body = '{{"k":"v"}}',
                headers = {{ ["content-type"] = "application/json" }}
            }})"#,
            mock.uri()
        );
        let result: mlua::Table = lua.load(chunk).eval_async().await.unwrap();
        assert_eq!(result.get::<u16>("status").unwrap(), 201);
        assert_eq!(result.get::<String>("body").unwrap(), "created");
    }

    #[tokio::test]
    async fn head_returns_empty_body() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("HEAD"))
            .and(wiremock::matchers::path("/head"))
            .respond_with(wiremock::ResponseTemplate::new(204))
            .mount(&mock)
            .await;

        let state = test_state();
        let lua = setup(&state);
        let chunk = format!(r#"return http.head("{}/head")"#, mock.uri());
        let result: mlua::Table = lua.load(chunk).eval_async().await.unwrap();
        assert_eq!(result.get::<u16>("status").unwrap(), 204);
        assert_eq!(result.get::<String>("body").unwrap(), "");
    }

    #[tokio::test]
    async fn put_sends_body() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("PUT"))
            .and(wiremock::matchers::path("/put"))
            .and(wiremock::matchers::body_string("updated"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&mock)
            .await;

        let state = test_state();
        let lua = setup(&state);
        let chunk = format!(
            r#"return http.put("{}/put", {{ body = "updated" }})"#,
            mock.uri()
        );
        let result: mlua::Table = lua.load(chunk).eval_async().await.unwrap();
        assert_eq!(result.get::<u16>("status").unwrap(), 200);
    }

    #[tokio::test]
    async fn delete_works() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("DELETE"))
            .and(wiremock::matchers::path("/del"))
            .respond_with(wiremock::ResponseTemplate::new(204).set_body_string(""))
            .mount(&mock)
            .await;

        let state = test_state();
        let lua = setup(&state);
        let chunk = format!(r#"return http.delete("{}/del")"#, mock.uri());
        let result: mlua::Table = lua.load(chunk).eval_async().await.unwrap();
        assert_eq!(result.get::<u16>("status").unwrap(), 204);
    }

    #[tokio::test]
    async fn patch_sends_body() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("PATCH"))
            .and(wiremock::matchers::path("/patch"))
            .and(wiremock::matchers::body_string("patched"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&mock)
            .await;

        let state = test_state();
        let lua = setup(&state);
        let chunk = format!(
            r#"return http.patch("{}/patch", {{ body = "patched" }})"#,
            mock.uri()
        );
        let result: mlua::Table = lua.load(chunk).eval_async().await.unwrap();
        assert_eq!(result.get::<u16>("status").unwrap(), 200);
    }

    #[tokio::test]
    async fn get_without_opts_works() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/simple"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&mock)
            .await;

        let state = test_state();
        let lua = setup(&state);
        let chunk = format!(r#"return http.get("{}/simple")"#, mock.uri());
        let result: mlua::Table = lua.load(chunk).eval_async().await.unwrap();
        assert_eq!(result.get::<u16>("status").unwrap(), 200);
        assert_eq!(result.get::<String>("body").unwrap(), "ok");
    }

    #[tokio::test]
    async fn invalid_url_returns_error() {
        let state = test_state();
        let lua = setup(&state);
        let result: Result<mlua::Table, _> = lua
            .load(r#"return http.get("http://0.0.0.0:1/nope")"#)
            .eval_async()
            .await;
        assert!(result.is_err());
    }
}

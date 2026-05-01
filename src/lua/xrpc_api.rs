use axum::response::Response;
use http_body_util::BodyExt;
use mlua::{Lua, LuaSerdeExt, Result as LuaResult};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use crate::AppState;
use crate::auth::Claims;
use crate::lexicon::LexiconType;
use crate::xrpc;

/// Convert an axum Response into a Lua table with `{ status, body }`.
async fn response_to_lua_table(lua: &Lua, response: Response) -> LuaResult<mlua::Table> {
    let status = response.status().as_u16();
    let body_bytes = response
        .into_body()
        .collect()
        .await
        .map_err(|e| mlua::Error::runtime(format!("failed to read response body: {e}")))?
        .to_bytes();
    let body = String::from_utf8_lossy(&body_bytes).to_string();

    let table = lua.create_table()?;
    table.set("status", status)?;
    table.set("body", body)?;
    Ok(table)
}

/// Convert an Option<mlua::Table> of params into a HashMap<String, Value>.
fn lua_table_to_params(lua: &Lua, table: Option<mlua::Table>) -> LuaResult<HashMap<String, Value>> {
    match table {
        Some(t) => {
            let value: Value = lua.from_value(mlua::Value::Table(t))?;
            match value {
                Value::Object(map) => Ok(map.into_iter().collect()),
                _ => Ok(HashMap::new()),
            }
        }
        None => Ok(HashMap::new()),
    }
}

pub fn register_xrpc_api(
    lua: &Lua,
    state: Arc<AppState>,
    caller_did: Option<String>,
) -> LuaResult<()> {
    let xrpc_table = lua.create_table()?;

    // xrpc.query(method, params?)
    {
        let state = state.clone();
        let caller_did = caller_did.clone();
        let func = lua.create_async_function(
            move |lua, (method, params): (String, Option<mlua::Table>)| {
                let state = state.clone();
                let caller_did = caller_did.clone();
                async move {
                    let mut params = lua_table_to_params(&lua, params)?;
                    let claims = caller_did.map(Claims::internal);

                    let response =
                        execute_local_query(&state, &method, &mut params, claims.as_ref())
                            .await
                            .map_err(|e| mlua::Error::runtime(format!("xrpc query failed: {e}")))?;

                    response_to_lua_table(&lua, response).await
                }
            },
        )?;
        xrpc_table.set("query", func)?;
    }

    // xrpc.procedure(method, input, params?)
    {
        let state = state.clone();
        let caller_did = caller_did.clone();
        let func = lua.create_async_function(
            move |lua, (method, input, params): (String, mlua::Value, Option<mlua::Table>)| {
                let state = state.clone();
                let caller_did = caller_did.clone();
                async move {
                    let mut params = lua_table_to_params(&lua, params)?;
                    let input: Value = lua.from_value(input)?;
                    let claims = caller_did.clone().map(Claims::internal).ok_or_else(|| {
                        mlua::Error::runtime(
                            "xrpc.procedure requires authentication (no caller_did)",
                        )
                    })?;

                    let response =
                        execute_local_procedure(&state, &method, &claims, &input, &mut params)
                            .await
                            .map_err(|e| {
                                mlua::Error::runtime(format!("xrpc procedure failed: {e}"))
                            })?;

                    response_to_lua_table(&lua, response).await
                }
            },
        )?;
        xrpc_table.set("procedure", func)?;
    }

    lua.globals().set("xrpc", xrpc_table)?;
    Ok(())
}

/// Execute a query XRPC — local handler if known, proxy if not.
async fn execute_local_query(
    state: &AppState,
    method: &str,
    params: &mut HashMap<String, Value>,
    claims: Option<&Claims>,
) -> Result<Response, crate::error::AppError> {
    let lexicon = state.lexicons.get(method).await;

    match lexicon {
        Some(lex) => {
            if lex.lexicon_type != LexiconType::Query {
                return Err(crate::error::AppError::BadRequest(format!(
                    "{method} is not a query endpoint"
                )));
            }
            if let Some(ref param_schema) = lex.parameters {
                xrpc::coerce_params(params, param_schema);
            }
            xrpc::query::handle_query(state, method, params, &lex, claims).await
        }
        None => {
            let query_string = params_to_query_string(params);
            xrpc::proxy_to_authority(state, method, &query_string, None).await
        }
    }
}

/// Build a query string from a params HashMap (used by proxy path).
fn params_to_query_string(params: &HashMap<String, Value>) -> String {
    params
        .iter()
        .map(|(k, v)| {
            let val = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            format!("{}={}", urlencoding::encode(k), urlencoding::encode(&val))
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// Execute a procedure XRPC — local handler if known, proxy if not.
async fn execute_local_procedure(
    state: &AppState,
    method: &str,
    claims: &Claims,
    input: &Value,
    params: &mut HashMap<String, Value>,
) -> Result<Response, crate::error::AppError> {
    let lexicon = state.lexicons.get(method).await;

    match lexicon {
        Some(lex) => {
            if lex.lexicon_type != LexiconType::Procedure {
                return Err(crate::error::AppError::BadRequest(format!(
                    "{method} is not a procedure endpoint"
                )));
            }
            if let Some(ref param_schema) = lex.parameters {
                xrpc::coerce_params(params, param_schema);
            }
            xrpc::procedure::handle_procedure(state, method, claims, input, params, &lex).await
        }
        None => {
            let query_string = params_to_query_string(params);
            xrpc::proxy_to_authority(state, method, &query_string, Some(input)).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db::DatabaseBackend;
    use crate::lexicon::{LexiconRegistry, LexiconType, ParsedLexicon, ProcedureAction};
    use crate::lua::sandbox;
    use mlua::LuaSerdeExt;
    use serde_json::json;
    use tokio::sync::watch;

    fn test_state() -> AppState {
        let config = Config {
            host: "127.0.0.1".into(),
            port: 3000,
            database_url: String::new(),
            database_backend: DatabaseBackend::Sqlite,
            public_url: String::new(),
            session_secret: "test-secret".into(),
            jetstream_url: String::new(),
            relay_url: String::new(),
            plc_url: String::new(),
            static_dir: String::new(),
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
        // Single-connection pool so the in-memory DB is shared across the
        // pool's queries (separate connections to `sqlite::memory:` get
        // independent DBs otherwise).
        let test_db = sqlx::pool::PoolOptions::<sqlx::Any>::new()
            .max_connections(1)
            .connect_lazy("sqlite::memory:")
            .unwrap();
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
                DatabaseBackend::Sqlite,
            ),
            session_store: crate::auth::oauth_store::DbSessionStore::new(
                test_db.clone(),
                DatabaseBackend::Sqlite,
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
                DatabaseBackend::Sqlite,
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
        }
    }

    /// Create the `scripts` table on the in-memory test DB and insert one
    /// row keyed by trigger id. The trigger-keyed dispatcher reads from
    /// here at firing time; `make_*_lexicon`'s `script` field is now
    /// inert (kept on the struct for forward-compat with row loaders but
    /// not consulted by dispatch).
    async fn seed_script(state: &AppState, trigger: &str, body: &str) {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS scripts (
                id          TEXT PRIMARY KEY,
                body        TEXT NOT NULL,
                description TEXT,
                script_type TEXT NOT NULL DEFAULT 'lua',
                created_at  TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
        )
        .execute(&state.db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT OR REPLACE INTO scripts (id, body, script_type, created_at, updated_at)
             VALUES (?, ?, 'lua', datetime('now'), datetime('now'))",
        )
        .bind(trigger)
        .bind(body)
        .execute(&state.db)
        .await
        .unwrap();
    }

    fn make_query_lexicon(id: &str, script: Option<&str>) -> ParsedLexicon {
        ParsedLexicon {
            id: id.to_string(),
            lexicon_type: LexiconType::Query,
            record_key: None,
            parameters: None,
            input: None,
            output: None,
            record_schema: None,
            raw: json!({}),
            revision: 1,
            target_collection: None,
            action: ProcedureAction::Create,
            script: script.map(|s| s.to_string()),
            index_hook: None,
            token_cost: None,
            space_type: None,
        }
    }

    fn make_procedure_lexicon(id: &str, script: Option<&str>) -> ParsedLexicon {
        ParsedLexicon {
            id: id.to_string(),
            lexicon_type: LexiconType::Procedure,
            record_key: None,
            parameters: None,
            input: None,
            output: None,
            record_schema: None,
            raw: json!({}),
            revision: 1,
            target_collection: None,
            action: ProcedureAction::Create,
            script: script.map(|s| s.to_string()),
            index_hook: None,
            token_cost: None,
            space_type: None,
        }
    }

    // -----------------------------------------------------------------------
    // Registration
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn register_xrpc_api_creates_global() {
        let state = Arc::new(test_state());
        let lua = sandbox::create_sandbox().unwrap();
        register_xrpc_api(&lua, state, Some("did:plc:test".into())).unwrap();

        let xrpc: mlua::Table = lua.globals().get("xrpc").unwrap();
        assert!(xrpc.get::<mlua::Function>("query").is_ok());
        assert!(xrpc.get::<mlua::Function>("procedure").is_ok());
    }

    #[tokio::test]
    async fn register_xrpc_api_without_caller_did() {
        let state = Arc::new(test_state());
        let lua = sandbox::create_sandbox().unwrap();
        register_xrpc_api(&lua, state, None).unwrap();

        let xrpc: mlua::Table = lua.globals().get("xrpc").unwrap();
        assert!(xrpc.get::<mlua::Function>("query").is_ok());
        assert!(xrpc.get::<mlua::Function>("procedure").is_ok());
    }

    // -----------------------------------------------------------------------
    // lua_table_to_params
    // -----------------------------------------------------------------------

    #[test]
    fn lua_table_to_params_none_returns_empty() {
        let lua = sandbox::create_sandbox().unwrap();
        let result = lua_table_to_params(&lua, None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn lua_table_to_params_converts_string_values() {
        let lua = sandbox::create_sandbox().unwrap();
        let table = lua.create_table().unwrap();
        table.set("handle", "user.bsky.social").unwrap();
        table.set("limit", 10).unwrap();

        let result = lua_table_to_params(&lua, Some(table)).unwrap();
        assert_eq!(result.get("handle").unwrap(), "user.bsky.social");
        assert_eq!(result.get("limit").unwrap(), 10);
    }

    // -----------------------------------------------------------------------
    // params_to_query_string
    // -----------------------------------------------------------------------

    #[test]
    fn params_to_query_string_empty() {
        let params = HashMap::new();
        assert_eq!(params_to_query_string(&params), "");
    }

    #[test]
    fn params_to_query_string_encodes_values() {
        let mut params = HashMap::new();
        params.insert("handle".into(), Value::String("user.bsky.social".into()));
        let qs = params_to_query_string(&params);
        assert!(qs.contains("handle=user.bsky.social"));
    }

    #[test]
    fn params_to_query_string_url_encodes_special_chars() {
        let mut params = HashMap::new();
        params.insert(
            "uri".into(),
            Value::String("at://did:plc:abc/col/rkey".into()),
        );
        let qs = params_to_query_string(&params);
        assert!(qs.contains("uri=at%3A%2F%2Fdid%3Aplc%3Aabc%2Fcol%2Frkey"));
    }

    // -----------------------------------------------------------------------
    // execute_local_query
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn query_local_script_returns_json() {
        let state = test_state();

        // Register a scripted query that returns a static response
        let lexicon = make_query_lexicon("test.echo", None);
        state.lexicons.upsert(lexicon).await;
        seed_script(
            &state,
            "xrpc.query:test.echo",
            r#"function handle() return { greeting = "hello" } end"#,
        )
        .await;

        let mut params = HashMap::new();
        let result = execute_local_query(&state, "test.echo", &mut params, None).await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());

        let response = result.unwrap();
        assert_eq!(response.status(), 200);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["greeting"], "hello");
    }

    #[tokio::test]
    async fn query_local_script_receives_params() {
        let state = test_state();

        let lexicon = make_query_lexicon("test.greet", None);
        state.lexicons.upsert(lexicon).await;
        seed_script(
            &state,
            "xrpc.query:test.greet",
            r#"function handle() return { greeting = "hello " .. params.name } end"#,
        )
        .await;

        let mut params = HashMap::new();
        params.insert("name".into(), Value::String("world".into()));
        let result = execute_local_query(&state, "test.greet", &mut params, None).await;
        assert!(result.is_ok());

        let body = result
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["greeting"], "hello world");
    }

    #[tokio::test]
    async fn query_local_script_receives_caller_did() {
        let state = test_state();

        let lexicon = make_query_lexicon("test.whoami", None);
        state.lexicons.upsert(lexicon).await;
        seed_script(
            &state,
            "xrpc.query:test.whoami",
            r#"function handle()
                return { did = caller_did or "anonymous" }
            end"#,
        )
        .await;

        // With caller_did
        let claims = Claims::internal("did:plc:testuser".into());
        let mut params = HashMap::new();
        let result = execute_local_query(&state, "test.whoami", &mut params, Some(&claims)).await;
        assert!(result.is_ok());
        let body = result
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["did"], "did:plc:testuser");

        // Without caller_did
        let mut params = HashMap::new();
        let result = execute_local_query(&state, "test.whoami", &mut params, None).await;
        assert!(result.is_ok());
        let body = result
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["did"], "anonymous");
    }

    #[tokio::test]
    async fn query_rejects_procedure_lexicon() {
        let state = test_state();

        let lexicon = make_procedure_lexicon("test.create", None);
        state.lexicons.upsert(lexicon).await;

        let mut params = HashMap::new();
        let result = execute_local_query(&state, "test.create", &mut params, None).await;
        assert!(result.is_err());
        let err = format!("{:?}", result.unwrap_err());
        assert!(err.contains("not a query endpoint"), "got: {err}");
    }

    #[tokio::test]
    async fn procedure_rejects_query_lexicon() {
        let state = test_state();

        let lexicon = make_query_lexicon("test.echo", Some("function handle() end"));
        state.lexicons.upsert(lexicon).await;

        let claims = Claims::internal("did:plc:test".into());
        let mut params = HashMap::new();
        let result =
            execute_local_procedure(&state, "test.echo", &claims, &json!({}), &mut params).await;
        assert!(result.is_err());
        let err = format!("{:?}", result.unwrap_err());
        assert!(err.contains("not a procedure endpoint"), "got: {err}");
    }

    // -----------------------------------------------------------------------
    // Lua integration: xrpc.query from within a script
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn lua_script_calls_xrpc_query() {
        let state = test_state();

        // Register a simple query that the outer script will call
        let inner_lexicon = make_query_lexicon("test.inner", None);
        state.lexicons.upsert(inner_lexicon).await;
        seed_script(
            &state,
            "xrpc.query:test.inner",
            r#"function handle() return { value = 42 } end"#,
        )
        .await;

        let state_arc = Arc::new(state);
        let lua = sandbox::create_sandbox().unwrap();

        register_xrpc_api(&lua, state_arc, None).unwrap();

        // Script that calls xrpc.query and parses the result
        lua.load(
            r#"
            function handle()
                local resp = xrpc.query("test.inner")
                local data = json.decode(resp.body)
                return { status = resp.status, inner_value = data.value }
            end
            "#,
        )
        .exec()
        .unwrap();

        // Register json global for the script
        let json_table = lua.create_table().unwrap();
        let decode = lua
            .create_function(|lua, s: String| {
                let val: Value = serde_json::from_str(&s)
                    .map_err(|e| mlua::Error::runtime(format!("json decode: {e}")))?;
                lua.to_value(&val)
            })
            .unwrap();
        json_table.set("decode", decode).unwrap();
        lua.globals().set("json", json_table).unwrap();

        let handle: mlua::Function = lua.globals().get("handle").unwrap();
        let result: mlua::Value = handle.call_async(()).await.unwrap();
        let json_result: Value = lua.from_value(result).unwrap();

        assert_eq!(json_result["status"], 200);
        assert_eq!(json_result["inner_value"], 42);
    }

    // -----------------------------------------------------------------------
    // Lua integration: xrpc.procedure requires caller_did
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn lua_xrpc_procedure_fails_without_caller_did() {
        let state = test_state();
        let state_arc = Arc::new(state);
        let lua = sandbox::create_sandbox().unwrap();

        // Register with no caller_did
        register_xrpc_api(&lua, state_arc, None).unwrap();

        lua.load(
            r#"
            function handle()
                return xrpc.procedure("test.something", {})
            end
            "#,
        )
        .exec()
        .unwrap();

        let handle: mlua::Function = lua.globals().get("handle").unwrap();
        let result: Result<mlua::Value, _> = handle.call_async(()).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("requires authentication"),
            "expected auth error, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // response_to_lua_table
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn response_to_lua_table_converts_correctly() {
        let lua = sandbox::create_sandbox().unwrap();
        let response = axum::response::Response::builder()
            .status(200)
            .body(axum::body::Body::from(r#"{"ok":true}"#))
            .unwrap();

        let table = response_to_lua_table(&lua, response).await.unwrap();
        assert_eq!(table.get::<u16>("status").unwrap(), 200);
        assert_eq!(table.get::<String>("body").unwrap(), r#"{"ok":true}"#);
    }

    #[tokio::test]
    async fn response_to_lua_table_preserves_error_status() {
        let lua = sandbox::create_sandbox().unwrap();
        let response = axum::response::Response::builder()
            .status(404)
            .body(axum::body::Body::from("not found"))
            .unwrap();

        let table = response_to_lua_table(&lua, response).await.unwrap();
        assert_eq!(table.get::<u16>("status").unwrap(), 404);
        assert_eq!(table.get::<String>("body").unwrap(), "not found");
    }

    // -----------------------------------------------------------------------
    // Claims::internal
    // -----------------------------------------------------------------------

    #[test]
    fn claims_internal_has_no_client_key() {
        let claims = Claims::internal("did:plc:test".into());
        assert_eq!(claims.did(), "did:plc:test");
        assert!(claims.client_key().is_none());
    }
}

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use mlua::{Lua, LuaSerdeExt, Result as LuaResult};
use serde_json::Value as JsonValue;
use sqlx::{Column, Row};
use std::sync::Arc;

use crate::AppState;
use crate::db::DatabaseBackend;
use crate::plugin::host::{
    MAX_FILTER_DEPTH, RecordsError, backlinks_query, is_valid_json_field_path, records_count,
    records_get, records_query, records_search,
};
use crate::raw_sql_guard::check_raw_sql_tables;
use happyview_plugin_sdk::wire::{
    BacklinksQuery, Condition, Filter, RecordsCount, RecordsPage, RecordsQuery, RecordsSearch, Sort,
};

/// Parse Lua's `{field, op, value}` / `{combine, child, child}` filter table
/// shape into the SDK's wire `Filter`. This grammar is Lua-specific: a plain
/// Lua value in the `value` field rather than a pre-stringified one, and `op`
/// defaulting to `=`.
fn parse_filter_node(table: &mlua::Table, depth: u8) -> LuaResult<Filter> {
    if depth >= MAX_FILTER_DEPTH {
        return Err(mlua::Error::runtime(format!(
            "filter nesting too deep (max {MAX_FILTER_DEPTH} levels)",
        )));
    }

    if let Ok(field) = table.get::<String>("field") {
        if !is_valid_json_field_path(&field) {
            return Err(mlua::Error::runtime(format!(
                "invalid filter field '{field}': use alphanumeric names with optional dot notation and array indices (e.g. 'name', 'author.handle', 'tags[0]')",
            )));
        }

        let op: String = table
            .get::<String>("op")
            .unwrap_or_else(|_| "=".to_string());

        let val: mlua::Value = table.get("value")?;
        let value = match val {
            mlua::Value::String(s) => s.to_str()?.to_string(),
            mlua::Value::Integer(n) => n.to_string(),
            mlua::Value::Number(n) => n.to_string(),
            mlua::Value::Boolean(b) => (if b { "true" } else { "false" }).to_string(),
            other => {
                return Err(mlua::Error::runtime(format!(
                    "unsupported filter value type for '{field}': {}",
                    other.type_name()
                )));
            }
        };

        return Ok(Filter::Condition(Condition {
            field,
            op,
            value: JsonValue::String(value),
        }));
    }

    let combine: String = table
        .get::<String>("combine")
        .unwrap_or_else(|_| "AND".to_string());

    let mut conditions = Vec::new();
    for child in table.sequence_values::<mlua::Table>() {
        conditions.push(parse_filter_node(&child?, depth + 1)?);
    }

    if conditions.is_empty() {
        return Err(mlua::Error::runtime("filter group has no conditions"));
    }

    Ok(Filter::Group {
        combine,
        conditions,
    })
}

fn lua_err(e: RecordsError) -> mlua::Error {
    match e {
        RecordsError::InvalidSpec(msg) => mlua::Error::runtime(msg),
        RecordsError::Database(e) => mlua::Error::runtime(format!("DB query failed: {e}")),
    }
}

fn page_to_lua(lua: &Lua, page: RecordsPage) -> LuaResult<mlua::Value> {
    let result = lua.create_table()?;
    if let Some(cursor) = page.cursor {
        result.set("cursor", cursor)?;
    }
    let values: Vec<mlua::Value> = page
        .records
        .iter()
        .map(|r| lua.to_value(r))
        .collect::<LuaResult<_>>()?;
    let records = lua.create_sequence_from(values)?;
    records.set_metatable(Some(lua.array_metatable()))?;
    result.set("records", records)?;
    Ok(mlua::Value::Table(result))
}

/// Register the `db` table with database query functions.
pub fn register_db_api(lua: &Lua, state: Arc<AppState>) -> LuaResult<()> {
    let db_table = lua.create_table()?;

    // db.query({ collection, did?, limit?, offset?, cursor?, sort?, sortDirection?, filter? }) -> { records, cursor? }
    let state_query = state.clone();
    let query_fn = lua.create_async_function(move |lua, opts: mlua::Table| {
        let state = state_query.clone();
        async move {
            let filter = match opts.get::<Option<mlua::Table>>("filter")? {
                Some(t) => Some(parse_filter_node(&t, 0)?),
                None => None,
            };
            // Validated independent of whether `sort` is given: a caller who
            // passes a bogus `sortDirection` gets told so even though it would
            // otherwise be silently unused.
            let sort_direction: Option<String> = opts.get("sortDirection")?;
            if let Some(ref direction) = sort_direction
                && direction != "asc"
                && direction != "desc"
            {
                return Err(mlua::Error::runtime(format!(
                    "invalid sortDirection '{direction}': must be 'asc' or 'desc'"
                )));
            }
            let sort = opts.get::<Option<String>>("sort")?.map(|field| Sort {
                field,
                direction: sort_direction.unwrap_or_else(|| "desc".into()),
            });
            // A bare `offset` is accepted for custom sorts by turning it into
            // the same cursor the previous page would have returned.
            let cursor = match (
                opts.get::<Option<String>>("cursor")?,
                opts.get::<Option<i64>>("offset")?,
            ) {
                (Some(c), _) => Some(c),
                (None, Some(off)) if sort.is_some() => Some(BASE64.encode(off.to_string())),
                _ => None,
            };
            let page = records_query(
                &state.db,
                state.db_backend,
                RecordsQuery {
                    collection: opts.get("collection")?,
                    did: opts.get("did").ok(),
                    filter,
                    sort,
                    limit: opts.get::<Option<u32>>("limit")?,
                    cursor,
                },
            )
            .await
            .map_err(lua_err)?;
            page_to_lua(&lua, page)
        }
    })?;
    db_table.set("query", query_fn)?;

    // db.get(uri) -> record table or nil
    let state_get = state.clone();
    let get_fn = lua.create_async_function(move |lua, uri: String| {
        let state = state_get.clone();
        async move {
            let record = records_get(&state.db, state.db_backend, &uri)
                .await
                .map_err(lua_err)?;
            match record {
                Some(record) => lua.to_value(&record),
                None => Ok(mlua::Value::Nil),
            }
        }
    })?;
    db_table.set("get", get_fn)?;

    // db.search({ collection, field, query, limit? }) -> { records }
    let state_search = state.clone();
    let search_fn = lua.create_async_function(move |lua, opts: mlua::Table| {
        let state = state_search.clone();
        async move {
            let records = records_search(
                &state.db,
                state.db_backend,
                RecordsSearch {
                    collection: opts.get("collection")?,
                    field: opts.get("field")?,
                    query: opts.get("query")?,
                    limit: opts.get::<Option<u32>>("limit")?,
                },
            )
            .await
            .map_err(lua_err)?;

            let record_values: Vec<mlua::Value> = records
                .iter()
                .map(|r| lua.to_value(r))
                .collect::<LuaResult<_>>()?;
            let records_table = lua.create_sequence_from(record_values)?;
            records_table.set_metatable(Some(lua.array_metatable()))?;

            let result_table = lua.create_table()?;
            result_table.set("records", records_table)?;

            Ok(mlua::Value::Table(result_table))
        }
    })?;
    db_table.set("search", search_fn)?;

    // db.count(collection, did?) -> integer
    let state_count = state.clone();
    let count_fn =
        lua.create_async_function(move |_, (collection, did): (String, Option<String>)| {
            let state = state_count.clone();
            async move {
                records_count(
                    &state.db,
                    state.db_backend,
                    RecordsCount {
                        collection,
                        did,
                        filter: None,
                    },
                )
                .await
                .map_err(lua_err)
            }
        })?;
    db_table.set("count", count_fn)?;

    // db.backlinks({ collection, uri, did?, limit?, cursor? }) -> { records, cursor? }
    // Find records in `collection` that reference the given AT URI via record_refs.
    let state_backlinks = state.clone();
    let backlinks_fn = lua.create_async_function(move |lua, opts: mlua::Table| {
        let state = state_backlinks.clone();
        async move {
            let page = backlinks_query(
                &state.db,
                state.db_backend,
                BacklinksQuery {
                    collection: opts.get("collection")?,
                    uri: opts.get("uri")?,
                    did: opts.get("did").ok(),
                    limit: opts.get::<Option<u32>>("limit")?,
                    cursor: opts.get("cursor").ok(),
                },
            )
            .await
            .map_err(lua_err)?;
            page_to_lua(&lua, page)
        }
    })?;
    db_table.set("backlinks", backlinks_fn)?;

    // db.raw(sql, params?) -> rows[]
    let state_raw = state.clone();
    let raw_fn =
        lua.create_async_function(move |lua, (sql, params): (String, Option<mlua::Table>)| {
            let state = state_raw.clone();
            async move {
                // Protect HappyView's internal tables (secrets, auth, config,
                // AppView bookkeeping) from raw access; own tables are fine.
                check_raw_sql_tables(&sql).map_err(mlua::Error::runtime)?;

                let mut query = crate::db::query(&sql);
                if let Some(ref params_table) = params {
                    for value in params_table.sequence_values::<mlua::Value>() {
                        let value = value?;
                        query = match value {
                            mlua::Value::String(s) => query.bind(s.to_str()?.to_string()),
                            mlua::Value::Integer(n) => query.bind(n),
                            mlua::Value::Number(n) => query.bind(n),
                            mlua::Value::Boolean(b) => query.bind(if b { 1_i32 } else { 0_i32 }),
                            mlua::Value::Nil => query.bind(Option::<String>::None),
                            other => {
                                return Err(mlua::Error::runtime(format!(
                                    "unsupported parameter type: {}",
                                    other.type_name()
                                )));
                            }
                        };
                    }
                }

                let rows = query
                    .fetch_all(&state.db)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("db.raw query failed: {e}")))?;

                // Convert rows to Lua tables
                let mut lua_rows: Vec<mlua::Value> = Vec::with_capacity(rows.len());
                for row in &rows {
                    let row_table = lua.create_table()?;
                    for col in row.columns() {
                        let name = col.name();
                        let lua_val: mlua::Value = match row.try_get::<String, _>(name) {
                            Ok(s) => mlua::Value::String(lua.create_string(&s)?),
                            Err(_) => match row.try_get::<i64, _>(name) {
                                Ok(n) => mlua::Value::Integer(n),
                                Err(_) => match row.try_get::<i32, _>(name) {
                                    Ok(n) => mlua::Value::Integer(n as i64),
                                    Err(_) => match row.try_get::<f64, _>(name) {
                                        Ok(n) => mlua::Value::Number(n),
                                        Err(_) => match row.try_get::<bool, _>(name) {
                                            Ok(b) => mlua::Value::Boolean(b),
                                            Err(_) => mlua::Value::Nil,
                                        },
                                    },
                                },
                            },
                        };
                        row_table.set(name, lua_val)?;
                    }
                    lua_rows.push(mlua::Value::Table(row_table));
                }

                let result = lua.create_sequence_from(lua_rows)?;
                result.set_metatable(Some(lua.array_metatable()))?;
                Ok(result)
            }
        })?;
    db_table.set("raw", raw_fn)?;

    // db.backend() -> "sqlite" | "postgres"
    let backend = state.db_backend;
    let backend_fn = lua.create_function(move |_, ()| {
        Ok(match backend {
            DatabaseBackend::Sqlite => "sqlite",
            DatabaseBackend::Postgres => "postgres",
        })
    })?;
    db_table.set("backend", backend_fn)?;

    lua.globals().set("db", db_table)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db::DatabaseBackend;
    use crate::lexicon::LexiconRegistry;
    use serde_json::json;
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
            telemetry_collector_url: String::new(),
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
                    None,
                )
                .expect("Failed to create test linked-repo OAuth client"),
            ),
            linked_repos_client_kid: None,
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
            client_jwks: Vec::new(),
            telemetry_counters: std::sync::Arc::new(crate::telemetry::counters::Counters::new()),
        }
    }

    fn setup(state: &AppState) -> Lua {
        let lua = Lua::new();
        register_db_api(&lua, Arc::new(state.clone())).unwrap();
        lua
    }

    #[tokio::test]
    async fn raw_allows_non_select_on_own_tables() {
        let state = test_state();
        let lua = setup(&state);
        // Non-SELECT statements are allowed against non-internal tables. Passes
        // table validation; may then fail on the (empty in-memory) DB.
        let result: Result<mlua::Value, _> = lua
            .load(r#"return db.raw("DELETE FROM my_table")"#)
            .eval_async()
            .await;
        if let Err(e) = &result {
            let err = e.to_string();
            assert!(
                !err.contains("internal HappyView table"),
                "should have passed table validation but got: {err}"
            );
        }
    }

    #[tokio::test]
    async fn raw_blocks_internal_tables() {
        let state = test_state();
        let lua = setup(&state);
        let result: Result<mlua::Value, _> = lua
            .load(r#"return db.raw("SELECT * FROM happyview_dpop_keys")"#)
            .eval_async()
            .await;
        let err = result.expect_err("querying an internal table must be rejected");
        assert!(
            err.to_string().contains("internal HappyView table"),
            "expected an internal-table error, got: {err}"
        );
    }

    #[tokio::test]
    async fn raw_allows_select() {
        let state = test_state();
        let lua = setup(&state);
        let result: Result<mlua::Value, _> =
            lua.load(r#"return db.raw("SELECT 1")"#).eval_async().await;
        // Should either succeed (SQLite in-memory) or fail with a DB connection error,
        // but NOT a validation error.
        if let Err(e) = &result {
            let err = e.to_string();
            assert!(
                !err.contains("only supports SELECT"),
                "should have passed validation but got: {err}"
            );
        }
    }

    #[tokio::test]
    async fn query_accepts_nested_sort_field() {
        let state = test_state();
        let lua = setup(&state);
        let result: Result<mlua::Value, _> = lua
            .load(r#"return db.query({ collection = "test", sort = "author.handle" })"#)
            .eval_async()
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            !err.contains("invalid sort field"),
            "nested sort field should be accepted, got: {err}"
        );
    }

    #[tokio::test]
    async fn query_accepts_array_index_sort_field() {
        let state = test_state();
        let lua = setup(&state);
        let result: Result<mlua::Value, _> = lua
            .load(r#"return db.query({ collection = "test", sort = "tags[0]" })"#)
            .eval_async()
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            !err.contains("invalid sort field"),
            "array index sort field should be accepted, got: {err}"
        );
    }

    #[tokio::test]
    async fn query_rejects_invalid_sort_field() {
        let state = test_state();
        let lua = setup(&state);
        let result: Result<mlua::Value, _> = lua
            .load(r#"return db.query({ collection = "test", sort = "name; DROP TABLE" })"#)
            .eval_async()
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("invalid field"),
            "expected sort field error, got: {err}"
        );
    }

    #[tokio::test]
    async fn query_rejects_invalid_sort_direction() {
        let state = test_state();
        let lua = setup(&state);
        let result: Result<mlua::Value, _> = lua
            .load(r#"return db.query({ collection = "test", sortDirection = "sideways" })"#)
            .eval_async()
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("invalid sortDirection"),
            "expected sortDirection error, got: {err}"
        );
    }

    #[test]
    fn cursor_round_trip() {
        let encoded = crate::db::encode_cursor("2026-03-12T10:00:00Z", "at://did:plc:abc/col/rkey");
        let (ts, uri) = crate::db::decode_cursor(&encoded).unwrap();
        assert_eq!(ts, "2026-03-12T10:00:00Z");
        assert_eq!(uri, "at://did:plc:abc/col/rkey");
    }

    #[test]
    fn decode_invalid_cursor_returns_none() {
        assert!(crate::db::decode_cursor("not-valid-base64!!!").is_none());
    }

    #[test]
    fn decode_cursor_missing_pipe_returns_none() {
        let encoded = BASE64.encode("no-pipe-here");
        assert!(crate::db::decode_cursor(&encoded).is_none());
    }

    // -----------------------------------------------------------------------
    // parse_filter_node
    // -----------------------------------------------------------------------

    fn make_condition_table(lua: &Lua, field: &str, op: &str, value: &str) -> mlua::Table {
        let t = lua.create_table().unwrap();
        t.set("field", field).unwrap();
        t.set("op", op).unwrap();
        t.set("value", value).unwrap();
        t
    }

    #[test]
    fn filter_simple_condition() {
        let lua = Lua::new();
        let t = make_condition_table(&lua, "name", "=", "alice");
        let node = parse_filter_node(&t, 0).unwrap();
        assert_eq!(
            node,
            Filter::Condition(Condition {
                field: "name".into(),
                op: "=".into(),
                value: json!("alice")
            })
        );
    }

    #[test]
    fn filter_defaults_op_to_equals() {
        let lua = Lua::new();
        let t = lua.create_table().unwrap();
        t.set("field", "status").unwrap();
        t.set("value", "active").unwrap();
        let node = parse_filter_node(&t, 0).unwrap();
        assert_eq!(
            node,
            Filter::Condition(Condition {
                field: "status".into(),
                op: "=".into(),
                value: json!("active")
            })
        );
    }

    #[test]
    fn filter_rejects_invalid_field() {
        let lua = Lua::new();
        let t = make_condition_table(&lua, "name; DROP TABLE", "=", "x");
        let err = parse_filter_node(&t, 0).unwrap_err();
        assert!(err.to_string().contains("invalid filter field"));
    }

    #[test]
    fn filter_and_group() {
        let lua = Lua::new();
        let group = lua.create_table().unwrap();
        group.set("combine", "AND").unwrap();
        let c1 = make_condition_table(&lua, "status", "=", "active");
        let c2 = make_condition_table(&lua, "age", ">", "18");
        group.set(1, c1).unwrap();
        group.set(2, c2).unwrap();
        let node = parse_filter_node(&group, 0).unwrap();
        assert_eq!(
            node,
            Filter::Group {
                combine: "AND".into(),
                conditions: vec![
                    Filter::Condition(Condition {
                        field: "status".into(),
                        op: "=".into(),
                        value: json!("active")
                    }),
                    Filter::Condition(Condition {
                        field: "age".into(),
                        op: ">".into(),
                        value: json!("18")
                    }),
                ],
            }
        );
    }

    #[test]
    fn filter_rejects_empty_group() {
        let lua = Lua::new();
        let group = lua.create_table().unwrap();
        group.set("combine", "AND").unwrap();
        let err = parse_filter_node(&group, 0).unwrap_err();
        assert!(err.to_string().contains("filter group has no conditions"));
    }

    #[test]
    fn filter_rejects_excessive_depth() {
        let lua = Lua::new();
        let c = make_condition_table(&lua, "x", "=", "1");
        let err = parse_filter_node(&c, MAX_FILTER_DEPTH).unwrap_err();
        assert!(err.to_string().contains("filter nesting too deep"));
    }

    #[test]
    fn filter_integer_value() {
        let lua = Lua::new();
        let t = lua.create_table().unwrap();
        t.set("field", "count").unwrap();
        t.set("op", ">").unwrap();
        t.set("value", 42).unwrap();
        let node = parse_filter_node(&t, 0).unwrap();
        assert_eq!(
            node,
            Filter::Condition(Condition {
                field: "count".into(),
                op: ">".into(),
                value: json!("42")
            })
        );
    }

    #[test]
    fn filter_boolean_value() {
        let lua = Lua::new();
        let t = lua.create_table().unwrap();
        t.set("field", "active").unwrap();
        t.set("value", true).unwrap();
        let node = parse_filter_node(&t, 0).unwrap();
        assert_eq!(
            node,
            Filter::Condition(Condition {
                field: "active".into(),
                op: "=".into(),
                value: json!("true")
            })
        );
    }

    #[test]
    fn filter_nested_field_path() {
        let lua = Lua::new();
        let t = make_condition_table(&lua, "author.websites[0].url", "=", "https://example.com");
        let node = parse_filter_node(&t, 0).unwrap();
        assert_eq!(
            node,
            Filter::Condition(Condition {
                field: "author.websites[0].url".into(),
                op: "=".into(),
                value: json!("https://example.com"),
            })
        );
    }

    // -----------------------------------------------------------------------
    // query sort direction
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn query_accepts_valid_sort_direction() {
        let state = test_state();
        let lua = setup(&state);
        let result: Result<mlua::Value, _> = lua
            .load(r#"return db.query({ collection = "test", sortDirection = "asc" })"#)
            .eval_async()
            .await;
        // Should fail with a DB connection error, NOT a validation error
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            !err.contains("invalid sortDirection"),
            "should have passed validation but got: {err}"
        );
    }
}

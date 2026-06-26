mod common;

use atrium_identity::did::{CommonDidResolver, CommonDidResolverConfig};
use atrium_identity::handle::{AtprotoHandleResolver, AtprotoHandleResolverConfig};
use atrium_oauth::{
    AtprotoLocalhostClientMetadata, DefaultHttpClient, KnownScope, OAuthClientConfig,
    OAuthResolverConfig, Scope,
};
use happyview::AppState;
use happyview::config::Config;
use happyview::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use happyview::lexicon::LexiconRegistry;
use happyview::lua::db_api::register_db_api;
use mlua::Lua;
use serial_test::serial;
use std::sync::Arc;
use tokio::sync::watch;

use common::db;

async fn test_state_with_pool(pool: sqlx::AnyPool, backend: DatabaseBackend) -> AppState {
    let config = Config {
        host: "127.0.0.1".into(),
        port: 3000,
        database_url: String::new(),
        database_backend: backend,
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
        state_store: happyview::auth::oauth_store::DbStateStore::new(oauth_pool.clone(), backend),
        session_store: happyview::auth::oauth_store::DbSessionStore::new(oauth_pool, backend),
        resolver: OAuthResolverConfig {
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
        db: pool.clone(),
        db_backend: backend,
        lexicons: LexiconRegistry::new(),
        collections_tx: tx,
        labeler_subscriptions_tx: labeler_tx,
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
        oauth_state_store: happyview::auth::oauth_store::DbStateStore::new(pool.clone(), backend),
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
        official_registry_config: happyview::plugin::official_registry::RegistryConfig::production(
        ),
        domain_cache: happyview::domain::DomainCache::new(),
        proxy_config: std::sync::Arc::new(arc_swap::ArcSwap::new(std::sync::Arc::new(
            happyview::proxy_config::ProxyConfig::default(),
        ))),
        backfill_db: pool.clone(),
        backfill_events_tx: tokio::sync::broadcast::channel(16).0,
        verbose_event_logging: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }
}

async fn seed_records(pool: &sqlx::AnyPool, backend: DatabaseBackend) {
    let records = [
        (
            "at://did:plc:test/test.collection/rkey1",
            "did:plc:test",
            "test.collection",
            "rkey1",
            serde_json::json!({"name": "Test One", "value": 1}),
            "bafyone",
        ),
        (
            "at://did:plc:test/test.collection/rkey2",
            "did:plc:test",
            "test.collection",
            "rkey2",
            serde_json::json!({"name": "Test Two", "value": 2}),
            "bafytwo",
        ),
        (
            "at://did:plc:other/test.collection/rkey3",
            "did:plc:other",
            "test.collection",
            "rkey3",
            serde_json::json!({"name": "Other Record", "value": 3}),
            "bafythree",
        ),
    ];

    let now = now_rfc3339();
    let sql = adapt_sql(
        "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        backend,
    );
    for (uri, did, collection, rkey, record, cid) in &records {
        sqlx::query(&sql)
            .bind(uri)
            .bind(did)
            .bind(collection)
            .bind(rkey)
            .bind(serde_json::to_string(record).unwrap_or_default())
            .bind(cid)
            .bind(&now)
            .execute(pool)
            .await
            .expect("failed to seed record");
    }
}

fn setup_lua(state: &AppState) -> Lua {
    let lua = Lua::new();
    register_db_api(&lua, Arc::new(state.clone())).unwrap();
    lua
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn db_get_returns_record() {
    common::require_db!();
    let pool = db::test_pool().await;
    let backend = db::test_backend();
    db::truncate_all(&pool).await;
    seed_records(&pool, backend).await;
    let state = test_state_with_pool(pool, backend).await;
    let lua = setup_lua(&state);

    let result: mlua::Table = lua
        .load(r#"return db.get("at://did:plc:test/test.collection/rkey1")"#)
        .eval_async()
        .await
        .unwrap();

    assert_eq!(
        result.get::<String>("uri").unwrap(),
        "at://did:plc:test/test.collection/rkey1"
    );
    assert_eq!(result.get::<String>("name").unwrap(), "Test One");
}

#[tokio::test]
#[serial]
async fn db_get_returns_nil_for_missing() {
    common::require_db!();
    let pool = db::test_pool().await;
    let backend = db::test_backend();
    db::truncate_all(&pool).await;
    let state = test_state_with_pool(pool, backend).await;
    let lua = setup_lua(&state);

    let result: mlua::Value = lua
        .load(r#"return db.get("at://did:plc:nonexistent/test.collection/nope")"#)
        .eval_async()
        .await
        .unwrap();

    assert!(result.is_nil());
}

#[tokio::test]
#[serial]
async fn db_query_returns_records() {
    common::require_db!();
    let pool = db::test_pool().await;
    let backend = db::test_backend();
    db::truncate_all(&pool).await;
    seed_records(&pool, backend).await;
    let state = test_state_with_pool(pool, backend).await;
    let lua = setup_lua(&state);

    let result: mlua::Table = lua
        .load(r#"return db.query({ collection = "test.collection" })"#)
        .eval_async()
        .await
        .unwrap();

    let records: mlua::Table = result.get("records").unwrap();
    assert_eq!(records.raw_len(), 3);
}

#[tokio::test]
#[serial]
async fn db_query_respects_limit() {
    common::require_db!();
    let pool = db::test_pool().await;
    let backend = db::test_backend();
    db::truncate_all(&pool).await;
    seed_records(&pool, backend).await;
    let state = test_state_with_pool(pool, backend).await;
    let lua = setup_lua(&state);

    let result: mlua::Table = lua
        .load(r#"return db.query({ collection = "test.collection", limit = 1 })"#)
        .eval_async()
        .await
        .unwrap();

    let records: mlua::Table = result.get("records").unwrap();
    assert_eq!(records.raw_len(), 1);

    // Should have a cursor since there are more records
    let cursor: String = result.get("cursor").unwrap();
    assert!(!cursor.is_empty());
}

#[tokio::test]
#[serial]
async fn db_count_returns_total() {
    common::require_db!();
    let pool = db::test_pool().await;
    let backend = db::test_backend();
    db::truncate_all(&pool).await;
    seed_records(&pool, backend).await;
    let state = test_state_with_pool(pool, backend).await;
    let lua = setup_lua(&state);

    let count: i64 = lua
        .load(r#"return db.count("test.collection")"#)
        .eval_async()
        .await
        .unwrap();

    assert_eq!(count, 3);
}

#[tokio::test]
#[serial]
async fn db_count_with_did_filter() {
    common::require_db!();
    let pool = db::test_pool().await;
    let backend = db::test_backend();
    db::truncate_all(&pool).await;
    seed_records(&pool, backend).await;
    let state = test_state_with_pool(pool, backend).await;
    let lua = setup_lua(&state);

    let count: i64 = lua
        .load(r#"return db.count("test.collection", "did:plc:test")"#)
        .eval_async()
        .await
        .unwrap();

    assert_eq!(count, 2);
}

#[tokio::test]
#[serial]
async fn db_search_finds_matching() {
    common::require_db!();
    let pool = db::test_pool().await;
    let backend = db::test_backend();
    db::truncate_all(&pool).await;
    seed_records(&pool, backend).await;
    let state = test_state_with_pool(pool, backend).await;
    let lua = setup_lua(&state);

    let result: mlua::Table = lua
        .load(
            r#"return db.search({ collection = "test.collection", field = "name", query = "Test" })"#,
        )
        .eval_async()
        .await
        .unwrap();

    let records: mlua::Table = result.get("records").unwrap();
    // "Test One" and "Test Two" match; "Other Record" does not
    assert_eq!(records.raw_len(), 2);
}

#[tokio::test]
#[serial]
async fn db_raw_select_works() {
    common::require_db!();
    let pool = db::test_pool().await;
    let backend = db::test_backend();
    db::truncate_all(&pool).await;
    seed_records(&pool, backend).await;
    let state = test_state_with_pool(pool, backend).await;
    let lua = setup_lua(&state);

    let result: mlua::Table = lua
        .load(
            r#"return db.raw("SELECT COUNT(*) as cnt FROM happyview_records WHERE collection = $1", {"test.collection"})"#,
        )
        .eval_async()
        .await
        .unwrap();

    // Result is an array of row tables
    let first_row: mlua::Table = result.get(1).unwrap();
    let cnt: i64 = first_row.get("cnt").unwrap();
    assert_eq!(cnt, 3);
}

#[tokio::test]
#[serial]
async fn db_query_filter_equals() {
    common::require_db!();
    let pool = db::test_pool().await;
    let backend = db::test_backend();
    db::truncate_all(&pool).await;
    seed_records(&pool, backend).await;
    let state = test_state_with_pool(pool, backend).await;
    let lua = setup_lua(&state);

    let result: mlua::Table = lua
        .load(
            r#"return db.query({
                collection = "test.collection",
                filter = { field = "name", value = "Test One" }
            })"#,
        )
        .eval_async()
        .await
        .unwrap();

    let records: mlua::Table = result.get("records").unwrap();
    assert_eq!(records.raw_len(), 1);
    let first: mlua::Table = records.get(1).unwrap();
    assert_eq!(first.get::<String>("name").unwrap(), "Test One");
}

#[tokio::test]
#[serial]
async fn db_query_filter_not_equals() {
    common::require_db!();
    let pool = db::test_pool().await;
    let backend = db::test_backend();
    db::truncate_all(&pool).await;
    seed_records(&pool, backend).await;
    let state = test_state_with_pool(pool, backend).await;
    let lua = setup_lua(&state);

    let result: mlua::Table = lua
        .load(
            r#"return db.query({
                collection = "test.collection",
                filter = { field = "name", op = "!=", value = "Test One" }
            })"#,
        )
        .eval_async()
        .await
        .unwrap();

    let records: mlua::Table = result.get("records").unwrap();
    assert_eq!(records.raw_len(), 2);
}

#[tokio::test]
#[serial]
async fn db_query_filter_no_match_returns_empty() {
    common::require_db!();
    let pool = db::test_pool().await;
    let backend = db::test_backend();
    db::truncate_all(&pool).await;
    seed_records(&pool, backend).await;
    let state = test_state_with_pool(pool, backend).await;
    let lua = setup_lua(&state);

    let result: mlua::Table = lua
        .load(
            r#"return db.query({
                collection = "test.collection",
                filter = { field = "name", value = "Nonexistent" }
            })"#,
        )
        .eval_async()
        .await
        .unwrap();

    let records: mlua::Table = result.get("records").unwrap();
    assert_eq!(records.raw_len(), 0);
}

#[tokio::test]
#[serial]
async fn db_query_filter_and_group() {
    common::require_db!();
    let pool = db::test_pool().await;
    let backend = db::test_backend();
    db::truncate_all(&pool).await;
    seed_records(&pool, backend).await;
    let state = test_state_with_pool(pool, backend).await;
    let lua = setup_lua(&state);

    let result: mlua::Table = lua
        .load(
            r#"return db.query({
                collection = "test.collection",
                filter = {
                    combine = "AND",
                    { field = "name", op = "LIKE", value = "Test%" },
                    { field = "value", op = ">", value = "1" }
                }
            })"#,
        )
        .eval_async()
        .await
        .unwrap();

    let records: mlua::Table = result.get("records").unwrap();
    assert_eq!(records.raw_len(), 1);
    let first: mlua::Table = records.get(1).unwrap();
    assert_eq!(first.get::<String>("name").unwrap(), "Test Two");
}

#[tokio::test]
#[serial]
async fn db_query_sort_by_json_field() {
    common::require_db!();
    let pool = db::test_pool().await;
    let backend = db::test_backend();
    db::truncate_all(&pool).await;
    seed_records(&pool, backend).await;
    let state = test_state_with_pool(pool, backend).await;
    let lua = setup_lua(&state);

    let result: mlua::Table = lua
        .load(
            r#"return db.query({
                collection = "test.collection",
                sort = "name",
                sortDirection = "asc"
            })"#,
        )
        .eval_async()
        .await
        .unwrap();

    let records: mlua::Table = result.get("records").unwrap();
    assert_eq!(records.raw_len(), 3);
    let first: mlua::Table = records.get(1).unwrap();
    let last: mlua::Table = records.get(3).unwrap();
    assert_eq!(first.get::<String>("name").unwrap(), "Other Record");
    assert_eq!(last.get::<String>("name").unwrap(), "Test Two");
}

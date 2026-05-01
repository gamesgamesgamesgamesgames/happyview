//! Integration tests for the local-only Record API surface
//! (`Record.delete_local`, `r:save_local`, `r:delete_local`) and the
//! auth-boundary errors when label / record-event / query scripts reach
//! for PDS-touching methods (`r:save`, `r:delete`).

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
use happyview::lua::record::register_record_api_no_auth;
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
    }
}

async fn seed_record(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    uri: &str,
    did: &str,
    collection: &str,
    rkey: &str,
    record: serde_json::Value,
) {
    let sql = adapt_sql(
        "INSERT INTO records (uri, did, collection, rkey, record, cid, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        backend,
    );
    sqlx::query(&sql)
        .bind(uri)
        .bind(did)
        .bind(collection)
        .bind(rkey)
        .bind(serde_json::to_string(&record).unwrap_or_default())
        .bind("bafyseed")
        .bind(now_rfc3339())
        .execute(pool)
        .await
        .expect("failed to seed record");
}

async fn count_records(pool: &sqlx::AnyPool, backend: DatabaseBackend, uri: &str) -> i64 {
    let sql = adapt_sql("SELECT COUNT(*) FROM records WHERE uri = ?", backend);
    let (count,): (i64,) = sqlx::query_as(&sql)
        .bind(uri)
        .fetch_one(pool)
        .await
        .expect("count query");
    count
}

async fn fetch_record_body(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    uri: &str,
) -> Option<serde_json::Value> {
    let sql = adapt_sql("SELECT record FROM records WHERE uri = ?", backend);
    let row: Option<(String,)> = sqlx::query_as(&sql)
        .bind(uri)
        .fetch_optional(pool)
        .await
        .expect("fetch record");
    row.map(|(s,)| serde_json::from_str(&s).expect("record json"))
}

/// Build a sandbox with the Record API registered in **no-auth mode** —
/// the same shape label / record-event / query scripts get.
fn setup_no_auth_lua(state: &AppState) -> Lua {
    let lua = Lua::new();
    register_record_api_no_auth(&lua, Arc::new(state.clone())).expect("register record api");
    lua
}

// ---------------------------------------------------------------------------
// Record.delete_local(uri) static
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
#[ignore]
async fn record_static_delete_local_returns_true_when_row_existed() {
    let pool = db::test_pool().await;
    let backend = db::test_backend();
    db::truncate_all(&pool).await;

    let uri = "at://did:plc:test/test.collection/rkey1";
    seed_record(
        &pool,
        backend,
        uri,
        "did:plc:test",
        "test.collection",
        "rkey1",
        serde_json::json!({"name": "kept"}),
    )
    .await;

    let state = test_state_with_pool(pool.clone(), backend).await;
    let lua = setup_no_auth_lua(&state);

    let deleted: bool = lua
        .load(format!(r#"return Record.delete_local("{uri}")"#))
        .eval_async()
        .await
        .expect("delete_local call");
    assert!(deleted, "expected true (row existed before)");

    let after = count_records(&pool, backend, uri).await;
    assert_eq!(after, 0, "row should be gone");
}

#[tokio::test]
#[serial]
#[ignore]
async fn record_static_delete_local_returns_false_when_row_absent() {
    let pool = db::test_pool().await;
    let backend = db::test_backend();
    db::truncate_all(&pool).await;
    let state = test_state_with_pool(pool, backend).await;
    let lua = setup_no_auth_lua(&state);

    let deleted: bool = lua
        .load(r#"return Record.delete_local("at://did:plc:nope/test.collection/none")"#)
        .eval_async()
        .await
        .expect("delete_local call");
    assert!(!deleted, "no row → false (idempotent)");
}

// ---------------------------------------------------------------------------
// r:delete_local() instance method
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
#[ignore]
async fn record_instance_delete_local_removes_row_and_clears_uri() {
    let pool = db::test_pool().await;
    let backend = db::test_backend();
    db::truncate_all(&pool).await;

    let uri = "at://did:plc:test/test.collection/rkey1";
    seed_record(
        &pool,
        backend,
        uri,
        "did:plc:test",
        "test.collection",
        "rkey1",
        serde_json::json!({"name": "doomed"}),
    )
    .await;

    let state = test_state_with_pool(pool.clone(), backend).await;
    let lua = setup_no_auth_lua(&state);

    // Load via Record.load(), then call :delete_local() — this is the
    // primary shape we expect from a label-script reaction.
    let uri_after: mlua::Value = lua
        .load(format!(
            r#"
            local r = Record.load("{uri}")
            assert(r ~= nil, "record not loaded")
            r:delete_local()
            return r._uri
            "#
        ))
        .eval_async()
        .await
        .expect("delete_local instance call");

    assert!(matches!(uri_after, mlua::Value::Nil), "_uri should be nil");
    assert_eq!(count_records(&pool, backend, uri).await, 0);
}

// ---------------------------------------------------------------------------
// r:save_local() instance method
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
#[ignore]
async fn record_instance_save_local_updates_existing_row() {
    let pool = db::test_pool().await;
    let backend = db::test_backend();
    db::truncate_all(&pool).await;

    let uri = "at://did:plc:test/test.collection/rkey1";
    seed_record(
        &pool,
        backend,
        uri,
        "did:plc:test",
        "test.collection",
        "rkey1",
        serde_json::json!({"text": "original"}),
    )
    .await;

    let state = test_state_with_pool(pool.clone(), backend).await;
    let lua = setup_no_auth_lua(&state);

    // Redact-style flow: load, mutate, save_local.
    lua.load(format!(
        r#"
        local r = Record.load("{uri}")
        assert(r ~= nil)
        r.text = "[redacted]"
        r:save_local()
        "#
    ))
    .exec_async()
    .await
    .expect("save_local instance call");

    let body = fetch_record_body(&pool, backend, uri).await.unwrap();
    assert_eq!(body["text"], "[redacted]");
    // $type is injected automatically by the serializer.
    assert_eq!(body["$type"], "test.collection");
    // Row count unchanged (upsert).
    assert_eq!(count_records(&pool, backend, uri).await, 1);
}

#[tokio::test]
#[serial]
#[ignore]
async fn record_save_local_creates_new_row_when_repo_set() {
    let pool = db::test_pool().await;
    let backend = db::test_backend();
    db::truncate_all(&pool).await;
    let state = test_state_with_pool(pool.clone(), backend).await;
    let lua = setup_no_auth_lua(&state);

    // Build a fresh record. `:set_repo` provides the DID for the URI;
    // without auth there's no fallback. We also manually `:set_rkey`
    // since there's no key_type from a (missing) lexicon.
    let uri: String = lua
        .load(
            r#"
            local r = Record.new("test.collection", { value = 42 })
            r:set_repo("did:plc:newowner")
            r:set_rkey("brandnew")
            r:save_local()
            return r._uri
            "#,
        )
        .eval_async()
        .await
        .expect("save_local creating call");

    assert_eq!(uri, "at://did:plc:newowner/test.collection/brandnew");
    let body = fetch_record_body(&pool, backend, &uri).await.unwrap();
    assert_eq!(body["value"], 42);
    assert_eq!(body["$type"], "test.collection");
}

#[tokio::test]
#[serial]
#[ignore]
async fn record_save_local_errors_without_did_when_no_uri() {
    let pool = db::test_pool().await;
    let backend = db::test_backend();
    db::truncate_all(&pool).await;
    let state = test_state_with_pool(pool, backend).await;
    let lua = setup_no_auth_lua(&state);

    // No `:set_repo` and no claims → :save_local() must error.
    let err = lua
        .load(
            r#"
            local r = Record.new("test.collection", { value = 1 })
            r:save_local()
            "#,
        )
        .exec_async()
        .await
        .expect_err("expected error: no DID resolvable");
    let msg = err.to_string();
    assert!(
        msg.contains("save_local() needs a DID"),
        "expected DID-required message, got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// PDS-touching methods error cleanly when no auth present
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
#[ignore]
async fn record_save_errors_without_pds_auth() {
    let pool = db::test_pool().await;
    let backend = db::test_backend();
    db::truncate_all(&pool).await;

    let uri = "at://did:plc:test/test.collection/rkey1";
    seed_record(
        &pool,
        backend,
        uri,
        "did:plc:test",
        "test.collection",
        "rkey1",
        serde_json::json!({"value": 1}),
    )
    .await;

    let state = test_state_with_pool(pool.clone(), backend).await;
    let lua = setup_no_auth_lua(&state);

    let err = lua
        .load(format!(
            r#"
            local r = Record.load("{uri}")
            r:save()
            "#
        ))
        .exec_async()
        .await
        .expect_err("expected NO_PDS_AUTH error");
    let msg = err.to_string();
    assert!(
        msg.contains("no PDS auth"),
        "expected NO_PDS_AUTH message, got: {msg}"
    );

    // The original row should be untouched.
    let body = fetch_record_body(&pool, backend, uri).await.unwrap();
    assert_eq!(body["value"], 1);
}

#[tokio::test]
#[serial]
#[ignore]
async fn record_delete_errors_without_pds_auth() {
    let pool = db::test_pool().await;
    let backend = db::test_backend();
    db::truncate_all(&pool).await;

    let uri = "at://did:plc:test/test.collection/rkey1";
    seed_record(
        &pool,
        backend,
        uri,
        "did:plc:test",
        "test.collection",
        "rkey1",
        serde_json::json!({"value": 1}),
    )
    .await;

    let state = test_state_with_pool(pool.clone(), backend).await;
    let lua = setup_no_auth_lua(&state);

    let err = lua
        .load(format!(
            r#"
            local r = Record.load("{uri}")
            r:delete()
            "#
        ))
        .exec_async()
        .await
        .expect_err("expected NO_PDS_AUTH error");
    let msg = err.to_string();
    assert!(
        msg.contains("no PDS auth"),
        "expected NO_PDS_AUTH message, got: {msg}"
    );

    // No-auth :delete() must NOT touch the local DB either —
    // the row should still be there.
    assert_eq!(count_records(&pool, backend, uri).await, 1);
}

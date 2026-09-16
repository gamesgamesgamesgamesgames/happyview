//! The SDK-built `http` standard library fixture, end to end: loaded from
//! `tests/fixtures/sdk_http`, called through the executor, and through Lua
//! `require("http")`.

use happyview::db::adapt_sql;
use happyview::plugin::library::LibraryCallContext;
use happyview::plugin::{LoadedPlugin, PluginManifest, PluginSource, loader};
use happyview::test_support::{memory_pool, migrated_memory_pool, test_state_with_pool};
use wiremock::matchers::{body_string, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn state_with_http() -> happyview::AppState {
    let plugin = loader::load_from_file(std::path::Path::new("tests/fixtures/sdk_http"))
        .await
        .expect("sdk_http fixture not built. Run: cargo build --manifest-path tests/fixtures/sdk_http/Cargo.toml --target wasm32-unknown-unknown --release");
    let state = test_state_with_pool(memory_pool().await);
    state.plugin_registry.install(plugin).await.unwrap();
    state
}

/// Same fixture wasm, loaded under a manifest declaring
/// `network:request:defined` instead of the fixture's own
/// `network:request:unrestricted` — the two are mutually exclusive, so this
/// can't go through `loader::load_from_file` against the fixture's own
/// `manifest.json`. Built the same way `admin_plugins_libraries.rs`'s
/// `library()` helper builds a `LoadedPlugin` by hand.
///
/// Unlike `state_with_http`, this needs a migrated pool: `store_allowed_hosts`
/// writes `happyview_plugin_configs`, whose `plugin_id` is a foreign key into
/// `happyview_plugins`, so a matching row is inserted for `sdk_http` too.
async fn state_with_http_defined() -> happyview::AppState {
    let wasm_bytes = tokio::fs::read(
        "tests/fixtures/sdk_http/target/wasm32-unknown-unknown/release/sdk_http.wasm",
    )
    .await
    .expect("sdk_http fixture not built. Run: cargo build --manifest-path tests/fixtures/sdk_http/Cargo.toml --target wasm32-unknown-unknown --release");

    let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
        "id": "sdk_http", "name": "SDK HTTP fixture", "version": "1.0.0", "api_version": "2",
        "plugin_type": "library", "namespace": "http",
        "capabilities": ["network:request:defined"], "allowed_hosts": [],
    }))
    .unwrap();
    let plugin = LoadedPlugin {
        info: manifest.clone().into(),
        source: PluginSource::File {
            path: "tests/fixtures/sdk_http".into(),
        },
        wasm_bytes,
        manifest: Some(manifest),
    };

    let pool = migrated_memory_pool().await;
    let sql = adapt_sql(
        "INSERT INTO happyview_plugins (id, source, api_version) VALUES (?, 'file', '2')",
        happyview::db::DatabaseBackend::Sqlite,
    );
    happyview::db::query(&sql)
        .bind("sdk_http")
        .execute(&pool)
        .await
        .unwrap();

    let state = test_state_with_pool(pool);
    state.plugin_registry.install(plugin).await.unwrap();
    state
}

#[tokio::test]
async fn http_get_through_executor() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/hello"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("hi")
                .insert_header("X-Test", "yes"),
        )
        .mount(&server)
        .await;

    let state = state_with_http().await;
    let out = state
        .plugin_executor()
        .call_library(
            "sdk_http",
            "get",
            &[serde_json::json!(format!("{}/hello", server.uri()))],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap();
    assert_eq!(out["status"], 200);
    assert_eq!(out["body"], "hi");
    assert_eq!(out["headers"]["x-test"], "yes");
}

#[tokio::test]
async fn http_post_sends_headers_and_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/echo"))
        .and(header("content-type", "application/json"))
        .and(body_string(r#"{"a":1}"#))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;

    let state = state_with_http().await;
    let out = state
        .plugin_executor()
        .call_library(
            "sdk_http",
            "post",
            &[
                serde_json::json!(format!("{}/echo", server.uri())),
                serde_json::json!({"headers": {"Content-Type": "application/json"}, "body": r#"{"a":1}"#}),
            ],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap();
    assert_eq!(out["status"], 201);
}

#[tokio::test]
async fn http_via_lua_require() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/lua"))
        .respond_with(ResponseTemplate::new(200).set_body_string("from lua"))
        .mount(&server)
        .await;

    let state = state_with_http().await;
    let lua = happyview::lua::sandbox_for_tests();
    happyview::lua::require_api_for_tests(&lua, &state).await;
    lua.load(format!(
        r#"local http = require("http")
           function handle() local r = http.get("{}/lua"); return r.status .. ":" .. r.body end"#,
        server.uri()
    ))
    .exec()
    .unwrap();
    let handle: mlua::Function = lua.globals().get("handle").unwrap();
    let out: String = handle.call_async(()).await.unwrap();
    assert_eq!(out, "200:from lua");
}

#[tokio::test]
async fn defined_capability_with_no_hosts_stored_refuses_the_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/hello"))
        .respond_with(ResponseTemplate::new(200).set_body_string("hi"))
        .mount(&server)
        .await;

    let state = state_with_http_defined().await;
    let err = state
        .plugin_executor()
        .call_library(
            "sdk_http",
            "get",
            &[serde_json::json!(format!("{}/hello", server.uri()))],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .expect_err("no hosts are configured yet, so the request must be refused");
    let message = err.to_string();
    assert!(message.contains("no hosts are configured"), "{message}");
}

#[tokio::test]
async fn defined_capability_with_the_host_stored_allows_the_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/hello"))
        .respond_with(ResponseTemplate::new(200).set_body_string("hi"))
        .mount(&server)
        .await;

    let state = state_with_http_defined().await;
    let host = reqwest::Url::parse(&server.uri())
        .unwrap()
        .host_str()
        .unwrap()
        .to_string();
    // The mock server binds to a random port on `127.0.0.1`, so the stored
    // host is the bare address — `network:request:defined` matches on host
    // only, never on port.
    happyview::plugin::config::store_allowed_hosts(
        &state.db,
        state.db_backend,
        "sdk_http",
        &[host],
    )
    .await
    .unwrap();

    let out = state
        .plugin_executor()
        .call_library(
            "sdk_http",
            "get",
            &[serde_json::json!(format!("{}/hello", server.uri()))],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap();
    assert_eq!(out["status"], 200);
    assert_eq!(out["body"], "hi");
}

#[tokio::test]
async fn allowed_hosts_export_reflects_the_operator_configured_list() {
    let state = state_with_http_defined().await;
    happyview::plugin::config::store_allowed_hosts(
        &state.db,
        state.db_backend,
        "sdk_http",
        &["api.example.com".to_string()],
    )
    .await
    .unwrap();

    let out = state
        .plugin_executor()
        .call_library(
            "sdk_http",
            "allowed_hosts",
            &[],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap();
    assert_eq!(out, serde_json::json!(["api.example.com"]));
}

#[tokio::test]
async fn allowed_hosts_export_is_empty_under_the_fixtures_own_unrestricted_manifest() {
    let state = state_with_http().await;
    let out = state
        .plugin_executor()
        .call_library(
            "sdk_http",
            "allowed_hosts",
            &[],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap();
    assert_eq!(out, serde_json::json!([]));
}

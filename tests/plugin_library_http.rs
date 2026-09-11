//! The SDK-built `http` standard library fixture, end to end: loaded from
//! `tests/fixtures/sdk_http`, called through the executor, and through Lua
//! `require("http")`.

use happyview::plugin::library::LibraryCallContext;
use happyview::plugin::loader;
use happyview::test_support::{memory_pool, test_state_with_pool};
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

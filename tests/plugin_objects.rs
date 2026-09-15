//! The `sdk_objects` fixture end to end: the object document an immediate
//! method receives, and each record/table host import called from wasm.

use happyview::plugin::library::LibraryCallContext;
use happyview::plugin::loader;
use happyview::test_support::{memory_pool, test_state_with_pool};
use serde_json::json;

async fn state_with_objects() -> happyview::AppState {
    let plugin = loader::load_from_file(std::path::Path::new("tests/fixtures/sdk_objects"))
        .await
        .expect("sdk_objects fixture not built. Run: cargo build --manifest-path tests/fixtures/sdk_objects/Cargo.toml --target wasm32-unknown-unknown --release");
    let state = test_state_with_pool(memory_pool().await);
    for sql in [
        "CREATE TABLE happyview_records (uri TEXT PRIMARY KEY, did TEXT NOT NULL, collection TEXT NOT NULL, rkey TEXT, record TEXT NOT NULL, cid TEXT, indexed_at TEXT, created_at TEXT)",
        "CREATE TABLE happyview_record_refs (source_uri TEXT NOT NULL, target_uri TEXT NOT NULL, collection TEXT NOT NULL)",
        "INSERT INTO happyview_records VALUES ('at://a/c/1', 'did:plc:a', 'c', '1', '{\"n\":\"one\"}', 'cid1', NULL, '2026-01-01T00:00:01Z')",
        "INSERT INTO happyview_records VALUES ('at://a/c/2', 'did:plc:a', 'c', '2', '{\"n\":\"two\"}', 'cid2', NULL, '2026-01-01T00:00:02Z')",
        "INSERT INTO happyview_record_refs VALUES ('at://a/c/2', 'at://a/c/1', 'c')",
        "CREATE TABLE leaderboard (name TEXT, score INTEGER)",
        "INSERT INTO leaderboard VALUES ('x', 50), ('y', 150)",
    ] {
        happyview::db::query(sql).execute(&state.db).await.unwrap();
    }
    state.plugin_registry.install(plugin).await.unwrap();
    state
}

#[tokio::test]
async fn immediate_method_receives_the_whole_document() {
    let state = state_with_objects().await;
    let doc = json!({"args": [1], "steps": [{"add": [2]}, {"add": [3]}], "call": {"name": "doc", "args": []}});
    let out = state
        .plugin_executor()
        .call_library(
            "sdk_objects",
            "chain",
            std::slice::from_ref(&doc),
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap();
    assert_eq!(out, doc);
}

#[tokio::test]
async fn record_imports_round_trip_through_wasm() {
    let state = state_with_objects().await;
    let exec = state.plugin_executor();
    let ctx = LibraryCallContext::default();

    let page = exec
        .call_library(
            "sdk_objects",
            "records_query",
            &[json!({"collection": "c", "limit": 1})],
            &ctx,
            0,
        )
        .await
        .unwrap();
    assert_eq!(page["records"][0]["uri"], "at://a/c/2");
    assert!(page["cursor"].is_string());

    let n = exec
        .call_library(
            "sdk_objects",
            "records_count",
            &[json!({"collection": "c"})],
            &ctx,
            0,
        )
        .await
        .unwrap();
    assert_eq!(n, json!(2));

    let got = exec
        .call_library(
            "sdk_objects",
            "records_get",
            &[json!("at://a/c/1")],
            &ctx,
            0,
        )
        .await
        .unwrap();
    assert_eq!(got["n"], "one");

    let found = exec
        .call_library(
            "sdk_objects",
            "records_search",
            &[json!({"collection": "c", "field": "n", "query": "tw"})],
            &ctx,
            0,
        )
        .await
        .unwrap();
    assert_eq!(found.as_array().unwrap().len(), 1);

    let back = exec
        .call_library(
            "sdk_objects",
            "backlinks_query",
            &[json!({"uri": "at://a/c/1", "collection": "c"})],
            &ctx,
            0,
        )
        .await
        .unwrap();
    assert_eq!(back["records"][0]["uri"], "at://a/c/2");

    let rows = exec.call_library("sdk_objects", "table_query", &[json!({"table": "leaderboard", "filter": {"field": "score", "op": ">", "value": "100"}})], &ctx, 0).await.unwrap();
    assert_eq!(rows[0]["name"], "y");

    let backend = exec
        .call_library("sdk_objects", "backend", &[], &ctx, 0)
        .await
        .unwrap();
    assert_eq!(backend, json!("sqlite"));
}

#[tokio::test]
async fn chain_through_lua_require() {
    let state = state_with_objects().await;
    let lua = happyview::lua::sandbox_for_tests();
    happyview::lua::require_api_for_tests(&lua, &state).await;
    lua.load(
        r#"local o = require("objects")
           function handle()
             local d = o.chain("a"):add(1):doc()
             local page = o.records_query({ collection = "c", limit = 1 })
             return d.args[1] .. ":" .. #d.steps .. ":" .. page.records[1].uri
           end"#,
    )
    .exec()
    .unwrap();
    let handle: mlua::Function = lua.globals().get("handle").unwrap();
    let out: String = handle.call_async(()).await.unwrap();
    assert_eq!(out, "a:1:at://a/c/2");
}

#[tokio::test]
async fn invalid_spec_is_a_plugin_error_not_a_trap() {
    let state = state_with_objects().await;
    let err = state
        .plugin_executor()
        .call_library(
            "sdk_objects",
            "table_query",
            &[json!({"table": "happyview_plugin_secrets"})],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("INVALID_SPEC"), "{err}");
}

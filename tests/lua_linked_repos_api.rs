mod common;

use common::app::TestApp;
use happyview::linked_repos::db;
use serial_test::serial;
use std::sync::Arc;

async fn lua_with_api(app: &TestApp) -> mlua::Lua {
    let lua = mlua::Lua::new();
    happyview::lua::linked_repos_api::register_linked_repos_api(&lua, Arc::new(app.state.clone()))
        .unwrap();
    lua
}

#[tokio::test]
#[serial]
async fn global_is_registered() {
    common::require_db!();
    let app = TestApp::new().await;
    let lua = lua_with_api(&app).await;

    let ok: bool = lua
        .load(
            r#"return type(linked_repos) == "table"
                   and type(linked_repos.get) == "function"
                   and type(linked_repos.list) == "function""#,
        )
        .eval_async()
        .await
        .unwrap();
    assert!(ok);
}

#[tokio::test]
#[serial]
async fn list_returns_grants() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(
        &app.state,
        None,
        None,
        Some("mirror"),
        "atproto",
        "did:plc:admin",
    )
    .await
    .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", Some("target.test"))
        .await
        .unwrap();

    let lua = lua_with_api(&app).await;
    let (count, did, reason): (i64, String, String) = lua
        .load(
            r#"local all = linked_repos.list()
               return #all, all[1].did, all[1].reason"#,
        )
        .eval_async()
        .await
        .unwrap();

    assert_eq!(count, 1);
    assert_eq!(did, "did:plc:target");
    assert_eq!(reason, "mirror");
}

#[tokio::test]
#[serial]
async fn get_returns_nil_for_unknown_did() {
    common::require_db!();
    let app = TestApp::new().await;
    let lua = lua_with_api(&app).await;

    let is_nil: bool = lua
        .load(r#"return linked_repos.get("did:plc:nobody") == nil"#)
        .eval_async()
        .await
        .unwrap();
    assert!(is_nil);
}

#[tokio::test]
#[serial]
async fn get_exposes_handle_fields() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(
        &app.state,
        None,
        None,
        None,
        "repo:com.example.note?action=create",
        "did:plc:admin",
    )
    .await
    .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", Some("target.test"))
        .await
        .unwrap();

    let lua = lua_with_api(&app).await;
    let (did, handle, status): (String, String, String) = lua
        .load(
            r#"local repo = linked_repos.get("did:plc:target")
               return repo.did, repo.handle, repo.status"#,
        )
        .eval_async()
        .await
        .unwrap();

    assert_eq!(did, "did:plc:target");
    assert_eq!(handle, "target.test");
    assert_eq!(status, "active");
}

#[tokio::test]
#[serial]
async fn create_record_raises_on_missing_scope() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(
        &app.state,
        None,
        None,
        None,
        "repo:com.example.allowed?action=create",
        "did:plc:admin",
    )
    .await
    .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", None)
        .await
        .unwrap();

    let lua = lua_with_api(&app).await;
    let err = lua
        .load(
            r#"local repo = linked_repos.get("did:plc:target")
               repo:create_record{ collection = "com.example.forbidden", record = { a = 1 } }"#,
        )
        .exec_async()
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("com.example.forbidden"), "got: {err}");
}

#[tokio::test]
#[serial]
async fn get_raises_when_grant_needs_reauth() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", None)
        .await
        .unwrap();
    db::mark_needs_reauth(&app.state, &grant.id, "revoked")
        .await
        .unwrap();

    let lua = lua_with_api(&app).await;
    let err = lua
        .load(r#"local repo = linked_repos.get("did:plc:target")"#)
        .exec_async()
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("needs reauthorization"), "got: {err}");
}

#[tokio::test]
#[serial]
async fn create_record_requires_a_collection() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(&app.state, None, None, None, "repo:*", "did:plc:admin")
        .await
        .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", None)
        .await
        .unwrap();

    let lua = lua_with_api(&app).await;
    let err = lua
        .load(
            r#"local repo = linked_repos.get("did:plc:target")
               repo:create_record{ record = { a = 1 } }"#,
        )
        .exec_async()
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("collection is required"), "got: {err}");
}

#[tokio::test]
#[serial]
async fn put_record_swap_cid_is_passed_through_to_the_scope_check() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(
        &app.state,
        None,
        None,
        None,
        "repo:com.example.note?action=update",
        "did:plc:admin",
    )
    .await
    .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", None)
        .await
        .unwrap();

    let lua = lua_with_api(&app).await;

    // Without swap_cid: refused, naming both actions.
    let err_no_swap = lua
        .load(
            r#"local repo = linked_repos.get("did:plc:target")
               repo:put_record{ collection = "com.example.note", rkey = "abc", record = { a = 1 } }"#,
        )
        .exec_async()
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err_no_swap.contains("action=create") && err_no_swap.contains("action=update"),
        "got: {err_no_swap}"
    );

    // With swap_cid: scope check passes; failure comes from session
    // restoration instead (no OAuth session exists for did:plc:target).
    let err_with_swap = lua
        .load(
            r#"local repo = linked_repos.get("did:plc:target")
               repo:put_record{
                   collection = "com.example.note",
                   rkey = "abc",
                   record = { a = 1 },
                   swap_cid = "bafyreiabc123",
               }"#,
        )
        .exec_async()
        .await
        .unwrap_err()
        .to_string();
    assert!(
        !err_with_swap.contains("lacks the scope"),
        "scope check should have passed, got: {err_with_swap}"
    );
    assert!(
        err_with_swap.contains("needs reauthorization"),
        "expected a session/auth failure, got: {err_with_swap}"
    );
}

#[tokio::test]
#[serial]
async fn delete_record_raises_on_missing_scope() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(
        &app.state,
        None,
        None,
        None,
        "repo:com.example.allowed?action=create",
        "did:plc:admin",
    )
    .await
    .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", None)
        .await
        .unwrap();

    let lua = lua_with_api(&app).await;
    let err = lua
        .load(
            r#"local repo = linked_repos.get("did:plc:target")
               repo:delete_record{ collection = "com.example.allowed", rkey = "abc" }"#,
        )
        .exec_async()
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("delete"), "got: {err}");
    assert!(err.contains("com.example.allowed"), "got: {err}");
}

#[tokio::test]
#[serial]
async fn delete_record_requires_a_collection() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(&app.state, None, None, None, "repo:*", "did:plc:admin")
        .await
        .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", None)
        .await
        .unwrap();

    let lua = lua_with_api(&app).await;
    let err = lua
        .load(
            r#"local repo = linked_repos.get("did:plc:target")
               repo:delete_record{ rkey = "abc" }"#,
        )
        .exec_async()
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("collection is required"), "got: {err}");
}

#[tokio::test]
#[serial]
async fn delete_record_requires_a_rkey() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(&app.state, None, None, None, "repo:*", "did:plc:admin")
        .await
        .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", None)
        .await
        .unwrap();

    let lua = lua_with_api(&app).await;
    let err = lua
        .load(
            r#"local repo = linked_repos.get("did:plc:target")
               repo:delete_record{ collection = "com.example.note" }"#,
        )
        .exec_async()
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("rkey is required"), "got: {err}");
}

#[tokio::test]
#[serial]
async fn upload_blob_passes_non_utf8_bytes_through_intact() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(
        &app.state,
        None,
        None,
        None,
        "blob:image/png",
        "did:plc:admin",
    )
    .await
    .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", None)
        .await
        .unwrap();

    let lua = lua_with_api(&app).await;
    let err = lua
        .load(
            r#"local repo = linked_repos.get("did:plc:target")
               local bytes = string.char(0xFF, 0xFE, 0x00, 0x01)
               repo:upload_blob(bytes, "image/png")"#,
        )
        .exec_async()
        .await
        .unwrap_err()
        .to_string();

    assert!(
        !err.contains("lacks a blob scope"),
        "scope check should have passed, got: {err}"
    );
    assert!(
        !err.to_lowercase().contains("utf-8") && !err.to_lowercase().contains("utf8"),
        "bytes should not have been rejected as invalid UTF-8, got: {err}"
    );
    assert!(
        err.contains("needs reauthorization"),
        "expected a session/auth failure, got: {err}"
    );
}

#[tokio::test]
#[serial]
async fn upload_blob_raises_on_missing_blob_scope() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(
        &app.state,
        None,
        None,
        None,
        "blob:video/mp4",
        "did:plc:admin",
    )
    .await
    .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", None)
        .await
        .unwrap();

    let lua = lua_with_api(&app).await;
    let err = lua
        .load(
            r#"local repo = linked_repos.get("did:plc:target")
               local bytes = string.char(0xFF, 0xFE, 0x00, 0x01)
               repo:upload_blob(bytes, "image/png")"#,
        )
        .exec_async()
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("lacks a blob scope"), "got: {err}");
    assert!(err.contains("image/png"), "got: {err}");
}

#[tokio::test]
#[serial]
async fn call_converts_params_table_without_raising() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", None)
        .await
        .unwrap();

    let lua = lua_with_api(&app).await;
    let err = lua
        .load(
            r#"local repo = linked_repos.get("did:plc:target")
               repo:call("com.example.doThing", { params = { foo = "bar", n = 1 } })"#,
        )
        .exec_async()
        .await
        .unwrap_err()
        .to_string();

    assert!(
        err.contains("needs reauthorization"),
        "expected a session/auth failure, got: {err}"
    );
}

#[tokio::test]
#[serial]
async fn call_converts_input_table_without_raising() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", None)
        .await
        .unwrap();

    let lua = lua_with_api(&app).await;
    let err = lua
        .load(
            r#"local repo = linked_repos.get("did:plc:target")
               repo:call("com.example.doThing", { input = { foo = "bar", nested = { a = 1 } } })"#,
        )
        .exec_async()
        .await
        .unwrap_err()
        .to_string();

    assert!(
        err.contains("needs reauthorization"),
        "expected a session/auth failure, got: {err}"
    );
}

#[tokio::test]
#[serial]
async fn stale_handle_is_rejected_after_grant_needs_reauth() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(&app.state, None, None, None, "repo:*", "did:plc:admin")
        .await
        .unwrap();
    db::bind_did(&app.state, &grant.id, "did:plc:target", None)
        .await
        .unwrap();

    let lua = lua_with_api(&app).await;
    let get_and_flip: mlua::Table = lua
        .load(
            r#"local repo = linked_repos.get("did:plc:target")
               return { repo = repo }"#,
        )
        .eval_async()
        .await
        .unwrap();
    let repo: mlua::Value = get_and_flip.get("repo").unwrap();
    lua.globals().set("stale_repo", repo).unwrap();

    db::mark_needs_reauth(&app.state, &grant.id, "revoked")
        .await
        .unwrap();

    let err = lua
        .load(r#"stale_repo:create_record{ collection = "com.example.note", record = { a = 1 } }"#)
        .exec_async()
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("needs reauthorization"), "got: {err}");
}

#[tokio::test]
#[serial]
async fn global_is_registered_for_record_event_scripts() {
    common::require_db!();
    let app = TestApp::new().await;

    let grant = db::create(
        &app.state,
        None,
        None,
        Some("mirror"),
        "atproto",
        "did:plc:abcdefghijklmnopqrstuvwx",
    )
    .await
    .unwrap();
    db::bind_did(
        &app.state,
        &grant.id,
        "did:plc:234567abcdefghijklmnopqr",
        Some("target.test"),
    )
    .await
    .unwrap();

    let script = happyview::lua::ResolvedScript {
        id: "record.create:com.example.note".to_string(),
        language: happyview::lua::ScriptLanguage::Lua,
        body: r#"
            function handle()
              local all = linked_repos.list()
              return {
                kind = type(linked_repos),
                get = type(linked_repos.get),
                list = type(linked_repos.list),
                count = #all,
                did = all[1] and all[1].did or nil,
              }
            end
        "#
        .to_string(),
    };

    let record = serde_json::json!({ "title": "hello" });
    let out = happyview::lua::run_record_event_once(
        &app.state,
        &script,
        happyview::lua::RecordEventPayload {
            nsid: "com.example.note",
            action: "create",
            uri: "at://did:plc:author/com.example.note/abc",
            did: "did:plc:author",
            rkey: "abc",
            record: Some(&record),
        },
    )
    .await
    .unwrap()
    .expect("script returned a table");

    assert_eq!(out["kind"], "table");
    assert_eq!(out["get"], "function");
    assert_eq!(out["list"], "function");
    assert_eq!(out["count"], 1);
    assert_eq!(out["did"], "did:plc:234567abcdefghijklmnopqr");
}

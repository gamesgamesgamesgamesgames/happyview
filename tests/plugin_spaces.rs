//! End-to-end coverage of the fifteen spaces host imports, through the
//! executor and the `sdk_spaces` fixture rather than the bare Rust functions
//! in `src/plugin/host/spaces.rs` (already unit-tested there). What this pins
//! is that the capability gate, the feature flag, and `require_spaces_caller`
//! are reachable from a real WASM module — not the space-membership rules
//! themselves, which `src/spaces/service.rs`'s own tests already cover.

mod common;

use axum::body::Body;
use axum::http::Request;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

use happyview::plugin::library::LibraryCallContext;
use happyview::plugin::{LoadedPlugin, PluginInfo, PluginManifest, PluginSource};

const FIXTURE: &str =
    "tests/fixtures/sdk_spaces/target/wasm32-unknown-unknown/release/sdk_spaces.wasm";

fn fixture_bytes() -> Vec<u8> {
    std::fs::read(FIXTURE).expect(
        "spaces fixture not built. Run: cargo build --manifest-path tests/fixtures/sdk_spaces/Cargo.toml --target wasm32-unknown-unknown --release",
    )
}

/// The fixture registered as a library under `id`, with `capabilities`.
/// `plugin_registry.register` never runs the loader's import check, so this
/// also registers manifests missing a capability the fixture still imports,
/// to pin that the host function itself refuses the call rather than
/// trusting whatever the module happened to import.
fn spaces_plugin(id: &str, capabilities: &[&str]) -> LoadedPlugin {
    let manifest: PluginManifest = serde_json::from_value(json!({
        "id": id, "name": id, "version": "1.0.0", "api_version": "2",
        "plugin_type": "library", "namespace": id,
        "capabilities": capabilities,
    }))
    .unwrap();
    LoadedPlugin {
        info: PluginInfo {
            id: id.into(),
            name: id.into(),
            version: "1.0.0".into(),
            api_version: "2".into(),
            icon_url: None,
            required_secrets: vec![],
            auth_type: "oauth2".into(),
            config_schema: None,
        },
        source: PluginSource::File {
            path: "tests/fixtures/sdk_spaces".into(),
        },
        wasm_bytes: fixture_bytes(),
        manifest: Some(manifest),
    }
}

async fn set_spaces_enabled(app: &TestApp, enabled: bool) {
    let (name, value) = app.admin_cookie();
    let req = Request::builder()
        .method("PUT")
        .uri("/admin/settings/feature.spaces_enabled")
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "value": enabled.to_string() }).to_string(),
        ))
        .unwrap();
    assert!(
        app.router
            .clone()
            .oneshot(req)
            .await
            .unwrap()
            .status()
            .is_success(),
        "failed to set spaces_enabled to {enabled}"
    );
}

async fn enable_spaces(app: &TestApp) {
    set_spaces_enabled(app, true).await;
}

fn ctx_for(did: &str) -> LibraryCallContext {
    LibraryCallContext {
        caller_did: Some(did.to_string()),
        ..LibraryCallContext::default()
    }
}

fn rand_did(label: &str) -> String {
    format!("did:plc:{label}{}", Uuid::new_v4().simple())
}

fn rand_skey(label: &str) -> String {
    format!("{label}{}", Uuid::new_v4().simple())
}

async fn call(
    app: &TestApp,
    plugin_id: &str,
    function: &str,
    args: &[Value],
    ctx: &LibraryCallContext,
) -> Result<Value, happyview::plugin::ExecutionError> {
    app.state
        .plugin_executor()
        .call_library(plugin_id, function, args, ctx, 0)
        .await
}

/// A registered fixture, the flag on, and a fresh space created by `creator`.
/// Returns the space's `at://` URI. `create_space` also seats `creator` as a
/// write member, per `spaces::service::create_space`.
async fn seeded_space(app: &TestApp, creator: &str) -> String {
    enable_spaces(app).await;
    app.state
        .plugin_registry
        .register(spaces_plugin(
            "sdk_spaces",
            &["spaces:read", "spaces:write"],
        ))
        .await;

    let created = call(
        app,
        "sdk_spaces",
        "create",
        &[json!({
            "type": "com.example.forum",
            "skey": rand_skey("main"),
        })],
        &ctx_for(creator),
    )
    .await
    .expect("create should succeed");
    created["uri"]
        .as_str()
        .expect("uri in create response")
        .to_string()
}

#[tokio::test]
#[serial(spaces_feature_flag)]
async fn create_info_write_and_query_round_trip_the_record() {
    common::require_db!();
    let app = TestApp::new().await;
    let creator = rand_did("creator");
    let space_uri = seeded_space(&app, &creator).await;

    let info = call(
        &app,
        "sdk_spaces",
        "info",
        &[json!({"uri": space_uri})],
        &LibraryCallContext::default(),
    )
    .await
    .expect("info should succeed");
    assert_eq!(info["did"], creator);

    let written = call(
        &app,
        "sdk_spaces",
        "write_record",
        &[json!({
            "uri": space_uri,
            "collection": "com.example.item",
            "record": {"text": "hello"},
        })],
        &ctx_for(&creator),
    )
    .await
    .expect("write_record should succeed");
    let record_uri = written["uri"]
        .as_str()
        .expect("uri in write_record response");

    let page = call(
        &app,
        "sdk_spaces",
        "query",
        &[json!({"uri": space_uri})],
        &LibraryCallContext::default(),
    )
    .await
    .expect("query should succeed");
    let records = page["records"].as_array().expect("records array");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["uri"], record_uri);
    assert_eq!(records[0]["author_did"], creator);
}

#[tokio::test]
#[serial(spaces_feature_flag)]
async fn write_record_by_a_non_member_is_not_authorized() {
    common::require_db!();
    let app = TestApp::new().await;
    let creator = rand_did("creator");
    let space_uri = seeded_space(&app, &creator).await;
    let stranger = rand_did("stranger");

    let err = call(
        &app,
        "sdk_spaces",
        "write_record",
        &[json!({
            "uri": space_uri,
            "collection": "com.example.item",
            "record": {"text": "hi"},
        })],
        &ctx_for(&stranger),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("NOT_AUTHORIZED"), "{err}");
}

#[tokio::test]
#[serial(spaces_feature_flag)]
async fn add_member_then_that_dids_write_succeeds() {
    common::require_db!();
    let app = TestApp::new().await;
    let creator = rand_did("creator");
    let space_uri = seeded_space(&app, &creator).await;
    let member = rand_did("member");

    call(
        &app,
        "sdk_spaces",
        "add_member",
        &[json!({
            "uri": space_uri,
            "did": member,
            "access": "write",
        })],
        &ctx_for(&creator),
    )
    .await
    .expect("add_member should succeed");

    let written = call(
        &app,
        "sdk_spaces",
        "write_record",
        &[json!({
            "uri": space_uri,
            "collection": "com.example.item",
            "record": {"text": "from the new member"},
        })],
        &ctx_for(&member),
    )
    .await
    .expect("write_record by the newly added member should succeed");
    assert_eq!(
        written["uri"].as_str().map(|s| s.contains(&member)),
        Some(true)
    );
}

/// `remove_member` and `delete` both decode as `Option<Value>` on the SDK
/// side — the exact class of wrapper the envelope ordering in `wire.rs`
/// affects — so each needs its success path pinned through the executor,
/// not just its authorization checks.
#[tokio::test]
#[serial(spaces_feature_flag)]
async fn remove_member_then_delete_clear_access_and_info() {
    common::require_db!();
    let app = TestApp::new().await;
    let creator = rand_did("creator");
    let space_uri = seeded_space(&app, &creator).await;
    let member = rand_did("member");

    call(
        &app,
        "sdk_spaces",
        "add_member",
        &[json!({"uri": space_uri, "did": member, "access": "write"})],
        &ctx_for(&creator),
    )
    .await
    .expect("add_member should succeed");

    call(
        &app,
        "sdk_spaces",
        "remove_member",
        &[json!({"uri": space_uri, "did": member})],
        &ctx_for(&creator),
    )
    .await
    .expect("remove_member should succeed");

    let access = call(
        &app,
        "sdk_spaces",
        "access",
        &[json!({"uri": space_uri, "did": member})],
        &LibraryCallContext::default(),
    )
    .await
    .expect("access should succeed");
    assert_eq!(access, Value::Null);

    call(
        &app,
        "sdk_spaces",
        "delete",
        &[json!({"uri": space_uri})],
        &ctx_for(&creator),
    )
    .await
    .expect("delete should succeed");

    let info = call(
        &app,
        "sdk_spaces",
        "info",
        &[json!({"uri": space_uri})],
        &LibraryCallContext::default(),
    )
    .await
    .expect("info should succeed");
    assert_eq!(info, Value::Null);
}

#[tokio::test]
#[serial(spaces_feature_flag)]
async fn set_member_on_an_existing_member_is_not_a_conflict() {
    common::require_db!();
    let app = TestApp::new().await;
    let creator = rand_did("creator");
    let space_uri = seeded_space(&app, &creator).await;
    let member = rand_did("member");

    call(
        &app,
        "sdk_spaces",
        "add_member",
        &[json!({"uri": space_uri, "did": member, "access": "read"})],
        &ctx_for(&creator),
    )
    .await
    .expect("add_member should succeed");

    // `add_member` again on the same DID conflicts...
    let err = call(
        &app,
        "sdk_spaces",
        "add_member",
        &[json!({"uri": space_uri, "did": member, "access": "write"})],
        &ctx_for(&creator),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("CONFLICT"), "{err}");

    // ...but `set_member` upserts instead.
    let updated = call(
        &app,
        "sdk_spaces",
        "set_member",
        &[json!({"uri": space_uri, "did": member, "access": "write"})],
        &ctx_for(&creator),
    )
    .await
    .expect("set_member on an existing member should succeed");
    assert_eq!(updated["access"], "write");

    let members = call(
        &app,
        "sdk_spaces",
        "members",
        &[json!({"uri": space_uri})],
        &LibraryCallContext::default(),
    )
    .await
    .expect("members should succeed");
    let members = members.as_array().expect("members array");
    let listed = members
        .iter()
        .find(|m| m["did"] == member)
        .expect("upserted member should be listed");
    assert_eq!(listed["access"], "write");
}

#[tokio::test]
#[serial(spaces_feature_flag)]
async fn create_invite_then_accept_invite_makes_the_joiner_a_read_member() {
    common::require_db!();
    let app = TestApp::new().await;
    let creator = rand_did("creator");
    let space_uri = seeded_space(&app, &creator).await;
    let joiner = rand_did("joiner");

    let invite = call(
        &app,
        "sdk_spaces",
        "create_invite",
        &[json!({"uri": space_uri})],
        &ctx_for(&creator),
    )
    .await
    .expect("create_invite should succeed");
    let token = invite["token"]
        .as_str()
        .expect("token in create_invite response");
    assert_eq!(invite["access"], "read");

    call(
        &app,
        "sdk_spaces",
        "accept_invite",
        &[json!({"token": token})],
        &ctx_for(&joiner),
    )
    .await
    .expect("accept_invite should succeed");

    let access = call(
        &app,
        "sdk_spaces",
        "access",
        &[json!({"uri": space_uri, "did": joiner})],
        &LibraryCallContext::default(),
    )
    .await
    .expect("access should succeed");
    assert_eq!(access, json!("read"));
}

/// `delete_record`'s own-records-only check compares the *stored*
/// `author_did` against the caller, but the URI it looks up is always
/// rebuilt from the caller's own DID (mirroring `write_record`, where a
/// record's path segment is always its author). A normal write can therefore
/// never produce a URI whose path DID differs from its stored author, so
/// this seeds that mismatch directly through `spaces::db`, the same way
/// `delete_record_forbidden_for_non_author` in `src/spaces/service.rs` does
/// at the Rust-function level — the write path is exempt from the API this
/// file otherwise exercises.
#[tokio::test]
#[serial(spaces_feature_flag)]
async fn delete_record_by_a_non_author_is_not_authorized() {
    common::require_db!();
    let app = TestApp::new().await;
    let creator = rand_did("creator");
    let space_uri = seeded_space(&app, &creator).await;
    let other_writer = rand_did("otherwriter");

    call(
        &app,
        "sdk_spaces",
        "add_member",
        &[json!({"uri": space_uri, "did": other_writer, "access": "write"})],
        &ctx_for(&creator),
    )
    .await
    .expect("add_member should succeed");

    let uri = happyview::spaces::SpaceUri::parse(&space_uri).expect("valid space uri");
    let space = happyview::spaces::db::get_space_by_address(
        &app.state.db,
        app.state.db_backend,
        &uri.did,
        &uri.type_nsid,
        &uri.skey,
    )
    .await
    .expect("get_space_by_address should succeed")
    .expect("space should exist");

    let collection = "com.example.item";
    let rkey = "fixed-rkey-for-ownership-test";
    let record_uri = format!(
        "at://{}/space/{}/{}/{}/{}/{}",
        space.did, space.type_nsid, space.skey, other_writer, collection, rkey
    );
    let content = json!({"text": "authored by the creator"});
    let cid = happyview::cid_verify::compute_record_cid(&content)
        .expect("test record must be DAG-CBOR encodable")
        .to_string();
    happyview::spaces::db::insert_space_record(
        &app.state.db,
        app.state.db_backend,
        &happyview::spaces::types::SpaceRecord {
            uri: record_uri.clone(),
            space_id: space.id,
            author_did: creator,
            collection: collection.to_string(),
            rkey: rkey.to_string(),
            record: content,
            cid,
            indexed_at: happyview::db::now_rfc3339(),
        },
    )
    .await
    .expect("failed to seed mismatched-author record");

    let err = call(
        &app,
        "sdk_spaces",
        "delete_record",
        &[json!({
            "uri": space_uri,
            "collection": collection,
            "rkey": rkey,
        })],
        &ctx_for(&other_writer),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("NOT_AUTHORIZED"), "{err}");

    // The record must still be present since the delete was rejected.
    let still_there =
        happyview::spaces::db::get_space_record(&app.state.db, app.state.db_backend, &record_uri)
            .await
            .expect("get_space_record should succeed");
    assert!(still_there.is_some());
}

#[tokio::test]
#[serial(spaces_feature_flag)]
async fn a_write_with_no_caller_did_is_bad_input_naming_the_caller() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    app.state
        .plugin_registry
        .register(spaces_plugin(
            "sdk_spaces",
            &["spaces:read", "spaces:write"],
        ))
        .await;

    let err = call(
        &app,
        "sdk_spaces",
        "create",
        &[json!({"type": "com.example.forum", "skey": rand_skey("main")})],
        &LibraryCallContext::default(),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("BAD_INPUT"), "{err}");
    assert!(err.to_string().contains("caller"), "{err}");
}

#[tokio::test]
#[serial(spaces_feature_flag)]
async fn create_without_spaces_write_is_forbidden() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    app.state
        .plugin_registry
        .register(spaces_plugin("sdk_spaces_readonly", &["spaces:read"]))
        .await;

    let err = call(
        &app,
        "sdk_spaces_readonly",
        "create",
        &[json!({"type": "com.example.forum", "skey": rand_skey("main")})],
        &ctx_for(&rand_did("creator")),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("FORBIDDEN"), "{err}");
}

#[tokio::test]
#[serial(spaces_feature_flag)]
async fn feature_flag_off_disables_every_operation() {
    common::require_db!();
    let app = TestApp::new().await;
    // `get_setting` falls back to the `FEATURE_SPACES_ENABLED` env var when no
    // row exists, so asserting the disabled state needs the row set
    // explicitly rather than relying on that fallback reading as off.
    set_spaces_enabled(&app, false).await;

    app.state
        .plugin_registry
        .register(spaces_plugin(
            "sdk_spaces",
            &["spaces:read", "spaces:write"],
        ))
        .await;

    let err = call(
        &app,
        "sdk_spaces",
        "info",
        &[json!({"uri": "at://did:plc:x/space/com.example.forum/main"})],
        &LibraryCallContext::default(),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("SPACES_DISABLED"), "{err}");

    // A write refuses the same way, before it ever resolves the space or
    // checks membership.
    let err = call(
        &app,
        "sdk_spaces",
        "write_record",
        &[json!({
            "uri": "at://did:plc:x/space/com.example.forum/main",
            "collection": "com.example.item",
            "record": {"text": "hello"},
        })],
        &ctx_for("did:plc:x"),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("SPACES_DISABLED"), "{err}");
}

/// Loads and installs the `sdk_spaces` fixture through the loader, so its
/// manifest's declared capabilities are cross-checked against its actual
/// imports the way an operator's upload would be. Not exercised by the tests
/// above, which register directly to probe capability combinations the
/// loader itself would refuse; kept here to pin that a real upload of this
/// fixture is accepted at all.
#[tokio::test]
#[serial(spaces_feature_flag)]
async fn the_fixture_installs_through_the_loader() {
    common::require_db!();
    let app = TestApp::new().await;
    let plugin = happyview::plugin::loader::load_from_file(std::path::Path::new(
        "tests/fixtures/sdk_spaces",
    ))
    .await
    .expect(
        "sdk_spaces fixture not built. Run: cargo build --manifest-path \
         tests/fixtures/sdk_spaces/Cargo.toml --target wasm32-unknown-unknown --release",
    );
    app.state.plugin_registry.install(plugin).await.unwrap();
}

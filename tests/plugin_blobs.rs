//! End-to-end coverage of the three blob host imports, through the executor
//! and the `sdk_blobs` fixture rather than the Rust functions in
//! `src/blobs.rs` (already unit-tested there).
//!
//! What this pins that those cannot: that the wire types survive the WASM
//! boundary for content that is not valid UTF-8, that the capability gate
//! refuses a call the module can still make, and that `exists` — which the
//! SDK derives from `stat` rather than importing — still answers.

mod common;

use serde_json::{Value, json};

use common::app::TestApp;

use happyview::plugin::library::LibraryCallContext;
use happyview::plugin::{LoadedPlugin, PluginInfo, PluginManifest, PluginSource};

const FIXTURE: &str =
    "tests/fixtures/sdk_blobs/target/wasm32-unknown-unknown/release/sdk_blobs.wasm";

fn fixture_bytes() -> Vec<u8> {
    std::fs::read(FIXTURE).expect(
        "blobs fixture not built. Run: cargo build --manifest-path tests/fixtures/sdk_blobs/Cargo.toml --target wasm32-unknown-unknown --release",
    )
}

/// The fixture registered as a library with `capabilities`.
///
/// `plugin_registry.register` never runs the loader's import check, so this
/// can register a manifest missing a capability the fixture still imports —
/// which is how the gate on the host function itself gets tested, rather
/// than the loader's refusal to install such a plugin at all.
fn blobs_plugin(id: &str, capabilities: &[&str]) -> LoadedPlugin {
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
            path: "tests/fixtures/sdk_blobs".into(),
        },
        wasm_bytes: fixture_bytes(),
        manifest: Some(manifest),
    }
}

fn caller() -> LibraryCallContext {
    LibraryCallContext {
        caller_did: Some("did:plc:caller".into()),
        ..LibraryCallContext::default()
    }
}

async fn call(app: &TestApp, plugin: &str, function: &str, args: &[Value]) -> Value {
    app.state
        .plugin_executor()
        .call_library(plugin, function, args, &caller(), 0)
        .await
        .unwrap_or_else(|e| panic!("{function}: {e:?}"))
}

/// Bytes that are not valid UTF-8, so the wire's byte-array branch is the one
/// exercised. A string payload would only ever prove the other branch.
const ARTIFACT: [u8; 5] = [0x00, 0x61, 0xff, 0x73, 0x6d];

#[tokio::test]
async fn bytes_survive_the_boundary_and_come_back_whole() {
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .register(blobs_plugin("sdk_blobs", &["blobs:read", "blobs:write"]))
        .await;

    let cid = call(
        &app,
        "sdk_blobs",
        "put",
        &[json!(ARTIFACT.to_vec()), json!("application/wasm")],
    )
    .await;
    let cid = cid.as_str().expect("put answers a cid").to_string();
    assert_eq!(
        cid,
        happyview::cid_verify::raw_cid(&ARTIFACT)
            .expect("cid")
            .to_string(),
        "a plugin's blob must land on the CID the network would mint"
    );

    let got = call(&app, "sdk_blobs", "get", &[json!(cid)]).await;
    let bytes: Vec<u8> = serde_json::from_value(got["bytes"].clone()).expect("bytes");
    assert_eq!(bytes, ARTIFACT, "the bytes came back changed");
    assert_eq!(got["mime_type"], "application/wasm");
    assert_eq!(got["size"], ARTIFACT.len());

    let stat = call(&app, "sdk_blobs", "stat", &[json!(cid)]).await;
    assert_eq!(stat["cid"], cid);
    assert_eq!(stat["mime_type"], "application/wasm");
    assert_eq!(stat["size"], ARTIFACT.len());
    assert!(
        stat.get("bytes").is_none(),
        "a stat should not carry the payload: {stat}"
    );

    assert_eq!(call(&app, "sdk_blobs", "exists", &[json!(cid)]).await, true);
}

/// Absent is an ordinary answer on both reads, which is what lets a caller
/// ask whether a blob is held without transferring it.
#[tokio::test]
async fn an_unstored_cid_is_null_rather_than_an_error() {
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .register(blobs_plugin("sdk_blobs", &["blobs:read", "blobs:write"]))
        .await;

    let absent = happyview::cid_verify::raw_cid(b"never stored")
        .expect("cid")
        .to_string();

    assert_eq!(
        call(&app, "sdk_blobs", "get", &[json!(absent)]).await,
        Value::Null
    );
    assert_eq!(
        call(&app, "sdk_blobs", "stat", &[json!(absent)]).await,
        Value::Null
    );
    assert_eq!(
        call(&app, "sdk_blobs", "exists", &[json!(absent)]).await,
        false
    );
}

/// The two capabilities are separate decisions, so each import refuses
/// without its own — and the refusal comes from the host function, not from
/// the module declining to import.
#[tokio::test]
async fn each_import_refuses_without_its_capability() {
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .register(blobs_plugin("read_only", &["blobs:read"]))
        .await;
    app.state
        .plugin_registry
        .register(blobs_plugin("write_only", &["blobs:write"]))
        .await;

    // A reader cannot store.
    let refused = app
        .state
        .plugin_executor()
        .call_library(
            "read_only",
            "put",
            &[json!(ARTIFACT.to_vec()), json!("application/wasm")],
            &caller(),
            0,
        )
        .await
        .expect_err("put without blobs:write should be refused");
    let refused = format!("{refused:?}");
    assert!(
        refused.contains("blobs:write"),
        "the refusal should name the missing capability, got {refused}"
    );

    // A writer can store but cannot read back.
    let cid = call(
        &app,
        "write_only",
        "put",
        &[json!(ARTIFACT.to_vec()), json!("application/wasm")],
    )
    .await;
    let cid = cid.as_str().expect("cid").to_string();

    for function in ["get", "stat"] {
        let refused = app
            .state
            .plugin_executor()
            .call_library("write_only", function, &[json!(cid)], &caller(), 0)
            .await
            .expect_err(&format!("{function} without blobs:read should be refused"));
        let refused = format!("{refused:?}");
        assert!(
            refused.contains("blobs:read"),
            "{function} without blobs:read should name the capability, got {refused}"
        );
    }
}

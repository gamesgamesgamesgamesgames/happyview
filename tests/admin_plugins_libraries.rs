mod common;

use happyview::plugin::{LoadedPlugin, PluginInfo, PluginManifest, PluginSource};
use serde_json::json;
use serial_test::serial;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

use common::app::TestApp;

/// Duplicated from `tests/plugin_integration.rs`'s `graph_registry` module —
/// the smallest library `LoadedPlugin` with a set of dependencies.
fn library(id: &str, version: &str, deps: &[(&str, &str)]) -> LoadedPlugin {
    let deps_json: Vec<serde_json::Value> = deps
        .iter()
        .map(|(id, v)| serde_json::json!({"id": id, "version": v}))
        .collect();
    let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
        "id": id, "name": id, "version": version, "api_version": "2",
        "plugin_type": "library", "dependencies": deps_json,
    }))
    .unwrap();
    LoadedPlugin {
        info: PluginInfo {
            id: id.into(),
            name: id.into(),
            version: version.into(),
            api_version: "2".into(),
            icon_url: None,
            required_secrets: vec![],
            auth_type: "oauth2".into(),
            config_schema: None,
        },
        source: PluginSource::File {
            path: format!("/tmp/{id}").into(),
        },
        // Valid, trivial WASM — not `vec![]`, which `analyze_imports` (and so
        // `capabilities::report`) rejects as unparseable, leaving a parser error
        // in `capabilities.undeclared`.
        wasm_bytes: wat::parse_str(r#"(module (memory (export "memory") 1))"#).unwrap(),
        manifest: Some(manifest),
    }
}

/// Mount a manifest + WASM pair on `app.mock_server` at `/<id>/manifest.json`
/// and `/<id>/plugin.wasm`. The module imports `imports` (stub signatures —
/// preview never instantiates) and exports `memory`, `alloc`, `dealloc`.
async fn serve_plugin(app: &TestApp, id: &str, caps: &[&str], imports: &[&str]) {
    let manifest = json!({
        "id": id, "name": id, "version": "1.0.0", "api_version": "2",
        "plugin_type": "library", "capabilities": caps, "wasm_file": "plugin.wasm",
    });

    Mock::given(method("GET"))
        .and(path(format!("/{id}/manifest.json")))
        .respond_with(ResponseTemplate::new(200).set_body_json(manifest))
        .mount(&app.mock_server)
        .await;

    let imports_wat: String = imports
        .iter()
        .map(|n| format!(r#"(import "env" "{n}" (func))"#))
        .collect::<Vec<_>>()
        .join(" ");
    let wasm = wat::parse_str(format!(
        "(module {imports_wat} (memory (export \"memory\") 1) \
         (func (export \"alloc\") (param i32) (result i32) i32.const 8) \
         (func (export \"dealloc\") (param i32 i32)))"
    ))
    .unwrap();

    Mock::given(method("GET"))
        .and(path(format!("/{id}/plugin.wasm")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(wasm, "application/wasm"))
        .mount(&app.mock_server)
        .await;
}

#[tokio::test]
#[serial]
async fn list_filters_by_type_and_exposes_dependencies() {
    common::require_db!();
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .install(library("db", "1.0.0", &[]))
        .await
        .unwrap();
    app.state
        .plugin_registry
        .install(library("record", "1.0.0", &[("db", "^1")]))
        .await
        .unwrap();
    app.install_fake_plugin("steam", "1.0.0").await;

    let body = app.get_json("/admin/plugins?type=library").await;
    let plugins = body["plugins"].as_array().unwrap();
    assert_eq!(plugins.len(), 2);
    let record = plugins.iter().find(|p| p["id"] == "record").unwrap();
    assert_eq!(record["plugin_type"], "library");
    assert_eq!(record["namespace"], "record");
    assert_eq!(record["dependencies"][0]["id"], "db");
    // The fixture declares nothing and imports nothing, so both should come
    // back empty — a regression in `capabilities::report()` on this fixture
    // (e.g. an unparseable-WASM error swallowed into `undeclared`) fails here.
    assert_eq!(record["capabilities"]["declared"], serde_json::json!([]));
    assert_eq!(record["capabilities"]["undeclared"], serde_json::json!([]));

    let all = app.get_json("/admin/plugins").await;
    assert_eq!(all["plugins"].as_array().unwrap().len(), 3);
    let steam = all["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "steam")
        .unwrap();
    assert_eq!(steam["plugin_type"], "auth");
}

#[tokio::test]
#[serial]
async fn delete_is_refused_with_409_while_depended_upon() {
    common::require_db!();
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .install(library("db", "1.0.0", &[]))
        .await
        .unwrap();
    app.state
        .plugin_registry
        .install(library("record", "1.0.0", &[("db", "^1")]))
        .await
        .unwrap();

    let (status, body) = app.delete_json("/admin/plugins/db").await;
    assert_eq!(status, 409);
    assert_eq!(body["dependents"], serde_json::json!(["record"]));
    assert!(app.state.plugin_registry.get("db").await.is_some());

    let (status, body) = app.delete_json("/admin/plugins/db?force=true").await;
    assert_eq!(status, 200);
    assert_eq!(body["removed"], serde_json::json!(["record", "db"]));
    assert!(app.state.plugin_registry.get("record").await.is_none());
}

#[tokio::test]
#[serial]
async fn delete_without_dependents_still_returns_204() {
    common::require_db!();
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .install(library("http", "1.0.0", &[]))
        .await
        .unwrap();
    let (status, _) = app.delete_json("/admin/plugins/http").await;
    assert_eq!(status, 204);
}

#[tokio::test]
#[serial]
async fn preview_reports_capabilities_with_risk() {
    common::require_db!();
    let app = TestApp::new().await;
    serve_plugin(&app, "dbplug", &["database:write"], &["host_db_execute"]).await;
    let body = app
        .post_json(
            "/admin/plugins/preview",
            serde_json::json!({"url": format!("{}/dbplug/manifest.json", app.mock_server.uri())}),
        )
        .await;
    assert_eq!(body["plugin_type"], "library");
    assert_eq!(
        body["capabilities"]["declared"][0]["name"],
        "database:write"
    );
    assert_eq!(body["capabilities"]["declared"][0]["risk"], "critical");
    assert_eq!(body["capabilities"]["undeclared"], serde_json::json!([]));
    assert_eq!(body["sha256"].as_str().unwrap().len(), 64);
}

#[tokio::test]
#[serial]
async fn preview_refuses_undeclared_imports() {
    common::require_db!();
    let app = TestApp::new().await;
    serve_plugin(&app, "sneaky", &[], &["host_db_execute"]).await;
    let (status, body) = app
        .post_json_status(
            "/admin/plugins/preview",
            serde_json::json!({"url": format!("{}/sneaky/manifest.json", app.mock_server.uri())}),
        )
        .await;
    assert_eq!(status, 400);
    assert!(body["error"].as_str().unwrap().contains("host_db_execute"));
}

#[tokio::test]
#[serial]
async fn install_requires_consent_to_cover_every_capability() {
    common::require_db!();
    let app = TestApp::new().await;
    serve_plugin(
        &app,
        "dbplug",
        &["database:write", "kv:read"],
        &["host_db_execute", "host_kv_get"],
    )
    .await;
    let url = format!("{}/dbplug/manifest.json", app.mock_server.uri());
    let (status, body) = app
        .post_json_status(
            "/admin/plugins",
            serde_json::json!({"url": url, "accepted_capabilities": ["kv:read"]}),
        )
        .await;
    assert_eq!(status, 400);
    assert!(body["error"].as_str().unwrap().contains("database:write"));

    let (status, _) = app
        .post_json_status(
            "/admin/plugins",
            serde_json::json!({"url": url, "accepted_capabilities": ["kv:read", "database:write"]}),
        )
        .await;
    assert_eq!(status, 200);
}

#[tokio::test]
#[serial]
async fn reload_requires_consent_to_cover_new_capabilities() {
    common::require_db!();
    let app = TestApp::new().await;

    // v1 only needs kv:read, and is installed with consent to exactly that.
    serve_plugin(&app, "dbplug2", &["kv:read"], &["host_kv_get"]).await;
    let v1_url = format!("{}/dbplug2/manifest.json", app.mock_server.uri());
    let (status, _) = app
        .post_json_status(
            "/admin/plugins",
            serde_json::json!({"url": v1_url, "accepted_capabilities": ["kv:read"]}),
        )
        .await;
    assert_eq!(status, 200);

    // v2 is served under a second URL, same plugin id, and now also needs
    // database:write.
    let manifest = json!({
        "id": "dbplug2", "name": "dbplug2", "version": "1.0.0", "api_version": "2",
        "plugin_type": "library", "capabilities": ["kv:read", "database:write"],
        "wasm_file": "plugin.wasm",
    });
    Mock::given(method("GET"))
        .and(path("/dbplug2-v2/manifest.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(manifest))
        .mount(&app.mock_server)
        .await;
    let imports_wat: String = ["host_kv_get", "host_db_execute"]
        .iter()
        .map(|n| format!(r#"(import "env" "{n}" (func))"#))
        .collect::<Vec<_>>()
        .join(" ");
    let wasm = wat::parse_str(format!(
        "(module {imports_wat} (memory (export \"memory\") 1) \
         (func (export \"alloc\") (param i32) (result i32) i32.const 8) \
         (func (export \"dealloc\") (param i32 i32)))"
    ))
    .unwrap();
    Mock::given(method("GET"))
        .and(path("/dbplug2-v2/plugin.wasm"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(wasm, "application/wasm"))
        .mount(&app.mock_server)
        .await;
    let v2_url = format!("{}/dbplug2-v2/manifest.json", app.mock_server.uri());

    let (status, body) = app
        .post_json_status(
            "/admin/plugins/dbplug2/reload",
            serde_json::json!({"url": v2_url, "accepted_capabilities": ["kv:read"]}),
        )
        .await;
    assert_eq!(status, 400);
    assert!(body["error"].as_str().unwrap().contains("database:write"));
    // The refusal happens before the old version is removed, so it must
    // still be installed and unaffected.
    assert!(app.state.plugin_registry.get("dbplug2").await.is_some());

    let (status, _) = app
        .post_json_status(
            "/admin/plugins/dbplug2/reload",
            serde_json::json!({"url": v2_url, "accepted_capabilities": ["kv:read", "database:write"]}),
        )
        .await;
    assert_eq!(status, 200);
}

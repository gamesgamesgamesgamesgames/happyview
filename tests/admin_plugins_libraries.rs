mod common;

use axum::body::Body;
use axum::http::Request;
use happyview::plugin::{LoadedPlugin, PluginInfo, PluginManifest, PluginSource};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

use common::app::TestApp;

/// Duplicated from `tests/plugin_integration.rs`'s `graph_registry` module —
/// the smallest library `LoadedPlugin` with a set of dependencies.
fn library(id: &str, version: &str, deps: &[(&str, &str)]) -> LoadedPlugin {
    library_with_capabilities(id, version, deps, &[])
}

/// As `library`, but with a declared capability set — for the
/// `network:request:defined` allowed-hosts endpoint tests, which check what
/// a plugin declares rather than instantiate it.
fn library_with_capabilities(
    id: &str,
    version: &str,
    deps: &[(&str, &str)],
    capabilities: &[&str],
) -> LoadedPlugin {
    let deps_json: Vec<serde_json::Value> = deps
        .iter()
        .map(|(id, v)| serde_json::json!({"id": id, "version": v}))
        .collect();
    let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
        "id": id, "name": id, "version": version, "api_version": "2",
        "plugin_type": "library", "dependencies": deps_json, "capabilities": capabilities,
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

// ---------------------------------------------------------------------------
// `network:request:defined` allowed-hosts endpoints
// ---------------------------------------------------------------------------

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap()
    }
}

fn admin_get(
    uri: &str,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(cookie.0, cookie.1)
        .body(Body::empty())
        .unwrap()
}

fn admin_put(
    uri: &str,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
    body: &Value,
) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header(cookie.0, cookie.1)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

/// `happyview_plugin_configs.plugin_id` is a foreign key into
/// `happyview_plugins` (see `src/plugin/config.rs`'s own test helper of the
/// same shape) — a plugin installed directly into the in-memory registry,
/// bypassing the admin install endpoint, needs a matching row before the PUT
/// endpoint can store anything for it. An upsert, not a plain insert: `id`
/// is this test's identifier and the harness's Postgres truncation list
/// (`tests/common/db.rs`) does not reset `happyview_plugins` between test
/// binary runs, so a plain `INSERT` would only succeed once.
async fn insert_plugin_row(app: &TestApp, id: &str) {
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_plugins (id, source, api_version) VALUES (?, 'file', '2')
         ON CONFLICT (id) DO UPDATE SET source = excluded.source",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(id)
        .execute(&app.state.db)
        .await
        .unwrap();
}

#[tokio::test]
#[serial]
async fn allowed_hosts_round_trips_through_put_and_get() {
    common::require_db!();
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .install(library_with_capabilities(
            "netdefined",
            "1.0.0",
            &[],
            &["network:request:defined"],
        ))
        .await
        .unwrap();
    insert_plugin_row(&app, "netdefined").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_put(
            "/admin/plugins/netdefined/allowed-hosts",
            app.admin_cookie(),
            &json!({"hosts": ["api.example.com", "*.cdn.example.com"]}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = json_body(resp).await;
    assert_eq!(
        body["hosts"],
        json!(["api.example.com", "*.cdn.example.com"])
    );

    let resp = app
        .router
        .clone()
        .oneshot(admin_get(
            "/admin/plugins/netdefined/allowed-hosts",
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = json_body(resp).await;
    assert_eq!(
        body["hosts"],
        json!(["api.example.com", "*.cdn.example.com"])
    );
}

#[tokio::test]
#[serial]
async fn allowed_hosts_put_rejects_an_invalid_host_by_name() {
    common::require_db!();
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .install(library_with_capabilities(
            "netdefined2",
            "1.0.0",
            &[],
            &["network:request:defined"],
        ))
        .await
        .unwrap();
    insert_plugin_row(&app, "netdefined2").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_put(
            "/admin/plugins/netdefined2/allowed-hosts",
            app.admin_cookie(),
            &json!({"hosts": ["http://not-a-host"]}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body = json_body(resp).await;
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("http://not-a-host"),
        "{body}"
    );
}

#[tokio::test]
#[serial]
async fn allowed_hosts_refused_for_a_plugin_that_does_not_declare_the_capability() {
    common::require_db!();
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .install(library("plain", "1.0.0", &[]))
        .await
        .unwrap();
    insert_plugin_row(&app, "plain").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_put(
            "/admin/plugins/plain/allowed-hosts",
            app.admin_cookie(),
            &json!({"hosts": ["api.example.com"]}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body = json_body(resp).await;
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("network:request:defined"),
        "{body}"
    );

    let resp = app
        .router
        .clone()
        .oneshot(admin_get(
            "/admin/plugins/plain/allowed-hosts",
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
}

#[tokio::test]
#[serial]
async fn allowed_hosts_unknown_plugin_returns_404() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_get(
            "/admin/plugins/does-not-exist/allowed-hosts",
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);

    let resp = app
        .router
        .clone()
        .oneshot(admin_put(
            "/admin/plugins/does-not-exist/allowed-hosts",
            app.admin_cookie(),
            &json!({"hosts": []}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
}

#[tokio::test]
#[serial]
async fn allowed_hosts_requires_permission() {
    common::require_db!();
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .install(library_with_capabilities(
            "netdefined3",
            "1.0.0",
            &[],
            &["network:request:defined"],
        ))
        .await
        .unwrap();
    insert_plugin_row(&app, "netdefined3").await;

    let non_admin = common::auth::admin_cookie_header("did:plc:notadmin", &app.state.cookie_key);

    let resp = app
        .router
        .clone()
        .oneshot(admin_get(
            "/admin/plugins/netdefined3/allowed-hosts",
            non_admin.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);

    let resp = app
        .router
        .clone()
        .oneshot(admin_put(
            "/admin/plugins/netdefined3/allowed-hosts",
            non_admin,
            &json!({"hosts": ["api.example.com"]}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);
}

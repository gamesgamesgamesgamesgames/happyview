//! The `/external-auth` routes answer for auth plugins only. A library or
//! interpreter installed beside them is neither listed as a sign-in provider
//! nor reachable through a provider's routes.

mod common;

use axum::body::Body;
use axum::http::Request;
use happyview::plugin::{LoadedPlugin, PluginInfo, PluginManifest, PluginSource};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;

/// A plugin of `plugin_type` whose module exports nothing, so any route that
/// got as far as running it would fail with a 500 rather than a 404.
fn plugin(id: &str, plugin_type: &str) -> LoadedPlugin {
    let manifest: PluginManifest = serde_json::from_value(json!({
        "id": id, "name": id, "version": "1.0.0", "api_version": "2",
        "plugin_type": plugin_type, "namespace": id, "capabilities": [],
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
            // `connect` refuses any other auth type before running the module.
            auth_type: "api_key".into(),
            config_schema: None,
        },
        source: PluginSource::File {
            path: format!("tests/fixtures/{id}").into(),
        },
        wasm_bytes: wat::parse_str(r#"(module (memory (export "memory") 1))"#).unwrap(),
        manifest: Some(manifest),
    }
}

async fn get_status(app: &TestApp, path: &str) -> (u16, Value) {
    let cookie = app.admin_cookie();
    let req = Request::builder()
        .uri(path)
        .header(cookie.0, cookie.1)
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
#[serial]
async fn only_auth_plugins_are_sign_in_providers() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let registry = &app.state.plugin_registry;
    registry.register(plugin("kinds_auth", "auth")).await;
    registry.register(plugin("kinds_library", "library")).await;
    registry
        .register(plugin("kinds_interpreter", "interpreter"))
        .await;
    // A plugin with no manifest is an auth plugin.
    let mut bare = plugin("kinds_bare", "auth");
    bare.manifest = None;
    registry.register(bare).await;

    let providers = app.get_json("/external-auth/providers").await;
    let ids: Vec<&str> = providers
        .as_array()
        .expect("a list of providers")
        .iter()
        .filter_map(|p| p["id"].as_str())
        .collect();
    assert!(ids.contains(&"kinds_auth"), "{ids:?}");
    assert!(ids.contains(&"kinds_bare"), "{ids:?}");
    assert!(!ids.contains(&"kinds_library"), "{ids:?}");
    assert!(!ids.contains(&"kinds_interpreter"), "{ids:?}");

    for id in ["kinds_library", "kinds_interpreter"] {
        let (status, body) = get_status(
            &app,
            &format!("/external-auth/{id}/authorize?redirect_uri=https://example.com/back"),
        )
        .await;
        assert_eq!(status, 404, "authorize {id}: {body}");

        let (status, body) = app
            .post_json_status(
                &format!("/external-auth/{id}/connect"),
                json!({ "config": {} }),
            )
            .await;
        assert_eq!(status, 404, "connect {id}: {body}");

        let (status, body) = app
            .post_json_status(&format!("/external-auth/{id}/refresh"), json!({}))
            .await;
        assert_eq!(status, 404, "refresh {id}: {body}");
        assert!(
            body.to_string().contains("Plugin not found"),
            "refresh {id}: {body}"
        );
    }
}

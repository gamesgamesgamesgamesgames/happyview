mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::app::TestApp;

async fn json_body(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
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

fn admin_post(
    uri: &str,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(cookie.0, cookie.1)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
#[serial]
async fn official_plugins_endpoint_returns_cached_list() {
    common::require_db!();
    let app = TestApp::new().await;

    let gh = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path(
            "/repos/gamesgamesgamesgamesgames/happyview-plugins/releases",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "tag_name": "steam-v1.2.0",
                "name": "steam-v1.2.0",
                "published_at": "2026-04-10T00:00:00Z",
                "body": "- logging improvements",
                "html_url": "https://example.com/steam-v1.2.0"
            }
        ])))
        .mount(&gh)
        .await;

    Mock::given(method("GET"))
        .and(path("/download/steam-v1.2.0/manifest.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "steam",
            "name": "Steam",
            "version": "1.2.0",
            "api_version": "2",
            "description": "Steam OAuth plugin",
            "icon_url": "https://example.com/steam.png",
            "wasm_file": "steam.wasm",
            "required_secrets": [],
            "auth_type": "openid"
        })))
        .mount(&gh)
        .await;

    let config = happyview::plugin::official_registry::RegistryConfig {
        api_base: gh.uri(),
        release_base: format!("{}/download", gh.uri()),
    };

    happyview::plugin::official_registry::refresh_full(
        &app.state.http,
        &config,
        &app.state.official_registry,
    )
    .await
    .unwrap();

    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/plugins/official", app.admin_cookie()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    let plugins = body["plugins"].as_array().unwrap();
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0]["id"], "steam");
    assert_eq!(plugins[0]["name"], "Steam");
    assert_eq!(plugins[0]["latest_version"], "1.2.0");
    assert!(body["last_refreshed_at"].is_string());
}

#[tokio::test]
#[serial]
async fn plugins_list_populates_update_available_when_behind() {
    common::require_db!();
    let app = TestApp::new().await;

    let gh = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path(
            "/repos/gamesgamesgamesgamesgames/happyview-plugins/releases",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "tag_name": "steam-v1.2.0",
                "name": "steam-v1.2.0",
                "published_at": "2026-04-10T00:00:00Z",
                "body": "- logging improvements",
                "html_url": "https://example.com/steam-v1.2.0"
            },
            {
                "tag_name": "steam-v1.1.0",
                "name": "steam-v1.1.0",
                "published_at": "2026-03-01T00:00:00Z",
                "body": "- initial",
                "html_url": "https://example.com/steam-v1.1.0"
            }
        ])))
        .mount(&gh)
        .await;

    Mock::given(method("GET"))
        .and(path("/download/steam-v1.2.0/manifest.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "steam",
            "name": "Steam",
            "version": "1.2.0",
            "api_version": "2",
            "wasm_file": "steam.wasm",
            "required_secrets": [],
            "auth_type": "openid"
        })))
        .mount(&gh)
        .await;

    app.install_fake_plugin("steam", "1.1.0").await;

    let config = happyview::plugin::official_registry::RegistryConfig {
        api_base: gh.uri(),
        release_base: format!("{}/download", gh.uri()),
    };
    happyview::plugin::official_registry::refresh_full(
        &app.state.http,
        &config,
        &app.state.official_registry,
    )
    .await
    .unwrap();

    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/plugins", app.admin_cookie()))
        .await
        .unwrap();

    let body = json_body(resp).await;
    let plugins = body["plugins"].as_array().unwrap();
    let steam = plugins.iter().find(|p| p["id"] == "steam").unwrap();
    assert_eq!(steam["update_available"], true);
    assert_eq!(steam["latest_version"], "1.2.0");
    let pending = steam["pending_releases"].as_array().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["version"], "1.2.0");
}

#[tokio::test]
#[serial]
async fn check_update_endpoint_refreshes_cache_on_demand() {
    common::require_db!();
    // Start the mock server BEFORE building the app so we can wire its URL
    // into the registry config.
    let gh = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path(
            "/repos/gamesgamesgamesgamesgames/happyview-plugins/releases",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "tag_name": "steam-v2.0.0",
                "name": "steam-v2.0.0",
                "published_at": "2026-04-12T00:00:00Z",
                "body": "- major rewrite",
                "html_url": "https://example.com/steam-v2.0.0"
            }
        ])))
        .mount(&gh)
        .await;

    Mock::given(method("GET"))
        .and(path("/download/steam-v2.0.0/manifest.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "steam",
            "name": "Steam",
            "version": "2.0.0",
            "api_version": "2",
            "description": "Steam OAuth plugin",
            "icon_url": null,
            "wasm_file": "steam.wasm",
            "required_secrets": [],
            "auth_type": "openid"
        })))
        .mount(&gh)
        .await;

    let config = happyview::plugin::official_registry::RegistryConfig {
        api_base: gh.uri(),
        release_base: format!("{}/download", gh.uri()),
    };
    let app = TestApp::new_with_registry_config(config).await;

    // Install a plugin at 1.0.0 — the cache starts empty, so /admin/plugins
    // should initially report no update available.
    app.install_fake_plugin("steam", "1.0.0").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/plugins", app.admin_cookie()))
        .await
        .unwrap();
    let body = json_body(resp).await;
    let steam_before = body["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "steam")
        .unwrap();
    assert_eq!(steam_before["update_available"], false);
    assert!(steam_before["latest_version"].is_null());

    // Force an on-demand refresh. The handler should call the mock GH API,
    // populate the cache, and return a summary with update fields filled in.
    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/plugins/steam/check-update",
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["id"], "steam");
    assert_eq!(body["update_available"], true);
    assert_eq!(body["latest_version"], "2.0.0");
    let pending = body["pending_releases"].as_array().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["version"], "2.0.0");

    // A follow-up /admin/plugins call should now also reflect the cache.
    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/plugins", app.admin_cookie()))
        .await
        .unwrap();
    let body = json_body(resp).await;
    let steam_after = body["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "steam")
        .unwrap();
    assert_eq!(steam_after["update_available"], true);
    assert_eq!(steam_after["latest_version"], "2.0.0");
}

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
        .and(path("/repos/happyproto/plugins/releases"))
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
        // The GitHub fallback, deterministically: these assert on its output.
        registry: None,
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
        .and(path("/repos/happyproto/plugins/releases"))
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
        // The GitHub fallback, deterministically: these assert on its output.
        registry: None,
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
        .and(path("/repos/happyproto/plugins/releases"))
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
        // The GitHub fallback, deterministically: these assert on its output.
        registry: None,
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

/// The whole point of the change: when the registry lists a package, that is
/// the catalogue, and the GitHub walk is not consulted at all. The GitHub mock
/// is mounted and asserted *unused*, so a regression that keeps preferring it
/// fails here rather than silently serving the old source.
#[tokio::test]
#[serial]
async fn the_registry_is_preferred_over_the_github_walk() {
    common::require_db!();
    let app = TestApp::new().await;

    let registry = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/xrpc/at.happyproto.registry.searchPackages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "packages": [{
                "uri": "at://did:plc:pub/at.happyproto.plugin.profile/happyview-http",
                "cid": "bafy",
                "did": "did:plc:pub",
                "record": { "name": "HTTP", "description": "Outbound HTTP." },
                "status": "active",
                "latestVersion": "1.0.0",
                "indexedAt": "2026-10-01T00:00:00.000000+00:00"
            }]
        })))
        .mount(&registry)
        .await;
    Mock::given(method("GET"))
        .and(path("/xrpc/at.happyproto.registry.getRelease"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "uri": "at://did:plc:pub/at.happyproto.plugin.release/happyview-http:1.0.0",
            "record": {
                "version": "1.0.0",
                "artifacts": {
                    "package": { "url": "https://example.com/d/happyview-http.wasm" }
                }
            },
            "indexedAt": "2026-10-01T00:00:00.000000+00:00"
        })))
        .mount(&registry)
        .await;

    let gh = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/happyproto/plugins/releases"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "tag_name": "steam-v9.9.9",
            "name": "steam-v9.9.9",
            "published_at": "2026-04-10T00:00:00Z",
            "body": "",
            "html_url": "https://example.com/steam"
        }])))
        .expect(0)
        .mount(&gh)
        .await;

    let config = happyview::plugin::official_registry::RegistryConfig {
        registry: Some(happyview::plugin::registry_source::RegistrySource {
            base_url: registry.uri(),
            limit: 100,
        }),
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
    assert_eq!(plugins[0]["id"], "happyview-http");
    assert_eq!(plugins[0]["name"], "HTTP");
    assert_eq!(plugins[0]["latest_version"], "1.0.0");
    // Install still goes through the module's own url, so the manifest is the
    // one beside it rather than anything the registry serves.
    assert_eq!(
        plugins[0]["manifest_url"],
        "https://example.com/d/manifest.json"
    );
}

/// An empty registry must not empty the catalogue. Replacing a working plugin
/// list with no plugin list is worse than the staleness the fallback carries,
/// so a registry with nothing to say falls through to GitHub.
#[tokio::test]
#[serial]
async fn an_empty_registry_falls_back_to_github() {
    common::require_db!();
    let app = TestApp::new().await;

    let registry = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/xrpc/at.happyproto.registry.searchPackages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "packages": [] })))
        .mount(&registry)
        .await;

    let gh = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/happyproto/plugins/releases"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "tag_name": "steam-v1.2.0",
            "name": "steam-v1.2.0",
            "published_at": "2026-04-10T00:00:00Z",
            "body": "",
            "html_url": "https://example.com/steam-v1.2.0"
        }])))
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

    let config = happyview::plugin::official_registry::RegistryConfig {
        registry: Some(happyview::plugin::registry_source::RegistrySource {
            base_url: registry.uri(),
            limit: 100,
        }),
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
    let body = json_body(resp).await;
    let plugins = body["plugins"].as_array().unwrap();
    assert_eq!(plugins.len(), 1, "the fallback should have answered");
    assert_eq!(plugins[0]["id"], "steam");
}

/// A registry that is down is the same case as an empty one: the catalogue it
/// could not serve must not become an empty catalogue.
#[tokio::test]
#[serial]
async fn an_unreachable_registry_falls_back_to_github() {
    common::require_db!();
    let app = TestApp::new().await;

    let registry = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/xrpc/at.happyproto.registry.searchPackages"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&registry)
        .await;

    let gh = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/happyproto/plugins/releases"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "tag_name": "steam-v1.2.0",
            "name": "steam-v1.2.0",
            "published_at": "2026-04-10T00:00:00Z",
            "body": "",
            "html_url": "https://example.com/steam-v1.2.0"
        }])))
        .mount(&gh)
        .await;
    Mock::given(method("GET"))
        .and(path("/download/steam-v1.2.0/manifest.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&gh)
        .await;

    let config = happyview::plugin::official_registry::RegistryConfig {
        registry: Some(happyview::plugin::registry_source::RegistrySource {
            base_url: registry.uri(),
            limit: 100,
        }),
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
    let body = json_body(resp).await;
    assert_eq!(body["plugins"].as_array().unwrap().len(), 1);
}

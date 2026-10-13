mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use wiremock::matchers::{method, path, query_param};
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
        disabled: false,
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
    assert_eq!(
        plugins[0]["official"], true,
        "the GitHub walk reads only the official repo"
    );
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
        disabled: false,
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
        disabled: false,
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

/// Checking one plugin for an update reads the registry, as the full refresh
/// does. The GitHub mock is asserted unused, so a check that still walked
/// only GitHub would fail here rather than drop or misreport the plugin.
#[tokio::test]
#[serial]
async fn check_update_reads_the_registry() {
    common::require_db!();
    let registry = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/xrpc/at.happyproto.registry.getPackage"))
        .and(query_param("slug", "happyview-http"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "uri": "at://did:plc:pub/at.happyproto.plugin.profile/happyview-http",
            "cid": "bafy",
            "did": "did:plc:pub",
            "record": { "name": "HTTP", "description": "Outbound HTTP." },
            "status": "active",
            "latestVersion": "1.0.0",
            "indexedAt": "2026-10-01T00:00:00.000000+00:00"
        })))
        .mount(&registry)
        .await;
    Mock::given(method("GET"))
        .and(path("/xrpc/at.happyproto.registry.listReleases"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "releases": [
                {
                    "uri": "at://did:plc:pub/at.happyproto.plugin.release/happyview-http:1.0.0",
                    "record": {
                        "version": "1.0.0",
                        "notes": "- faster\n- fewer bugs",
                        "artifacts": {
                            "package": { "url": "https://example.com/d/happyview-http.wasm" }
                        }
                    },
                    "indexedAt": "2026-10-01T00:00:00.000000+00:00"
                },
                {
                    "uri": "at://did:plc:pub/at.happyproto.plugin.release/happyview-http:0.9.0",
                    "record": {
                        "version": "0.9.0",
                        "artifacts": {
                            "package": { "url": "https://example.com/old/happyview-http.wasm" }
                        }
                    },
                    "indexedAt": "2026-09-01T00:00:00.000000+00:00"
                }
            ]
        })))
        .mount(&registry)
        .await;

    let gh = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/happyproto/plugins/releases"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
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
        disabled: false,
    };
    let app = TestApp::new_with_registry_config(config).await;
    app.install_fake_plugin("happyview-http", "0.9.0").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/plugins/happyview-http/check-update",
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["id"], "happyview-http");
    assert_eq!(body["update_available"], true);
    assert_eq!(body["latest_version"], "1.0.0");
    assert_eq!(
        body["latest_manifest_url"], "https://example.com/d/manifest.json",
        "the update installs the entry the gate approved"
    );
    let pending = body["pending_releases"].as_array().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["version"], "1.0.0");
}

/// When the registry lists a package, that is its catalogue entry. The GitHub
/// walk is still read, since it is official by construction, but here it lists
/// nothing, so a regression that served the walk instead of the registry fails
/// here rather than silently serving the old source.
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
        .and(path("/xrpc/at.happyproto.registry.listReleases"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "releases": [
                {
                    "uri": "at://did:plc:pub/at.happyproto.plugin.release/happyview-http:1.0.0",
                    "record": {
                        "version": "1.0.0",
                        "notes": "- faster\n- fewer bugs",
                        "artifacts": {
                            "package": { "url": "https://example.com/d/happyview-http.wasm" }
                        }
                    },
                    "indexedAt": "2026-10-01T00:00:00.000000+00:00"
                },
                {
                    "uri": "at://did:plc:pub/at.happyproto.plugin.release/happyview-http:0.9.0",
                    "record": {
                        "version": "0.9.0",
                        "artifacts": {
                            "package": { "url": "https://example.com/old/happyview-http.wasm" }
                        }
                    },
                    "indexedAt": "2026-09-01T00:00:00.000000+00:00"
                }
            ]
        })))
        .mount(&registry)
        .await;

    let gh = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/happyproto/plugins/releases"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&gh)
        .await;

    let config = happyview::plugin::official_registry::RegistryConfig {
        registry: Some(happyview::plugin::registry_source::RegistrySource {
            base_url: registry.uri(),
            limit: 100,
        }),
        api_base: gh.uri(),
        release_base: format!("{}/download", gh.uri()),
        disabled: false,
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
    assert_eq!(
        plugins[0]["official"], false,
        "a package from any publisher but HappyProto's is listed unbadged"
    );
    // Install still goes through the module's own url, so the manifest is the
    // one beside it rather than anything the registry serves.
    assert_eq!(
        plugins[0]["manifest_url"],
        "https://example.com/d/manifest.json"
    );
    // Every listed release, so the update preview can name each version
    // between what is installed and what is current -- not just the newest.
    // Read off the cache rather than the response: the browse endpoint serves
    // a summary, and the releases are what `compute_update_info` consumes.
    let cached = app.state.official_registry.read().await;
    let versions: Vec<&str> = cached.plugins["happyview-http"]
        .releases
        .iter()
        .map(|r| r.version.as_str())
        .collect();
    assert_eq!(versions, vec!["1.0.0", "0.9.0"]);
    // The release's own notes are what the update dialog renders, so they have
    // to survive the catalogue rather than being dropped on the way through.
    assert_eq!(
        cached.plugins["happyview-http"].releases[0].body,
        "- faster\n- fewer bugs"
    );
    assert_eq!(
        cached.plugins["happyview-http"].releases[1].body, "",
        "a release that published no notes carries none"
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
        disabled: false,
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
        disabled: false,
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

/// A registry where a stranger has published `happyview-http` and the
/// official publisher nothing, plus a GitHub walk listing `tags`.
async fn a_stranger_publishes_an_official_slug(tags: &[&str]) -> (MockServer, MockServer) {
    let registry = MockServer::start().await;
    let stranger = json!({
        "uri": "at://did:plc:someoneelse/at.happyproto.plugin.profile/happyview-http",
        "did": "did:plc:someoneelse",
        "record": { "name": "Not HTTP" },
        "status": "active",
        "latestVersion": "9.0.0"
    });
    Mock::given(method("GET"))
        .and(path("/xrpc/at.happyproto.registry.searchPackages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "packages": [stranger] })))
        .mount(&registry)
        .await;
    Mock::given(method("GET"))
        .and(path("/xrpc/at.happyproto.registry.getPackage"))
        .and(query_param("actor", "did:plc:someoneelse"))
        .respond_with(ResponseTemplate::new(200).set_body_json(stranger))
        .mount(&registry)
        .await;
    Mock::given(method("GET"))
        .and(path("/xrpc/at.happyproto.registry.getPackage"))
        .and(query_param(
            "actor",
            happyview::plugin::official_registry::OFFICIAL_PUBLISHER_DID,
        ))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "PackageNotFound", "message": "no such package"
        })))
        .mount(&registry)
        .await;
    Mock::given(method("GET"))
        .and(path("/xrpc/at.happyproto.registry.listReleases"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "releases": [{
                "status": "active",
                "record": {
                    "version": "9.0.0",
                    "artifacts": {
                        "package": { "url": "https://elsewhere.example/happyview-http.wasm" }
                    }
                }
            }]
        })))
        .mount(&registry)
        .await;

    let gh = MockServer::start().await;
    let releases: Vec<Value> = tags
        .iter()
        .map(|tag| {
            json!({
                "tag_name": tag,
                "name": tag,
                "published_at": "2026-04-10T00:00:00Z",
                "body": "",
                "html_url": "https://example.com/release"
            })
        })
        .collect();
    Mock::given(method("GET"))
        .and(path("/repos/happyproto/plugins/releases"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Value::Array(releases)))
        .mount(&gh)
        .await;
    (registry, gh)
}

fn config_for(
    registry: &MockServer,
    gh: &MockServer,
) -> happyview::plugin::official_registry::RegistryConfig {
    happyview::plugin::official_registry::RegistryConfig {
        registry: Some(happyview::plugin::registry_source::RegistrySource {
            base_url: registry.uri(),
            limit: 100,
        }),
        api_base: gh.uri(),
        release_base: format!("{}/download", gh.uri()),
        disabled: false,
    }
}

async fn http_summary(app: &TestApp) -> Value {
    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/plugins", app.admin_cookie()))
        .await
        .unwrap();
    json_body(resp)
        .await
        .get("plugins")
        .and_then(|p| p.as_array())
        .and_then(|p| p.iter().find(|p| p["id"] == "happyview-http").cloned())
        .expect("the installed plugin should be listed")
}

/// A stranger publishing an official slug to the registry, while the official
/// publisher has published nothing there, must not become the update offered
/// to the installed official plugin: the offer comes from the official walk.
#[tokio::test]
#[serial]
async fn a_strangers_package_is_never_the_update_for_an_official_install() {
    common::require_db!();

    // The walk still lists the official plugin: it is the offer.
    let (registry, gh) = a_stranger_publishes_an_official_slug(&["happyview-http-v1.1.0"]).await;
    let config = config_for(&registry, &gh);
    let app = TestApp::new_with_registry_config(config.clone()).await;
    app.install_fake_plugin_from(
        "happyview-http",
        "1.0.0",
        &format!(
            "{}/happyview-http-v1.0.0/happyview-http.wasm",
            config.release_base
        ),
    )
    .await;
    happyview::plugin::official_registry::refresh_full(
        &app.state.http,
        &config,
        &app.state.official_registry,
    )
    .await
    .unwrap();
    let summary = http_summary(&app).await;
    assert_eq!(summary["update_available"], true);
    assert_eq!(summary["latest_version"], "1.1.0");
    assert_eq!(
        summary["latest_manifest_url"],
        format!(
            "{}/happyview-http-v1.1.0/manifest.json",
            config.release_base
        ),
        "the update installs the official entry, not the stranger's"
    );
}

/// The same, with the walk listing nothing: no offer at all, rather than the
/// stranger's.
#[tokio::test]
#[serial]
async fn with_no_official_entry_an_official_install_is_offered_nothing() {
    common::require_db!();
    let (registry, gh) = a_stranger_publishes_an_official_slug(&[]).await;
    let config = config_for(&registry, &gh);
    let app = TestApp::new_with_registry_config(config.clone()).await;
    app.install_fake_plugin_from(
        "happyview-http",
        "1.0.0",
        &format!(
            "{}/happyview-http-v1.0.0/happyview-http.wasm",
            config.release_base
        ),
    )
    .await;
    happyview::plugin::official_registry::refresh_full(
        &app.state.http,
        &config,
        &app.state.official_registry,
    )
    .await
    .unwrap();
    let summary = http_summary(&app).await;
    assert_eq!(summary["update_available"], false);
    assert!(summary["latest_version"].is_null());
    assert!(summary["latest_manifest_url"].is_null());
}

/// The same, through the on-demand check.
#[tokio::test]
#[serial]
async fn check_update_never_offers_a_strangers_package_to_an_official_install() {
    common::require_db!();
    let (registry, gh) = a_stranger_publishes_an_official_slug(&[]).await;
    let config = config_for(&registry, &gh);
    let app = TestApp::new_with_registry_config(config.clone()).await;
    app.install_fake_plugin_from(
        "happyview-http",
        "1.0.0",
        &format!(
            "{}/happyview-http-v1.0.0/happyview-http.wasm",
            config.release_base
        ),
    )
    .await;
    // The cache names the stranger for this id, so the check reaches them.
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
        .oneshot(admin_post(
            "/admin/plugins/happyview-http/check-update",
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["update_available"], false);
    assert!(body["latest_version"].is_null());
    assert!(body["latest_manifest_url"].is_null());
}

// ---------------------------------------------------------------------------
// An install records the publisher of the catalogue entry it came from, and
// is offered updates only from that publisher.
// ---------------------------------------------------------------------------

const STRANGER: &str = "did:plc:someoneelse";

/// Serve a library plugin's manifest and module under `/<dir>/` on the app's
/// mock server, and answer the manifest URL.
async fn serve_module(app: &TestApp, dir: &str, id: &str, version: &str) -> String {
    let manifest = json!({
        "id": id, "name": id, "version": version, "api_version": "2",
        "plugin_type": "library", "capabilities": [], "wasm_file": "plugin.wasm",
    });
    Mock::given(method("GET"))
        .and(path(format!("/{dir}/manifest.json")))
        .respond_with(ResponseTemplate::new(200).set_body_json(manifest))
        .mount(&app.mock_server)
        .await;
    let wasm = wat::parse_str(
        "(module (memory (export \"memory\") 1) \
         (func (export \"alloc\") (param i32) (result i32) i32.const 8) \
         (func (export \"dealloc\") (param i32 i32)))",
    )
    .unwrap();
    Mock::given(method("GET"))
        .and(path(format!("/{dir}/plugin.wasm")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(wasm, "application/wasm"))
        .mount(&app.mock_server)
        .await;
    format!("{}/{dir}/manifest.json", app.mock_server.uri())
}

/// A catalogue entry: a GitHub-walk one when `publisher` is `None`, else a
/// registry one by that publisher.
fn catalogue_entry(
    id: &str,
    publisher: Option<&str>,
    manifest_url: &str,
    versions: &[&str],
) -> happyview::plugin::official_registry::OfficialPlugin {
    use happyview::plugin::official_registry::{OFFICIAL_PUBLISHER_DID, ReleaseEntry};
    happyview::plugin::official_registry::OfficialPlugin {
        id: id.into(),
        publisher_did: publisher.map(str::to_owned),
        official: publisher.is_none_or(|p| p == OFFICIAL_PUBLISHER_DID),
        name: id.into(),
        description: None,
        icon_url: None,
        latest_version: versions[0].into(),
        manifest_url: manifest_url.into(),
        wasm_url: manifest_url.replace("manifest.json", "plugin.wasm"),
        releases: versions
            .iter()
            .map(|v| ReleaseEntry {
                version: (*v).into(),
                name: format!("{id}-v{v}"),
                published_at: "2026-10-01T00:00:00Z".into(),
                body: String::new(),
            })
            .collect(),
    }
}

async fn put_in_catalogue(
    app: &TestApp,
    entry: happyview::plugin::official_registry::OfficialPlugin,
) {
    app.state
        .official_registry
        .write()
        .await
        .plugins
        .insert(entry.id.clone(), entry);
}

async fn recorded_publisher(app: &TestApp, id: &str) -> Option<String> {
    let sql = happyview::db::adapt_sql(
        "SELECT publisher_did FROM happyview_plugins WHERE id = ?",
        app.state.db_backend,
    );
    let (publisher,): (Option<String>,) = happyview::db::query_as(&sql)
        .bind(id)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    publisher
}

async fn install(app: &TestApp, url: &str) {
    let (status, body) = app
        .post_json_status("/admin/plugins", json!({ "url": url }))
        .await;
    assert_eq!(status, 200, "{body}");
}

async fn summary_of(app: &TestApp, id: &str) -> Value {
    app.get_json("/admin/plugins")
        .await
        .get("plugins")
        .and_then(|p| p.as_array())
        .and_then(|p| p.iter().find(|p| p["id"] == id).cloned())
        .expect("the installed plugin should be listed")
}

/// The server derives the publisher from the catalogue entry whose manifest
/// URL was installed: a registry entry's account, the official DID for a
/// GitHub-walk entry, and nothing for a URL the catalogue does not offer. A
/// reload to a new URL derives it again.
#[tokio::test]
#[serial]
async fn an_install_records_the_publisher_of_its_catalogue_entry() {
    common::require_db!();
    let app = TestApp::new_with_persisted_plugins(
        happyview::plugin::official_registry::RegistryConfig::production(),
    )
    .await;
    let official = happyview::plugin::official_registry::OFFICIAL_PUBLISHER_DID;

    let extra = serve_module(&app, "extra", "extra", "1.0.0").await;
    put_in_catalogue(
        &app,
        catalogue_entry("extra", Some(STRANGER), &extra, &["1.0.0"]),
    )
    .await;
    install(&app, &extra).await;
    assert_eq!(
        recorded_publisher(&app, "extra").await.as_deref(),
        Some(STRANGER)
    );

    let walked = serve_module(&app, "walked", "walked", "1.0.0").await;
    put_in_catalogue(&app, catalogue_entry("walked", None, &walked, &["1.0.0"])).await;
    install(&app, &walked).await;
    assert_eq!(
        recorded_publisher(&app, "walked").await.as_deref(),
        Some(official)
    );

    let typed = serve_module(&app, "typed", "typed", "1.0.0").await;
    install(&app, &typed).await;
    assert!(recorded_publisher(&app, "typed").await.is_none());

    let typed_official = serve_module(&app, "typed-v2", "typed", "1.1.0").await;
    put_in_catalogue(
        &app,
        catalogue_entry("typed", None, &typed_official, &["1.1.0"]),
    )
    .await;
    let (status, body) = app
        .post_json_status(
            "/admin/plugins/typed/reload",
            json!({ "url": typed_official }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        recorded_publisher(&app, "typed").await.as_deref(),
        Some(official)
    );
}

/// A third party's install of a slug the official publisher also uses is never
/// offered the official release.
#[tokio::test]
#[serial]
async fn a_third_party_install_is_never_offered_the_official_entry() {
    common::require_db!();
    let app = TestApp::new_with_persisted_plugins(
        happyview::plugin::official_registry::RegistryConfig::production(),
    )
    .await;
    let theirs = serve_module(&app, "their-lua", "lua", "1.0.0").await;
    put_in_catalogue(
        &app,
        catalogue_entry("lua", Some(STRANGER), &theirs, &["1.0.0"]),
    )
    .await;
    install(&app, &theirs).await;

    put_in_catalogue(
        &app,
        catalogue_entry(
            "lua",
            None,
            "https://elsewhere.example/lua/manifest.json",
            &["2.0.0"],
        ),
    )
    .await;
    let summary = summary_of(&app, "lua").await;
    assert_eq!(summary["update_available"], false);
    assert!(summary["latest_version"].is_null());
    assert!(summary["latest_manifest_url"].is_null());
}

/// Install the official `lua` from a GitHub-walk entry, at a URL the fallback
/// URL rule would not call official, so only the recorded publisher can keep
/// a stranger's release away from it, with stranger packages naming the same
/// artifact under other ids. Answers the app.
async fn an_official_lua_install(
    config: happyview::plugin::official_registry::RegistryConfig,
) -> TestApp {
    let app = TestApp::new_with_persisted_plugins(config).await;
    let ours = serve_module(&app, "our-lua", "lua", "1.0.0").await;
    put_in_catalogue(&app, catalogue_entry("lua", None, &ours, &["1.0.0"])).await;
    // Packages naming the official artifact under other ids: the install
    // still records `lua`'s own publisher.
    for n in 0..16 {
        put_in_catalogue(
            &app,
            catalogue_entry(&format!("notlua{n}"), Some(STRANGER), &ours, &["1.0.0"]),
        )
        .await;
    }
    install(&app, &ours).await;
    assert_eq!(
        recorded_publisher(&app, "lua").await.as_deref(),
        Some(happyview::plugin::official_registry::OFFICIAL_PUBLISHER_DID)
    );
    app
}

/// A registry with the stranger's `lua`, and the official one listed too
/// when `official_listed`, whose releases answer `official_releases`.
async fn registry_with_a_strangers_lua(
    official_listed: bool,
    official_releases: u16,
) -> MockServer {
    let official = happyview::plugin::official_registry::OFFICIAL_PUBLISHER_DID;
    let registry = MockServer::start().await;
    let package = |did: &str, latest: &str| {
        json!({
            "uri": format!("at://{did}/at.happyproto.plugin.profile/lua"),
            "did": did,
            "status": "active",
            "latestVersion": latest,
            "record": { "name": "lua" }
        })
    };
    let mut packages = vec![package(STRANGER, "9.0.0")];
    if official_listed {
        packages.push(package(official, "1.1.0"));
    }
    Mock::given(method("GET"))
        .and(path("/xrpc/at.happyproto.registry.searchPackages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "packages": packages })))
        .mount(&registry)
        .await;
    Mock::given(method("GET"))
        .and(path("/xrpc/at.happyproto.registry.listReleases"))
        .and(query_param("did", STRANGER))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "releases": [{
                "status": "active",
                "record": {
                    "version": "9.0.0",
                    "artifacts": { "package": { "url": "https://elsewhere.example/lua/plugin.wasm" } }
                }
            }]
        })))
        .mount(&registry)
        .await;
    Mock::given(method("GET"))
        .and(path("/xrpc/at.happyproto.registry.listReleases"))
        .and(query_param("did", official))
        .respond_with(ResponseTemplate::new(official_releases))
        .mount(&registry)
        .await;
    Mock::given(method("GET"))
        .and(path("/xrpc/at.happyproto.registry.getPackage"))
        .and(query_param("actor", STRANGER))
        .respond_with(ResponseTemplate::new(200).set_body_json(package(STRANGER, "9.0.0")))
        .mount(&registry)
        .await;
    Mock::given(method("GET"))
        .and(path("/xrpc/at.happyproto.registry.getPackage"))
        .and(query_param("actor", official))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "PackageNotFound", "message": "no such package"
        })))
        .mount(&registry)
        .await;
    registry
}

/// Refresh the catalogue from the app's own sources, then check the cache holds only the stranger's
/// `lua` and that the official install is offered nothing from it.
async fn github_walk_answering(status: u16) -> MockServer {
    let gh = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/happyproto/plugins/releases"))
        .respond_with(ResponseTemplate::new(status).set_body_json(json!([])))
        .mount(&gh)
        .await;
    gh
}

async fn refresh_and_expect_no_offer(app: &TestApp) {
    happyview::plugin::official_registry::refresh_full(
        &app.state.http,
        &app.state.official_registry_config,
        &app.state.official_registry,
    )
    .await
    .unwrap();
    assert_eq!(
        app.state.official_registry.read().await.plugins["lua"]
            .publisher_did
            .as_deref(),
        Some(STRANGER),
        "the scenario should leave only the stranger's entry"
    );

    let summary = summary_of(app, "lua").await;
    assert_eq!(summary["update_available"], false);
    assert!(summary["latest_version"].is_null());
    assert!(summary["latest_manifest_url"].is_null());
}

/// The official package sits past the registry's first page, so the listing
/// shows only the stranger's.
#[tokio::test]
#[serial]
async fn a_strangers_lua_is_not_offered_when_the_official_one_is_past_the_page() {
    common::require_db!();
    let registry = registry_with_a_strangers_lua(false, 200).await;
    let gh = github_walk_answering(200).await;
    let app = an_official_lua_install(config_for(&registry, &gh)).await;
    refresh_and_expect_no_offer(&app).await;
}

/// The official package is listed but its releases cannot be read, so it
/// yields no entry.
#[tokio::test]
#[serial]
async fn a_strangers_lua_is_not_offered_when_the_official_entry_fails_to_build() {
    common::require_db!();
    let registry = registry_with_a_strangers_lua(true, 503).await;
    let gh = github_walk_answering(200).await;
    let app = an_official_lua_install(config_for(&registry, &gh)).await;
    refresh_and_expect_no_offer(&app).await;
}

/// A restart: the cache starts empty and the GitHub walk is unreachable, so
/// nothing official is carried over. The on-demand check, which then reaches
/// the stranger through the cache, offers nothing either.
#[tokio::test]
#[serial]
async fn a_strangers_lua_is_not_offered_on_a_cold_start() {
    common::require_db!();
    let registry = registry_with_a_strangers_lua(false, 200).await;
    let gh = github_walk_answering(503).await;
    let app = an_official_lua_install(config_for(&registry, &gh)).await;
    app.state.official_registry.write().await.plugins.clear();
    refresh_and_expect_no_offer(&app).await;

    let (status, body) = app
        .post_json_status("/admin/plugins/lua/check-update", json!({}))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["update_available"], false);
    assert!(body["latest_version"].is_null());
    assert!(body["latest_manifest_url"].is_null());
}

/// A recorded install is never re-bound to another publisher by a reload:
/// a URL the catalogue offers under this id from someone else is refused
/// with 409 and the install is left as it was, while the recorded
/// publisher's own new release reloads.
#[tokio::test]
#[serial]
async fn reload_refuses_a_url_from_another_publisher() {
    common::require_db!();
    let app =
        an_official_lua_install(happyview::plugin::official_registry::RegistryConfig::production())
            .await;

    let theirs = serve_module(&app, "their-lua-v2", "lua", "2.0.0").await;
    put_in_catalogue(
        &app,
        catalogue_entry("lua", Some(STRANGER), &theirs, &["2.0.0"]),
    )
    .await;
    let (status, body) = app
        .post_json_status("/admin/plugins/lua/reload", json!({ "url": theirs }))
        .await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(
        recorded_publisher(&app, "lua").await.as_deref(),
        Some(happyview::plugin::official_registry::OFFICIAL_PUBLISHER_DID)
    );
    assert_eq!(summary_of(&app, "lua").await["version"], "1.0.0");

    // A hand-typed URL is no publisher's offer either.
    let typed = serve_module(&app, "typed-lua", "lua", "2.0.0").await;
    let (status, _) = app
        .post_json_status("/admin/plugins/lua/reload", json!({ "url": typed }))
        .await;
    assert_eq!(status, 409);

    let ours = serve_module(&app, "our-lua-v2", "lua", "2.0.0").await;
    put_in_catalogue(&app, catalogue_entry("lua", None, &ours, &["2.0.0"])).await;
    let (status, body) = app
        .post_json_status("/admin/plugins/lua/reload", json!({ "url": ours }))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(summary_of(&app, "lua").await["version"], "2.0.0");
    assert_eq!(
        recorded_publisher(&app, "lua").await.as_deref(),
        Some(happyview::plugin::official_registry::OFFICIAL_PUBLISHER_DID)
    );
}

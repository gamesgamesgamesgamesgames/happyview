mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
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
        .method(Method::PUT)
        .uri(uri)
        .header(cookie.0, cookie.1)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

fn admin_delete(
    uri: &str,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
) -> Request<Body> {
    Request::builder()
        .method(Method::DELETE)
        .uri(uri)
        .header(cookie.0, cookie.1)
        .body(Body::empty())
        .unwrap()
}

fn admin_post(
    uri: &str,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
    body: &Value,
) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(cookie.0, cookie.1)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

fn get_with_host(uri: &str, host: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("host", host)
        .body(Body::empty())
        .unwrap()
}

async fn seed_domain(app: &TestApp, id: &str, url: &str, is_primary: bool) {
    let now = happyview::db::now_rfc3339();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_domains (id, url, is_primary, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(id)
        .bind(url)
        .bind(if is_primary { 1i32 } else { 0i32 })
        .bind(&now)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .unwrap();
    app.state
        .domain_cache
        .insert(happyview::domain::Domain {
            id: id.into(),
            url: url.into(),
            is_primary,
            created_at: now.clone(),
            updated_at: now,
        })
        .await;
}

// ---------------------------------------------------------------------------
// Settings tests
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn settings_crud() {
    common::require_db!();
    let app = TestApp::new().await;

    // PUT a setting
    let resp = app
        .router
        .clone()
        .oneshot(admin_put(
            "/admin/settings/app_name",
            app.admin_cookie(),
            &json!({ "value": "Test App" }),
        ))
        .await
        .unwrap();

    assert!(
        resp.status().is_success(),
        "expected success on PUT, got {}",
        resp.status()
    );

    // GET all settings and verify the entry appears with source: "database"
    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/settings", app.admin_cookie()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = json_body(resp).await;
    let settings = json.as_array().unwrap();
    let app_name_entry = settings
        .iter()
        .find(|s| s["key"] == "app_name")
        .expect("app_name entry not found in settings");
    assert_eq!(app_name_entry["source"], "database");

    // DELETE the setting
    let resp = app
        .router
        .clone()
        .oneshot(admin_delete("/admin/settings/app_name", app.admin_cookie()))
        .await
        .unwrap();

    assert!(
        resp.status().is_success(),
        "expected success on DELETE, got {}",
        resp.status()
    );

    // GET again and verify it's removed
    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/settings", app.admin_cookie()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = json_body(resp).await;
    let settings = json.as_array().unwrap();
    let app_name_entry = settings.iter().find(|s| s["key"] == "app_name");
    assert!(
        app_name_entry.is_none(),
        "app_name entry should have been deleted"
    );
}

#[tokio::test]
#[serial]
async fn settings_requires_auth() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[serial]
async fn logo_upload_and_serve() {
    common::require_db!();
    let app = TestApp::new().await;

    let boundary = "----testboundary";
    // Minimal valid 1x1 PNG
    let png_bytes: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1
        0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xDE, // 8-bit RGB
        0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, // IDAT chunk
        0x08, 0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0xE2, 0x21, 0xBC,
        0x33, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, // IEND chunk
        0xAE, 0x42, 0x60, 0x82,
    ];
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"logo.png\"\r\nContent-Type: image/png\r\n\r\n"
    );
    let mut body_bytes = body.into_bytes();
    body_bytes.extend_from_slice(&png_bytes);
    body_bytes.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let cookie = app.admin_cookie();
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri("/admin/settings/logo")
                .header(cookie.0, cookie.1)
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        resp.status().is_success(),
        "expected success on logo upload, got {}",
        resp.status()
    );

    // GET /settings/logo (public route) and verify 200 with content-type: image/png
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/settings/logo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let content_type = resp
        .headers()
        .get("content-type")
        .expect("expected content-type header")
        .to_str()
        .unwrap();
    assert!(
        content_type.contains("image/png"),
        "expected image/png content-type, got {content_type}"
    );

    // DELETE /admin/settings/logo
    let resp = app
        .router
        .clone()
        .oneshot(admin_delete("/admin/settings/logo", app.admin_cookie()))
        .await
        .unwrap();

    assert!(
        resp.status().is_success(),
        "expected success on DELETE logo, got {}",
        resp.status()
    );

    // GET /settings/logo should now return 404
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/settings/logo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[serial]
async fn client_metadata_includes_settings() {
    common::require_db!();
    let app = TestApp::new().await;

    // PUT app_name setting
    let resp = app
        .router
        .clone()
        .oneshot(admin_put(
            "/admin/settings/app_name",
            app.admin_cookie(),
            &json!({ "value": "Test App" }),
        ))
        .await
        .unwrap();

    assert!(
        resp.status().is_success(),
        "expected success on PUT app_name, got {}",
        resp.status()
    );

    // GET /oauth-client-metadata.json (no auth) and verify client_name
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/oauth-client-metadata.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = json_body(resp).await;
    assert_eq!(
        json["client_name"], "Test App",
        "expected client_name to be 'Test App', got {:?}",
        json["client_name"]
    );
}

#[tokio::test]
#[serial]
async fn client_metadata_client_id_matches_path() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/oauth-client-metadata.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = json_body(resp).await;
    let client_id = json["client_id"].as_str().expect("client_id missing");
    assert!(
        client_id.ends_with("/oauth-client-metadata.json"),
        "client_id should end with /oauth-client-metadata.json, got {client_id}"
    );
}

#[tokio::test]
#[serial]
async fn client_metadata_client_uri_overridden_by_setting() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_put(
            "/admin/settings/client_uri",
            app.admin_cookie(),
            &json!({ "value": "https://example.test" }),
        ))
        .await
        .unwrap();
    assert!(resp.status().is_success());

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/oauth-client-metadata.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = json_body(resp).await;
    assert_eq!(
        json["client_uri"], "https://example.test",
        "expected client_uri override, got {:?}",
        json["client_uri"]
    );
}

#[tokio::test]
#[serial]
async fn client_metadata_jwks_includes_current_and_retiring_instance_keys() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;

    let seed_key = happyview::oauth::client_keys::generate_client_key("test-owner")
        .expect("generate client key");
    let seed_jwk =
        happyview::oauth::client_keys::to_atrium_jwk(&seed_key).expect("convert to atrium jwk");
    app.state.client_jwks = vec![seed_jwk];
    app.rebuild_router();

    seed_domain(&app, "primary-id", "http://127.0.0.1:0", true).await;

    let domain_url = "https://confidential.task21-fix1-test.invalid";
    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/domains",
            app.admin_cookie(),
            &json!({ "url": domain_url }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "expected 201 creating confidential domain, got {}",
        resp.status()
    );

    let current = happyview::oauth::client_keys::generate_client_key(
        happyview::oauth::client_keys::INSTANCE_OWNER,
    )
    .expect("generate current instance key");
    happyview::oauth::client_keys::insert_key(
        &app.state.db,
        app.state.db_backend,
        app.state.config.token_encryption_key.as_ref(),
        &current,
    )
    .await
    .expect("insert current instance key");

    let mut retiring = happyview::oauth::client_keys::generate_client_key(
        happyview::oauth::client_keys::INSTANCE_OWNER,
    )
    .expect("generate retiring instance key");
    retiring.status = happyview::oauth::client_keys::KeyStatus::Retiring;
    happyview::oauth::client_keys::insert_key(
        &app.state.db,
        app.state.db_backend,
        app.state.config.token_encryption_key.as_ref(),
        &retiring,
    )
    .await
    .expect("insert retiring instance key");

    let resp = app
        .router
        .clone()
        .oneshot(get_with_host(
            "/oauth-client-metadata.json",
            "confidential.task21-fix1-test.invalid",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = json_body(resp).await;

    let kids: Vec<String> = json["jwks"]["keys"]
        .as_array()
        .expect("expected jwks.keys to be an array")
        .iter()
        .map(|k| {
            k["kid"]
                .as_str()
                .expect("published key missing kid")
                .to_string()
        })
        .collect();

    assert!(
        kids.contains(&current.kid),
        "expected jwks.keys to contain the current instance kid {}, got {:?}",
        current.kid,
        kids
    );
    assert!(
        kids.contains(&retiring.kid),
        "expected jwks.keys to contain the retiring instance kid {}, got {:?}",
        retiring.kid,
        kids
    );
}

// ---------------------------------------------------------------------------
// Instance key revoke (Task 22a)
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn revoking_a_retiring_instance_key_removes_it_from_published_jwks() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;

    let seed_key = happyview::oauth::client_keys::generate_client_key("test-owner")
        .expect("generate client key");
    let seed_jwk =
        happyview::oauth::client_keys::to_atrium_jwk(&seed_key).expect("convert to atrium jwk");
    app.state.client_jwks = vec![seed_jwk];
    app.rebuild_router();

    seed_domain(&app, "primary-id", "http://127.0.0.1:0", true).await;

    let domain_url = "https://confidential.task22a-revoke-test.invalid";
    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/domains",
            app.admin_cookie(),
            &json!({ "url": domain_url }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "expected 201 creating confidential domain, got {}",
        resp.status()
    );

    let current = happyview::oauth::client_keys::generate_client_key(
        happyview::oauth::client_keys::INSTANCE_OWNER,
    )
    .expect("generate current instance key");
    happyview::oauth::client_keys::insert_key(
        &app.state.db,
        app.state.db_backend,
        app.state.config.token_encryption_key.as_ref(),
        &current,
    )
    .await
    .expect("insert current instance key");

    let mut retiring = happyview::oauth::client_keys::generate_client_key(
        happyview::oauth::client_keys::INSTANCE_OWNER,
    )
    .expect("generate retiring instance key");
    retiring.status = happyview::oauth::client_keys::KeyStatus::Retiring;
    happyview::oauth::client_keys::insert_key(
        &app.state.db,
        app.state.db_backend,
        app.state.config.token_encryption_key.as_ref(),
        &retiring,
    )
    .await
    .expect("insert retiring instance key");

    // Revoke the retiring key through the real admin route.
    let resp = app
        .router
        .clone()
        .oneshot(admin_delete(
            &format!("/admin/oauth/instance-key/{}", retiring.kid),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 revoking a retiring key, got {}",
        resp.status()
    );
    let body = json_body(resp).await;
    assert_eq!(body["kid"], retiring.kid);
    assert_eq!(
        body["sessions_destroyed"], 0,
        "no sessions were pinned to the retiring key in this test"
    );

    let resp = app
        .router
        .clone()
        .oneshot(get_with_host(
            "/oauth-client-metadata.json",
            "confidential.task22a-revoke-test.invalid",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = json_body(resp).await;
    let kids: Vec<String> = json["jwks"]["keys"]
        .as_array()
        .expect("expected jwks.keys to be an array")
        .iter()
        .map(|k| {
            k["kid"]
                .as_str()
                .expect("published key missing kid")
                .to_string()
        })
        .collect();
    assert!(
        !kids.contains(&retiring.kid),
        "revoked key must disappear from the published JWKS, got {:?}",
        kids
    );
    assert!(
        kids.contains(&current.kid),
        "the current key must still be published, got {:?}",
        kids
    );

    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/oauth/instance-key", app.admin_cookie()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let list = json_body(resp).await;
    let keys = list["keys"].as_array().expect("expected keys array");
    let revoked_entry = keys
        .iter()
        .find(|k| k["kid"] == retiring.kid)
        .expect("revoked key must still appear in the listing");
    assert_eq!(revoked_entry["status"], "revoked");
}

#[tokio::test]
#[serial]
async fn revoking_the_current_instance_key_is_refused_with_400() {
    common::require_db!();
    let app = TestApp::new().await;

    let current = happyview::oauth::client_keys::ensure_instance_key(
        &app.state.db,
        app.state.db_backend,
        app.state.config.token_encryption_key.as_ref(),
    )
    .await
    .expect("ensure instance key");

    let resp = app
        .router
        .clone()
        .oneshot(admin_delete(
            &format!("/admin/oauth/instance-key/{}", current.kid),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "expected 400 refusing to revoke the current key, got {}",
        resp.status()
    );

    let keys = happyview::oauth::client_keys::list_keys_for_owner(
        &app.state.db,
        app.state.db_backend,
        happyview::oauth::client_keys::INSTANCE_OWNER,
    )
    .await
    .expect("list instance keys");
    let row = keys
        .iter()
        .find(|k| k.kid == current.kid)
        .expect("current key must still exist");
    assert_eq!(
        row.status,
        happyview::oauth::client_keys::KeyStatus::Current,
        "a refused revoke must leave the key's status untouched"
    );
}

#[tokio::test]
#[serial]
async fn revoking_an_unknown_instance_key_404s() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_delete(
            "/admin/oauth/instance-key/not-a-real-kid",
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "expected 404 for an unknown kid, got {}",
        resp.status()
    );
}

#[tokio::test]
#[serial]
async fn instance_key_list_reports_per_kid_session_counts_including_zero() {
    common::require_db!();
    let app = TestApp::new().await;

    let current = happyview::oauth::client_keys::ensure_instance_key(
        &app.state.db,
        app.state.db_backend,
        app.state.config.token_encryption_key.as_ref(),
    )
    .await
    .expect("ensure instance key");

    let mut retiring = happyview::oauth::client_keys::generate_client_key(
        happyview::oauth::client_keys::INSTANCE_OWNER,
    )
    .expect("generate retiring instance key");
    retiring.status = happyview::oauth::client_keys::KeyStatus::Retiring;
    happyview::oauth::client_keys::insert_key(
        &app.state.db,
        app.state.db_backend,
        app.state.config.token_encryption_key.as_ref(),
        &retiring,
    )
    .await
    .expect("insert retiring instance key");

    // Two sessions pinned to `current`; none pinned to `retiring`.
    common::insert_oauth_session(
        &app.state.db,
        app.state.db_backend,
        "did:plc:ik-one",
        Some(&current.kid),
    )
    .await;
    common::insert_oauth_session(
        &app.state.db,
        app.state.db_backend,
        "did:plc:ik-two",
        Some(&current.kid),
    )
    .await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/oauth/instance-key", app.admin_cookie()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let list = json_body(resp).await;
    let keys = list["keys"].as_array().expect("expected keys array");

    let current_entry = keys
        .iter()
        .find(|k| k["kid"] == current.kid)
        .expect("current key must appear in the listing");
    assert_eq!(current_entry["status"], "current");
    assert_eq!(current_entry["session_count"], 2);

    let retiring_entry = keys
        .iter()
        .find(|k| k["kid"] == retiring.kid)
        .expect("retiring key must appear in the listing");
    assert_eq!(retiring_entry["status"], "retiring");
    assert_eq!(
        retiring_entry["session_count"], 0,
        "a kid with no sessions must report zero, not be omitted"
    );
}

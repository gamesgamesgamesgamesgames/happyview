mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use sha2::{Digest, Sha256};
use tower::ServiceExt;

use common::app::TestApp;

const PLATFORM_KEY: &str = "hv_0123456789abcdef0123456789abcdef";

fn platform_key() -> (String, String) {
    let hash = hex::encode(Sha256::digest(PLATFORM_KEY.as_bytes()));
    (PLATFORM_KEY.to_string(), hash)
}

async fn app_with_platform_key() -> TestApp {
    let mut app = TestApp::new().await;
    app.state.config.platform_api_key_hash = Some(platform_key().1);
    app.rebuild_router();
    app
}

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

fn bearer(method: Method, uri: &str, token: &str, body: Option<&Value>) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"));
    match body {
        Some(b) => builder
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(b).unwrap()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
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
}

#[tokio::test]
#[serial]
async fn platform_key_can_list_domains() {
    common::require_db!();
    let app = app_with_platform_key().await;
    seed_domain(&app, "d1", "http://127.0.0.1:0", true).await;

    let resp = app
        .router
        .clone()
        .oneshot(bearer(Method::GET, "/admin/domains", PLATFORM_KEY, None))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body.as_array().unwrap().len(), 1);
}

#[tokio::test]
#[serial]
async fn platform_key_is_refused_outside_its_permission_set() {
    common::require_db!();
    let app = app_with_platform_key().await;

    for uri in [
        "/admin/users",
        "/admin/api-keys",
        "/admin/scripts",
        "/admin/jobs",
    ] {
        let resp = app
            .router
            .clone()
            .oneshot(bearer(Method::GET, uri, PLATFORM_KEY, None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "GET {uri}");
    }
}

#[tokio::test]
#[serial]
async fn platform_key_cannot_transfer_super() {
    common::require_db!();
    let app = app_with_platform_key().await;

    let resp = app
        .router
        .clone()
        .oneshot(bearer(
            Method::POST,
            "/admin/users/transfer-super",
            PLATFORM_KEY,
            Some(&json!({ "target_user_id": "anything" })),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
#[serial]
async fn platform_key_is_rejected_when_feature_is_off() {
    common::require_db!();
    let app = TestApp::new().await; // no hash configured

    let resp = app
        .router
        .clone()
        .oneshot(bearer(Method::GET, "/admin/domains", PLATFORM_KEY, None))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[serial]
async fn unknown_key_is_still_rejected_when_feature_is_on() {
    common::require_db!();
    let app = app_with_platform_key().await;

    let resp = app
        .router
        .clone()
        .oneshot(bearer(
            Method::GET,
            "/admin/domains",
            "hv_ffffffffffffffffffffffffffffffff",
            None,
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[serial]
async fn regular_api_key_still_works_when_feature_is_on() {
    common::require_db!();
    let app = app_with_platform_key().await;

    // Create a key as the seeded super admin, with settings:manage.
    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .method(Method::POST)
                .uri("/admin/api-keys")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "name": "ci",
                        "permissions": ["settings:manage"],
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let key = json_body(resp).await["key"].as_str().unwrap().to_string();

    let resp = app
        .router
        .clone()
        .oneshot(bearer(Method::GET, "/admin/domains", &key, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
#[serial]
async fn config_reports_platform_managed() {
    common::require_db!();
    let off = TestApp::new().await;
    let resp = off
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/config")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(json_body(resp).await["platform_managed"], false);
    // Release the first app before starting another: both migrate under the
    // same advisory lock, so a live `off` would block `on` forever.
    drop(off);

    let on = app_with_platform_key().await;
    let resp = on
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/config")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(json_body(resp).await["platform_managed"], true);
}

#[tokio::test]
#[serial]
async fn platform_key_is_refused_on_auth_only_routes() {
    common::require_db!();
    let app = app_with_platform_key().await;

    let cases = [
        (Method::GET, "/api/setup/rotation-key", None),
        (Method::POST, "/api/setup/complete", Some(json!({}))),
        (
            Method::GET,
            "/admin/identity/resolve?identifier=alice.test",
            None,
        ),
    ];
    for (method, uri, body) in cases {
        let resp = app
            .router
            .clone()
            .oneshot(bearer(method.clone(), uri, PLATFORM_KEY, body.as_ref()))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{method} {uri}");
    }
}

#[tokio::test]
#[serial]
async fn platform_key_can_manage_domains() {
    common::require_db!();
    let app = app_with_platform_key().await;
    seed_domain(&app, "d1", "http://127.0.0.1:0", true).await;

    let resp = app
        .router
        .clone()
        .oneshot(bearer(
            Method::POST,
            "/admin/domains",
            PLATFORM_KEY,
            Some(&json!({ "url": "https://two.example.com" })),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let new_id = json_body(resp).await["id"].as_str().unwrap().to_string();

    let resp = app
        .router
        .clone()
        .oneshot(bearer(
            Method::POST,
            &format!("/admin/domains/{new_id}/primary"),
            PLATFORM_KEY,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .router
        .clone()
        .oneshot(bearer(
            Method::DELETE,
            "/admin/domains/d1",
            PLATFORM_KEY,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
#[serial]
async fn platform_key_can_read_stats() {
    common::require_db!();
    let app = app_with_platform_key().await;

    let resp = app
        .router
        .clone()
        .oneshot(bearer(Method::GET, "/admin/stats", PLATFORM_KEY, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

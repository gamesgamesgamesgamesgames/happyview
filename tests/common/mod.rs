#[allow(dead_code, unused_imports)]
pub mod app;
#[allow(dead_code, unused_imports)]
pub mod auth;
#[allow(dead_code, unused_imports)]
pub mod db;
#[allow(unused_imports)]
pub use db::insert_oauth_session;
#[allow(dead_code, unused_imports)]
pub mod fixtures;
#[allow(dead_code, unused_imports)]
pub mod plc;
#[allow(dead_code, unused_imports)]
pub mod syncer;
#[allow(dead_code, unused_imports)]
pub mod tls;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

/// Mint an API key for the test admin limited to `permissions`.
#[allow(dead_code)]
pub async fn api_key(app: &app::TestApp, permissions: &[&str]) -> String {
    let (name, value) = app.admin_cookie();
    let req = Request::builder()
        .method("POST")
        .uri("/admin/api-keys")
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "name": "moderator", "permissions": permissions }).to_string(),
        ))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    body["key"].as_str().unwrap().to_string()
}

#[allow(unused_macros)]
macro_rules! require_db {
    () => {
        if std::env::var("TEST_DATABASE_URL").is_err() {
            eprintln!("skipped (TEST_DATABASE_URL not set)");
            return;
        }
    };
}

#[allow(unused_imports)]
pub(crate) use require_db;

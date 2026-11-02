#[allow(dead_code, unused_imports)]
pub mod app;
#[allow(dead_code, unused_imports)]
pub mod auth;
#[allow(dead_code, unused_imports)]
pub mod burn;
#[allow(dead_code, unused_imports)]
pub mod db;
#[allow(dead_code, unused_imports)]
pub mod echo_interpreter;
#[allow(dead_code, unused_imports)]
pub mod fake_storage;
#[allow(unused_imports)]
pub use db::insert_oauth_session;
#[allow(dead_code, unused_imports)]
pub mod fixtures;
#[allow(dead_code, unused_imports)]
pub mod jetstream;
#[allow(dead_code, unused_imports)]
pub mod lua_plugin;
#[allow(dead_code, unused_imports)]
pub mod plc;
#[allow(dead_code, unused_imports)]
pub mod project;
#[allow(dead_code, unused_imports)]
pub mod quickjs_plugins;
#[allow(dead_code, unused_imports)]
pub mod segment_writer;
#[allow(dead_code, unused_imports)]
pub mod syncer;
#[allow(dead_code, unused_imports)]
pub mod tls;
#[allow(dead_code, unused_imports)]
pub mod watchdog;
#[allow(dead_code, unused_imports)]
pub mod workers;

/// Let the space inspector in this test process follow its setting, which a
/// serving build holds off until its dashboard ships. Process-wide and
/// one-way, so a test that needs it held off lives in a target that never
/// calls this.
#[allow(dead_code)]
pub fn let_space_inspector_follow_its_setting() {
    happyview::admin::enable_space_inspector_for_tests();
}

/// Mint an API key for the test admin limited to `permissions`.
#[allow(dead_code)]
pub async fn api_key(app: &app::TestApp, permissions: &[&str]) -> String {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let (name, value) = app.admin_cookie();
    let req = Request::builder()
        .method("POST")
        .uri("/admin/api-keys")
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "name": "moderator", "permissions": permissions }).to_string(),
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
            happyview::test_support::unavailable(happyview::test_support::NO_DATABASE);
            return;
        }
    };
}

#[allow(unused_imports)]
pub(crate) use require_db;

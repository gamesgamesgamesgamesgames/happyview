//! The script variable admin routes against a real database, on both sides of
//! an instance having `TOKEN_ENCRYPTION_KEY`.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::app::TestApp;
use happyview::db::{adapt_sql, query_as};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn save(app: &TestApp, key: &str, value: &str) {
    let (name, cookie) = app.admin_cookie();
    let response = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/script-variables")
                .header("content-type", "application/json")
                .header(name, cookie)
                .body(Body::from(
                    json!({ "key": key, "value": value }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

async fn list(app: &TestApp) -> Value {
    let (name, cookie) = app.admin_cookie();
    let response = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/script-variables")
                .header(name, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn stored(app: &TestApp, key: &str) -> (String, i64) {
    let sql = adapt_sql(
        "SELECT value, CAST(encrypted AS BIGINT) FROM happyview_script_variables WHERE key = ?",
        app.state.db_backend,
    );
    query_as(&sql)
        .bind(key)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_saved_value_is_stored_encrypted_and_listed_masked() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;

    save(&app, "API_TOKEN", "abcdefgh-secret").await;
    save(&app, "API_TOKEN", "ijklmnop-secret").await;

    let (value, encrypted) = stored(&app, "API_TOKEN").await;
    assert_eq!(encrypted, 1);
    assert!(!value.contains("secret"));
    let env = happyview::script::variables::load(
        &app.state.db,
        app.state.db_backend,
        app.state.config.token_encryption_key.as_ref(),
    )
    .await;
    assert_eq!(env["API_TOKEN"], "ijklmnop-secret");

    let listed = list(&app).await;
    assert_eq!(listed[0]["preview"], "****");
    assert_eq!(listed[0]["unreadable"], false);
}

/// Every value looks the same in the list, so a short one isn't told apart
/// from a long one.
#[tokio::test]
async fn the_mask_does_not_vary_with_the_value() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;

    save(&app, "SHORT", "a").await;
    save(&app, "LONG", &"b".repeat(200)).await;

    let listed = list(&app).await;
    assert_eq!(listed[0]["preview"], listed[1]["preview"]);
}

#[tokio::test]
async fn a_value_saved_without_a_key_stays_usable() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.state.config.token_encryption_key = None;
    app.rebuild_router();

    save(&app, "API_TOKEN", "plain").await;

    assert_eq!(stored(&app, "API_TOKEN").await, ("plain".to_string(), 0));
    assert_eq!(list(&app).await[0]["unreadable"], false);
}

/// A row that won't open under the current key is marked, and saving the
/// variable again recovers it.
#[tokio::test]
async fn a_value_from_another_key_is_marked_and_can_be_set_again() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    save(&app, "API_TOKEN", "secret").await;

    app.state.config.token_encryption_key = Some([0x77; 32]);
    app.rebuild_router();
    assert_eq!(list(&app).await[0]["unreadable"], true);

    save(&app, "API_TOKEN", "replacement").await;
    assert_eq!(list(&app).await[0]["unreadable"], false);
}

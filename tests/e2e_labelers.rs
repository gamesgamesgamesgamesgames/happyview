mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::db::{adapt_sql, now_rfc3339};
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

fn admin_post(
    uri: &str,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
    body: &Value,
) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(cookie.0, cookie.1)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

fn admin_patch(
    uri: &str,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
    body: &Value,
) -> Request<Body> {
    Request::builder()
        .method("PATCH")
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
        .method("DELETE")
        .uri(uri)
        .header(cookie.0, cookie.1)
        .body(Body::empty())
        .unwrap()
}

// ---------------------------------------------------------------------------
// POST /admin/labelers
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn labeler_add_returns_201() {
    common::require_db!();
    let app = TestApp::new().await;

    let body = json!({ "did": "did:plc:labeler1" });

    let resp = app
        .router
        .clone()
        .oneshot(admin_post("/admin/labelers", app.admin_cookie(), &body))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);
}

#[tokio::test]
#[serial]
async fn labeler_add_upsert_reactivates() {
    common::require_db!();
    let app = TestApp::new().await;

    let body = json!({ "did": "did:plc:labeler1" });

    // First add
    app.router
        .clone()
        .clone()
        .oneshot(admin_post("/admin/labelers", app.admin_cookie(), &body))
        .await
        .unwrap();

    // Pause it
    app.router
        .clone()
        .clone()
        .oneshot(admin_patch(
            "/admin/labelers/did:plc:labeler1",
            app.admin_cookie(),
            &json!({ "status": "paused" }),
        ))
        .await
        .unwrap();

    // Re-add (upsert should reactivate)
    let resp = app
        .router
        .clone()
        .clone()
        .oneshot(admin_post("/admin/labelers", app.admin_cookie(), &body))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);

    // Verify it's active again
    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/labelers", app.admin_cookie()))
        .await
        .unwrap();

    let json = json_body(resp).await;
    let labelers = json.as_array().unwrap();
    assert_eq!(labelers.len(), 1);
    assert_eq!(labelers[0]["status"], "active");
}

// ---------------------------------------------------------------------------
// GET /admin/labelers
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn labeler_list_empty() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/labelers", app.admin_cookie()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = json_body(resp).await;
    assert!(json.as_array().unwrap().is_empty());
}

#[tokio::test]
#[serial]
async fn labeler_list_returns_added() {
    common::require_db!();
    let app = TestApp::new().await;

    app.router
        .clone()
        .clone()
        .oneshot(admin_post(
            "/admin/labelers",
            app.admin_cookie(),
            &json!({ "did": "did:plc:lab1" }),
        ))
        .await
        .unwrap();

    app.router
        .clone()
        .clone()
        .oneshot(admin_post(
            "/admin/labelers",
            app.admin_cookie(),
            &json!({ "did": "did:plc:lab2" }),
        ))
        .await
        .unwrap();

    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/labelers", app.admin_cookie()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = json_body(resp).await;
    let labelers = json.as_array().unwrap();
    assert_eq!(labelers.len(), 2);
    assert_eq!(labelers[0]["did"], "did:plc:lab1");
    assert_eq!(labelers[0]["status"], "active");
    assert_eq!(labelers[1]["did"], "did:plc:lab2");
}

// ---------------------------------------------------------------------------
// PATCH /admin/labelers/{did}
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn labeler_update_status() {
    common::require_db!();
    let app = TestApp::new().await;

    app.router
        .clone()
        .clone()
        .oneshot(admin_post(
            "/admin/labelers",
            app.admin_cookie(),
            &json!({ "did": "did:plc:lab1" }),
        ))
        .await
        .unwrap();

    let resp = app
        .router
        .clone()
        .clone()
        .oneshot(admin_patch(
            "/admin/labelers/did:plc:lab1",
            app.admin_cookie(),
            &json!({ "status": "paused" }),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Verify status changed
    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/labelers", app.admin_cookie()))
        .await
        .unwrap();

    let json = json_body(resp).await;
    let labelers = json.as_array().unwrap();
    assert_eq!(labelers[0]["status"], "paused");
}

#[tokio::test]
#[serial]
async fn labeler_update_not_found() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_patch(
            "/admin/labelers/did:plc:nonexistent",
            app.admin_cookie(),
            &json!({ "status": "paused" }),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// DELETE /admin/labelers/{did}
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn labeler_delete_returns_204() {
    common::require_db!();
    let app = TestApp::new().await;

    app.router
        .clone()
        .clone()
        .oneshot(admin_post(
            "/admin/labelers",
            app.admin_cookie(),
            &json!({ "did": "did:plc:lab1" }),
        ))
        .await
        .unwrap();

    let resp = app
        .router
        .clone()
        .clone()
        .oneshot(admin_delete(
            "/admin/labelers/did:plc:lab1",
            app.admin_cookie(),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Verify it's gone
    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/labelers", app.admin_cookie()))
        .await
        .unwrap();

    let json = json_body(resp).await;
    assert!(json.as_array().unwrap().is_empty());
}

#[tokio::test]
#[serial]
async fn labeler_delete_not_found() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_delete(
            "/admin/labelers/did:plc:nonexistent",
            app.admin_cookie(),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[serial]
async fn labeler_delete_removes_labels() {
    common::require_db!();
    let app = TestApp::new().await;
    let backend = app.state.db_backend;

    // Add a labeler
    app.router
        .clone()
        .clone()
        .oneshot(admin_post(
            "/admin/labelers",
            app.admin_cookie(),
            &json!({ "did": "did:plc:lab1" }),
        ))
        .await
        .unwrap();

    // Seed some labels from that labeler
    let sql = adapt_sql(
        "INSERT INTO happyview_labels (src, uri, val, cts) VALUES (?, ?, ?, ?)",
        backend,
    );
    sqlx::query(&sql)
        .bind("did:plc:lab1")
        .bind("at://did:plc:user/test.collection/rkey1")
        .bind("adult-content")
        .bind(now_rfc3339())
        .execute(&app.state.db)
        .await
        .unwrap();

    // Delete the labeler
    app.router
        .clone()
        .clone()
        .oneshot(admin_delete(
            "/admin/labelers/did:plc:lab1",
            app.admin_cookie(),
        ))
        .await
        .unwrap();

    // Verify labels were also removed
    let sql = adapt_sql(
        "SELECT COUNT(*) FROM happyview_labels WHERE src = ?",
        backend,
    );
    let count: (i64,) = sqlx::query_as(&sql)
        .bind("did:plc:lab1")
        .fetch_one(&app.state.db)
        .await
        .unwrap();

    assert_eq!(count.0, 0);
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn labeler_no_auth_returns_401() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/labelers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

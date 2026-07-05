mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::db::adapt_sql;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

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

async fn seed_job(app: &TestApp, job_type: &str, status: &str) -> String {
    let id = Uuid::new_v4().to_string();
    let now = happyview::db::now_rfc3339();
    let input = serde_json::to_string(&json!({"test": true})).unwrap();

    let sql = adapt_sql(
        "INSERT INTO happyview_jobs (id, job_type, status, input, progress, created_by, created_at) VALUES (?, ?, ?, ?, '{}', ?, ?)",
        app.state.db_backend,
    );
    sqlx::query(&sql)
        .bind(&id)
        .bind(job_type)
        .bind(status)
        .bind(&input)
        .bind(&app.admin_did)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .expect("seed_job: insert failed");

    id
}

async fn set_job_status(app: &TestApp, id: &str, status: &str) {
    let sql = adapt_sql(
        "UPDATE happyview_jobs SET status = ? WHERE id = ?",
        app.state.db_backend,
    );
    sqlx::query(&sql)
        .bind(status)
        .bind(id)
        .execute(&app.state.db)
        .await
        .expect("set_job_status failed");
}

// ---------------------------------------------------------------------------
// List jobs
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn list_jobs_empty() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/jobs", app.admin_cookie()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["jobs"].as_array().unwrap().len(), 0);
    assert_eq!(body["cursor"], Value::Null);
}

#[tokio::test]
#[serial]
async fn list_jobs_returns_seeded_jobs() {
    common::require_db!();
    let app = TestApp::new().await;

    seed_job(&app, "test.export", "pending").await;
    seed_job(&app, "test.import", "running").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/jobs", app.admin_cookie()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    let jobs = body["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 2);
}

#[tokio::test]
#[serial]
async fn list_jobs_filters_by_status() {
    common::require_db!();
    let app = TestApp::new().await;

    seed_job(&app, "test.export", "pending").await;
    seed_job(&app, "test.import", "running").await;
    seed_job(&app, "test.cleanup", "completed").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/jobs?status=running", app.admin_cookie()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    let jobs = body["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["job_type"], "test.import");
    assert_eq!(jobs[0]["status"], "running");
}

// ---------------------------------------------------------------------------
// Get job
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn get_job_returns_details() {
    common::require_db!();
    let app = TestApp::new().await;

    let id = seed_job(&app, "test.export", "pending").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_get(&format!("/admin/jobs/{id}"), app.admin_cookie()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["id"], id);
    assert_eq!(body["job_type"], "test.export");
    assert_eq!(body["status"], "pending");
    assert_eq!(body["input"]["test"], true);
}

#[tokio::test]
#[serial]
async fn get_job_not_found() {
    common::require_db!();
    let app = TestApp::new().await;

    let fake_id = Uuid::new_v4();
    let resp = app
        .router
        .clone()
        .oneshot(admin_get(
            &format!("/admin/jobs/{fake_id}"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Cancel job
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn cancel_pending_job_sets_cancelled() {
    common::require_db!();
    let app = TestApp::new().await;

    let id = seed_job(&app, "test.export", "pending").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/jobs/{id}/cancel"),
            app.admin_cookie(),
            &json!({}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["status"], "cancelled");
}

#[tokio::test]
#[serial]
async fn cancel_running_job_sets_cancelling() {
    common::require_db!();
    let app = TestApp::new().await;

    let id = seed_job(&app, "test.export", "running").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/jobs/{id}/cancel"),
            app.admin_cookie(),
            &json!({}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["status"], "cancelling");
}

#[tokio::test]
#[serial]
async fn cancel_paused_job_sets_cancelled() {
    common::require_db!();
    let app = TestApp::new().await;

    let id = seed_job(&app, "test.export", "paused").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/jobs/{id}/cancel"),
            app.admin_cookie(),
            &json!({}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["status"], "cancelled");
}

#[tokio::test]
#[serial]
async fn cancel_completed_job_returns_409() {
    common::require_db!();
    let app = TestApp::new().await;

    let id = seed_job(&app, "test.export", "completed").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/jobs/{id}/cancel"),
            app.admin_cookie(),
            &json!({}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

// ---------------------------------------------------------------------------
// Pause job
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn pause_running_job_sets_pausing() {
    common::require_db!();
    let app = TestApp::new().await;

    let id = seed_job(&app, "test.export", "running").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/jobs/{id}/pause"),
            app.admin_cookie(),
            &json!({}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["status"], "pausing");
}

#[tokio::test]
#[serial]
async fn pause_pending_job_returns_409() {
    common::require_db!();
    let app = TestApp::new().await;

    let id = seed_job(&app, "test.export", "pending").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/jobs/{id}/pause"),
            app.admin_cookie(),
            &json!({}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

// ---------------------------------------------------------------------------
// Resume job
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn resume_paused_job_sets_pending() {
    common::require_db!();
    let app = TestApp::new().await;

    let id = seed_job(&app, "test.export", "paused").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/jobs/{id}/resume"),
            app.admin_cookie(),
            &json!({}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["status"], "pending");
}

#[tokio::test]
#[serial]
async fn resume_running_job_returns_409() {
    common::require_db!();
    let app = TestApp::new().await;

    let id = seed_job(&app, "test.export", "running").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/jobs/{id}/resume"),
            app.admin_cookie(),
            &json!({}),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

// ---------------------------------------------------------------------------
// Auth: unauthenticated requests
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn list_jobs_without_auth_returns_401() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/jobs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// Full lifecycle: pending → running → pausing → paused → pending → cancel
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn full_job_lifecycle() {
    common::require_db!();
    let app = TestApp::new().await;

    let id = seed_job(&app, "test.lifecycle", "pending").await;

    // Simulate worker claiming → running
    set_job_status(&app, &id, "running").await;

    // Pause the running job
    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/jobs/{id}/pause"),
            app.admin_cookie(),
            &json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json_body(resp).await["status"], "pausing");

    // Simulate worker acknowledging pause
    set_job_status(&app, &id, "paused").await;

    // Resume the paused job
    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/jobs/{id}/resume"),
            app.admin_cookie(),
            &json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json_body(resp).await["status"], "pending");

    // Cancel the pending job
    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/jobs/{id}/cancel"),
            app.admin_cookie(),
            &json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json_body(resp).await["status"], "cancelled");

    // Verify final state
    let resp = app
        .router
        .clone()
        .oneshot(admin_get(&format!("/admin/jobs/{id}"), app.admin_cookie()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let job = json_body(resp).await;
    assert_eq!(job["status"], "cancelled");
    assert!(job["completed_at"].is_string());
}

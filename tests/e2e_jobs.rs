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
    seed_job_with_dpop(app, job_type, status, false, None, None).await
}

async fn seed_job_with_dpop(
    app: &TestApp,
    job_type: &str,
    status: &str,
    inherit_auth: bool,
    api_client_id: Option<&str>,
    dpop_key_id: Option<&str>,
) -> String {
    let id = Uuid::new_v4().to_string();
    let now = happyview::db::now_rfc3339();
    let input = serde_json::to_string(&json!({"test": true})).unwrap();

    let sql = adapt_sql(
        "INSERT INTO happyview_jobs (id, job_type, status, input, progress, created_by, created_at, inherit_auth, api_client_id, dpop_key_id) VALUES (?, ?, ?, ?, '{}', ?, ?, ?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(&id)
        .bind(job_type)
        .bind(status)
        .bind(&input)
        .bind(&app.admin_did)
        .bind(&now)
        .bind(inherit_auth)
        .bind(api_client_id)
        .bind(dpop_key_id)
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
    happyview::db::query(&sql)
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
// Worker: claim → execute → complete
//
// This is the only coverage that exercises `claim_next_job`, whose
// `UPDATE ... RETURNING` writes `status = 'running'` *before* the returned row
// is decoded. A decode failure there leaves the job marked running with nobody
// executing it, and every read below goes through raw SQL rather than the admin
// API so a failure here points at the worker rather than the read path.
// ---------------------------------------------------------------------------

async fn seed_script(app: &TestApp, trigger_id: &str, body: &str) {
    // created_at/updated_at have no default on Postgres — bind them explicitly.
    let now = happyview::db::now_rfc3339();
    let sql = adapt_sql(
        "INSERT INTO happyview_scripts (id, body, script_type, created_at, updated_at) VALUES (?, ?, 'lua', ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(trigger_id)
        .bind(body)
        .bind(&now)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .expect("seed_script: insert failed");
}

async fn job_row(app: &TestApp, id: &str) -> (String, Option<String>, Option<String>) {
    let sql = adapt_sql(
        "SELECT status, result, error FROM happyview_jobs WHERE id = ?",
        app.state.db_backend,
    );
    happyview::db::query_as::<(String, Option<String>, Option<String>)>(&sql)
        .bind(id)
        .fetch_one(&app.state.db)
        .await
        .expect("job_row: select failed")
}

/// Poll until the job leaves the pending/running states, or time out.
async fn await_terminal_status(
    app: &TestApp,
    id: &str,
) -> (String, Option<String>, Option<String>) {
    for _ in 0..100 {
        let row = job_row(app, id).await;
        if !matches!(row.0.as_str(), "pending" | "running") {
            return row;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    job_row(app, id).await
}

#[tokio::test]
#[serial]
async fn worker_runs_pending_job_to_completion() {
    common::require_db!();
    let app = TestApp::new().await;

    seed_script(
        &app,
        "job.run:test.worker",
        "function handle() return { ok = true } end",
    )
    .await;
    let id = seed_job(&app, "test.worker", "pending").await;

    let worker = tokio::spawn(happyview::jobs::worker::run_worker(app.state.clone()));
    let (status, result, error) = await_terminal_status(&app, &id).await;
    worker.abort();

    assert_eq!(status, "completed", "job error: {error:?}");
    let result: Value = serde_json::from_str(&result.expect("no result persisted")).unwrap();
    assert_eq!(result["ok"], true);
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

// ---------------------------------------------------------------------------
// Job logs
// ---------------------------------------------------------------------------

async fn seed_job_log(app: &TestApp, job_id: &str, level: &str, message: &str) {
    let id = Uuid::new_v4().to_string();
    let now = happyview::db::now_rfc3339();
    let sql = adapt_sql(
        "INSERT INTO happyview_job_logs (id, job_id, level, message, created_at) VALUES (?, ?, ?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(&id)
        .bind(job_id)
        .bind(level)
        .bind(message)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .expect("seed_job_log: insert failed");
}

#[tokio::test]
#[serial]
async fn list_job_logs_empty() {
    common::require_db!();
    let app = TestApp::new().await;

    let id = seed_job(&app, "test.logs", "running").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_get(
            &format!("/admin/jobs/{id}/logs"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["logs"].as_array().unwrap().len(), 0);
    assert_eq!(body["cursor"], Value::Null);
}

#[tokio::test]
#[serial]
async fn list_job_logs_returns_seeded_logs() {
    common::require_db!();
    let app = TestApp::new().await;

    let id = seed_job(&app, "test.logs", "running").await;
    seed_job_log(&app, &id, "info", "starting migration").await;
    seed_job_log(&app, &id, "warn", "rate limited").await;
    seed_job_log(&app, &id, "info", "migration complete").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_get(
            &format!("/admin/jobs/{id}/logs"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    let logs = body["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 3);
    assert_eq!(logs[0]["message"], "starting migration");
    assert_eq!(logs[0]["level"], "info");
    assert_eq!(logs[1]["message"], "rate limited");
    assert_eq!(logs[1]["level"], "warn");
    assert_eq!(logs[2]["message"], "migration complete");
}

#[tokio::test]
#[serial]
async fn list_job_logs_pagination() {
    common::require_db!();
    let app = TestApp::new().await;

    let id = seed_job(&app, "test.logs-page", "running").await;
    for i in 0..5 {
        seed_job_log(&app, &id, "info", &format!("log-{i}")).await;
    }

    let resp = app
        .router
        .clone()
        .oneshot(admin_get(
            &format!("/admin/jobs/{id}/logs?limit=2"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    let logs = body["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0]["message"], "log-0");
    assert_eq!(logs[1]["message"], "log-1");
    assert!(body["cursor"].is_string());

    let cursor = body["cursor"].as_str().unwrap();
    let resp2 = app
        .router
        .clone()
        .oneshot(admin_get(
            &format!("/admin/jobs/{id}/logs?limit=2&cursor={cursor}"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();

    assert_eq!(resp2.status(), StatusCode::OK);
    let body2 = json_body(resp2).await;
    let logs2 = body2["logs"].as_array().unwrap();
    assert_eq!(logs2.len(), 2);
    assert_eq!(logs2[0]["message"], "log-2");
    assert_eq!(logs2[1]["message"], "log-3");
}

#[tokio::test]
#[serial]
async fn list_job_logs_without_auth_returns_401() {
    common::require_db!();
    let app = TestApp::new().await;

    let id = seed_job(&app, "test.logs-auth", "running").await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/admin/jobs/{id}/logs"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// DPoP context persistence
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn create_job_persists_dpop_context() {
    common::require_db!();
    let app = TestApp::new().await;

    let id = seed_job_with_dpop(
        &app,
        "test.dpop",
        "pending",
        true,
        Some("client_abc"),
        Some("key_xyz"),
    )
    .await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_get(&format!("/admin/jobs/{id}"), app.admin_cookie()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let job = json_body(resp).await;
    assert_eq!(job["inherit_auth"], true);
    assert_eq!(job["api_client_id"], "client_abc");
    assert_eq!(job["dpop_key_id"], "key_xyz");
}

#[tokio::test]
#[serial]
async fn create_job_without_dpop_has_null_fields() {
    common::require_db!();
    let app = TestApp::new().await;

    let id = seed_job(&app, "test.no-dpop", "pending").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_get(&format!("/admin/jobs/{id}"), app.admin_cookie()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let job = json_body(resp).await;
    assert_eq!(job["inherit_auth"], false);
    assert_eq!(job["api_client_id"], Value::Null);
    assert_eq!(job["dpop_key_id"], Value::Null);
}

#[tokio::test]
#[serial]
async fn get_job_returns_dpop_fields_in_response() {
    common::require_db!();
    let app = TestApp::new().await;

    let id = seed_job_with_dpop(
        &app,
        "test.dpop-api",
        "pending",
        true,
        Some("api_client_123"),
        Some("dpop_key_456"),
    )
    .await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_get(&format!("/admin/jobs/{id}"), app.admin_cookie()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let job = json_body(resp).await;
    assert_eq!(job["id"], id);
    assert_eq!(job["inherit_auth"], true);
    assert_eq!(job["api_client_id"], "api_client_123");
    assert_eq!(job["dpop_key_id"], "dpop_key_456");
}

#[tokio::test]
#[serial]
async fn list_jobs_includes_dpop_fields() {
    common::require_db!();
    let app = TestApp::new().await;

    seed_job_with_dpop(
        &app,
        "test.list-dpop",
        "pending",
        true,
        Some("list_client"),
        Some("list_key"),
    )
    .await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/jobs", app.admin_cookie()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    let jobs = body["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["api_client_id"], "list_client");
    assert_eq!(jobs[0]["dpop_key_id"], "list_key");
    assert_eq!(jobs[0]["inherit_auth"], true);
}

// ---------------------------------------------------------------------------
// Native job dispatch
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn scripts_cannot_claim_a_reserved_job_trigger() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/scripts",
            app.admin_cookie(),
            &json!({
                "id": "job.run:happyview.delete-collection",
                "body": "function handle() end",
                "script_type": "lua"
            }),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = json_body(resp).await;
    assert!(
        body.to_string().contains("reserved"),
        "error should explain the reservation: {body}"
    );
}

async fn seed_record(app: &TestApp, uri: &str, collection: &str) {
    // created_at has no default on Postgres (dropped by the TEXT migration,
    // see 20260318000000_uuid_to_text.sql), so it must be supplied explicitly
    // to work on both backends.
    let sql = adapt_sql(
        "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        app.state.db_backend,
    );
    let now = happyview::db::now_rfc3339();
    happyview::db::query(&sql)
        .bind(uri)
        .bind("did:plc:seed")
        .bind(collection)
        .bind("rkey1")
        .bind(r#"{"text":"hi"}"#)
        .bind("bafyseed")
        .bind(&now)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .expect("seed_record failed");
}

async fn count_in_collection(app: &TestApp, collection: &str) -> i64 {
    let sql = adapt_sql(
        "SELECT COUNT(*) FROM happyview_records WHERE collection = ?",
        app.state.db_backend,
    );
    let (n,): (i64,) = happyview::db::query_as(&sql)
        .bind(collection)
        .fetch_one(&app.state.db)
        .await
        .expect("count failed");
    n
}

#[tokio::test]
#[serial]
async fn native_delete_collection_job_empties_the_collection() {
    common::require_db!();
    let app = TestApp::new().await;

    for i in 0..25 {
        seed_record(
            &app,
            &format!("at://did:plc:seed/app.test.post/{i}"),
            "app.test.post",
        )
        .await;
    }
    seed_record(&app, "at://did:plc:seed/app.other.post/1", "app.other.post").await;
    assert_eq!(count_in_collection(&app, "app.test.post").await, 25);

    let id = Uuid::new_v4().to_string();
    let now = happyview::db::now_rfc3339();
    // inherit_auth is a bool column; on Postgres a literal `0` doesn't
    // implicitly cast to boolean, so it must be a bound parameter (as
    // seed_job_with_dpop does above), not embedded as a literal in the SQL.
    let sql = adapt_sql(
        "INSERT INTO happyview_jobs (id, job_type, status, input, progress, created_by, created_at, inherit_auth) \
         VALUES (?, 'happyview.delete-collection', 'pending', ?, '{}', ?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(&id)
        .bind(r#"{"collection":"app.test.post"}"#)
        .bind(&app.admin_did)
        .bind(&now)
        .bind(false)
        .execute(&app.state.db)
        .await
        .expect("enqueue failed");

    let worker = tokio::spawn(happyview::jobs::worker::run_worker(app.state.clone()));
    let (status, result, error) = await_terminal_status(&app, &id).await;
    worker.abort();

    assert_eq!(status, "completed", "job error: {error:?}");
    let result: Value = serde_json::from_str(&result.expect("no result")).unwrap();
    assert_eq!(result["deleted"], 25);

    assert_eq!(count_in_collection(&app, "app.test.post").await, 0);
    assert_eq!(
        count_in_collection(&app, "app.other.post").await,
        1,
        "other collections must be untouched"
    );
}

/// Insert `count` records into `collection` in chunks, staying well under
/// SQLite's default bound variable limit (999) per statement.
async fn seed_records_bulk(app: &TestApp, collection: &str, count: usize) {
    const CHUNK: usize = 100;
    let now = happyview::db::now_rfc3339();
    let mut i = 0usize;
    while i < count {
        let n = std::cmp::min(CHUNK, count - i);
        let placeholders = vec!["(?, ?, ?, ?, ?, ?, ?, ?)"; n].join(", ");
        let sql = adapt_sql(
            &format!(
                "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at) \
                 VALUES {placeholders}"
            ),
            app.state.db_backend,
        );
        let mut q = happyview::db::query(&sql);
        for j in 0..n {
            let idx = i + j;
            q = q
                .bind(format!("at://did:plc:bulk/{collection}/{idx}"))
                .bind("did:plc:bulk".to_string())
                .bind(collection.to_string())
                .bind(format!("rkey{idx}"))
                .bind(r#"{"text":"bulk"}"#.to_string())
                .bind(format!("bafybulk{idx}"))
                .bind(now.clone())
                .bind(now.clone());
        }
        q.execute(&app.state.db)
            .await
            .expect("seed_records_bulk: insert failed");
        i += n;
    }
}

async fn seed_job_row_with_status(
    app: &TestApp,
    id: &str,
    job_type: &str,
    status: &str,
    input: &Value,
) {
    let sql = adapt_sql(
        "INSERT INTO happyview_jobs (id, job_type, status, input, progress, created_by, created_at, inherit_auth) \
         VALUES (?, ?, ?, ?, '{}', ?, ?, ?)",
        app.state.db_backend,
    );
    let now = happyview::db::now_rfc3339();
    happyview::db::query(&sql)
        .bind(id)
        .bind(job_type)
        .bind(status)
        .bind(input.to_string())
        .bind(&app.admin_did)
        .bind(&now)
        .bind(false)
        .execute(&app.state.db)
        .await
        .expect("seed_job_row_with_status failed");
}

// Regression guard for B1: the delete loop's only historical exit was
// `affected == 0`, which never happens on a collection still receiving
// inserts (Jetstream ingest). The fix breaks on a *short* batch instead.
// BATCH_SIZE is 5000 (see delete_collection.rs); seeding one row over a full
// batch forces two DELETE statements — a full batch (affected == BATCH_SIZE,
// loop must continue) then a short one (affected == 1 < BATCH_SIZE, loop must
// break) — so an inverted or off-by-one comparison (e.g. `<=`) would leave a
// row behind and fail this test's final count assertions.
#[tokio::test]
#[serial]
async fn native_delete_collection_handles_more_than_one_batch() {
    common::require_db!();
    let app = TestApp::new().await;

    const SEEDED: usize = 5001;
    seed_records_bulk(&app, "app.bulk.post", SEEDED).await;
    assert_eq!(
        count_in_collection(&app, "app.bulk.post").await,
        SEEDED as i64
    );

    let id = Uuid::new_v4().to_string();
    let now = happyview::db::now_rfc3339();
    let sql = adapt_sql(
        "INSERT INTO happyview_jobs (id, job_type, status, input, progress, created_by, created_at, inherit_auth) \
         VALUES (?, 'happyview.delete-collection', 'pending', ?, '{}', ?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(&id)
        .bind(r#"{"collection":"app.bulk.post"}"#)
        .bind(&app.admin_did)
        .bind(&now)
        .bind(false)
        .execute(&app.state.db)
        .await
        .expect("enqueue failed");

    let worker = tokio::spawn(happyview::jobs::worker::run_worker(app.state.clone()));
    let (status, result, error) = await_terminal_status(&app, &id).await;
    worker.abort();

    assert_eq!(status, "completed", "job error: {error:?}");
    let result: Value = serde_json::from_str(&result.expect("no result")).unwrap();
    assert_eq!(result["deleted"], SEEDED);
    assert_eq!(count_in_collection(&app, "app.bulk.post").await, 0);
}

// Regression guard for B3: the should_stop branch was untested. Pre-setting
// the job row's status to `cancelling` before calling the handler makes
// should_stop's very first check (which runs before any batch) fire on
// iteration one — deterministic, no need to race the 5000-row batch.
#[tokio::test]
#[serial]
async fn native_delete_collection_stops_on_cancelling_without_failing() {
    common::require_db!();
    let app = TestApp::new().await;

    seed_record(&app, "at://did:plc:seed/app.stop.post/1", "app.stop.post").await;
    seed_record(&app, "at://did:plc:seed/app.stop.post/2", "app.stop.post").await;
    seed_record(&app, "at://did:plc:seed/app.stop.post/3", "app.stop.post").await;

    let id = Uuid::new_v4().to_string();
    let input = json!({"collection": "app.stop.post"});
    seed_job_row_with_status(
        &app,
        &id,
        "happyview.delete-collection",
        "cancelling",
        &input,
    )
    .await;

    let job = happyview::jobs::Job {
        id: id.clone(),
        job_type: "happyview.delete-collection".into(),
        status: "cancelling".into(),
        input,
        progress: json!({}),
        result: None,
        error: None,
        created_by: app.admin_did.clone(),
        started_at: None,
        completed_at: None,
        created_at: happyview::db::now_rfc3339(),
        inherit_auth: false,
        api_client_id: None,
        dpop_key_id: None,
    };

    let outcome = happyview::jobs::native::delete_collection::run(&app.state, &job).await;

    match outcome {
        happyview::jobs::native::NativeOutcome::Completed(result) => {
            assert_eq!(result["deleted"], 0);
            assert_eq!(result["stopped"], "cancelling");
        }
        happyview::jobs::native::NativeOutcome::Failed(e) => {
            panic!("a deliberate cancel must not be reported as a failure: {e}");
        }
    }

    assert_eq!(
        count_in_collection(&app, "app.stop.post").await,
        3,
        "no records should be deleted once should_stop fires before the first batch"
    );
}

#[tokio::test]
#[serial]
async fn native_delete_collection_stops_on_pausing_without_failing() {
    common::require_db!();
    let app = TestApp::new().await;

    seed_record(&app, "at://did:plc:seed/app.pause.post/1", "app.pause.post").await;

    let id = Uuid::new_v4().to_string();
    let input = json!({"collection": "app.pause.post"});
    seed_job_row_with_status(&app, &id, "happyview.delete-collection", "pausing", &input).await;

    let job = happyview::jobs::Job {
        id: id.clone(),
        job_type: "happyview.delete-collection".into(),
        status: "pausing".into(),
        input,
        progress: json!({}),
        result: None,
        error: None,
        created_by: app.admin_did.clone(),
        started_at: None,
        completed_at: None,
        created_at: happyview::db::now_rfc3339(),
        inherit_auth: false,
        api_client_id: None,
        dpop_key_id: None,
    };

    let outcome = happyview::jobs::native::delete_collection::run(&app.state, &job).await;

    match outcome {
        happyview::jobs::native::NativeOutcome::Completed(result) => {
            assert_eq!(result["deleted"], 0);
            assert_eq!(result["stopped"], "pausing");
        }
        happyview::jobs::native::NativeOutcome::Failed(e) => {
            panic!("a deliberate pause must not be reported as a failure: {e}");
        }
    }

    assert_eq!(
        count_in_collection(&app, "app.pause.post").await,
        1,
        "no records should be deleted once should_stop fires before the first batch"
    );
}

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::event_log::{EventLog, Severity, log_event};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;

async fn seed(app: &TestApp, event_type: &str, subject: &str) {
    log_event(
        &app.state.db,
        EventLog {
            event_type: event_type.to_string(),
            severity: Severity::Info,
            actor_did: None,
            subject: Some(subject.to_string()),
            detail: json!({}),
        },
        app.state.db_backend,
    )
    .await;
}

async fn call(
    app: &TestApp,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> axum::response::Response {
    let (name, value) = app.admin_cookie();
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header(name, value);
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    app.router
        .clone()
        .oneshot(req.body(body).unwrap())
        .await
        .unwrap()
}

async fn count_type(app: &TestApp, event_type: &str) -> i64 {
    let sql = happyview::db::adapt_sql(
        "SELECT COUNT(*) FROM happyview_event_logs WHERE event_type = ?",
        app.state.db_backend,
    );
    happyview::db::query_as::<(i64,)>(&sql)
        .bind(event_type)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
        .0
}

/// Drive the `happyview.purge-event-logs` job that `POST /admin/events/purge`
/// enqueues to completion, the same way `native_delete_collection_*` tests in
/// `tests/e2e_jobs.rs` run a native job directly: read the row the endpoint
/// inserted and hand it to the job's `run` function. `jobs::db` is
/// crate-private, so the row is read with a plain query rather than
/// `jobs::db::get_job`.
async fn run_job_to_completion(app: &TestApp, job_id: &str) {
    let sql = happyview::db::adapt_sql(
        "SELECT input FROM happyview_jobs WHERE id = ?",
        app.state.db_backend,
    );
    let (input_str,) = happyview::db::query_as::<(String,)>(&sql)
        .bind(job_id)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    let input: Value = serde_json::from_str(&input_str).unwrap();

    let job = happyview::jobs::Job {
        id: job_id.to_string(),
        job_type: "happyview.purge-event-logs".to_string(),
        status: "pending".to_string(),
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

    happyview::jobs::native::purge_event_logs::run(&app.state, &job).await;
}

async fn run_purge(app: &TestApp, filter: Value) -> axum::response::Response {
    let resp = call(app, "POST", "/admin/events/purge", Some(filter)).await;
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    if status == StatusCode::ACCEPTED {
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let job_id = body["job_id"].as_str().unwrap();
        run_job_to_completion(app, job_id).await;
    }
    axum::response::Response::builder()
        .status(status)
        .body(Body::from(bytes))
        .unwrap()
}

#[tokio::test]
#[serial]
async fn naming_a_protected_type_is_rejected() {
    common::require_db!();
    let app = TestApp::new().await;
    for filter in [
        json!({ "event_type": "space.moderator_read" }),
        json!({ "category": "space" }),
        json!({ "category": "record,event_logs" }),
    ] {
        let resp = call(&app, "POST", "/admin/events/purge", Some(filter)).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
#[serial]
async fn count_and_purge_skip_protected_events() {
    common::require_db!();
    let app = TestApp::new().await;
    seed(&app, "record.skipped", "at://x").await;
    seed(&app, "space.moderator_read", "at://x").await;
    seed(&app, "space.access_granted", "at://x").await;

    let resp = call(&app, "GET", "/admin/events/count?subject=at://x", None).await;
    let body: Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(
        body["count"], 1,
        "the preview counts only what the purge can delete"
    );

    run_purge(&app, json!({ "subject": "at://x" })).await;
    assert_eq!(count_type(&app, "record.skipped").await, 0);
    assert_eq!(count_type(&app, "space.moderator_read").await, 1);
    assert_eq!(count_type(&app, "space.access_granted").await, 1);
}

#[tokio::test]
#[serial]
async fn a_later_purge_keeps_the_record_of_an_earlier_one() {
    common::require_db!();
    let app = TestApp::new().await;
    seed(&app, "record.skipped", "at://y").await;
    run_purge(&app, json!({ "severity": "info" })).await;
    run_purge(&app, json!({ "severity": "warn" })).await;
    assert_eq!(count_type(&app, "event_logs.purged").await, 2);
}

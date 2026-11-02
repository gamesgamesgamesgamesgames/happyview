mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::db::adapt_sql;
use happyview::event_log::{
    EventLog, Severity, log_event, run_retention_sweep, sweep_ended_access_grants,
};
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
    let sql = adapt_sql(
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
    let sql = adapt_sql(
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
        attempt: 1,
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

async fn put_setting(app: &TestApp, key: &str, value: &str) -> StatusCode {
    call(
        app,
        "PUT",
        &format!("/admin/settings/{key}"),
        Some(json!({ "value": value })),
    )
    .await
    .status()
}

async fn retention_changes(app: &TestApp) -> Vec<(String, Value)> {
    let sql = adapt_sql(
        "SELECT subject, detail FROM happyview_event_logs \
         WHERE event_type = 'event_logs.retention_changed' ORDER BY created_at",
        app.state.db_backend,
    );
    happyview::db::query_as::<(String, String)>(&sql)
        .fetch_all(&app.state.db)
        .await
        .unwrap()
        .into_iter()
        .map(|(subject, detail)| (subject, serde_json::from_str(&detail).unwrap()))
        .collect()
}

#[tokio::test]
#[serial]
async fn saving_a_retention_logs_only_effective_changes() {
    common::require_db!();
    let app = TestApp::new().await;

    // An unset retention setting already holds its default.
    assert_eq!(
        put_setting(&app, "space_access_log_retention_days", "365").await,
        StatusCode::NO_CONTENT
    );
    put_setting(&app, "event_log_retention_days", "30").await;
    assert!(retention_changes(&app).await.is_empty());

    put_setting(&app, "event_log_retention_days", "14").await;
    put_setting(&app, "event_log_retention_days", "14").await;
    put_setting(&app, "space_access_log_retention_days", "90").await;
    assert_eq!(
        retention_changes(&app).await,
        vec![
            (
                "event_log_retention_days".to_string(),
                json!({ "from": 30, "to": 14 })
            ),
            (
                "space_access_log_retention_days".to_string(),
                json!({ "from": 365, "to": 90 })
            ),
        ]
    );
}

#[tokio::test]
#[serial]
async fn a_retention_change_is_not_stored_without_its_audit_event() {
    common::require_db!();
    let app = TestApp::new().await;

    rename_table(&app, "happyview_event_logs", "happyview_event_logs_hidden").await;
    let status = put_setting(&app, "event_log_retention_days", "14").await;
    let stored = happyview::admin::settings::try_get_setting(
        &app.state.db,
        "event_log_retention_days",
        app.state.db_backend,
    )
    .await;
    rename_table(&app, "happyview_event_logs_hidden", "happyview_event_logs").await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        stored.unwrap(),
        None,
        "the setting rolls back with its audit event"
    );
}

async fn rename_table(app: &TestApp, from: &str, to: &str) {
    happyview::db::query(&format!("ALTER TABLE {from} RENAME TO {to}"))
        .execute(&app.state.db)
        .await
        .unwrap();
}

fn days_ago(days: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339()
}

fn days_ahead(days: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::days(days)).to_rfc3339()
}

async fn insert_grant(
    app: &TestApp,
    id: &str,
    created_at: &str,
    expires_at: &str,
    revoked_at: Option<&str>,
) {
    happyview::db::query(&adapt_sql(
        "INSERT INTO happyview_space_access_grants \
         (id, user_id, user_did, scope, target, reason, created_at, expires_at, revoked_at) \
         VALUES (?, 'u', 'did:plc:u', 'account', 'did:plc:t', 'r', ?, ?, ?)",
        app.state.db_backend,
    ))
    .bind(id)
    .bind(created_at)
    .bind(expires_at)
    .bind(revoked_at)
    .execute(&app.state.db)
    .await
    .unwrap();
}

async fn insert_event(app: &TestApp, id: &str, event_type: &str, created_at: &str) {
    happyview::db::query(&adapt_sql(
        "INSERT INTO happyview_event_logs (id, event_type, severity, detail, created_at) \
         VALUES (?, ?, 'info', '{}', ?)",
        app.state.db_backend,
    ))
    .bind(id)
    .bind(event_type)
    .bind(created_at)
    .execute(&app.state.db)
    .await
    .unwrap();
}

/// Tie an event to a grant the way the grant API does: grant and revocation
/// events through `happyview_space_access_grant_events`, reads through
/// `happyview_space_access_reads`.
async fn link(app: &TestApp, table: &str, event_id: &str, grant_id: &str) {
    happyview::db::query(&adapt_sql(
        &format!("INSERT INTO {table} (event_id, grant_id) VALUES (?, ?)"),
        app.state.db_backend,
    ))
    .bind(event_id)
    .bind(grant_id)
    .execute(&app.state.db)
    .await
    .unwrap();
}

async fn event_ids(app: &TestApp) -> Vec<String> {
    let mut ids: Vec<String> =
        happyview::db::query_as::<(String,)>("SELECT id FROM happyview_event_logs")
            .fetch_all(&app.state.db)
            .await
            .unwrap()
            .into_iter()
            .map(|(id,)| id)
            .collect();
    ids.sort();
    ids
}

async fn grant_ids(app: &TestApp) -> Vec<String> {
    let mut ids: Vec<String> =
        happyview::db::query_as::<(String,)>("SELECT id FROM happyview_space_access_grants")
            .fetch_all(&app.state.db)
            .await
            .unwrap()
            .into_iter()
            .map(|(id,)| id)
            .collect();
    ids.sort();
    ids
}

#[tokio::test]
#[serial]
async fn retention_removes_grants_that_ended_before_the_cutoff() {
    common::require_db!();
    let app = TestApp::new().await;
    let created = days_ago(500);
    insert_grant(&app, "expired-long-ago", &created, &days_ago(400), None).await;
    insert_grant(
        &app,
        "revoked-long-ago",
        &created,
        &days_ahead(1),
        Some(&days_ago(400)),
    )
    .await;
    insert_grant(&app, "expired-recently", &created, &days_ago(10), None).await;
    insert_grant(&app, "active", &created, &days_ahead(1), None).await;

    let deleted = sweep_ended_access_grants(&app.state.db, 365, app.state.db_backend)
        .await
        .unwrap();
    assert_eq!(deleted, 2);
    assert_eq!(grant_ids(&app).await, vec!["active", "expired-recently"]);

    assert_eq!(
        sweep_ended_access_grants(&app.state.db, 0, app.state.db_backend)
            .await
            .unwrap(),
        0,
        "0 keeps them forever"
    );
}

/// A stored grant holds back its own grant event and the reads made under it,
/// however old, and nothing else: an unrelated protected event of the same
/// age goes on schedule.
#[tokio::test]
#[serial]
async fn a_stored_grant_holds_only_its_own_records() {
    common::require_db!();
    let app = TestApp::new().await;
    let old = days_ago(400);
    insert_grant(&app, "held", &days_ago(500), &days_ahead(30), None).await;
    insert_event(&app, "granted", "space.access_granted", &old).await;
    insert_event(&app, "read-under-grant", "space.moderator_read", &old).await;
    insert_event(&app, "unrelated-read", "space.moderator_read", &old).await;
    insert_event(&app, "unrelated-purge", "event_logs.purged", &old).await;
    insert_event(
        &app,
        "before-the-grant",
        "space.access_granted",
        &days_ago(600),
    )
    .await;
    link(
        &app,
        "happyview_space_access_grant_events",
        "granted",
        "held",
    )
    .await;
    link(
        &app,
        "happyview_space_access_reads",
        "read-under-grant",
        "held",
    )
    .await;

    run_retention_sweep(&app.state.db, 30, 365, app.state.db_backend).await;

    assert_eq!(event_ids(&app).await, vec!["granted", "read-under-grant"]);
}

/// An ended grant's records stay while its row does, the hold lifts once the
/// grant sweep removes the row, and a hold that cannot be checked deletes no
/// protected event at all.
#[tokio::test]
#[serial]
async fn the_hold_lasts_as_long_as_the_grant_row_and_fails_closed() {
    common::require_db!();
    let app = TestApp::new().await;
    let created = days_ago(800);
    insert_grant(&app, "long-over", &created, &days_ago(400), None).await;
    insert_event(&app, "granted", "space.access_granted", &created).await;
    link(
        &app,
        "happyview_space_access_grant_events",
        "granted",
        "long-over",
    )
    .await;
    insert_event(&app, "unrelated-purge", "event_logs.purged", &created).await;
    insert_event(&app, "plain", "record.skipped", &created).await;

    // With the grants table unreadable, nothing protected is deleted, though
    // the unprotected sweep still runs.
    rename_table(
        &app,
        "happyview_space_access_grants",
        "happyview_space_access_grants_hidden",
    )
    .await;
    run_retention_sweep(&app.state.db, 30, 365, app.state.db_backend).await;
    let unchecked = event_ids(&app).await;
    rename_table(
        &app,
        "happyview_space_access_grants_hidden",
        "happyview_space_access_grants",
    )
    .await;
    assert_eq!(
        unchecked,
        vec!["granted", "unrelated-purge"],
        "a failed grant lookup skips the protected sweep"
    );

    run_retention_sweep(&app.state.db, 30, 365, app.state.db_backend).await;
    assert_eq!(
        event_ids(&app).await,
        vec!["granted"],
        "held while the grant row exists"
    );

    let removed = sweep_ended_access_grants(&app.state.db, 365, app.state.db_backend)
        .await
        .unwrap();
    assert_eq!(removed, 1);
    run_retention_sweep(&app.state.db, 30, 365, app.state.db_backend).await;
    assert!(
        event_ids(&app).await.is_empty(),
        "released with the grant row"
    );
}

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::db::adapt_sql;
use happyview::spaces::types::{AppAccess, Policy, Space, SpaceConfig};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or(json!(null))
}

async fn send(
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

async fn put_setting(app: &TestApp, key: &str, value: &str) {
    let resp = send(
        app,
        "PUT",
        &format!("/admin/settings/{key}"),
        Some(json!({ "value": value })),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

async fn events_of(app: &TestApp, event_type: &str) -> Vec<(Option<String>, String)> {
    let sql = adapt_sql(
        "SELECT subject, detail FROM happyview_event_logs WHERE event_type = ? ORDER BY created_at",
        app.state.db_backend,
    );
    happyview::db::query_as(&sql)
        .bind(event_type)
        .fetch_all(&app.state.db)
        .await
        .unwrap()
}

#[tokio::test]
#[serial]
async fn config_reports_the_inspector_switch() {
    common::require_db!();
    let app = TestApp::new().await;
    let body = json_body(send(&app, "GET", "/config", None).await).await;
    assert_eq!(body["features"]["space_inspector"], false);

    put_setting(&app, "feature.space_inspector_enabled", "true").await;
    let body = json_body(send(&app, "GET", "/config", None).await).await;
    assert_eq!(body["features"]["space_inspector"], true);
}

#[tokio::test]
#[serial]
async fn inspector_is_off_by_default() {
    common::require_db!();
    let app = TestApp::new().await;
    let resp = send(&app, "GET", "/admin/spaces/inspector", None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["enabled"], false);
    assert_eq!(body["default_grant_minutes"], 60);
    assert_eq!(body["max_grant_minutes"], 60);
}

#[tokio::test]
#[serial]
async fn toggling_the_inspector_logs_each_change() {
    common::require_db!();
    let app = TestApp::new().await;
    put_setting(&app, "feature.space_inspector_enabled", "true").await;
    put_setting(&app, "feature.space_inspector_enabled", "false").await;
    assert_eq!(events_of(&app, "space_inspector.enabled").await.len(), 1);
    assert_eq!(events_of(&app, "space_inspector.disabled").await.len(), 1);
}

#[tokio::test]
#[serial]
async fn padded_true_turns_the_inspector_on() {
    common::require_db!();
    let app = TestApp::new().await;
    put_setting(&app, "feature.space_inspector_enabled", " true ").await;
    let body = json_body(send(&app, "GET", "/admin/spaces/inspector", None).await).await;
    assert_eq!(body["enabled"], true);
    assert_eq!(events_of(&app, "space_inspector.enabled").await.len(), 1);
}

#[tokio::test]
#[serial]
async fn saving_the_same_value_twice_logs_once() {
    common::require_db!();
    let app = TestApp::new().await;
    put_setting(&app, "feature.space_inspector_enabled", "true").await;
    put_setting(&app, "feature.space_inspector_enabled", "true").await;
    put_setting(&app, "event_log_retention_days", "14").await;
    put_setting(&app, "event_log_retention_days", "14").await;
    assert_eq!(events_of(&app, "space_inspector.enabled").await.len(), 1);
    let changes = events_of(&app, "event_logs.retention_changed").await;
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].0.as_deref(), Some("event_log_retention_days"));
    let detail: Value = serde_json::from_str(&changes[0].1).unwrap();
    assert_eq!(detail["from"], 30);
    assert_eq!(detail["to"], 14);
}

#[tokio::test]
#[serial]
async fn saving_a_retention_default_logs_nothing() {
    common::require_db!();
    let app = TestApp::new().await;
    put_setting(&app, "space_access_log_retention_days", "365").await;
    put_setting(&app, "event_log_retention_days", "30").await;
    assert!(
        events_of(&app, "event_logs.retention_changed")
            .await
            .is_empty(),
        "an unset retention setting already holds its default"
    );

    put_setting(&app, "space_access_log_retention_days", "14").await;
    let changes = events_of(&app, "event_logs.retention_changed").await;
    assert_eq!(changes.len(), 1);
    let detail: Value = serde_json::from_str(&changes[0].1).unwrap();
    assert_eq!(detail["from"], 365);
    assert_eq!(detail["to"], 14);
}

#[tokio::test]
#[serial]
async fn deleting_the_setting_counts_as_disabling() {
    common::require_db!();
    let app = TestApp::new().await;
    put_setting(&app, "feature.space_inspector_enabled", "true").await;
    let resp = send(
        &app,
        "DELETE",
        "/admin/settings/feature.space_inspector_enabled",
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(events_of(&app, "space_inspector.disabled").await.len(), 1);
}

async fn enable(app: &TestApp) {
    put_setting(app, "feature.space_inspector_enabled", "true").await;
}

async fn create_grant(app: &TestApp, body: Value) -> axum::response::Response {
    send(app, "POST", "/admin/spaces/access-grants", Some(body)).await
}

#[tokio::test]
#[serial]
async fn grant_creation_requires_the_inspector() {
    common::require_db!();
    let app = TestApp::new().await;
    let resp = create_grant(
        &app,
        json!({ "scope": "account", "target": "did:plc:abc", "reason": "r" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(json_body(resp).await["error"], "SpaceInspectorDisabled");
}

#[tokio::test]
#[serial]
async fn grant_is_created_logged_and_listed() {
    common::require_db!();
    let app = TestApp::new().await;
    enable(&app).await;

    let resp = create_grant(
        &app,
        json!({ "scope": "account", "target": "did:plc:abc", "reason": "  report #12 ", "duration_minutes": 30 }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let grant = json_body(resp).await;
    assert_eq!(grant["reason"], "report #12");
    assert_eq!(grant["scope"], "account");
    let created =
        chrono::DateTime::parse_from_rfc3339(grant["created_at"].as_str().unwrap()).unwrap();
    let expires =
        chrono::DateTime::parse_from_rfc3339(grant["expires_at"].as_str().unwrap()).unwrap();
    assert_eq!((expires - created).num_minutes(), 30);

    let events = events_of(&app, "space.access_granted").await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0.as_deref(), Some("did:plc:abc"));
    let detail: Value = serde_json::from_str(&events[0].1).unwrap();
    assert_eq!(detail["grant_id"], grant["id"]);
    assert_eq!(detail["reason"], "report #12");
    assert_eq!(detail["expires_at"], grant["expires_at"]);

    let list =
        json_body(send(&app, "GET", "/admin/spaces/access-grants?active=true", None).await).await;
    assert_eq!(list["grants"][0]["id"], grant["id"]);
}

#[tokio::test]
#[serial]
async fn grant_input_is_validated() {
    common::require_db!();
    let app = TestApp::new().await;
    enable(&app).await;

    let blank = create_grant(
        &app,
        json!({ "scope": "account", "target": "did:plc:abc", "reason": " \n " }),
    )
    .await;
    assert_eq!(blank.status(), StatusCode::BAD_REQUEST);
    let long = create_grant(
        &app,
        json!({ "scope": "account", "target": "did:plc:abc", "reason": "a".repeat(2001) }),
    )
    .await;
    assert_eq!(long.status(), StatusCode::BAD_REQUEST);
    let bad_did = create_grant(
        &app,
        json!({ "scope": "account", "target": "not-a-did", "reason": "r" }),
    )
    .await;
    assert_eq!(bad_did.status(), StatusCode::BAD_REQUEST);
    let no_space = create_grant(
        &app,
        json!({ "scope": "space", "target": "missing", "reason": "r" }),
    )
    .await;
    assert_eq!(no_space.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[serial]
async fn duration_is_clamped_to_the_instance_max() {
    common::require_db!();
    let app = TestApp::new().await;
    enable(&app).await;
    put_setting(&app, "space_inspector_max_grant_minutes", "20").await;

    let grant = json_body(
        create_grant(&app, json!({ "scope": "account", "target": "did:plc:abc", "reason": "r", "duration_minutes": 600 })).await,
    )
    .await;
    let created =
        chrono::DateTime::parse_from_rfc3339(grant["created_at"].as_str().unwrap()).unwrap();
    let expires =
        chrono::DateTime::parse_from_rfc3339(grant["expires_at"].as_str().unwrap()).unwrap();
    assert_eq!((expires - created).num_minutes(), 20);
}

#[tokio::test]
#[serial]
async fn revoking_ends_the_grant_once() {
    common::require_db!();
    let app = TestApp::new().await;
    enable(&app).await;
    let grant = json_body(
        create_grant(
            &app,
            json!({ "scope": "account", "target": "did:plc:abc", "reason": "r" }),
        )
        .await,
    )
    .await;
    let id = grant["id"].as_str().unwrap();

    let resp = send(
        &app,
        "DELETE",
        &format!("/admin/spaces/access-grants/{id}"),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(json_body(resp).await["revoked_at"].is_string());
    send(
        &app,
        "DELETE",
        &format!("/admin/spaces/access-grants/{id}"),
        None,
    )
    .await;
    assert_eq!(
        events_of(&app, "space.access_revoked").await.len(),
        1,
        "a second revoke is a no-op"
    );

    let list =
        json_body(send(&app, "GET", "/admin/spaces/access-grants?active=true", None).await).await;
    assert!(list["grants"].as_array().unwrap().is_empty());
}

/// Insert a grant owned by someone else, bypassing the API.
async fn foreign_grant(app: &TestApp, scope: &str, target: &str) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now();
    let sql = adapt_sql(
        "INSERT INTO happyview_space_access_grants (id, user_id, user_did, scope, target, reason, created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(&id)
        .bind("someone-else")
        .bind("did:plc:someone-else")
        .bind(scope)
        .bind(target)
        .bind("their reason")
        .bind(now.to_rfc3339())
        .bind((now + chrono::Duration::minutes(60)).to_rfc3339())
        .execute(&app.state.db)
        .await
        .unwrap();
    id
}

#[tokio::test]
#[serial]
async fn revoking_someone_elses_grant_needs_users_update() {
    common::require_db!();
    let app = TestApp::new().await;
    enable(&app).await;
    let id = foreign_grant(&app, "account", "did:plc:abc").await;

    let key = common::api_key(&app, &["spaces:inspect"]).await;
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/admin/spaces/access-grants/{id}"))
        .header("authorization", format!("Bearer {key}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // The super admin holds users:update.
    let resp = send(
        &app,
        "DELETE",
        &format!("/admin/spaces/access-grants/{id}"),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let detail: Value =
        serde_json::from_str(&events_of(&app, "space.access_revoked").await[0].1).unwrap();
    assert_eq!(detail["revoked_by"], app.admin_did);
}

async fn revoke_with_key(app: &TestApp, key: &str, id: &str) -> StatusCode {
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/admin/spaces/access-grants/{id}"))
        .header("authorization", format!("Bearer {key}"))
        .body(Body::empty())
        .unwrap();
    app.router.clone().oneshot(req).await.unwrap().status()
}

#[tokio::test]
#[serial]
async fn owners_can_revoke_their_grant_with_users_update() {
    common::require_db!();
    let app = TestApp::new().await;
    enable(&app).await;
    let grant = json_body(
        create_grant(
            &app,
            json!({ "scope": "account", "target": "did:plc:abc", "reason": "r" }),
        )
        .await,
    )
    .await;
    let id = grant["id"].as_str().unwrap();

    let key = common::api_key(&app, &["spaces:read"]).await;
    assert_eq!(revoke_with_key(&app, &key, id).await, StatusCode::FORBIDDEN);

    let key = common::api_key(&app, &["users:update"]).await;
    assert_eq!(revoke_with_key(&app, &key, id).await, StatusCode::OK);
    assert_eq!(events_of(&app, "space.access_revoked").await.len(), 1);
}

async fn seed_space(app: &TestApp) -> String {
    let now = happyview::db::now_rfc3339();
    let id = uuid::Uuid::new_v4().to_string();
    let space = Space {
        id: id.clone(),
        did: "did:plc:spaceaccess-creator".to_string(),
        authority_did: "did:plc:spaceaccess-creator".to_string(),
        creator_did: "did:plc:spaceaccess-creator".to_string(),
        type_nsid: "com.example.spaceaccess".to_string(),
        skey: "main".to_string(),
        display_name: None,
        description: None,
        read_policy: Policy::MemberList,
        write_policy: Policy::MemberList,
        app_access: AppAccess::Open,
        config: SpaceConfig::default(),
        revision: None,
        created_at: now.clone(),
        updated_at: now,
    };
    happyview::spaces::db::create_space(&app.state.db, app.state.db_backend, &space)
        .await
        .unwrap();
    id
}

#[tokio::test]
#[serial]
async fn revoking_a_space_grant_logs_the_space_uri() {
    common::require_db!();
    let app = TestApp::new().await;
    enable(&app).await;
    let space_id = seed_space(&app).await;
    let grant = json_body(
        create_grant(
            &app,
            json!({ "scope": "space", "target": space_id, "reason": "r" }),
        )
        .await,
    )
    .await;
    let id = grant["id"].as_str().unwrap();
    let resp = send(
        &app,
        "DELETE",
        &format!("/admin/spaces/access-grants/{id}"),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let granted = events_of(&app, "space.access_granted").await;
    let revoked = events_of(&app, "space.access_revoked").await;
    assert_eq!(
        granted[0].0.as_deref(),
        Some("at://did:plc:spaceaccess-creator/space/com.example.spaceaccess/main")
    );
    assert_eq!(revoked[0].0, granted[0].0);

    // A grant whose space no longer exists falls back to the raw target.
    let gone = foreign_grant(&app, "space", "deleted-space").await;
    let resp = send(
        &app,
        "DELETE",
        &format!("/admin/spaces/access-grants/{gone}"),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let revoked = events_of(&app, "space.access_revoked").await;
    assert_eq!(revoked[1].0.as_deref(), Some("deleted-space"));
}

async fn rename_events_table(app: &TestApp, from: &str, to: &str) {
    happyview::db::query(&format!("ALTER TABLE {from} RENAME TO {to}"))
        .execute(&app.state.db)
        .await
        .unwrap();
}

async fn grant_count(app: &TestApp) -> i64 {
    happyview::db::query_as::<(i64,)>("SELECT COUNT(*) FROM happyview_space_access_grants")
        .fetch_one(&app.state.db)
        .await
        .unwrap()
        .0
}

#[tokio::test]
#[serial]
async fn a_grant_is_not_created_when_its_audit_event_cannot_be_written() {
    common::require_db!();
    let app = TestApp::new().await;
    enable(&app).await;

    rename_events_table(&app, "happyview_event_logs", "happyview_event_logs_hidden").await;
    let resp = create_grant(
        &app,
        json!({ "scope": "account", "target": "did:plc:abc", "reason": "report" }),
    )
    .await;
    let status = resp.status();
    let grants = grant_count(&app).await;
    rename_events_table(&app, "happyview_event_logs_hidden", "happyview_event_logs").await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(grants, 0, "the grant rolls back with its audit event");
}

#[tokio::test]
#[serial]
async fn active_listing_keeps_an_old_grant_past_newer_ones() {
    common::require_db!();
    let app = TestApp::new().await;
    enable(&app).await;
    let old = json_body(
        create_grant(
            &app,
            json!({ "scope": "account", "target": "did:plc:old", "reason": "r", "duration_minutes": 60 }),
        )
        .await,
    )
    .await;

    // 200 newer, already-expired grants for the same user.
    let user_id: (String,) = happyview::db::query_as(&adapt_sql(
        "SELECT user_id FROM happyview_space_access_grants WHERE id = ?",
        app.state.db_backend,
    ))
    .bind(old["id"].as_str().unwrap())
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let insert = adapt_sql(
        "INSERT INTO happyview_space_access_grants (id, user_id, user_did, scope, target, reason, created_at, expires_at) VALUES (?, ?, 'did:plc:x', 'account', 'did:plc:newer', 'r', ?, ?)",
        app.state.db_backend,
    );
    let now = chrono::Utc::now();
    for i in 0..200 {
        happyview::db::query(&insert)
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(&user_id.0)
            .bind((now + chrono::Duration::seconds(i + 1)).to_rfc3339())
            .bind((now - chrono::Duration::minutes(1)).to_rfc3339())
            .execute(&app.state.db)
            .await
            .unwrap();
    }

    let list =
        json_body(send(&app, "GET", "/admin/spaces/access-grants?active=true", None).await).await;
    let ids: Vec<&str> = list["grants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![old["id"].as_str().unwrap()]);
}

#[tokio::test]
#[serial]
async fn the_inspector_stays_off_when_its_audit_event_cannot_be_written() {
    common::require_db!();
    let app = TestApp::new().await;

    rename_events_table(&app, "happyview_event_logs", "happyview_event_logs_hidden").await;
    let resp = send(
        &app,
        "PUT",
        "/admin/settings/feature.space_inspector_enabled",
        Some(json!({ "value": "true" })),
    )
    .await;
    let status = resp.status();
    let enabled = happyview::feature_flags::is_enabled(
        &app.state.db,
        happyview::feature_flags::FeatureFlag::SPACE_INSPECTOR,
        app.state.db_backend,
    )
    .await;
    rename_events_table(&app, "happyview_event_logs_hidden", "happyview_event_logs").await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!enabled, "the setting rolls back with its audit event");
}

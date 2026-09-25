//! The operator view of migration state.

mod common;

use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

async fn status(app: &TestApp) -> Value {
    let (name, value) = app.admin_cookie();
    let req = Request::builder()
        .method("GET")
        .uri("/admin/spaces/migration-status")
        .header(name, value)
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert!(
        resp.status().is_success(),
        "migration-status failed: {}",
        resp.status()
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

async fn seed_repo(app: &TestApp, space_id: &str, did: &str, mode: &str) {
    let now = happyview::db::now_rfc3339();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_spaces (id, did, authority_did, creator_did, type_nsid, skey, read_policy, write_policy, app_access, config, created_at, updated_at) \
         VALUES (?, ?, ?, ?, 'com.example.forum', ?, '{}', '{}', '{}', '{}', ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(space_id)
        .bind(did)
        .bind(did)
        .bind(did)
        .bind(space_id)
        .bind(&now)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .expect("seed space");

    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_space_repo_state (id, space_id, author_did, lthash_state, host_mode, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(Uuid::new_v4().to_string())
        .bind(space_id)
        .bind(did)
        .bind(vec![0u8; 2048])
        .bind(mode)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .expect("seed repo state");
}

async fn seed_session(app: &TestApp, did: &str, scope: &str) {
    let data = json!({ "token_set": { "scope": scope } }).to_string();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_oauth_sessions (did, session_data) VALUES (?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(did)
        .bind(&data)
        .execute(&app.state.db)
        .await
        .expect("seed session");
}

#[tokio::test]
#[serial]
async fn repos_are_counted_by_host_mode() {
    common::require_db!();
    let app = TestApp::new().await;

    seed_repo(&app, "sp-a", "did:plc:aaaaaaaaaaaaaaaaaaaaaaaa", "polyfill").await;
    seed_repo(&app, "sp-b", "did:plc:bbbbbbbbbbbbbbbbbbbbbbbb", "native").await;

    let body = status(&app).await;
    let spaces = body["spaces"].as_array().expect("spaces array");
    assert_eq!(spaces.len(), 2);

    let a = spaces.iter().find(|s| s["spaceId"] == "sp-a").unwrap();
    assert_eq!(a["polyfill"], json!(1));
    assert_eq!(a["native"], json!(0));

    let b = spaces.iter().find(|s| s["spaceId"] == "sp-b").unwrap();
    assert_eq!(b["native"], json!(1));
}

#[tokio::test]
#[serial]
async fn awaiting_authorization_is_distinguished_from_a_failure() {
    common::require_db!();
    let app = TestApp::new().await;

    // No session at all: awaiting authorization.
    seed_repo(
        &app,
        "sp-waiting",
        "did:plc:aaaaaaaaaaaaaaaaaaaaaaaa",
        "polyfill",
    )
    .await;
    // A session without a space: grant: also awaiting authorization.
    seed_repo(
        &app,
        "sp-nogrant",
        "did:plc:bbbbbbbbbbbbbbbbbbbbbbbb",
        "polyfill",
    )
    .await;
    seed_session(
        &app,
        "did:plc:bbbbbbbbbbbbbbbbbbbbbbbb",
        "atproto repo:com.example.post",
    )
    .await;
    // A session with a space: grant can migrate, so it is not awaiting.
    seed_repo(
        &app,
        "sp-ready",
        "did:plc:cccccccccccccccccccccccc",
        "polyfill",
    )
    .await;
    seed_session(
        &app,
        "did:plc:cccccccccccccccccccccccc",
        "atproto space:com.example.forum",
    )
    .await;

    let body = status(&app).await;
    assert_eq!(
        body["awaitingAuthorization"],
        json!(2),
        "two accounts are waiting on the user; the third can migrate: {body}"
    );
}

#[tokio::test]
#[serial]
async fn detection_results_report_which_tier_answered() {
    common::require_db!();
    let app = TestApp::new().await;

    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_space_pds_support (pds_endpoint, supported, tier, missing, checked_at) VALUES (?, ?, ?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind("https://pds.example")
        .bind(0)
        .bind("unreachable")
        .bind(r#"["com.atproto.space.createRecord"]"#)
        .bind(happyview::db::now_rfc3339())
        .execute(&app.state.db)
        .await
        .expect("seed detection");

    let body = status(&app).await;
    let row = &body["detection"][0];
    assert_eq!(row["pds"], "https://pds.example");
    assert_eq!(row["supported"], json!(false));
    assert_eq!(row["tier"], "unreachable");
    assert_eq!(row["missing"][0], "com.atproto.space.createRecord");
}

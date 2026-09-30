//! `com.atproto.space.listRepos`: the writer set a space host serves to syncers.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::db::now_rfc3339;
use happyview::spaces::db as spaces_db;
use happyview::spaces::types::*;
use happyview::spaces::writers;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

const AUTHORITY: &str = "did:plc:listrepos-authority";
const SPACE_TYPE: &str = "com.example.listrepos";

async fn enable_spaces(app: &TestApp) {
    let (name, value) = app.admin_cookie();
    let req = Request::builder()
        .method("PUT")
        .uri("/admin/settings/feature.spaces_enabled")
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(json!({ "value": "true" }).to_string()))
        .unwrap();
    assert!(
        app.router
            .clone()
            .oneshot(req)
            .await
            .unwrap()
            .status()
            .is_success()
    );
}

/// A space anyone may list, so these tests are about the listing alone.
async fn create_space(app: &TestApp) -> (String, String) {
    let now = now_rfc3339();
    let id = Uuid::new_v4().to_string();
    let skey = format!("s{}", Uuid::new_v4().simple());
    let space = Space {
        id: id.clone(),
        did: AUTHORITY.to_string(),
        authority_did: AUTHORITY.to_string(),
        creator_did: AUTHORITY.to_string(),
        type_nsid: SPACE_TYPE.to_string(),
        skey: skey.clone(),
        display_name: None,
        description: None,
        read_policy: Policy::MemberList,
        write_policy: Policy::MemberList,
        app_access: AppAccess::Open,
        config: SpaceConfig {
            membership_public: true,
            ..Default::default()
        },
        revision: None,
        created_at: now.clone(),
        updated_at: now,
    };
    spaces_db::create_space(&app.state.db, app.state.db_backend, &space)
        .await
        .expect("create_space failed");
    (id, format!("at://{AUTHORITY}/space/{SPACE_TYPE}/{skey}"))
}

/// The current time as a TID.
fn tid_now() -> String {
    const ALPHABET: &[u8; 32] = b"234567abcdefghijklmnopqrstuvwxyz";
    let micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64;
    let value = micros << 10;
    (0..13)
        .rev()
        .map(|i| ALPHABET[((value >> (i * 5)) & 31) as usize] as char)
        .collect()
}

/// Report `repo` as having written, returning the space revision it got.
async fn write(app: &TestApp, space_id: &str, repo: &str) -> String {
    let mut conn = app.state.db.acquire().await.unwrap();
    let rev = tid_now();
    match writers::record(
        &mut conn,
        app.state.db_backend,
        space_id,
        repo,
        &rev,
        &[7; 32],
    )
    .await
    .unwrap()
    {
        writers::Recorded::Advanced { space_rev, .. } => space_rev,
        writers::Recorded::Stale => panic!("a fresh rev should advance"),
    }
}

async fn list(app: &TestApp, query: &str) -> Value {
    let req = Request::builder()
        .uri(format!("/xrpc/com.atproto.space.listRepos?{query}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

fn dids(body: &Value) -> Vec<String> {
    body["repos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["did"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
#[serial]
async fn lists_each_writer_with_its_rev_and_hash() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    let (space_id, space) = create_space(&app).await;
    write(&app, &space_id, "did:plc:alice").await;

    let body = list(&app, &format!("space={}", urlencoding::encode(&space))).await;
    let repo = &body["repos"][0];
    assert_eq!(repo["did"], json!("did:plc:alice"));
    assert!(repo["rev"].is_string());
    assert!(repo["hash"]["$bytes"].is_string());
}

#[tokio::test]
#[serial]
async fn pages_through_writers_with_a_cursor() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    let (space_id, space) = create_space(&app).await;
    for did in ["did:plc:a", "did:plc:b", "did:plc:c"] {
        write(&app, &space_id, did).await;
    }

    let space = urlencoding::encode(&space);
    let first = list(&app, &format!("space={space}&limit=2")).await;
    assert_eq!(dids(&first), ["did:plc:a", "did:plc:b"]);
    let cursor = first["cursor"].as_str().expect("a full page has a cursor");

    let second = list(
        &app,
        &format!(
            "space={space}&limit=2&cursor={}",
            urlencoding::encode(cursor)
        ),
    )
    .await;
    assert_eq!(dids(&second), ["did:plc:c"]);
}

#[tokio::test]
#[serial]
async fn since_lists_only_repos_updated_after_a_space_revision() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    let (space_id, space) = create_space(&app).await;
    write(&app, &space_id, "did:plc:early").await;
    let seen = write(&app, &space_id, "did:plc:middle").await;
    let latest = write(&app, &space_id, "did:plc:late").await;

    let body = list(
        &app,
        &format!("space={}&since={seen}", urlencoding::encode(&space)),
    )
    .await;
    assert_eq!(dids(&body), ["did:plc:late"]);
    assert_eq!(body["spaceRev"], json!(latest));
}

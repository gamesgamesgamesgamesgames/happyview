mod common;

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Request, StatusCode};
use happyview::db::now_rfc3339;
use happyview::spaces::db as spaces_db;
use happyview::spaces::types::*;
use serde_json::json;
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

const AUTHORITY: &str = "did:plc:getspace-authority";
const MEMBER: &str = "did:plc:getspace-member";

fn space_uri(skey: &str) -> String {
    format!("at://{AUTHORITY}/space/com.example.getspace/{skey}")
}

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
            .is_success(),
        "failed to enable spaces"
    );
}

async fn create_space(app: &TestApp, skey: &str, membership_public: bool) -> String {
    let now = now_rfc3339();
    let id = Uuid::new_v4().to_string();
    let space = Space {
        id: id.clone(),
        did: AUTHORITY.to_string(),
        authority_did: AUTHORITY.to_string(),
        creator_did: AUTHORITY.to_string(),
        type_nsid: "com.example.getspace".to_string(),
        skey: skey.to_string(),
        display_name: None,
        description: None,
        read_policy: Policy::MemberList,
        write_policy: Policy::MemberList,
        app_access: AppAccess::Open,
        config: SpaceConfig {
            membership_public,
            ..Default::default()
        },
        revision: None,
        created_at: now.clone(),
        updated_at: now,
    };
    spaces_db::create_space(&app.state.db, app.state.db_backend, &space)
        .await
        .expect("create_space failed");
    id
}

async fn add_member(app: &TestApp, space_id: &str, did: &str) {
    spaces_db::add_member(
        &app.state.db,
        app.state.db_backend,
        &SpaceMember {
            id: Uuid::new_v4().to_string(),
            space_id: space_id.to_string(),
            did: did.to_string(),
            access: MemberAccess::READ,
            is_delegation: false,
            granted_by: Some(AUTHORITY.to_string()),
            created_at: now_rfc3339(),
        },
    )
    .await
    .expect("add_member failed");
}

fn cookie_for(app: &TestApp, did: &str) -> (HeaderName, HeaderValue) {
    common::auth::admin_cookie_header(did, &app.state.cookie_key)
}

fn get_space_req(skey: &str, cookie: Option<(HeaderName, HeaderValue)>) -> Request<Body> {
    let mut b = Request::builder().method("GET").uri(format!(
        "/xrpc/com.atproto.simplespace.getSpace?space={}",
        urlencoding::encode(&space_uri(skey))
    ));
    if let Some((name, value)) = cookie {
        b = b.header(name, value);
    }
    b.body(Body::empty()).unwrap()
}

/// The space owner can read the space.
#[tokio::test]
#[serial]
async fn get_space_allows_owner() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    create_space(&app, "owner", false).await;

    let resp = app
        .router
        .clone()
        .oneshot(get_space_req("owner", Some(cookie_for(&app, AUTHORITY))))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// A non-owner member must present a space credential, not an account session.
#[tokio::test]
#[serial]
async fn get_space_rejects_non_owner_member() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    let id = create_space(&app, "member", false).await;
    add_member(&app, &id, MEMBER).await;

    let resp = app
        .router
        .clone()
        .oneshot(get_space_req("member", Some(cookie_for(&app, MEMBER))))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// Outsiders cannot read the space even when membership is public on it.
#[tokio::test]
#[serial]
async fn get_space_public_rejects_anonymous() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    create_space(&app, "pub", true).await;

    let resp = app
        .router
        .clone()
        .oneshot(get_space_req("pub", None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

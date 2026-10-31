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

const AUTHORITY: &str = "did:plc:listrepos-authority";
const MEMBER: &str = "did:plc:listrepos-member";
const OUTSIDER: &str = "did:plc:listrepos-outsider";

fn space_uri(skey: &str) -> String {
    format!("at://{AUTHORITY}/space/com.example.listrepos/{skey}")
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

/// Create a space with the given skey and `membership_public`. Returns its id.
async fn create_space(app: &TestApp, skey: &str, membership_public: bool) -> String {
    let now = now_rfc3339();
    let id = Uuid::new_v4().to_string();
    let space = Space {
        id: id.clone(),
        did: AUTHORITY.to_string(),
        authority_did: AUTHORITY.to_string(),
        creator_did: AUTHORITY.to_string(),
        type_nsid: "com.example.listrepos".to_string(),
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
    add_member_with(app, space_id, did, MemberAccess::READ).await;
}

async fn add_member_with(app: &TestApp, space_id: &str, did: &str, access: MemberAccess) {
    spaces_db::add_member(
        &app.state.db,
        app.state.db_backend,
        &SpaceMember {
            id: Uuid::new_v4().to_string(),
            space_id: space_id.to_string(),
            did: did.to_string(),
            access,
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

fn list_repos_req(skey: &str, cookie: Option<(HeaderName, HeaderValue)>) -> Request<Body> {
    let mut b = Request::builder().method("GET").uri(format!(
        "/xrpc/com.atproto.space.listRepos?space={}",
        urlencoding::encode(&space_uri(skey))
    ));
    if let Some((name, value)) = cookie {
        b = b.header(name, value);
    }
    b.body(Body::empty()).unwrap()
}

/// A private space must not leak its participant list to an authenticated
/// non-member. Before the fix this returned 200.
#[tokio::test]
#[serial]
async fn list_repos_private_rejects_non_member() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    create_space(&app, "priv", false).await;

    let resp = app
        .router
        .clone()
        .oneshot(list_repos_req("priv", Some(cookie_for(&app, OUTSIDER))))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

/// A member of the private space can list its repos.
#[tokio::test]
#[serial]
async fn list_repos_private_allows_member() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    let id = create_space(&app, "priv2", false).await;
    add_member(&app, &id, MEMBER).await;

    let resp = app
        .router
        .clone()
        .oneshot(list_repos_req("priv2", Some(cookie_for(&app, MEMBER))))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// A member who may read only their own repo cannot list everyone's.
#[tokio::test]
#[serial]
async fn list_repos_rejects_a_read_self_member() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    let id = create_space(&app, "self", false).await;
    add_member_with(&app, &id, MEMBER, MemberAccess::READ_SELF).await;

    let resp = app
        .router
        .clone()
        .oneshot(list_repos_req("self", Some(cookie_for(&app, MEMBER))))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

/// Outsiders cannot list repos even when membership is public on the space.
#[tokio::test]
#[serial]
async fn list_repos_public_rejects_anonymous() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    create_space(&app, "pub", true).await;

    let resp = app
        .router
        .clone()
        .oneshot(list_repos_req("pub", None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

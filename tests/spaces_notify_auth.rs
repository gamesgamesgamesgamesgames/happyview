mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::db::now_rfc3339;
use happyview::spaces::db as spaces_db;
use happyview::spaces::types::*;
use serde_json::json;
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

const SPACE_DID: &str = "did:plc:spacehost";
const SPACE_TYPE: &str = "com.example.notify";
const SPACE_SKEY: &str = "main";

fn space_uri() -> String {
    format!("at://{SPACE_DID}/space/{SPACE_TYPE}/{SPACE_SKEY}")
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
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert!(resp.status().is_success(), "failed to enable spaces flag");
}

async fn create_space(app: &TestApp) {
    let now = now_rfc3339();
    let space = Space {
        id: Uuid::new_v4().to_string(),
        did: SPACE_DID.to_string(),
        authority_did: SPACE_DID.to_string(),
        creator_did: SPACE_DID.to_string(),
        type_nsid: SPACE_TYPE.to_string(),
        skey: SPACE_SKEY.to_string(),
        display_name: Some("Notify Space".to_string()),
        description: None,
        read_policy: Policy::MemberList,
        write_policy: Policy::MemberList,
        app_access: AppAccess::Open,
        config: SpaceConfig::default(),
        revision: None,
        created_at: now.clone(),
        updated_at: now,
    };
    spaces_db::create_space(&app.state.db, app.state.db_backend, &space)
        .await
        .expect("create_space failed");
}

fn notify_write_req(
    auth_cookie: Option<(axum::http::HeaderName, axum::http::HeaderValue)>,
) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.notifyWrite")
        .header("content-type", "application/json");
    if let Some((name, value)) = auth_cookie {
        b = b.header(name, value);
    }
    b.body(Body::from(
        json!({
            "space": space_uri(),
            "did": "did:plc:someauthor",
            "collection": "com.example.post",
            "rkey": "rk1",
        })
        .to_string(),
    ))
    .unwrap()
}

/// An unauthenticated caller must NOT be able to fire write notifications.
/// Before the fix this returned success (the caller was ignored entirely).
#[tokio::test]
#[serial]
async fn notify_write_rejects_unauthenticated() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    create_space(&app).await;

    let resp = app
        .router
        .clone()
        .oneshot(notify_write_req(None))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "unauthenticated notifyWrite must be rejected"
    );
}

/// A super admin is allowed (require_space_admin accepts authority or super).
#[tokio::test]
#[serial]
async fn notify_write_allows_super_admin() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    create_space(&app).await;

    let resp = app
        .router
        .clone()
        .oneshot(notify_write_req(Some(app.admin_cookie())))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// notifySpaceDeleted is likewise gated.
#[tokio::test]
#[serial]
async fn notify_space_deleted_rejects_unauthenticated() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    create_space(&app).await;

    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.notifySpaceDeleted")
        .header("content-type", "application/json")
        .body(Body::from(json!({ "space": space_uri() }).to_string()))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// A space whose authority is this instance, as spaces created here now are.
async fn create_instance_space(app: &TestApp, instance_did: &str) -> String {
    let now = now_rfc3339();
    let space = Space {
        id: Uuid::new_v4().to_string(),
        did: instance_did.to_string(),
        authority_did: instance_did.to_string(),
        creator_did: "did:plc:creator".to_string(),
        type_nsid: SPACE_TYPE.to_string(),
        skey: SPACE_SKEY.to_string(),
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
    spaces_db::create_space(&app.state.db, app.state.db_backend, &space)
        .await
        .expect("create_space failed");
    format!("at://{instance_did}/space/{SPACE_TYPE}/{SPACE_SKEY}")
}

/// Send `notifyWrite` in the lexicon's shape, as a repo host does, with service
/// auth signed by `signer` and addressed to this instance's space host.
async fn notify_write_as(signer: &str, repo: &str) -> StatusCode {
    let mut app = TestApp::new().await;
    let plc_store = common::plc::setup_mock_plc(&app.mock_server).await;
    let instance_did = app.setup_did_web().await;
    enable_spaces(&app).await;
    let space = create_instance_space(&app, &instance_did).await;

    let auth = app
        .service_auth_jwt_for(
            &plc_store,
            signer,
            &instance_did,
            "#atproto_space_host",
            Some("com.atproto.space.notifyWrite"),
        )
        .await;

    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.notifyWrite")
        .header("content-type", "application/json")
        .header("authorization", auth)
        .header("host", "127.0.0.1:0")
        .body(Body::from(
            json!({
                "space": space,
                "repo": repo,
                "rev": "3lzq2b3k4c22a",
                "hash": { "$bytes": "q83vEjRWeJq83vEjRWeJq83vEjRWeJq83vEjRWeJq80" },
            })
            .to_string(),
        ))
        .unwrap();
    app.router.clone().oneshot(req).await.unwrap().status()
}

/// A repo host notifies with service auth signed as the account that wrote.
#[tokio::test]
#[serial]
async fn notify_write_accepts_the_writers_repo_host() {
    common::require_db!();
    let status = notify_write_as("did:plc:writer", "did:plc:writer").await;
    assert_eq!(status, StatusCode::OK);
}

/// One account cannot report writes to another account's repo.
#[tokio::test]
#[serial]
async fn notify_write_rejects_a_notification_for_someone_elses_repo() {
    common::require_db!();
    let status = notify_write_as("did:plc:intruder", "did:plc:writer").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

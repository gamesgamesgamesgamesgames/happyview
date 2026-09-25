mod common;

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Request, StatusCode};
use happyview::db::now_rfc3339;
use happyview::spaces::db as spaces_db;
use happyview::spaces::types::*;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

const AUTHORITY: &str = "did:plc:mint-authority";
const MEMBER: &str = "did:plc:mint-member";
const ATTACKER: &str = "did:plc:mint-attacker";

fn space_uri() -> String {
    format!("at://{AUTHORITY}/space/com.example.mint/main")
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

fn cookie_for(app: &TestApp, did: &str) -> (HeaderName, HeaderValue) {
    common::auth::admin_cookie_header(did, &app.state.cookie_key)
}

async fn json_of(resp: axum::http::Response<Body>) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or(json!(null))
}

/// Set up a space with `MEMBER` as a read member, and return a fresh delegation
/// token issued to `MEMBER`.
async fn setup_and_get_delegation_token(app: &TestApp) -> String {
    enable_spaces(app).await;

    let space_id = Uuid::new_v4().to_string();
    let now = now_rfc3339();
    let space = Space {
        id: space_id.clone(),
        did: AUTHORITY.to_string(),
        authority_did: AUTHORITY.to_string(),
        creator_did: AUTHORITY.to_string(),
        type_nsid: "com.example.mint".to_string(),
        skey: "main".to_string(),
        display_name: None,
        description: None,
        read_policy: Policy::MemberList,
        write_policy: Policy::MemberList,
        app_access: AppAccess::Open,
        config: SpaceConfig::default(),
        revision: None,
        created_at: now.clone(),
        updated_at: now.clone(),
    };
    spaces_db::create_space(&app.state.db, app.state.db_backend, &space)
        .await
        .unwrap();
    spaces_db::add_member(
        &app.state.db,
        app.state.db_backend,
        &SpaceMember {
            id: Uuid::new_v4().to_string(),
            space_id,
            did: MEMBER.to_string(),
            access: MemberAccess::READ,
            is_delegation: false,
            granted_by: Some(AUTHORITY.to_string()),
            created_at: now,
        },
    )
    .await
    .unwrap();

    // MEMBER obtains a delegation token (proof of membership).
    let (name, value) = cookie_for(app, MEMBER);
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/xrpc/com.atproto.space.getDelegationToken?space={}",
            urlencoding::encode(&space_uri())
        ))
        .header(name, value)
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "getDelegationToken failed");
    json_of(resp).await["delegationToken"]
        .as_str()
        .expect("delegationToken")
        .to_string()
}

fn get_credential_req(grant: &str, cookie: (HeaderName, HeaderValue)) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.getSpaceCredential")
        .header(cookie.0, cookie.1)
        .header("content-type", "application/json")
        .body(Body::from(json!({ "grant": grant }).to_string()))
        .unwrap()
}

/// An attacker who captures a member's delegation token cannot mint a credential
/// in the member's name. Before the fix this returned 200.
#[tokio::test]
#[serial]
async fn get_space_credential_rejects_foreign_caller() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let grant = setup_and_get_delegation_token(&app).await;

    let resp = app
        .router
        .clone()
        .oneshot(get_credential_req(&grant, cookie_for(&app, ATTACKER)))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "a caller must not mint a credential from another member's delegation token"
    );
}

/// The member the delegation token was issued to can mint their own credential.
#[tokio::test]
#[serial]
async fn get_space_credential_allows_own_caller() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let grant = setup_and_get_delegation_token(&app).await;

    let resp = app
        .router
        .clone()
        .oneshot(get_credential_req(&grant, cookie_for(&app, MEMBER)))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_of(resp).await;
    assert!(
        body["credential"].as_str().is_some(),
        "expected a credential"
    );
}

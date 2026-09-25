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

const AUTHORITY: &str = "did:plc:cred-authority";
const MEMBER: &str = "did:plc:cred-holder";

fn space_uri() -> String {
    format!("at://{AUTHORITY}/space/com.example.cred/main")
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

/// Removing a member from a space revokes their outstanding space credentials.
/// Before the fix, removeMember left the credential active for its full TTL.
#[tokio::test]
#[serial]
async fn remove_member_revokes_credentials() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    // Create a space and a member.
    let space_id = Uuid::new_v4().to_string();
    let now = now_rfc3339();
    let space = Space {
        id: space_id.clone(),
        did: AUTHORITY.to_string(),
        authority_did: AUTHORITY.to_string(),
        creator_did: AUTHORITY.to_string(),
        type_nsid: "com.example.cred".to_string(),
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
            space_id: space_id.clone(),
            did: MEMBER.to_string(),
            access: MemberAccess::READ,
            is_delegation: false,
            granted_by: Some(AUTHORITY.to_string()),
            created_at: now.clone(),
        },
    )
    .await
    .unwrap();

    // The member holds an outstanding credential (represented by its hash).
    let token_hash = "member-credential-hash";
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_space_credentials (id, space_id, issued_to, token_hash, expires_at, created_at) VALUES (?, ?, ?, ?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(Uuid::new_v4().to_string())
        .bind(&space_id)
        .bind(MEMBER)
        .bind(token_hash)
        .bind(&now)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .unwrap();

    assert!(
        !spaces_db::is_space_credential_revoked(&app.state.db, app.state.db_backend, token_hash)
            .await
            .unwrap()
    );

    // The authority removes the member.
    let (cookie_name, cookie_val) =
        common::auth::admin_cookie_header(AUTHORITY, &app.state.cookie_key);
    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.simplespace.removeMember")
        .header(cookie_name, cookie_val)
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "space": space_uri(), "did": MEMBER }).to_string(),
        ))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // The credential is now revoked.
    assert!(
        spaces_db::is_space_credential_revoked(&app.state.db, app.state.db_backend, token_hash)
            .await
            .unwrap(),
        "removing a member must revoke their outstanding credentials"
    );
}

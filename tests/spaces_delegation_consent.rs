//! Space-as-member entries (`isDelegation`): naming another space as a member
//! brings in its members only when whoever adds the entry administers that
//! space too.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::db::now_rfc3339;
use happyview::spaces::db as spaces_db;
use happyview::spaces::types::*;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

const TYPE: &str = "com.example.delegation";

fn rand_did(label: &str) -> String {
    format!("did:plc:{label}{}", Uuid::new_v4().simple())
}

async fn setup() -> TestApp {
    let app = TestApp::new().await;
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
    app
}

/// A member-list space created by `creator`, with `creator` as its only
/// member. Returns (id, address).
async fn create_space(app: &TestApp, creator: &str) -> (String, String) {
    let now = now_rfc3339();
    let id = Uuid::new_v4().to_string();
    let skey = Uuid::new_v4().simple().to_string();
    let space = Space {
        id: id.clone(),
        did: creator.to_string(),
        authority_did: creator.to_string(),
        creator_did: creator.to_string(),
        type_nsid: TYPE.to_string(),
        skey: skey.clone(),
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
        .unwrap();
    add_row(app, &id, creator, false, creator).await;
    (id, format!("at://{creator}/space/{TYPE}/{skey}"))
}

/// A member row stored directly, as an older version would have stored it.
async fn add_row(app: &TestApp, space_id: &str, did: &str, is_delegation: bool, granted_by: &str) {
    spaces_db::add_member(
        &app.state.db,
        app.state.db_backend,
        &SpaceMember {
            id: Uuid::new_v4().to_string(),
            space_id: space_id.to_string(),
            did: did.to_string(),
            access: MemberAccess::READ,
            is_delegation,
            granted_by: Some(granted_by.to_string()),
            created_at: now_rfc3339(),
        },
    )
    .await
    .unwrap();
}

async fn post(app: &TestApp, did: &str, nsid: &str, body: Value) -> (StatusCode, Value) {
    let (name, value) = common::auth::admin_cookie_header(did, &app.state.cookie_key);
    let req = Request::builder()
        .method("POST")
        .uri(format!("/xrpc/{nsid}"))
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap_or(json!(null)))
}

async fn listed(app: &TestApp, actor: &str, address: &str) -> Vec<String> {
    let (name, value) = common::auth::admin_cookie_header(actor, &app.state.cookie_key);
    let req = Request::builder()
        .uri(format!(
            "/xrpc/com.atproto.simplespace.listMembers?space={}",
            urlencoding::encode(address)
        ))
        .header(name, value)
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    body["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["did"].as_str().unwrap().to_string())
        .collect()
}

async fn is_member(app: &TestApp, space_id: &str, did: &str) -> bool {
    happyview::spaces::members::is_member(&app.state.db, app.state.db_backend, space_id, did)
        .await
        .unwrap()
        .is_some()
}

async fn stored(app: &TestApp, space_id: &str, did: &str) -> bool {
    spaces_db::get_member(&app.state.db, app.state.db_backend, space_id, did)
        .await
        .unwrap()
        .is_some()
}

#[tokio::test]
#[serial]
async fn delegating_to_a_space_requires_administering_it() {
    common::require_db!();
    let app = setup().await;
    let (x_owner, y_owner, y_member) = (rand_did("x"), rand_did("y"), rand_did("ym"));
    let (x, x_addr) = create_space(&app, &x_owner).await;
    let (y, y_addr) = create_space(&app, &y_owner).await;
    add_row(&app, &y, &y_member, false, &y_owner).await;

    // By address or by id, through either method.
    for target in [&y_addr, &y] {
        for (nsid, body) in [
            (
                "com.atproto.simplespace.putMember",
                json!({ "space": x_addr, "did": target, "read": true, "write": false, "isDelegation": true }),
            ),
            (
                "com.atproto.simplespace.addMember",
                json!({ "space": x_addr, "did": target, "isDelegation": true }),
            ),
        ] {
            let (status, body) = post(&app, &x_owner, nsid, body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{nsid} {target}: {body}");
            assert!(!stored(&app, &x, target).await, "{nsid} {target}");
        }
    }
    assert!(!is_member(&app, &x, &y_member).await);
    assert!(!listed(&app, &x_owner, &x_addr).await.contains(&y_member));
}

#[tokio::test]
#[serial]
async fn delegating_to_a_space_you_administer_brings_in_its_members() {
    common::require_db!();
    let app = setup().await;
    let (owner, member) = (rand_did("o"), rand_did("m"));
    let (x, x_addr) = create_space(&app, &owner).await;
    let (y, y_addr) = create_space(&app, &owner).await;
    add_row(&app, &y, &member, false, &owner).await;

    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.simplespace.putMember",
        json!({ "space": x_addr, "did": y_addr, "read": true, "write": false, "isDelegation": true }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(is_member(&app, &x, &member).await);
    // listMembers keeps its shape: the named space's members are listed.
    assert!(listed(&app, &owner, &x_addr).await.contains(&member));

    // By id, through addMember.
    let (z, z_addr) = create_space(&app, &owner).await;
    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.simplespace.addMember",
        json!({ "space": z_addr, "did": y, "isDelegation": true }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(is_member(&app, &z, &member).await);
}

#[tokio::test]
#[serial]
async fn an_entry_stored_without_the_named_spaces_admin_brings_in_no_members() {
    common::require_db!();
    let app = setup().await;
    let (x_owner, y_owner, y_member) = (rand_did("x"), rand_did("y"), rand_did("ym"));
    let (x, x_addr) = create_space(&app, &x_owner).await;
    let (y, y_addr) = create_space(&app, &y_owner).await;
    add_row(&app, &y, &y_member, false, &y_owner).await;

    // Stored as an older version stored it, by X's creator alone.
    add_row(&app, &x, &y_addr, true, &x_owner).await;
    assert!(stored(&app, &x, &y_addr).await);
    assert!(!is_member(&app, &x, &y_member).await);
    assert!(!listed(&app, &x_owner, &x_addr).await.contains(&y_member));
}

/// Entries naming a space not on this host are stored as they always were,
/// and bring in nothing, even once a space with that address appears.
#[tokio::test]
#[serial]
async fn an_entry_naming_a_space_not_here_brings_in_nothing() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("o");
    let (x, x_addr) = create_space(&app, &owner).await;
    let elsewhere = format!("at://{}/space/{TYPE}/elsewhere", rand_did("r"));
    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.simplespace.putMember",
        json!({ "space": x_addr, "did": elsewhere, "read": true, "write": false, "isDelegation": true }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(stored(&app, &x, &elsewhere).await);
    assert!(
        spaces_db::consented_delegations(&app.state.db, app.state.db_backend, &x)
            .await
            .unwrap()
            .is_empty()
    );
}

/// The migration's backfill counts an entry already stored as added by the
/// named space's administrator when its adder is that space's creator or a
/// super admin, or both spaces have the same creator.
#[tokio::test]
#[serial]
async fn stored_entries_are_backfilled_by_who_added_them() {
    common::require_db!();
    let app = setup().await;
    let (x_owner, y_owner, y_member) = (rand_did("x"), rand_did("y"), rand_did("ym"));
    let (y, y_addr) = create_space(&app, &y_owner).await;
    add_row(&app, &y, &y_member, false, &y_owner).await;

    // Unconsented: X's creator alone.
    let (unconsented, _) = create_space(&app, &x_owner).await;
    add_row(&app, &unconsented, &y_addr, true, &x_owner).await;
    // Added by Y's creator.
    let (by_target, _) = create_space(&app, &x_owner).await;
    add_row(&app, &by_target, &y_addr, true, &y_owner).await;
    // Same creator, by id.
    let (same_creator, _) = create_space(&app, &y_owner).await;
    add_row(&app, &same_creator, &y, true, &y_owner).await;
    // By the instance's super admin.
    let (by_super, _) = create_space(&app, &x_owner).await;
    add_row(&app, &by_super, &y_addr, true, &app.admin_did).await;

    let migration =
        include_str!("../migrations/postgres/20261118180000_space_delegation_consent.sql");
    let backfill = &migration[migration.find("UPDATE").unwrap()..];
    happyview::db::query(backfill)
        .execute(&app.state.db)
        .await
        .unwrap();

    assert!(!is_member(&app, &unconsented, &y_member).await);
    assert!(is_member(&app, &by_target, &y_member).await);
    assert!(is_member(&app, &same_creator, &y_member).await);
    assert!(is_member(&app, &by_super, &y_member).await);
}

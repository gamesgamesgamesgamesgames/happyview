//! `com.atproto.space.{createSpace, updateSpace, getSpace, deleteSpace}` over
//! HTTP: a space's lifecycle, configured by one `policy`.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

const MEMBERSPACE: &str = "com.atproto.memberspace.policy";
const MANAGEDSPACE: &str = "com.atproto.managedspace.policy";

fn rand_did(label: &str) -> String {
    format!("did:plc:{label}{}", Uuid::new_v4().simple())
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
            .is_success()
    );
}

async fn json_of(resp: axum::http::Response<Body>) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or(json!(null))
}

async fn post(app: &TestApp, method: &str, did: &str, body: Value) -> (StatusCode, Value) {
    let (name, value) = common::auth::admin_cookie_header(did, &app.state.cookie_key);
    let req = Request::builder()
        .method("POST")
        .uri(format!("/xrpc/{method}"))
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    (status, json_of(resp).await)
}

async fn get_space(app: &TestApp, space: &str, did: &str) -> (StatusCode, Value) {
    let (name, value) = common::auth::admin_cookie_header(did, &app.state.cookie_key);
    let req = Request::builder()
        .uri(format!(
            "/xrpc/com.atproto.space.getSpace?space={}",
            urlencoding::encode(space)
        ))
        .header(name, value)
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    (status, json_of(resp).await)
}

async fn create(app: &TestApp, creator: &str, skey: &str, policy: Value) -> String {
    let (status, body) = post(
        app,
        "com.atproto.space.createSpace",
        creator,
        json!({ "spaceType": "com.example.forum", "skey": skey, "policy": policy }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["uri"].as_str().unwrap().to_string()
}

async fn setup() -> (TestApp, String) {
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    (app, rand_did("creator"))
}

/// A space is created with a policy, on a DID of its own, and describes
/// itself with that policy and the defaults the lexicon gives it.
#[tokio::test]
#[serial]
async fn a_space_is_created_with_its_policy() {
    common::require_db!();
    let (app, creator) = setup().await;
    let space = create(
        &app,
        &creator,
        "main",
        json!({ "$type": MEMBERSPACE, "publicRead": true }),
    )
    .await;
    assert!(space.starts_with("at://did:plc:"), "{space}");
    assert!(space.ends_with("/space/com.example.forum/main"), "{space}");

    let (status, body) = get_space(&app, &space, &creator).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["uri"], json!(space));
    assert_eq!(
        body["policy"],
        json!({
            "$type": MEMBERSPACE,
            "publicRead": true,
            "publicWrite": false,
            "publishPolicy": false,
            "publishMembers": false,
        })
    );
}

/// With no `skey`, the space is keyed by a TID.
#[tokio::test]
#[serial]
async fn a_space_without_an_skey_gets_a_tid() {
    common::require_db!();
    let (app, creator) = setup().await;
    let (status, body) = post(
        &app,
        "com.atproto.space.createSpace",
        &creator,
        json!({ "spaceType": "com.example.forum", "policy": { "$type": MEMBERSPACE } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let skey = body["uri"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_string();
    assert_eq!(skey.len(), 13, "{skey}");
}

#[tokio::test]
#[serial]
async fn a_second_space_with_one_type_and_key_already_exists() {
    common::require_db!();
    let (app, creator) = setup().await;
    create(&app, &creator, "main", json!({ "$type": MEMBERSPACE })).await;
    let (status, body) = post(
        &app,
        "com.atproto.space.createSpace",
        &creator,
        json!({ "spaceType": "com.example.forum", "skey": "main", "policy": { "$type": MEMBERSPACE } }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], json!("SpaceAlreadyExists"));
}

/// `personalspace`, and any policy this host does not implement, is refused
/// by name rather than stored.
#[tokio::test]
#[serial]
async fn an_unimplemented_policy_is_unsupported() {
    common::require_db!();
    let (app, creator) = setup().await;
    for policy in [
        json!({ "$type": "com.atproto.personalspace.policy" }),
        json!({ "$type": "com.atproto.simplespace.defs#memberListPolicy" }),
    ] {
        let (status, body) = post(
            &app,
            "com.atproto.space.createSpace",
            &creator,
            json!({ "spaceType": "com.example.forum", "skey": "x", "policy": policy }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{policy}");
        assert_eq!(body["error"], json!("UnsupportedPolicy"), "{policy}");
    }
}

#[tokio::test]
#[serial]
async fn a_managed_space_round_trips() {
    common::require_db!();
    let (app, creator) = setup().await;
    let policy = json!({ "$type": MANAGEDSPACE, "managingApp": "did:web:app.example#forum", "publishPolicy": false });
    let space = create(&app, &creator, "managed", policy.clone()).await;
    let (_, body) = get_space(&app, &space, &creator).await;
    assert_eq!(body["policy"], policy);
}

/// `updateSpace` replaces the policy wholesale, and only the space's owner may.
#[tokio::test]
#[serial]
async fn the_owner_replaces_the_policy() {
    common::require_db!();
    let (app, creator) = setup().await;
    let space = create(
        &app,
        &creator,
        "main",
        json!({ "$type": MEMBERSPACE, "publicRead": true }),
    )
    .await;

    let (status, body) = post(
        &app,
        "com.atproto.space.updateSpace",
        &rand_did("stranger"),
        json!({ "space": space, "policy": { "$type": MEMBERSPACE } }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], json!("NotSpaceOwner"));

    let (status, body) = post(
        &app,
        "com.atproto.space.updateSpace",
        &creator,
        json!({ "space": space, "policy": { "$type": MEMBERSPACE, "publicWrite": true } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, body) = get_space(&app, &space, &creator).await;
    assert_eq!(
        body["policy"]["publicRead"],
        json!(false),
        "replaced, not merged"
    );
    assert_eq!(body["policy"]["publicWrite"], json!(true));
}

/// A policy of another `$type` migrates the space. Its member list, which only
/// `memberspace` keeps, is discarded, and migrating back does not restore it.
#[tokio::test]
#[serial]
async fn migrating_to_another_policy_discards_the_member_list() {
    common::require_db!();
    let (app, creator) = setup().await;
    let space = create(&app, &creator, "main", json!({ "$type": MEMBERSPACE })).await;
    let member = rand_did("member");
    let (status, _) = post(
        &app,
        "com.atproto.simplespace.putMember",
        &creator,
        json!({ "space": space, "did": member, "read": true, "write": true }),
    )
    .await;
    assert!(status.is_success());

    for policy in [
        json!({ "$type": MANAGEDSPACE, "managingApp": "did:plc:app" }),
        json!({ "$type": MEMBERSPACE }),
    ] {
        let (status, body) = post(
            &app,
            "com.atproto.space.updateSpace",
            &creator,
            json!({ "space": space, "policy": policy }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    let row = happyview::spaces::db::get_space_by_address(
        &app.state.db,
        app.state.db_backend,
        space.trim_start_matches("at://").split('/').next().unwrap(),
        "com.example.forum",
        "main",
    )
    .await
    .unwrap()
    .unwrap();
    let members =
        happyview::spaces::members::resolve_members(&app.state.db, app.state.db_backend, &row.id)
            .await
            .unwrap();
    assert!(
        members.iter().all(|m| m.did != member),
        "the member list was discarded: {members:?}"
    );
}

/// A configuration change within one policy keeps the member list.
#[tokio::test]
#[serial]
async fn changing_a_policys_configuration_keeps_the_member_list() {
    common::require_db!();
    let (app, creator) = setup().await;
    let space = create(&app, &creator, "main", json!({ "$type": MEMBERSPACE })).await;
    let member = rand_did("member");
    post(
        &app,
        "com.atproto.simplespace.putMember",
        &creator,
        json!({ "space": space, "did": member, "read": true, "write": false }),
    )
    .await;
    post(
        &app,
        "com.atproto.space.updateSpace",
        &creator,
        json!({ "space": space, "policy": { "$type": MEMBERSPACE, "publicWrite": true } }),
    )
    .await;

    let row = happyview::spaces::db::get_space_by_address(
        &app.state.db,
        app.state.db_backend,
        space.trim_start_matches("at://").split('/').next().unwrap(),
        "com.example.forum",
        "main",
    )
    .await
    .unwrap()
    .unwrap();
    let members =
        happyview::spaces::members::resolve_members(&app.state.db, app.state.db_backend, &row.id)
            .await
            .unwrap();
    assert!(members.iter().any(|m| m.did == member), "{members:?}");
}

/// A space is described to its owner. Anyone else, unless they hold a
/// credential for it, is told no such space exists.
#[tokio::test]
#[serial]
async fn a_space_is_not_disclosed_to_a_stranger() {
    common::require_db!();
    let (app, creator) = setup().await;
    let space = create(&app, &creator, "main", json!({ "$type": MEMBERSPACE })).await;
    let (status, body) = get_space(&app, &space, &rand_did("stranger")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], json!("SpaceNotFound"));

    let missing = space.replace("/main", "/nothing");
    let (status, body) = get_space(&app, &missing, &creator).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], json!("SpaceNotFound"));
}

#[tokio::test]
#[serial]
async fn the_owner_deletes_a_space() {
    common::require_db!();
    let (app, creator) = setup().await;
    let space = create(&app, &creator, "main", json!({ "$type": MEMBERSPACE })).await;

    let (status, body) = post(
        &app,
        "com.atproto.space.deleteSpace",
        &rand_did("stranger"),
        json!({ "space": space }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], json!("NotSpaceOwner"));

    let (status, _) = post(
        &app,
        "com.atproto.space.deleteSpace",
        &creator,
        json!({ "space": space }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = get_space(&app, &space, &creator).await;
    assert_eq!(body["error"], json!("SpaceNotFound"));
}

/// The `simplespace` lifecycle methods are gone. A removed method answers as a
/// method that never existed does.
#[tokio::test]
#[serial]
async fn the_simplespace_lifecycle_methods_are_gone() {
    common::require_db!();
    let (app, creator) = setup().await;
    let (never, _) = post(
        &app,
        "com.atproto.simplespace.zzzNeverExisted",
        &creator,
        json!({}),
    )
    .await;
    for method in [
        "com.atproto.simplespace.createSpace",
        "com.atproto.simplespace.updateSpace",
        "com.atproto.simplespace.deleteSpace",
        "dev.happyview.space.createSpace",
    ] {
        let (status, _) = post(&app, method, &creator, json!({})).await;
        assert_eq!(status, never, "{method}");
    }
}

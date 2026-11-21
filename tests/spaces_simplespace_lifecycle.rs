//! The `com.atproto.simplespace` lifecycle methods (`createSpace`,
//! `updateSpace`, `getSpace`, `deleteSpace`), served beside
//! `com.atproto.space.*Space` for clients written before the Oct 29 text.
//!
//! A simplespace client configures a space with a read policy, a write policy
//! and app access. HappyView stores the policy they amount to, so the same
//! space reads back through either generation, and refuses combinations that
//! amount to none (HAP-388 H6).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

const SPACE_TYPE: &str = "com.example.forum";

fn rand_did(label: &str) -> String {
    format!("did:plc:{label}{}", Uuid::new_v4().simple())
}

fn rand_skey(label: &str) -> String {
    format!("{label}{}", Uuid::new_v4().simple())
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

async fn json_of(resp: axum::http::Response<Body>) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or(json!(null))
}

async fn post(app: &TestApp, nsid: &str, did: &str, body: Value) -> (StatusCode, Value) {
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
    (status, json_of(resp).await)
}

async fn get(app: &TestApp, nsid: &str, space: &str, did: &str) -> (StatusCode, Value) {
    let (name, value) = common::auth::admin_cookie_header(did, &app.state.cookie_key);
    let req = Request::builder()
        .uri(format!("/xrpc/{nsid}?space={}", urlencoding::encode(space)))
        .header(name, value)
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    (status, json_of(resp).await)
}

fn simple(kind: &str) -> Value {
    json!({ "$type": format!("com.atproto.simplespace.defs#{kind}") })
}

fn managing_app(app: &str) -> Value {
    json!({
        "$type": "com.atproto.simplespace.defs#managingAppPolicy",
        "managingApp": app,
    })
}

/// `simplespace.createSpace` with `rules` merged into a fresh type and key.
async fn create(app: &TestApp, creator: &str, skey: &str, rules: Value) -> (StatusCode, Value) {
    let mut body = json!({ "spaceType": SPACE_TYPE, "skey": skey });
    for (k, v) in rules.as_object().unwrap() {
        body[k] = v.clone();
    }
    post(app, "com.atproto.simplespace.createSpace", creator, body).await
}

#[tokio::test]
#[serial]
async fn a_simplespace_space_is_created_on_a_did_of_its_own_and_reads_back_both_ways() {
    common::require_db!();
    let app = setup().await;
    let creator = rand_did("creator");
    let skey = rand_skey("s");
    let (status, body) = create(
        &app,
        &creator,
        &skey,
        json!({
            "readPolicy": simple("publicPolicy"),
            "writePolicy": simple("memberListPolicy"),
            "appAccess": { "$type": "com.atproto.simplespace.defs#open" },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let space = body["uri"].as_str().unwrap().to_string();
    assert!(
        !space.starts_with(&format!("at://{creator}/")),
        "the space has a DID of its own: {space}"
    );

    let (status, body) = get(&app, "com.atproto.simplespace.getSpace", &space, &creator).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["uri"], json!(space));
    assert_eq!(
        body["config"]["readPolicy"]["$type"],
        json!("com.atproto.simplespace.defs#publicPolicy")
    );
    assert_eq!(
        body["config"]["writePolicy"]["$type"],
        json!("com.atproto.simplespace.defs#memberListPolicy")
    );

    let (status, body) = get(&app, "com.atproto.space.getSpace", &space, &creator).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["space"], json!(space));
    assert_eq!(
        body["policy"],
        json!({
            "$type": "com.atproto.memberspace.policy",
            "publicRead": true,
            "publicWrite": false,
            "publishPolicy": false,
            "publishMembers": false,
        })
    );
}

/// A managing app on both sides is a managedspace.
#[tokio::test]
#[serial]
async fn a_managing_app_on_both_sides_is_a_managed_space() {
    common::require_db!();
    let app = setup().await;
    let creator = rand_did("creator");
    let (status, body) = create(
        &app,
        &creator,
        &rand_skey("s"),
        json!({ "policy": "managing-app", "managingApp": "did:web:app.example.com#forum" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let space = body["uri"].as_str().unwrap().to_string();
    let (_, body) = get(&app, "com.atproto.space.getSpace", &space, &creator).await;
    assert_eq!(
        body["policy"]["$type"],
        json!("com.atproto.managedspace.policy")
    );
    assert_eq!(
        body["policy"]["managingApp"],
        json!("did:web:app.example.com#forum")
    );
}

/// Rules that amount to no policy are refused before anything is created.
#[tokio::test]
#[serial]
async fn rules_that_amount_to_no_policy_are_refused() {
    common::require_db!();
    let app = setup().await;
    let creator = rand_did("creator");
    let skey = rand_skey("s");
    for rules in [
        json!({
            "readPolicy": managing_app("did:web:app.example.com#forum"),
            "writePolicy": simple("memberListPolicy"),
        }),
        json!({
            "policy": "managing-app",
            "managingApp": "did:web:app.example.com#forum",
            "appAccess": {
                "$type": "com.atproto.simplespace.defs#allowList",
                "allowed": ["https://app.example.com/client-metadata.json"],
            },
        }),
    ] {
        let (status, body) = create(&app, &creator, &skey, rules.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{rules}");
        assert_eq!(body["error"], json!("UnsupportedPolicy"), "{rules}");
    }
    // Nothing was created, so the key is still free.
    let (status, body) = create(
        &app,
        &creator,
        &skey,
        json!({ "readPolicy": simple("memberListPolicy") }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

#[tokio::test]
#[serial]
async fn unimplemented_variants_name_what_they_are() {
    common::require_db!();
    let app = setup().await;
    let creator = rand_did("creator");
    let (status, body) = create(
        &app,
        &creator,
        &rand_skey("s"),
        json!({ "readPolicy": { "$type": "com.example.defs#bespokePolicy" } }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], json!("UnsupportedPolicy"));

    let (status, body) = create(
        &app,
        &creator,
        &rand_skey("s"),
        json!({ "appAccess": { "$type": "com.example.defs#bespokeAccess" } }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], json!("UnsupportedAppAccess"));
}

/// `type` is the earlier name of `spaceType`.
#[tokio::test]
#[serial]
async fn the_type_may_be_named_type() {
    common::require_db!();
    let app = setup().await;
    let creator = rand_did("creator");
    let (status, body) = post(
        &app,
        "com.atproto.simplespace.createSpace",
        &creator,
        json!({ "type": SPACE_TYPE, "skey": rand_skey("s") }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(
        body["uri"]
            .as_str()
            .unwrap()
            .contains("/space/com.example.forum/")
    );
}

/// `allowedCollections` is no longer enforced. It is dropped, and the answer
/// says so (HAP-388 H5).
#[tokio::test]
#[serial]
async fn allowed_collections_are_dropped_with_a_warning() {
    common::require_db!();
    let app = setup().await;
    let creator = rand_did("creator");
    let (status, body) = create(
        &app,
        &creator,
        &rand_skey("s"),
        json!({ "config": { "allowedCollections": ["com.example.post"] } }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["warnings"],
        json!(["allowedCollections is not supported and was ignored"])
    );
    let space = body["uri"].as_str().unwrap().to_string();
    let (_, body) = get(&app, "com.atproto.simplespace.getSpace", &space, &creator).await;
    assert!(
        body["space"]["config"]["allowedCollections"].is_null(),
        "{body}"
    );
}

#[tokio::test]
#[serial]
async fn update_replaces_the_rules_it_names_and_refuses_a_mix() {
    common::require_db!();
    let app = setup().await;
    let creator = rand_did("creator");
    let (_, body) = create(&app, &creator, &rand_skey("s"), json!({})).await;
    let space = body["uri"].as_str().unwrap().to_string();

    let (status, body) = post(
        &app,
        "com.atproto.simplespace.updateSpace",
        &creator,
        json!({ "space": space, "readPolicy": simple("publicPolicy"), "displayName": "Forum" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["uri"], json!(space));
    // The space object is serialised as simplespace answered it, in snake case.
    assert_eq!(body["space"]["display_name"], json!("Forum"));
    let (_, got) = get(&app, "com.atproto.space.getSpace", &space, &creator).await;
    assert_eq!(got["policy"]["publicRead"], json!(true));

    let (status, body) = post(
        &app,
        "com.atproto.simplespace.updateSpace",
        &creator,
        json!({ "space": space, "writePolicy": managing_app("did:web:app.example.com#forum") }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], json!("UnsupportedPolicy"));
    let (_, got) = get(&app, "com.atproto.space.getSpace", &space, &creator).await;
    assert_eq!(
        got["policy"]["$type"],
        json!("com.atproto.memberspace.policy"),
        "a refused update changes nothing"
    );

    let other = rand_did("other");
    let (status, _) = post(
        &app,
        "com.atproto.simplespace.updateSpace",
        &other,
        json!({ "space": space, "readPolicy": simple("memberListPolicy") }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "only the owner updates");
}

/// A simplespace client may address a space under its creator's DID, as
/// spaces were addressed before each had its own. Simplespace routes resolve
/// that address to the space; the `space.*` routes do not (HAP-388 H9).
#[tokio::test]
#[serial]
async fn a_creator_addressed_space_resolves_on_simplespace_routes_only() {
    common::require_db!();
    let app = setup().await;
    let creator = rand_did("creator");
    let skey = rand_skey("s");
    let (_, body) = create(&app, &creator, &skey, json!({})).await;
    let space = body["uri"].as_str().unwrap().to_string();
    let by_creator = format!("at://{creator}/space/{SPACE_TYPE}/{skey}");

    let (status, body) = get(
        &app,
        "com.atproto.simplespace.getSpace",
        &by_creator,
        &creator,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["uri"],
        json!(space),
        "answered with the space's own address"
    );

    let member = rand_did("member");
    let (status, body) = post(
        &app,
        "com.atproto.simplespace.putMember",
        &creator,
        json!({ "space": by_creator, "did": member, "read": true, "write": false }),
    )
    .await;
    assert!(status.is_success(), "{body}");

    let (status, body) = get(&app, "com.atproto.space.getSpace", &by_creator, &creator).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], json!("SpaceNotFound"));

    let (status, body) = post(
        &app,
        "com.atproto.simplespace.deleteSpace",
        &creator,
        json!({ "space": by_creator }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = get(&app, "com.atproto.simplespace.getSpace", &space, &creator).await;
    assert!(!status.is_success(), "deleted");
}

/// Own-records-only access has no form in a member list. It is refused rather
/// than narrowed to no read access (HAP-388 H5).
#[tokio::test]
#[serial]
async fn read_self_access_is_refused() {
    common::require_db!();
    let app = setup().await;
    let creator = rand_did("creator");
    let (_, body) = create(&app, &creator, &rand_skey("s"), json!({})).await;
    let space = body["uri"].as_str().unwrap().to_string();
    let (status, body) = post(
        &app,
        "com.atproto.simplespace.addMember",
        &creator,
        json!({ "space": space, "did": rand_did("member"), "access": "read_self" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["message"].as_str().unwrap().contains("read_self"));
}

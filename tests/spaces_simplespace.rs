//! HTTP-level coverage for the `com.atproto.simplespace.*` management surface.
//!
//! The authoritative method list is the lexicon directory on
//! `bluesky-social/atproto @ permissioned-data-alpha`:
//! `checkUserAccess, createSpace, defs, deleteSpace, getSpace, listMembers,
//! putMember, removeMember, updateSpace`. Anything outside that set is either a
//! legacy alias or does not belong.
//!
//! Harness mirrors `spaces_records.rs`.

mod common;

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

fn rand_did(label: &str) -> String {
    format!("did:plc:{label}{}", Uuid::new_v4().simple())
}

fn rand_skey(label: &str) -> String {
    format!("{label}{}", Uuid::new_v4().simple())
}

fn cookie_for(app: &TestApp, did: &str) -> (HeaderName, HeaderValue) {
    common::auth::admin_cookie_header(did, &app.state.cookie_key)
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

async fn json_of(resp: axum::http::Response<Body>) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or(json!(null))
}

async fn post(app: &TestApp, nsid: &str, did: &str, body: Value) -> axum::http::Response<Body> {
    let (name, value) = cookie_for(app, did);
    let req = Request::builder()
        .method("POST")
        .uri(format!("/xrpc/{nsid}"))
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    app.router.clone().oneshot(req).await.unwrap()
}

/// A method that has never existed, for comparison.
///
/// Unmatched `/xrpc/...` paths do not 404 here: they fall through to the SPA
/// fallback and come back 401. A removed method is detected by responding the
/// same as a method that was never served, not by a bare status assertion.
const CONTROL_METHOD: &str = "com.atproto.simplespace.zzzNeverExisted";

async fn get(app: &TestApp, uri: &str, did: &str) -> axum::http::Response<Body> {
    let (name, value) = cookie_for(app, did);
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .header(name, value)
        .body(Body::empty())
        .unwrap();
    app.router.clone().oneshot(req).await.unwrap()
}

fn member_list_policy() -> Value {
    json!({ "$type": "com.atproto.simplespace.defs#memberListPolicy" })
}

/// Create a space over HTTP and return its `at://` URI.
async fn create_space(app: &TestApp, authority: &str, skey: &str) -> String {
    let resp = post(
        app,
        "com.atproto.simplespace.createSpace",
        authority,
        json!({
            "type": "com.example.forum",
            "skey": skey,
            "readPolicy": member_list_policy(),
            "writePolicy": member_list_policy(),
            "appAccess": { "$type": "com.atproto.simplespace.defs#open" },
        }),
    )
    .await;
    assert!(
        resp.status().is_success(),
        "createSpace failed: {}",
        resp.status()
    );
    let body = json_of(resp).await;
    body["uri"]
        .as_str()
        .or_else(|| body["space"]["uri"].as_str())
        .unwrap_or_else(|| panic!("createSpace response has no uri: {body}"))
        .to_string()
}

// ---------------------------------------------------------------------------
// putMember
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn put_member_sets_both_booleans_together() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("auth");
    let member = rand_did("member");
    let space = create_space(&app, &authority, &rand_skey("s")).await;

    // Read-only first.
    let resp = post(
        &app,
        "com.atproto.simplespace.putMember",
        &authority,
        json!({ "space": space, "did": member, "read": true, "write": false }),
    )
    .await;
    assert!(
        resp.status().is_success(),
        "putMember failed: {}",
        resp.status()
    );

    let listed = json_of(
        get(
            &app,
            &format!(
                "/xrpc/com.atproto.simplespace.listMembers?space={}",
                urlencoding::encode(&space)
            ),
            &authority,
        )
        .await,
    )
    .await;
    let found = listed["members"]
        .as_array()
        .expect("members array")
        .iter()
        .find(|m| m["did"] == member.as_str())
        .expect("member present");
    assert_eq!(found["read"], json!(true));
    assert_eq!(found["write"], json!(false));

    // putMember replaces the pair wholesale, so the second call must flip write
    // on without needing read restated as a separate operation.
    let resp = post(
        &app,
        "com.atproto.simplespace.putMember",
        &authority,
        json!({ "space": space, "did": member, "read": true, "write": true }),
    )
    .await;
    assert!(resp.status().is_success());

    let listed = json_of(
        get(
            &app,
            &format!(
                "/xrpc/com.atproto.simplespace.listMembers?space={}",
                urlencoding::encode(&space)
            ),
            &authority,
        )
        .await,
    )
    .await;
    let found = listed["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["did"] == member.as_str())
        .expect("member present");
    assert_eq!(found["write"], json!(true));
}

#[tokio::test]
#[serial]
async fn put_member_requires_both_booleans() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("auth");
    let space = create_space(&app, &authority, &rand_skey("s")).await;

    // Omitting `write` must fail rather than default; the lexicon marks both
    // required.
    let resp = post(
        &app,
        "com.atproto.simplespace.putMember",
        &authority,
        json!({ "space": space, "did": rand_did("m"), "read": true }),
    )
    .await;
    assert!(
        !resp.status().is_success(),
        "putMember accepted a missing `write`, which would revoke it"
    );
}

#[tokio::test]
#[serial]
async fn add_member_is_gone() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("auth");
    let space = create_space(&app, &authority, &rand_skey("s")).await;

    let body = json!({ "space": space, "did": rand_did("m"), "access": "read" });
    let removed = post(
        &app,
        "com.atproto.simplespace.addMember",
        &authority,
        body.clone(),
    )
    .await;
    let control = post(&app, CONTROL_METHOD, &authority, body).await;

    assert_eq!(
        removed.status(),
        control.status(),
        "addMember should behave like a method that was never served"
    );
    assert!(!removed.status().is_success());
}

// ---------------------------------------------------------------------------
// Method placement
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn get_space_is_served_under_simplespace_not_space() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("auth");
    let space = create_space(&app, &authority, &rand_skey("s")).await;
    let encoded = urlencoding::encode(&space);

    let ok = get(
        &app,
        &format!("/xrpc/com.atproto.simplespace.getSpace?space={encoded}"),
        &authority,
    )
    .await;
    assert!(
        ok.status().is_success(),
        "simplespace.getSpace should serve: {}",
        ok.status()
    );

    // The lexicons define no getSpace in the protocol namespace.
    let gone = get(
        &app,
        &format!("/xrpc/com.atproto.space.getSpace?space={encoded}"),
        &authority,
    )
    .await;
    let control = get(
        &app,
        &format!("/xrpc/com.atproto.space.zzzNeverExisted?space={encoded}"),
        &authority,
    )
    .await;
    assert_eq!(
        gone.status(),
        control.status(),
        "space.getSpace should behave like a method that was never served"
    );
    assert!(!gone.status().is_success());
}

#[tokio::test]
#[serial]
async fn config_endpoints_are_gone() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("auth");
    let space = create_space(&app, &authority, &rand_skey("s")).await;

    // The lexicons define no such methods; config is surfaced by getSpace and
    // set by updateSpace.
    let encoded = urlencoding::encode(&space);
    let control = get(
        &app,
        &format!("/xrpc/{CONTROL_METHOD}?space={encoded}"),
        &authority,
    )
    .await;
    let control_status = control.status();

    let get_config = get(
        &app,
        &format!("/xrpc/com.atproto.simplespace.getConfig?space={encoded}"),
        &authority,
    )
    .await;
    assert_eq!(get_config.status(), control_status);

    let update_config = post(
        &app,
        "com.atproto.simplespace.updateConfig",
        &authority,
        json!({ "space": space }),
    )
    .await;
    assert!(!update_config.status().is_success());
}

// ---------------------------------------------------------------------------
// Policy validation
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn create_space_rejects_an_unimplemented_policy_variant() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("auth");
    let resp = post(
        &app,
        "com.atproto.simplespace.createSpace",
        &authority,
        json!({
            "type": "com.example.forum",
            "skey": rand_skey("s"),
            "readPolicy": { "$type": "com.example.defs#bespokePolicy" },
            "writePolicy": member_list_policy(),
            "appAccess": { "$type": "com.atproto.simplespace.defs#open" },
        }),
    )
    .await;

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = json_of(resp).await;
    assert_eq!(body["error"], "UnsupportedPolicy");
}

#[tokio::test]
#[serial]
async fn create_space_rejects_an_unimplemented_app_access_variant() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("auth");
    let resp = post(
        &app,
        "com.atproto.simplespace.createSpace",
        &authority,
        json!({
            "type": "com.example.forum",
            "skey": rand_skey("s"),
            "readPolicy": member_list_policy(),
            "writePolicy": member_list_policy(),
            "appAccess": { "$type": "com.example.defs#bespokeAccess" },
        }),
    )
    .await;

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = json_of(resp).await;
    assert_eq!(body["error"], "UnsupportedAppAccess");
}

#[tokio::test]
#[serial]
async fn a_managing_app_policy_round_trips_through_get_space() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("auth");
    let skey = rand_skey("s");
    let resp = post(
        &app,
        "com.atproto.simplespace.createSpace",
        &authority,
        json!({
            "type": "com.example.forum",
            "skey": skey,
            "readPolicy": {
                "$type": "com.atproto.simplespace.defs#managingAppPolicy",
                "managingApp": "did:web:app.example.com#forum",
            },
            "writePolicy": member_list_policy(),
            "appAccess": { "$type": "com.atproto.simplespace.defs#open" },
        }),
    )
    .await;
    assert!(
        resp.status().is_success(),
        "createSpace failed: {}",
        resp.status()
    );
    let space = json_of(resp).await["uri"].as_str().unwrap().to_string();

    let got = json_of(
        get(
            &app,
            &format!(
                "/xrpc/com.atproto.simplespace.getSpace?space={}",
                urlencoding::encode(&space)
            ),
            &authority,
        )
        .await,
    )
    .await;

    // The app must survive inside the policy value, not alongside it.
    let read_policy = &got["config"]["readPolicy"];
    assert_eq!(
        read_policy["$type"],
        "com.atproto.simplespace.defs#managingAppPolicy"
    );
    assert_eq!(read_policy["managingApp"], "did:web:app.example.com#forum");
}

// ---------------------------------------------------------------------------
// unregisterNotify / listBlobs
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn unregister_notify_removes_a_registration_and_is_idempotent() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("auth");
    let syncer = rand_did("syncer");
    let space = create_space(&app, &authority, &rand_skey("s")).await;

    let reg = post(
        &app,
        "com.atproto.space.registerNotify",
        &syncer,
        json!({ "space": space, "serviceDid": syncer, "endpoint": "https://syncer.example" }),
    )
    .await;
    assert!(
        reg.status().is_success(),
        "registerNotify failed: {}",
        reg.status()
    );

    let first = post(
        &app,
        "com.atproto.space.unregisterNotify",
        &syncer,
        json!({ "space": space, "service": syncer }),
    )
    .await;
    assert!(first.status().is_success());
    assert_eq!(json_of(first).await["removed"], json!(1));

    // The lexicon says it "succeeds whether or not a matching registration
    // existed", so a repeat withdrawal is not an error.
    let second = post(
        &app,
        "com.atproto.space.unregisterNotify",
        &syncer,
        json!({ "space": space, "service": syncer }),
    )
    .await;
    assert!(
        second.status().is_success(),
        "unregisterNotify must be idempotent"
    );
    assert_eq!(json_of(second).await["removed"], json!(0));
}

#[tokio::test]
#[serial]
async fn unregister_notify_refuses_another_services_registration() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("auth");
    let syncer = rand_did("syncer");
    let stranger = rand_did("stranger");
    let space = create_space(&app, &authority, &rand_skey("s")).await;

    post(
        &app,
        "com.atproto.space.registerNotify",
        &syncer,
        json!({ "space": space, "serviceDid": syncer, "endpoint": "https://syncer.example" }),
    )
    .await;

    // Withdrawing someone else's registration would stop them syncing.
    let resp = post(
        &app,
        "com.atproto.space.unregisterNotify",
        &stranger,
        json!({ "space": space, "service": syncer }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// Real CIDs: record values must encode as DAG-CBOR, and a malformed `$link` is
// rejected before the record is ever stored.
const MY_BLOB_CID: &str = "bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy";
const THEIR_BLOB_CID: &str = "bafkreibme22gw2h7y2h7tg2fhqotaqjucnbc24deqo72b6mkl2egm4gn4a";
const RECORD_LINK_CID: &str = "bafyreidfayvfuwqa7qlnopdjiqrxzs6blmoeu4rujcjtnci5beludirz2a";

#[tokio::test]
#[serial]
async fn list_blobs_returns_only_blob_refs_from_the_named_repo() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let authority = rand_did("auth");
    let other = rand_did("other");
    let space = create_space(&app, &authority, &rand_skey("s")).await;

    for did in [&authority, &other] {
        let resp = post(
            &app,
            "com.atproto.simplespace.putMember",
            &authority,
            json!({ "space": space, "did": did, "read": true, "write": true }),
        )
        .await;
        assert!(resp.status().is_success());
    }

    let with_blob = |cid: &str| {
        json!({
            "$type": "com.example.note",
            "image": { "$type": "blob", "ref": { "$link": cid },
                       "mimeType": "image/png", "size": 1 },
            // A record link, which is not a blob and must not be listed.
            "subject": { "cid": { "$link": RECORD_LINK_CID } },
        })
    };

    for (did, cid) in [(&authority, MY_BLOB_CID), (&other, THEIR_BLOB_CID)] {
        let resp = post(
            &app,
            "com.atproto.space.createRecord",
            did,
            json!({ "space": space, "collection": "com.example.note", "record": with_blob(cid) }),
        )
        .await;
        assert!(
            resp.status().is_success(),
            "createRecord failed: {}",
            resp.status()
        );
    }

    let listed = json_of(
        get(
            &app,
            &format!(
                "/xrpc/com.atproto.space.listBlobs?space={}&repo={}",
                urlencoding::encode(&space),
                urlencoding::encode(&authority)
            ),
            &authority,
        )
        .await,
    )
    .await;

    let cids: Vec<&str> = listed["cids"]
        .as_array()
        .expect("cids array")
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();

    assert!(cids.contains(&MY_BLOB_CID));
    assert!(
        !cids.contains(&THEIR_BLOB_CID),
        "listBlobs is scoped to one repo; another author's blob leaked"
    );
    assert!(
        !cids.contains(&RECORD_LINK_CID),
        "a record link is not a blob and getBlob could not serve it"
    );
}

// ---------------------------------------------------------------------------
// community.lexicon.service.describe
// ---------------------------------------------------------------------------

async fn describe_body(app: &TestApp) -> Value {
    let req = Request::builder()
        .method("GET")
        .uri("/xrpc/community.lexicon.service.describe")
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert!(
        resp.status().is_success(),
        "describe must answer unauthenticated: {}",
        resp.status()
    );
    json_of(resp).await
}

fn advertised(body: &Value) -> Vec<String> {
    body["methods"]
        .as_array()
        .expect("methods array")
        .iter()
        .map(|m| m["value"].as_str().expect("value").to_string())
        .collect()
}

#[tokio::test]
#[serial]
async fn describe_lists_every_routed_method() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let body = describe_body(&app).await;
    let methods = advertised(&body);
    assert!(!methods.is_empty());

    // Compare each advertised method against a method that was never served.
    let control = get(&app, &format!("/xrpc/{CONTROL_METHOD}"), "did:plc:probe").await;
    let control_status = control.status();

    for nsid in &methods {
        let resp = get(&app, &format!("/xrpc/{nsid}"), "did:plc:probe").await;
        assert_ne!(
            resp.status(),
            control_status,
            "advertised but not routed: {nsid}"
        );
    }
}

#[tokio::test]
#[serial]
async fn describe_advertises_no_legacy_aliases() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    for nsid in advertised(&describe_body(&app).await) {
        assert!(
            !nsid.starts_with("dev.happyview."),
            "legacy alias leaked into the advertisement: {nsid}"
        );
    }
}

#[tokio::test]
#[serial]
async fn describe_omits_space_methods_when_the_feature_is_off() {
    common::require_db!();
    let app = TestApp::new().await;
    // Spaces stay disabled.

    let body = describe_body(&app).await;
    let methods = advertised(&body);

    assert!(
        !methods
            .iter()
            .any(|m| m.starts_with("com.atproto.space.")
                || m.starts_with("com.atproto.simplespace.")),
        "spaces are disabled but the advertisement names them: {methods:?}"
    );
}

#[tokio::test]
#[serial]
async fn describe_entries_carry_the_lexicon_type() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let body = describe_body(&app).await;
    // The proposed lexicon has no `roles`.
    assert!(
        body.get("roles").is_none(),
        "roles is not in the schema: {body}"
    );
    for entry in body["methods"].as_array().unwrap() {
        assert_eq!(entry["$type"], "community.lexicon.service.describe#nsid");
    }
}

#[tokio::test]
#[serial]
async fn describe_names_itself() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    // A client reading the list should see the method it just called.
    assert!(
        advertised(&describe_body(&app).await)
            .contains(&"community.lexicon.service.describe".to_string())
    );
}

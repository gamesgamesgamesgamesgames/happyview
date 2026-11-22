//! `com.atproto.memberspace.*`: the Nov 12 batch methods (`putMembers`,
//! `removeMembers`), the Oct 29 single ones (`putMember`, `removeMember`), and
//! `listMembers`, over one member list, with writer-set upkeep.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

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

async fn json_of(resp: axum::http::Response<Body>) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or(json!(null))
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
    (status, json_of(resp).await)
}

async fn get(app: &TestApp, did: &str, path: &str) -> (StatusCode, Value) {
    let (name, value) = common::auth::admin_cookie_header(did, &app.state.cookie_key);
    let req = Request::builder()
        .uri(path)
        .header(name, value)
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    (status, json_of(resp).await)
}

async fn create(app: &TestApp, owner: &str, policy: Value) -> String {
    let (status, body) = post(
        app,
        owner,
        "com.atproto.space.createSpace",
        json!({ "spaceType": "com.example.forum", "skey": format!("s{}", Uuid::new_v4().simple()), "policy": policy }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["space"].as_str().unwrap().to_string()
}

async fn members(app: &TestApp, owner: &str, space: &str) -> Vec<Value> {
    let (status, body) = get(
        app,
        owner,
        &format!(
            "/xrpc/com.atproto.memberspace.listMembers?space={}&limit=1000",
            urlencoding::encode(space)
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["members"].as_array().unwrap().clone()
}

fn member<'a>(list: &'a [Value], did: &str) -> Option<&'a Value> {
    list.iter().find(|m| m["did"] == json!(did))
}

/// `did` writes a record to `space`, which puts it in the writer set.
async fn write(app: &TestApp, did: &str, space: &str) {
    let (status, body) = post(
        app,
        did,
        "com.atproto.space.createRecord",
        json!({ "space": space, "collection": "com.example.note", "record": { "$type": "com.example.note", "text": "hi" } }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

async fn writers(app: &TestApp, space: &str) -> Vec<String> {
    let uri = happyview::spaces::SpaceUri::parse(space).unwrap();
    let row = happyview::spaces::db::get_space_by_address(
        &app.state.db,
        app.state.db_backend,
        &uri.did,
        &uri.type_nsid,
        &uri.skey,
    )
    .await
    .unwrap()
    .unwrap();
    happyview::spaces::writers::list(&app.state.db, app.state.db_backend, &row.id, None, 1000)
        .await
        .unwrap()
        .into_iter()
        .map(|w| w.repo_did)
        .collect()
}

fn memberspace() -> Value {
    json!({ "$type": "com.atproto.memberspace.policy" })
}

#[tokio::test]
#[serial]
async fn put_members_applies_a_batch_and_a_repeated_did_takes_its_last_entry() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let space = create(&app, &owner, memberspace()).await;
    let (a, b) = (rand_did("a"), rand_did("b"));

    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.memberspace.putMembers",
        json!({ "space": space, "members": [
            { "did": a, "read": true, "write": true },
            { "did": b, "read": true, "write": false },
            { "did": a, "read": true, "write": false },
        ]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let list = members(&app, &owner, &space).await;
    assert_eq!(
        member(&list, &a),
        Some(&json!({ "did": a, "read": true, "write": false }))
    );
    assert_eq!(
        member(&list, &b),
        Some(&json!({ "did": b, "read": true, "write": false }))
    );

    let many: Vec<Value> = (0..101)
        .map(|i| json!({ "did": format!("did:plc:many{i}"), "read": true, "write": false }))
        .collect();
    let (status, _) = post(
        &app,
        &owner,
        "com.atproto.memberspace.putMembers",
        json!({ "space": space, "members": many }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "at most 100");

    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.memberspace.putMembers",
        json!({ "space": space, "members": [{ "did": "not-a-did", "read": true, "write": true }] }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

/// A member whose write access is withdrawn, or who is removed, leaves the
/// writer set. A DID that is not a member is ignored by `removeMembers`, and
/// keeps its place in the writer set.
#[tokio::test]
#[serial]
async fn the_writer_set_follows_the_member_list() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let space = create(&app, &owner, memberspace()).await;
    let (a, b, c) = (rand_did("a"), rand_did("b"), rand_did("c"));
    let (status, _) = post(
        &app,
        &owner,
        "com.atproto.memberspace.putMembers",
        json!({ "space": space, "members": [
            { "did": a, "read": true, "write": true },
            { "did": b, "read": true, "write": true },
            { "did": c, "read": true, "write": true },
        ]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for did in [&a, &b, &c] {
        write(&app, did, &space).await;
    }
    assert_eq!(writers(&app, &space).await.len(), 3);

    // Withdrawing write access drops the writer.
    let (status, _) = post(
        &app,
        &owner,
        "com.atproto.memberspace.putMembers",
        json!({ "space": space, "members": [{ "did": a, "read": true, "write": false }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!writers(&app, &space).await.contains(&a));

    // Removing a member drops it; a non-member is ignored, even one in the
    // writer set.
    let row = {
        let uri = happyview::spaces::SpaceUri::parse(&space).unwrap();
        happyview::spaces::db::get_space_by_address(
            &app.state.db,
            app.state.db_backend,
            &uri.did,
            &uri.type_nsid,
            &uri.skey,
        )
        .await
        .unwrap()
        .unwrap()
    };
    assert!(
        happyview::spaces::db::remove_member(&app.state.db, app.state.db_backend, &row.id, &c)
            .await
            .unwrap()
    );
    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.memberspace.removeMembers",
        json!({ "space": space, "dids": [b, c, rand_did("nobody")] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let left = writers(&app, &space).await;
    assert!(!left.contains(&b), "a removed member leaves the writer set");
    assert!(left.contains(&c), "a non-member keeps its place");
    let list = members(&app, &owner, &space).await;
    assert!(member(&list, &b).is_none());
}

#[tokio::test]
#[serial]
async fn list_members_pages_by_did() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let space = create(&app, &owner, memberspace()).await;
    let dids: Vec<String> = (0..3).map(|i| format!("did:plc:page{i}")).collect();
    let entries: Vec<Value> = dids
        .iter()
        .map(|d| json!({ "did": d, "read": true, "write": false }))
        .collect();
    post(
        &app,
        &owner,
        "com.atproto.memberspace.putMembers",
        json!({ "space": space, "members": entries }),
    )
    .await;
    let path = |q: &str| {
        format!(
            "/xrpc/com.atproto.memberspace.listMembers?space={}{q}",
            urlencoding::encode(&space)
        )
    };

    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let q = match &cursor {
            Some(c) => format!("&limit=2&cursor={}", urlencoding::encode(c)),
            None => "&limit=2".into(),
        };
        let (status, body) = get(&app, &owner, &path(&q)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        for m in body["members"].as_array().unwrap() {
            seen.push(m["did"].as_str().unwrap().to_string());
        }
        match body["cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    let mut expected = dids.clone();
    expected.push(owner.clone());
    expected.sort();
    assert_eq!(seen, expected, "every member once, in DID order");

    for q in ["&limit=0", "&limit=1001"] {
        let (status, _) = get(&app, &owner, &path(q)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{q}");
    }
    let (status, body) = get(&app, &owner, &path("&cursor=garbage")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], json!("InvalidCursor"));
}

/// The member methods refuse a space under another policy, a caller who does
/// not own the space, and a space that does not exist, each by its own error.
#[tokio::test]
#[serial]
async fn member_methods_name_why_they_refuse() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let managed = create(
        &app,
        &owner,
        json!({ "$type": "com.atproto.managedspace.policy", "managingApp": "did:web:app.example#forum" }),
    )
    .await;
    let member_space = create(&app, &owner, memberspace()).await;
    let someone = rand_did("someone");

    for (nsid, body) in [
        (
            "putMembers",
            json!({ "members": [{ "did": someone, "read": true, "write": false }] }),
        ),
        ("removeMembers", json!({ "dids": [someone] })),
        (
            "putMember",
            json!({ "did": someone, "read": true, "write": false }),
        ),
        ("removeMember", json!({ "did": someone })),
    ] {
        let nsid = format!("com.atproto.memberspace.{nsid}");
        let with = |space: &str| {
            let mut b = body.clone();
            b["space"] = json!(space);
            b
        };
        let (status, resp) = post(&app, &owner, &nsid, with(&managed)).await;
        assert_eq!(
            (status, &resp["error"]),
            (StatusCode::BAD_REQUEST, &json!("PolicyMismatch")),
            "{nsid}"
        );
        let (status, resp) = post(&app, &someone, &nsid, with(&member_space)).await;
        assert_eq!(
            (status, &resp["error"]),
            (StatusCode::FORBIDDEN, &json!("NotAuthorized")),
            "{nsid}"
        );
        let missing = format!("{member_space}x");
        let (status, resp) = post(&app, &owner, &nsid, with(&missing)).await;
        assert_eq!(
            (status, &resp["error"]),
            (StatusCode::BAD_REQUEST, &json!("SpaceNotFound")),
            "{nsid}"
        );
    }

    let list = |space: &str| {
        format!(
            "/xrpc/com.atproto.memberspace.listMembers?space={}",
            urlencoding::encode(space)
        )
    };
    let (status, resp) = get(&app, &owner, &list(&managed)).await;
    assert_eq!(
        (status, &resp["error"]),
        (StatusCode::BAD_REQUEST, &json!("PolicyMismatch"))
    );
    let (status, resp) = get(&app, &someone, &list(&member_space)).await;
    assert_eq!(
        (status, &resp["error"]),
        (StatusCode::FORBIDDEN, &json!("NotAuthorized"))
    );
}

/// Oct 29's one-at-a-time methods act on the same member list.
#[tokio::test]
#[serial]
async fn the_single_member_methods_act_on_the_same_list() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let space = create(&app, &owner, memberspace()).await;
    let a = rand_did("a");
    let (status, _) = post(
        &app,
        &owner,
        "com.atproto.memberspace.putMember",
        json!({ "space": space, "did": a, "read": true, "write": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        member(&members(&app, &owner, &space).await, &a),
        Some(&json!({ "did": a, "read": true, "write": true }))
    );
    let (status, _) = post(
        &app,
        &owner,
        "com.atproto.memberspace.removeMember",
        json!({ "space": space, "did": a }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(member(&members(&app, &owner, &space).await, &a).is_none());
}

/// A DID named twice in `removeMembers` is removed once, and the rest of the
/// batch still applies.
#[tokio::test]
#[serial]
async fn remove_members_takes_a_did_named_twice() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let (a, b) = (rand_did("a"), rand_did("b"));
    let space = create(&app, &owner, memberspace()).await;
    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.memberspace.putMembers",
        json!({ "space": space, "members": [
            { "did": a, "read": true, "write": false },
            { "did": b, "read": true, "write": false },
        ] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.memberspace.removeMembers",
        json!({ "space": space, "dids": [a, a, b] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let list = members(&app, &owner, &space).await;
    assert!(
        member(&list, &a).is_none() && member(&list, &b).is_none(),
        "{list:?}"
    );
}

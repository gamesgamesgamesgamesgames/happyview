//! A space's published policy and member list: `com.atproto.space.policy/self`
//! and `com.atproto.memberspace.member/<did>` in the authority's repo, kept in
//! step with the policy (`publishPolicy`, `publishMembers`) and the member
//! list by every method that changes them, and never written directly.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

const POLICY: &str = "com.atproto.space.policy";
const MEMBER: &str = "com.atproto.memberspace.member";

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

async fn update(app: &TestApp, owner: &str, space: &str, policy: Value) {
    let (status, body) = post(
        app,
        owner,
        "com.atproto.space.updateSpace",
        json!({ "space": space, "policy": policy }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn put_members(app: &TestApp, owner: &str, space: &str, members: Value) {
    let (status, body) = post(
        app,
        owner,
        "com.atproto.memberspace.putMembers",
        json!({ "space": space, "members": members }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn remove_members(app: &TestApp, owner: &str, space: &str, dids: Value) {
    let (status, body) = post(
        app,
        owner,
        "com.atproto.memberspace.removeMembers",
        json!({ "space": space, "dids": dids }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn space_row(app: &TestApp, space: &str) -> happyview::spaces::types::Space {
    let uri = happyview::spaces::SpaceUri::parse(space).unwrap();
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
}

/// The authority's records in `collection`, by rkey.
async fn published(app: &TestApp, space: &str, collection: &str) -> Vec<(String, Value)> {
    let row = space_row(app, space).await;
    happyview::spaces::db::repo_collection_records(
        &app.state.db,
        app.state.db_backend,
        &row.id,
        &row.authority_did,
        collection,
        None,
    )
    .await
    .unwrap()
    .into_iter()
    .map(|r| (r.rkey, r.record))
    .collect()
}

async fn member_records(app: &TestApp, space: &str) -> Vec<(String, bool, bool)> {
    published(app, space, MEMBER)
        .await
        .into_iter()
        .map(|(rkey, v)| {
            assert_eq!(v["$type"], json!(MEMBER));
            (rkey, v["read"] == json!(true), v["write"] == json!(true))
        })
        .collect()
}

/// The authority repo's head rev.
async fn authority_rev(app: &TestApp, space: &str) -> Option<String> {
    let row = space_row(app, space).await;
    let mut conn = app.state.db.acquire().await.unwrap();
    happyview::spaces::db::read_repo_state(
        &mut conn,
        app.state.db_backend,
        &row.id,
        &row.authority_did,
    )
    .await
    .unwrap()
    .and_then(|s| s.rev)
}

fn memberspace(extra: Value) -> Value {
    let mut policy = json!({ "$type": "com.atproto.memberspace.policy" });
    policy
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    policy
}

#[tokio::test]
#[serial]
async fn the_member_list_is_published_and_follows_each_batch_in_one_commit() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let (a, b) = (rand_did("a"), rand_did("b"));
    let space = create(&app, &owner, memberspace(json!({ "publishMembers": true }))).await;

    // The creator is on the list from the start.
    assert_eq!(
        member_records(&app, &space).await,
        vec![(owner.clone(), true, true)]
    );

    let before = authority_rev(&app, &space).await;
    put_members(
        &app,
        &owner,
        &space,
        json!([
            { "did": a, "read": true, "write": false },
            { "did": b, "read": true, "write": true },
        ]),
    )
    .await;
    let after_put = authority_rev(&app, &space).await;
    assert_ne!(before, after_put, "one commit for the batch");
    let row = space_row(&app, &space).await;
    let (ops, _) = happyview::spaces::oplog::list_ops(
        &app.state.db,
        app.state.db_backend,
        &row.id,
        &row.authority_did,
        None,
        1000,
    )
    .await
    .unwrap();
    let in_put: Vec<_> = ops
        .iter()
        .filter(|op| Some(&op.rev) == after_put.as_ref())
        .collect();
    assert_eq!(in_put.len(), 2, "both members in the batch's commit");

    let mut expected = vec![
        (owner.clone(), true, true),
        (a.clone(), true, false),
        (b.clone(), true, true),
    ];
    expected.sort();
    assert_eq!(member_records(&app, &space).await, expected);

    // A change of access updates the record; putting the same access again
    // commits nothing.
    put_members(
        &app,
        &owner,
        &space,
        json!([{ "did": a, "read": true, "write": true }]),
    )
    .await;
    assert!(
        member_records(&app, &space)
            .await
            .contains(&(a.clone(), true, true))
    );
    let settled = authority_rev(&app, &space).await;
    put_members(
        &app,
        &owner,
        &space,
        json!([{ "did": a, "read": true, "write": true }]),
    )
    .await;
    assert_eq!(
        authority_rev(&app, &space).await,
        settled,
        "nothing to publish"
    );

    remove_members(&app, &owner, &space, json!([b, rand_did("nobody")])).await;
    let records = member_records(&app, &space).await;
    assert!(records.iter().all(|(did, ..)| did != &b), "{records:?}");
    assert_eq!(records.len(), 2);
}

#[tokio::test]
#[serial]
async fn the_policy_record_follows_the_policy() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let space = create(&app, &owner, memberspace(json!({ "publishPolicy": true }))).await;

    let records = published(&app, &space, POLICY).await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].0, "self");
    assert_eq!(
        records[0].1,
        json!({
            "$type": POLICY,
            "policy": {
                "$type": "com.atproto.memberspace.policy",
                "publicRead": false,
                "publicWrite": false,
                "publishPolicy": true,
                "publishMembers": false,
            },
        })
    );
    // Only the policy was asked for.
    assert!(member_records(&app, &space).await.is_empty());

    update(
        &app,
        &owner,
        &space,
        memberspace(json!({ "publicRead": true, "publishPolicy": true })),
    )
    .await;
    let records = published(&app, &space, POLICY).await;
    assert_eq!(records[0].1["policy"]["publicRead"], json!(true));

    update(&app, &owner, &space, memberspace(json!({}))).await;
    assert!(
        published(&app, &space, POLICY).await.is_empty(),
        "withdrawn"
    );
}

#[tokio::test]
#[serial]
async fn turning_publish_members_on_and_off_publishes_and_withdraws_the_whole_list() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let a = rand_did("a");
    let space = create(&app, &owner, memberspace(json!({}))).await;
    put_members(
        &app,
        &owner,
        &space,
        json!([{ "did": a, "read": true, "write": false }]),
    )
    .await;
    assert!(member_records(&app, &space).await.is_empty());

    update(
        &app,
        &owner,
        &space,
        memberspace(json!({ "publishMembers": true })),
    )
    .await;
    assert_eq!(member_records(&app, &space).await.len(), 2);

    update(&app, &owner, &space, memberspace(json!({}))).await;
    assert!(member_records(&app, &space).await.is_empty());
}

#[tokio::test]
#[serial]
async fn migrating_to_managedspace_withdraws_the_member_list_and_publishes_its_policy() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let space = create(
        &app,
        &owner,
        memberspace(json!({ "publishPolicy": true, "publishMembers": true })),
    )
    .await;
    assert_eq!(member_records(&app, &space).await.len(), 1);

    update(
        &app,
        &owner,
        &space,
        json!({
            "$type": "com.atproto.managedspace.policy",
            "managingApp": "did:web:app.example#space_manager",
            "publishPolicy": true,
        }),
    )
    .await;
    assert!(member_records(&app, &space).await.is_empty());
    let records = published(&app, &space, POLICY).await;
    assert_eq!(
        records[0].1["policy"],
        json!({
            "$type": "com.atproto.managedspace.policy",
            "managingApp": "did:web:app.example#space_manager",
            "publishPolicy": true,
        })
    );
}

#[tokio::test]
#[serial]
async fn the_single_member_methods_keep_the_published_list() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let (a, b) = (rand_did("a"), rand_did("b"));
    let space = create(&app, &owner, memberspace(json!({ "publishMembers": true }))).await;

    // Oct 29's putMember and removeMember.
    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.memberspace.putMember",
        json!({ "space": space, "did": a, "read": true, "write": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        member_records(&app, &space)
            .await
            .contains(&(a.clone(), true, false))
    );

    // Simplespace's putMember and removeMember.
    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.simplespace.putMember",
        json!({ "space": space, "did": b, "read": true, "write": true }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(
        member_records(&app, &space)
            .await
            .contains(&(b.clone(), true, true))
    );

    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.simplespace.removeMember",
        json!({ "space": space, "did": b }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.memberspace.removeMember",
        json!({ "space": space, "did": a }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        member_records(&app, &space).await,
        vec![(owner.clone(), true, true)]
    );
}

#[tokio::test]
#[serial]
async fn a_reader_reads_the_published_records() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let reader = rand_did("reader");
    let space = create(
        &app,
        &owner,
        memberspace(json!({ "publishPolicy": true, "publishMembers": true })),
    )
    .await;
    put_members(
        &app,
        &owner,
        &space,
        json!([{ "did": reader, "read": true, "write": false }]),
    )
    .await;
    let authority = space_row(&app, &space).await.authority_did;

    let (status, body) = get(
        &app,
        &reader,
        &format!(
            "/xrpc/com.atproto.space.getRecord?space={}&repo={}&collection={MEMBER}&rkey={}",
            urlencoding::encode(&space),
            urlencoding::encode(&authority),
            urlencoding::encode(&reader),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["value"]["read"], json!(true));
    assert_eq!(body["value"]["write"], json!(false));

    let (status, body) = get(
        &app,
        &reader,
        &format!(
            "/xrpc/com.atproto.space.getRecord?space={}&repo={}&collection={POLICY}&rkey=self",
            urlencoding::encode(&space),
            urlencoding::encode(&authority),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["value"]["policy"]["$type"],
        json!("com.atproto.memberspace.policy")
    );
}

#[tokio::test]
#[serial]
async fn the_published_collections_are_never_written_directly() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let writer = rand_did("writer");
    let space = create(&app, &owner, memberspace(json!({ "publishMembers": true }))).await;
    put_members(
        &app,
        &owner,
        &space,
        json!([{ "did": writer, "read": true, "write": true }]),
    )
    .await;

    for collection in [POLICY, MEMBER] {
        let rkey = if collection == POLICY {
            "self".to_string()
        } else {
            writer.clone()
        };
        let record = json!({ "$type": collection, "read": true, "write": true });
        for (nsid, body) in [
            (
                "com.atproto.space.createRecord",
                json!({ "space": space, "collection": collection, "record": record }),
            ),
            (
                "com.atproto.space.putRecord",
                json!({ "space": space, "collection": collection, "rkey": rkey, "record": record }),
            ),
            (
                "com.atproto.space.deleteRecord",
                json!({ "space": space, "collection": collection, "rkey": rkey }),
            ),
            (
                "com.atproto.space.applyWrites",
                json!({ "space": space, "writes": [
                    { "action": "create", "collection": "com.example.note", "value": { "$type": "com.example.note" } },
                    { "action": "create", "collection": collection, "rkey": rkey, "value": record },
                ] }),
            ),
        ] {
            for caller in [&writer, &owner] {
                let (status, resp) = post(&app, caller, nsid, body.clone()).await;
                assert_eq!(
                    status,
                    StatusCode::BAD_REQUEST,
                    "{nsid} {collection}: {resp}"
                );
                assert_eq!(resp["error"], json!("InvalidRequest"), "{nsid}: {resp}");
            }
        }
    }
    // Nothing reached the store, and the published list is untouched.
    let row = space_row(&app, &space).await;
    let (records, _) = happyview::spaces::db::list_space_records(
        &app.state.db,
        app.state.db_backend,
        &row.id,
        Some(&writer),
        None,
        100,
        None,
        false,
    )
    .await
    .unwrap();
    assert!(records.is_empty(), "{records:?}");
    assert_eq!(member_records(&app, &space).await.len(), 2);
}

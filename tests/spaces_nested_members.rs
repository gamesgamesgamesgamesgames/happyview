//! Spaces nested in a memberspace (`dev.happyview.space.*NestedSpace`):
//! resolution with access caps, cycles and bounds, credential revocation when
//! membership changes, direct-only listed and published members, and the
//! simplespace mapping of space-members.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

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

fn memberspace(extra: Value) -> Value {
    let mut policy = json!({ "$type": "com.atproto.memberspace.policy" });
    policy
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    policy
}

async fn nest(app: &TestApp, owner: &str, space: &str, nested: &str, read: bool, write: bool) {
    let (status, body) = post(
        app,
        owner,
        "dev.happyview.space.putNestedSpace",
        json!({ "space": space, "nested": nested, "read": read, "write": write }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn unnest(app: &TestApp, owner: &str, space: &str, nested: &str) {
    let (status, body) = post(
        app,
        owner,
        "dev.happyview.space.removeNestedSpace",
        json!({ "space": space, "nested": nested }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// The effective list, as access checks resolve it: (did, read, write).
async fn members(app: &TestApp, _owner: &str, space: &str) -> Vec<(String, bool, bool)> {
    let row = space_row(app, space).await;
    happyview::spaces::members::resolve_members(&app.state.db, app.state.db_backend, &row.id)
        .await
        .expect("resolves")
        .into_iter()
        .map(|m| (m.did, m.access.read, m.access.write))
        .collect()
}

/// The list `listMembers` gives: (did, read, write).
async fn listed(app: &TestApp, owner: &str, space: &str) -> Vec<(String, bool, bool)> {
    let (status, body) = get(
        app,
        owner,
        &format!(
            "/xrpc/com.atproto.memberspace.listMembers?space={}&limit=100",
            urlencoding::encode(space)
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["did"].as_str().unwrap().to_string(),
                m["read"] == json!(true),
                m["write"] == json!(true),
            )
        })
        .collect()
}

/// The member records published in a space's authority repo: (rkey, read,
/// write).
async fn published(app: &TestApp, space: &str) -> Vec<(String, Value, Value)> {
    let row = space_row(app, space).await;
    happyview::spaces::db::repo_collection_records(
        &app.state.db,
        app.state.db_backend,
        &row.id,
        &row.authority_did,
        MEMBER,
        None,
    )
    .await
    .unwrap()
    .into_iter()
    .map(|r| (r.rkey, r.record["read"].clone(), r.record["write"].clone()))
    .collect()
}

fn entry<'a>(list: &'a [(String, bool, bool)], did: &str) -> Option<&'a (String, bool, bool)> {
    list.iter().find(|m| m.0 == did)
}

/// Mint a credential for `did` in `space`, returning nothing; read the
/// outstanding ones with [`outstanding`].
async fn mint(app: &TestApp, space: &str, did: &str) {
    let row = space_row(app, space).await;
    let keyring = app.state.config.keyring().expect("a token encryption key");
    let key = {
        use rand::Rng;
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        p256::ecdsa::SigningKey::from_slice(&bytes).unwrap()
    };
    happyview::spaces::auth::issue_credential(
        &app.state.db,
        app.state.db_backend,
        &app.state.http,
        &keyring,
        &app.state.config.public_url,
        &app.state.config.plc_url,
        &row,
        did,
        None,
        &row.authority_did,
        Some(&happyview::spaces::http_signature::did_key(
            key.verifying_key(),
        )),
    )
    .await
    .expect("minted");
}

async fn outstanding(app: &TestApp, space: &str, did: &str) -> usize {
    let row = space_row(app, space).await;
    happyview::spaces::db::outstanding_credential_jtis(
        &app.state.db,
        app.state.db_backend,
        &row.id,
        did,
    )
    .await
    .unwrap()
    .len()
}

/// Add `count` direct members to a space straight into the store, for the
/// graphs too large to build through the API. Returns their DIDs.
async fn bulk_members(app: &TestApp, space: &str, count: usize, label: &str) -> Vec<String> {
    let row = space_row(app, space).await;
    let dids: Vec<String> = (0..count)
        .map(|i| format!("did:plc:{label}{i:06}"))
        .collect();
    for chunk in dids.chunks(500) {
        let values: Vec<String> = chunk
            .iter()
            .map(|did| {
                format!(
                    "('{}', '{}', '{did}', 1, 0, 0, 0, NULL, '2026-11-13T00:00:00Z')",
                    Uuid::new_v4(),
                    row.id
                )
            })
            .collect();
        let sql = format!(
            "INSERT INTO happyview_space_members (id, space_id, member_did, can_read, can_write, read_self, is_delegation, granted_by, created_at) VALUES {}",
            values.join(", ")
        );
        happyview::db::query(&sql)
            .execute(&app.state.db)
            .await
            .unwrap();
    }
    dids
}

#[tokio::test]
#[serial]
async fn a_nested_spaces_members_get_at_most_the_access_its_reference_grants() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let (writer, other) = (rand_did("writer"), rand_did("other"));
    let a = create(&app, &owner, memberspace(json!({}))).await;
    let b = create(&app, &owner, memberspace(json!({}))).await;
    let c = create(&app, &owner, memberspace(json!({}))).await;
    put_members(
        &app,
        &owner,
        &b,
        json!([{ "did": writer, "read": true, "write": true }]),
    )
    .await;
    put_members(
        &app,
        &owner,
        &c,
        json!([{ "did": other, "read": true, "write": true }]),
    )
    .await;

    nest(&app, &owner, &a, &b, true, false).await;
    let list = members(&app, &owner, &a).await;
    assert_eq!(
        entry(&list, &writer),
        Some(&(writer.clone(), true, false)),
        "{list:?}"
    );

    // Reached a second way with more access, the most permissive wins.
    nest(&app, &owner, &c, &b, true, true).await;
    nest(&app, &owner, &a, &c, true, true).await;
    let list = members(&app, &owner, &a).await;
    assert_eq!(
        entry(&list, &writer),
        Some(&(writer.clone(), true, true)),
        "{list:?}"
    );
    assert_eq!(entry(&list, &other), Some(&(other.clone(), true, true)));

    let (status, body) = get(
        &app,
        &owner,
        &format!(
            "/xrpc/dev.happyview.space.listNestedSpaces?space={}",
            urlencoding::encode(&a)
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let nested: Vec<&str> = body["nested"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["space"].as_str().unwrap())
        .collect();
    let mut expected = vec![b.as_str(), c.as_str()];
    expected.sort();
    assert_eq!(nested, expected);

    unnest(&app, &owner, &a, &c).await;
    let list = members(&app, &owner, &a).await;
    assert_eq!(entry(&list, &writer), Some(&(writer.clone(), true, false)));
    assert!(entry(&list, &other).is_none());

    // A reference that withholds read withholds it from the members it
    // brings in.
    let d = create(&app, &owner, memberspace(json!({}))).await;
    nest(&app, &owner, &d, &b, false, true).await;
    let list = members(&app, &owner, &d).await;
    assert_eq!(
        entry(&list, &writer),
        Some(&(writer.clone(), false, true)),
        "{list:?}"
    );
}

#[tokio::test]
#[serial]
async fn a_cycle_of_nested_spaces_resolves() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let (ma, mb) = (rand_did("ma"), rand_did("mb"));
    let a = create(&app, &owner, memberspace(json!({}))).await;
    let b = create(&app, &owner, memberspace(json!({}))).await;
    put_members(
        &app,
        &owner,
        &a,
        json!([{ "did": ma, "read": true, "write": false }]),
    )
    .await;
    put_members(
        &app,
        &owner,
        &b,
        json!([{ "did": mb, "read": true, "write": false }]),
    )
    .await;
    nest(&app, &owner, &a, &b, true, false).await;
    nest(&app, &owner, &b, &a, true, false).await;
    for space in [&a, &b] {
        let list = members(&app, &owner, space).await;
        assert!(
            entry(&list, &ma).is_some() && entry(&list, &mb).is_some(),
            "{list:?}"
        );
    }
}

#[tokio::test]
#[serial]
async fn only_a_local_memberspace_can_be_nested() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let a = create(&app, &owner, memberspace(json!({}))).await;
    let managed = create(
        &app,
        &owner,
        json!({ "$type": "com.atproto.managedspace.policy", "managingApp": "did:web:app.example#m" }),
    )
    .await;
    for (nested, code) in [
        (managed.as_str(), "PolicyMismatch"),
        (
            "at://did:plc:elsewhere/space/com.example.forum/x",
            "InvalidRequest",
        ),
        ("not a space", "InvalidRequest"),
    ] {
        let (status, body) = post(
            &app,
            &owner,
            "dev.happyview.space.putNestedSpace",
            json!({ "space": a, "nested": nested, "read": true, "write": false }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{nested}: {body}");
        assert_eq!(body["error"], json!(code), "{nested}: {body}");
    }
    // Only the owner nests.
    let b = create(&app, &owner, memberspace(json!({}))).await;
    let (status, body) = post(
        &app,
        &rand_did("stranger"),
        "dev.happyview.space.putNestedSpace",
        json!({ "space": a, "nested": b, "read": true, "write": false }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
#[serial]
async fn nesting_requires_administering_the_nested_space() {
    common::require_db!();
    let app = setup().await;
    let (x, y) = (rand_did("x"), rand_did("y"));
    let member = rand_did("member");
    let xs = create(&app, &x, memberspace(json!({ "publishMembers": true }))).await;
    let ys = create(&app, &y, memberspace(json!({}))).await;
    put_members(
        &app,
        &y,
        &ys,
        json!([{ "did": member, "read": true, "write": true }]),
    )
    .await;

    // Every way of nesting Y's space in X's is refused, by any name.
    let ys_id = space_row(&app, &ys).await.id;
    let ys_ats = ys
        .replacen("at://", "ats://", 1)
        .replacen("/space/", "/", 1);
    for (nsid, body) in [
        (
            "com.atproto.simplespace.putMember",
            json!({ "space": xs, "did": ys_id, "read": true, "write": false, "isDelegation": true }),
        ),
        (
            "com.atproto.simplespace.addMember",
            json!({ "space": xs, "did": ys_ats }),
        ),
        (
            "dev.happyview.space.putNestedSpace",
            json!({ "space": xs, "nested": ys, "read": true, "write": false }),
        ),
        (
            "com.atproto.simplespace.putMember",
            json!({ "space": xs, "did": ys, "read": true, "write": false }),
        ),
        (
            "com.atproto.simplespace.addMember",
            json!({ "space": xs, "did": ys }),
        ),
    ] {
        let (status, body) = post(&app, &x, nsid, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{nsid}: {body}");
    }
    let (status, body) = get(
        &app,
        &x,
        &format!(
            "/xrpc/dev.happyview.space.listNestedSpaces?space={}",
            urlencoding::encode(&xs)
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["nested"], json!([]));
    assert!(entry(&members(&app, &x, &xs).await, &member).is_none());
    assert!(entry(&listed(&app, &x, &xs).await, &member).is_none());
    assert!(published(&app, &xs).await.iter().all(|r| r.0 != member));

    // Nor can Y, who administers the nested space but not X's.
    let (status, body) = post(
        &app,
        &y,
        "dev.happyview.space.putNestedSpace",
        json!({ "space": xs, "nested": ys, "read": true, "write": false }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
#[serial]
async fn removing_a_member_known_only_through_a_nested_space_fails_the_batch() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let (direct, derived) = (rand_did("direct"), rand_did("derived"));
    let a = create(&app, &owner, memberspace(json!({}))).await;
    let b = create(&app, &owner, memberspace(json!({}))).await;
    put_members(
        &app,
        &owner,
        &a,
        json!([{ "did": direct, "read": true, "write": false }]),
    )
    .await;
    put_members(
        &app,
        &owner,
        &b,
        json!([{ "did": derived, "read": true, "write": false }]),
    )
    .await;
    nest(&app, &owner, &a, &b, true, false).await;

    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.memberspace.removeMembers",
        json!({ "space": a, "dids": [direct, derived] }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], json!("InvalidRequest"));
    assert!(body["message"].as_str().unwrap().contains(&b), "{body}");
    // Nothing in the batch was applied.
    assert!(entry(&members(&app, &owner, &a).await, &direct).is_some());
}

#[tokio::test]
#[serial]
async fn losing_read_through_a_reference_two_levels_down_revokes_credentials() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let member = rand_did("member");
    let a = create(&app, &owner, memberspace(json!({}))).await;
    let b = create(&app, &owner, memberspace(json!({}))).await;
    let c = create(&app, &owner, memberspace(json!({}))).await;
    put_members(
        &app,
        &owner,
        &c,
        json!([{ "did": member, "read": true, "write": false }]),
    )
    .await;
    nest(&app, &owner, &a, &b, true, false).await;
    nest(&app, &owner, &b, &c, true, false).await;
    assert!(entry(&members(&app, &owner, &a).await, &member).is_some());

    mint(&app, &a, &member).await;
    mint(&app, &b, &member).await;
    assert_eq!(outstanding(&app, &a, &member).await, 1);

    // B drops C: the member reached A only through it, two levels down.
    unnest(&app, &owner, &b, &c).await;
    assert_eq!(outstanding(&app, &a, &member).await, 0, "revoked in A");
    assert_eq!(outstanding(&app, &b, &member).await, 0, "revoked in B");

    // And when the member leaves C itself.
    nest(&app, &owner, &b, &c, true, false).await;
    mint(&app, &a, &member).await;
    remove_members(&app, &owner, &c, json!([member])).await;
    assert_eq!(outstanding(&app, &a, &member).await, 0, "revoked in A");

    // A member who keeps read another way keeps their credential.
    put_members(
        &app,
        &owner,
        &c,
        json!([{ "did": member, "read": true, "write": false }]),
    )
    .await;
    put_members(
        &app,
        &owner,
        &a,
        json!([{ "did": member, "read": true, "write": false }]),
    )
    .await;
    mint(&app, &a, &member).await;
    unnest(&app, &owner, &b, &c).await;
    assert_eq!(
        outstanding(&app, &a, &member).await,
        1,
        "still a direct member"
    );
}

#[tokio::test]
#[serial]
async fn listed_and_published_members_are_the_spaces_own() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let (direct, nested) = (rand_did("direct"), rand_did("nested"));
    let a = create(&app, &owner, memberspace(json!({ "publishMembers": true }))).await;
    let b = create(&app, &owner, memberspace(json!({}))).await;
    let c = create(&app, &owner, memberspace(json!({}))).await;
    nest(&app, &owner, &a, &b, true, false).await;
    nest(&app, &owner, &b, &c, true, true).await;
    put_members(
        &app,
        &owner,
        &a,
        json!([{ "did": direct, "read": true, "write": true }]),
    )
    .await;
    // A change two levels down changes who reads A, but not A's own list.
    put_members(
        &app,
        &owner,
        &c,
        json!([{ "did": nested, "read": true, "write": true }]),
    )
    .await;
    assert_eq!(
        entry(&members(&app, &owner, &a).await, &nested),
        Some(&(nested.clone(), true, false)),
        "reads A through the nested spaces"
    );

    let list = listed(&app, &owner, &a).await;
    assert_eq!(entry(&list, &direct), Some(&(direct.clone(), true, true)));
    assert!(entry(&list, &nested).is_none(), "{list:?}");
    let records = published(&app, &a).await;
    assert!(
        records
            .iter()
            .any(|r| *r == (direct.clone(), json!(true), json!(true))),
        "{records:?}"
    );
    assert!(records.iter().all(|r| r.0 != nested), "{records:?}");

    unnest(&app, &owner, &a, &b).await;
    assert!(published(&app, &a).await.iter().any(|r| r.0 == direct));
}

#[tokio::test]
#[serial]
async fn simplespace_space_members_are_nested_spaces() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let member = rand_did("member");
    let a = create(&app, &owner, memberspace(json!({}))).await;
    let b = create(&app, &owner, memberspace(json!({}))).await;
    put_members(
        &app,
        &owner,
        &b,
        json!([{ "did": member, "read": true, "write": true }]),
    )
    .await;

    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.simplespace.putMember",
        json!({ "space": a, "did": b, "read": true, "write": false, "isDelegation": true }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        entry(&members(&app, &owner, &a).await, &member),
        Some(&(member.clone(), true, false))
    );

    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.simplespace.removeMember",
        json!({ "space": a, "did": b }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(entry(&members(&app, &owner, &a).await, &member).is_none());
}

#[tokio::test]
#[serial]
async fn a_member_list_past_the_bound_is_refused_by_name() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let a = create(&app, &owner, memberspace(json!({}))).await;
    let b = create(&app, &owner, memberspace(json!({}))).await;
    bulk_members(&app, &b, happyview::spaces::members::MAX_RESOLVED, "wide").await;

    // Nesting a space that would take A past the bound is refused, and
    // leaves nothing behind.
    let (status, body) = post(
        &app,
        &owner,
        "dev.happyview.space.putNestedSpace",
        json!({ "space": a, "nested": b, "read": true, "write": false }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], json!("MemberListTooLarge"));
    assert_eq!(members(&app, &owner, &a).await.len(), 1, "only the owner");

    // A nested space that grows past it makes access checks fail, by name.
    let c = create(&app, &owner, memberspace(json!({}))).await;
    nest(&app, &owner, &a, &c, true, false).await;
    bulk_members(&app, &c, happyview::spaces::members::MAX_RESOLVED, "grown").await;
    let row = space_row(&app, &a).await;
    let err =
        happyview::spaces::members::is_member(&app.state.db, app.state.db_backend, &row.id, &owner)
            .await
            .expect_err("past the bound");
    assert!(
        matches!(
            err,
            happyview::error::AppError::XrpcError {
                code: "MemberListTooLarge",
                ..
            }
        ),
        "{err:?}"
    );
    // The space's own member list still reads.
    assert_eq!(listed(&app, &owner, &a).await.len(), 1, "only the owner");
}

/// The cost of one access check on a graph as deep as resolution goes, and
/// close to as wide: ten levels of nesting, each space with 900 members.
#[tokio::test]
#[serial]
async fn an_access_check_on_a_wide_deep_graph_stays_cheap() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let mut spaces = Vec::new();
    for level in 0..10 {
        let space = create(&app, &owner, memberspace(json!({}))).await;
        bulk_members(&app, &space, 900, &format!("l{level}m")).await;
        spaces.push(space);
    }
    for pair in spaces.windows(2) {
        nest(&app, &owner, &pair[0], &pair[1], true, false).await;
    }
    let root = space_row(&app, &spaces[0]).await;
    let deepest = "did:plc:l9m000899";

    let runs = 5;
    let started = std::time::Instant::now();
    for _ in 0..runs {
        let access = happyview::spaces::members::is_member(
            &app.state.db,
            app.state.db_backend,
            &root.id,
            deepest,
        )
        .await
        .unwrap()
        .expect("reached ten levels down");
        assert!(access.read && !access.write);
    }
    let per_check = started.elapsed() / runs;
    eprintln!("one access check over 10 levels x 900 members: {per_check:?}");
    assert!(
        per_check < std::time::Duration::from_secs(2),
        "an access check took {per_check:?}"
    );
}

/// A space past the resolution bound admits no one, so a change below it
/// revokes its credentials for the accounts that change touches, and a
/// space's own nesting can still be undone once it is past the bound.
#[tokio::test]
#[serial]
async fn spaces_past_the_bound_still_take_member_changes() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let member = rand_did("member");
    let a = create(&app, &owner, memberspace(json!({}))).await;
    let b = create(&app, &owner, memberspace(json!({}))).await;
    let wide = create(&app, &owner, memberspace(json!({}))).await;
    put_members(
        &app,
        &owner,
        &b,
        json!([{ "did": member, "read": true, "write": false }]),
    )
    .await;
    nest(&app, &owner, &a, &b, true, false).await;
    nest(&app, &owner, &a, &wide, true, false).await;
    mint(&app, &a, &member).await;
    // A nested space grows past the bound after the credential was issued.
    bulk_members(
        &app,
        &wide,
        happyview::spaces::members::MAX_RESOLVED,
        "past",
    )
    .await;

    // The member leaves B: A can't be resolved to see whether they still
    // read it, and admits no one, so the credential goes.
    remove_members(&app, &owner, &b, json!([member])).await;
    assert_eq!(outstanding(&app, &a, &member).await, 0, "revoked in A");
    // A's references still change while it stays past the bound.
    let empty = create(&app, &owner, memberspace(json!({}))).await;
    nest(&app, &owner, &a, &empty, true, false).await;

    // Removing the nesting that took A past the bound works, and revokes the
    // credentials of accounts A no longer admits.
    put_members(
        &app,
        &owner,
        &b,
        json!([{ "did": member, "read": true, "write": false }]),
    )
    .await;
    unnest(&app, &owner, &a, &wide).await;
    mint(&app, &a, &member).await;
    let grown = create(&app, &owner, memberspace(json!({}))).await;
    nest(&app, &owner, &a, &grown, true, false).await;
    bulk_members(
        &app,
        &grown,
        happyview::spaces::members::MAX_RESOLVED,
        "grown",
    )
    .await;
    // An account A admitted once, still holding a credential for it.
    let other = rand_did("other");
    let row = space_row(&app, &a).await;
    happyview::db::query(&happyview::db::adapt_sql(
        "INSERT INTO happyview_space_credentials (id, space_id, issued_to, token_hash, jti, expires_at, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        app.state.db_backend,
    ))
    .bind(Uuid::new_v4().to_string())
    .bind(&row.id)
    .bind(&other)
    .bind(format!("hash{}", Uuid::new_v4().simple()))
    .bind(format!("jti{}", Uuid::new_v4().simple()))
    .bind("9999-12-31T00:00:00+00:00")
    .bind("2026-11-19T00:00:00+00:00")
    .execute(&app.state.db)
    .await
    .unwrap();
    unnest(&app, &owner, &a, &grown).await;
    assert_eq!(
        outstanding(&app, &a, &member).await,
        1,
        "still reads A through B"
    );
    assert_eq!(outstanding(&app, &a, &other).await, 0, "no longer admitted");
}

/// A credential issued while a member's removal is committing in a space
/// two levels down follows the removal: issue reads the effective list again
/// under a share lock on every space it reads, so it waits for the removal
/// and is refused, rather than stored after the removal's revocation looked.
#[tokio::test]
#[serial]
async fn a_credential_issued_while_a_removal_commits_two_levels_down_follows_it() {
    common::require_db!();
    let app = setup().await;
    let backend = app.state.db_backend;
    let owner = rand_did("owner");
    let member = rand_did("member");
    let a = create(&app, &owner, memberspace(json!({}))).await;
    let b = create(&app, &owner, memberspace(json!({}))).await;
    let c = create(&app, &owner, memberspace(json!({}))).await;
    put_members(
        &app,
        &owner,
        &c,
        json!([{ "did": member, "read": true, "write": false }]),
    )
    .await;
    nest(&app, &owner, &a, &b, true, false).await;
    nest(&app, &owner, &b, &c, true, false).await;
    let a_row = space_row(&app, &a).await;
    let c_row = space_row(&app, &c).await;

    // The member's removal from C, as `removeMembers` makes it, not yet
    // committed.
    let mut removal = happyview::db::begin_write(&app.state.db, backend)
        .await
        .unwrap();
    happyview::spaces::db::lock_space(&mut *removal, backend, &c_row.id)
        .await
        .unwrap();
    assert!(
        happyview::spaces::db::remove_direct_member(&mut *removal, backend, &c_row.id, &member)
            .await
            .unwrap()
    );

    let state = app.state.clone();
    let issued_to = member.clone();
    let issue = tokio::spawn(async move {
        let keyring = state.config.keyring().expect("a token encryption key");
        happyview::spaces::auth::issue_credential(
            &state.db,
            state.db_backend,
            &state.http,
            &keyring,
            &state.config.public_url,
            &state.config.plc_url,
            &a_row,
            &issued_to,
            None,
            &a_row.authority_did,
            None,
        )
        .await
        .map(|_| ())
    });
    if backend == happyview::db::DatabaseBackend::Postgres {
        for _ in 0..100 {
            let waiting: i64 = happyview::db::query_scalar(
                "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock'",
            )
            .fetch_one(&app.state.db)
            .await
            .unwrap();
            if waiting > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
    } else {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    removal.commit().await.unwrap();

    let result = issue.await.unwrap();
    assert!(
        matches!(result, Err(happyview::error::AppError::Forbidden(_))),
        "{result:?}"
    );
    assert_eq!(outstanding(&app, &a, &member).await, 0);
}

/// A credential stored while a nested space's removal is waiting to start
/// is revoked by it: issuing holds the nesting space's row share-locked from
/// its membership check to the insert, and the removal takes that row's lock
/// before it reads who reads the space.
#[tokio::test]
#[serial]
async fn a_credential_issued_while_a_nested_space_is_removed_follows_the_removal() {
    common::require_db!();
    let app = setup().await;
    let backend = app.state.db_backend;
    let owner = rand_did("owner");
    let member = rand_did("member");
    let a = create(&app, &owner, memberspace(json!({}))).await;
    let b = create(&app, &owner, memberspace(json!({}))).await;
    put_members(
        &app,
        &owner,
        &b,
        json!([{ "did": member, "read": true, "write": false }]),
    )
    .await;
    nest(&app, &owner, &a, &b, true, false).await;
    let a_row = space_row(&app, &a).await;

    // An issue that has found the member admitted to A, and not yet stored
    // the credential.
    let mut issue = happyview::db::begin_write(&app.state.db, backend)
        .await
        .unwrap();
    if backend == happyview::db::DatabaseBackend::Postgres {
        happyview::db::query("SELECT id FROM happyview_spaces WHERE id = $1 FOR SHARE")
            .bind(&a_row.id)
            .execute(&mut *issue)
            .await
            .unwrap();
    }

    let router = app.router.clone();
    let (name, value) = common::auth::admin_cookie_header(&owner, &app.state.cookie_key);
    let body = json!({ "space": a, "nested": b }).to_string();
    let removal = tokio::spawn(async move {
        let req = Request::builder()
            .method("POST")
            .uri("/xrpc/dev.happyview.space.removeNestedSpace")
            .header(name, value)
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        router.oneshot(req).await.unwrap().status()
    });
    if backend == happyview::db::DatabaseBackend::Postgres {
        for _ in 0..100 {
            let waiting: i64 = happyview::db::query_scalar(
                "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock'",
            )
            .fetch_one(&app.state.db)
            .await
            .unwrap();
            if waiting > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
    } else {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    happyview::db::query(&happyview::db::adapt_sql(
        "INSERT INTO happyview_space_credentials (id, space_id, issued_to, token_hash, jti, expires_at, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        backend,
    ))
    .bind(Uuid::new_v4().to_string())
    .bind(&a_row.id)
    .bind(&member)
    .bind(format!("hash{}", Uuid::new_v4().simple()))
    .bind(format!("jti{}", Uuid::new_v4().simple()))
    .bind("9999-12-31T00:00:00+00:00")
    .bind("2026-11-19T00:00:00+00:00")
    .execute(&mut *issue)
    .await
    .unwrap();
    issue.commit().await.unwrap();

    assert_eq!(removal.await.unwrap(), StatusCode::OK);
    assert_eq!(outstanding(&app, &a, &member).await, 0);
}

async fn nested_list(app: &TestApp, owner: &str, space: &str) -> Vec<String> {
    let (status, body) = get(
        app,
        owner,
        &format!(
            "/xrpc/dev.happyview.space.listNestedSpaces?space={}",
            urlencoding::encode(space)
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["nested"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["space"].as_str().unwrap().to_string())
        .collect()
}

/// Moving a space to a policy without a member list discards its nested
/// spaces with its members: the spaces that nest it stop admitting their
/// members, and moving back doesn't bring them back.
#[tokio::test]
#[serial]
async fn discarding_a_member_list_discards_its_nested_spaces() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let member = rand_did("member");
    let p = create(&app, &owner, memberspace(json!({}))).await;
    let y = create(&app, &owner, memberspace(json!({}))).await;
    let q = create(&app, &owner, memberspace(json!({}))).await;
    put_members(
        &app,
        &owner,
        &y,
        json!([{ "did": member, "read": true, "write": false }]),
    )
    .await;
    nest(&app, &owner, &p, &y, true, false).await;
    nest(&app, &owner, &q, &p, true, false).await;
    mint(&app, &q, &member).await;

    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.space.updateSpace",
        json!({ "space": p, "policy": {
            "$type": "com.atproto.managedspace.policy",
            "managingApp": "did:plc:app",
        }}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(entry(&members(&app, &owner, &q).await, &member).is_none());
    assert_eq!(outstanding(&app, &q, &member).await, 0, "revoked in Q");

    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.space.updateSpace",
        json!({ "space": p, "policy": memberspace(json!({})) }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(nested_list(&app, &owner, &p).await.is_empty());
    assert!(entry(&members(&app, &owner, &q).await, &member).is_none());
}

/// A space past the bound whose nesting space still resolves, because the
/// part past the bound sits deeper than the nesting space reads: a removal in
/// it revokes the nesting space's credentials for the accounts it took away.
#[tokio::test]
#[serial]
async fn a_removal_in_a_space_past_the_bound_revokes_above_it() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let member = rand_did("member");
    let q = create(&app, &owner, memberspace(json!({}))).await;
    let p = create(&app, &owner, memberspace(json!({}))).await;
    let e = create(&app, &owner, memberspace(json!({}))).await;
    put_members(
        &app,
        &owner,
        &e,
        json!([{ "did": member, "read": true, "write": false }]),
    )
    .await;
    nest(&app, &owner, &q, &p, true, false).await;
    nest(&app, &owner, &p, &e, true, false).await;
    // P -> C1 -> ... -> C8 -> H: H is nine levels below P, ten below Q, so Q
    // never reads it.
    let mut above = p.clone();
    for _ in 0..8 {
        let c = create(&app, &owner, memberspace(json!({}))).await;
        nest(&app, &owner, &above, &c, true, false).await;
        above = c;
    }
    let h = create(&app, &owner, memberspace(json!({}))).await;
    nest(&app, &owner, &above, &h, true, false).await;
    mint(&app, &q, &member).await;
    bulk_members(&app, &h, happyview::spaces::members::MAX_RESOLVED, "deep").await;
    assert!(entry(&members(&app, &owner, &q).await, &member).is_some());

    unnest(&app, &owner, &p, &e).await;
    assert_eq!(outstanding(&app, &q, &member).await, 0, "revoked in Q");
}

/// Deleting a nested space removes the references to it, and revokes the
/// credentials its members held for the spaces that nested it.
#[tokio::test]
#[serial]
async fn deleting_a_nested_space_removes_it_from_the_spaces_that_nest_it() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let member = rand_did("member");
    let a = create(&app, &owner, memberspace(json!({}))).await;
    let b = create(&app, &owner, memberspace(json!({}))).await;
    put_members(
        &app,
        &owner,
        &b,
        json!([{ "did": member, "read": true, "write": false }]),
    )
    .await;
    nest(&app, &owner, &a, &b, true, false).await;
    mint(&app, &a, &member).await;

    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.space.deleteSpace",
        json!({ "space": b }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(outstanding(&app, &a, &member).await, 0, "revoked in A");
    assert!(nested_list(&app, &owner, &a).await.is_empty());
}

/// Revocation follows a cycle of nested spaces.
#[tokio::test]
#[serial]
async fn a_removal_in_a_cycle_revokes_around_it() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let member = rand_did("member");
    let a = create(&app, &owner, memberspace(json!({}))).await;
    let b = create(&app, &owner, memberspace(json!({}))).await;
    put_members(
        &app,
        &owner,
        &b,
        json!([{ "did": member, "read": true, "write": false }]),
    )
    .await;
    nest(&app, &owner, &a, &b, true, false).await;
    nest(&app, &owner, &b, &a, true, false).await;
    mint(&app, &a, &member).await;
    remove_members(&app, &owner, &b, json!([member])).await;
    assert_eq!(outstanding(&app, &a, &member).await, 0, "revoked in A");
}

#[tokio::test]
#[serial]
async fn a_space_cant_nest_itself() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let a = create(&app, &owner, memberspace(json!({}))).await;
    let (status, body) = post(
        &app,
        &owner,
        "dev.happyview.space.putNestedSpace",
        json!({ "space": a, "nested": a, "read": true, "write": false }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], json!("InvalidRequest"));
}

/// `simplespace` names a space as a member every way it always could: by
/// space id, by address, by `ats://` address, or by a space elsewhere, and
/// answers with the member as it always shaped it.
#[tokio::test]
#[serial]
async fn simplespace_names_spaces_as_members_every_way_it_could() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let member = rand_did("member");
    let a = create(&app, &owner, memberspace(json!({}))).await;
    let b = create(&app, &owner, memberspace(json!({}))).await;
    put_members(
        &app,
        &owner,
        &b,
        json!([{ "did": member, "read": true, "write": true }]),
    )
    .await;
    let a_row = space_row(&app, &a).await;
    let b_row = space_row(&app, &b).await;

    // By space id: nested, with the member shaped as before.
    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.simplespace.putMember",
        json!({ "space": a, "did": b_row.id, "read": true, "write": false, "isDelegation": true }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let shaped = &body["member"];
    for field in [
        "id",
        "space_id",
        "did",
        "access",
        "is_delegation",
        "created_at",
    ] {
        assert!(!shaped[field].is_null(), "{field}: {body}");
    }
    assert_eq!(shaped["space_id"], json!(a_row.id));
    assert_eq!(shaped["did"], json!(b_row.id));
    assert_eq!(shaped["is_delegation"], json!(true));
    assert_eq!(nested_list(&app, &owner, &a).await, vec![b.clone()]);
    assert_eq!(
        entry(&members(&app, &owner, &a).await, &member),
        Some(&(member.clone(), true, false))
    );

    // Adding it again is a conflict, by any name.
    for name in [b.clone(), b_row.id.clone()] {
        let (status, body) = post(
            &app,
            &owner,
            "com.atproto.simplespace.addMember",
            json!({ "space": a, "did": name, "isDelegation": true }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{name}: {body}");
    }

    // Removed by id.
    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.simplespace.removeMember",
        json!({ "space": a, "did": b_row.id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(nested_list(&app, &owner, &a).await.is_empty());

    // A member row naming the space from before nesting: adding the space
    // again is a conflict, as it always was, and removing it removes the row.
    happyview::db::query(&happyview::db::adapt_sql(
        "INSERT INTO happyview_space_members (id, space_id, member_did, can_read, can_write, read_self, is_delegation, granted_by, created_at) VALUES (?, ?, ?, 1, 0, 0, 1, NULL, ?)",
        app.state.db_backend,
    ))
    .bind(Uuid::new_v4().to_string())
    .bind(&a_row.id)
    .bind(&b)
    .bind("2026-11-01T00:00:00Z")
    .execute(&app.state.db)
    .await
    .unwrap();
    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.simplespace.addMember",
        json!({ "space": a, "did": b }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(nested_list(&app, &owner, &a).await.is_empty());
    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.simplespace.removeMember",
        json!({ "space": a, "did": b }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Added by address, removed by its ats:// form, along with a member row
    // naming the same space by its id from before nesting.
    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.simplespace.addMember",
        json!({ "space": a, "did": b }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let ats = b.replacen("at://", "ats://", 1).replacen("/space/", "/", 1);
    happyview::db::query(&happyview::db::adapt_sql(
        "INSERT INTO happyview_space_members (id, space_id, member_did, can_read, can_write, read_self, is_delegation, granted_by, created_at) VALUES (?, ?, ?, 1, 0, 0, 1, NULL, ?)",
        app.state.db_backend,
    ))
    .bind(Uuid::new_v4().to_string())
    .bind(&a_row.id)
    .bind(&b_row.id)
    .bind("2026-11-01T00:00:00Z")
    .execute(&app.state.db)
    .await
    .unwrap();
    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.simplespace.removeMember",
        json!({ "space": a, "did": ats }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(nested_list(&app, &owner, &a).await.is_empty());
    assert!(entry(&members(&app, &owner, &a).await, &member).is_none());
    let rows: i64 = happyview::db::query_scalar(&happyview::db::adapt_sql(
        "SELECT count(*) FROM happyview_space_members WHERE space_id = ? AND is_delegation = 1",
        app.state.db_backend,
    ))
    .bind(&a_row.id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(rows, 0, "the member row went too");

    // A space elsewhere is stored as a member naming it, and brings in no
    // members.
    let elsewhere = "at://did:plc:elsewhere/space/com.example.forum/x";
    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.simplespace.putMember",
        json!({ "space": a, "did": elsewhere, "read": true, "write": false, "isDelegation": true }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["member"]["did"], json!(elsewhere));
    assert!(nested_list(&app, &owner, &a).await.is_empty());
    assert_eq!(members(&app, &owner, &a).await.len(), 1, "only the owner");
    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.simplespace.addMember",
        json!({ "space": a, "did": elsewhere }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, body) = post(
        &app,
        &owner,
        "com.atproto.simplespace.removeMember",
        json!({ "space": a, "did": elsewhere }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// A reference brings in members only while its address names the space it
/// was made to, so a space later found at that address isn't nested by it.
#[tokio::test]
#[serial]
async fn a_reference_follows_the_space_it_was_made_to() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let member = rand_did("member");
    let a = create(&app, &owner, memberspace(json!({}))).await;
    let b = create(&app, &owner, memberspace(json!({}))).await;
    put_members(
        &app,
        &owner,
        &b,
        json!([{ "did": member, "read": true, "write": false }]),
    )
    .await;
    nest(&app, &owner, &a, &b, true, false).await;
    assert!(entry(&members(&app, &owner, &a).await, &member).is_some());

    // The reference now records another space than the one at its address.
    let a_row = space_row(&app, &a).await;
    happyview::db::query(&happyview::db::adapt_sql(
        "UPDATE happyview_space_nested SET target_space_id = ? WHERE space_id = ?",
        app.state.db_backend,
    ))
    .bind(Uuid::new_v4().to_string())
    .bind(&a_row.id)
    .execute(&app.state.db)
    .await
    .unwrap();
    assert!(entry(&members(&app, &owner, &a).await, &member).is_none());
}

/// Delegation entries naming other addresses on a space's DID don't count
/// against what a change to the space reads.
#[tokio::test]
#[serial]
async fn entries_naming_other_addresses_dont_hold_up_a_change() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let member = rand_did("member");
    let b = create(&app, &owner, memberspace(json!({}))).await;
    let x = create(&app, &rand_did("other"), memberspace(json!({}))).await;
    put_members(
        &app,
        &owner,
        &b,
        json!([{ "did": member, "read": true, "write": false }]),
    )
    .await;
    let b_row = space_row(&app, &b).await;
    let x_row = space_row(&app, &x).await;
    let rows: Vec<String> = (0..=happyview::spaces::members::MAX_RESOLVED)
        .map(|i| {
            format!(
                "('{}', '{}', 'at://{}/space/com.example.other/n{i}', 1, 0, 0, 1, NULL, '2026-11-01T00:00:00Z')",
                Uuid::new_v4(),
                x_row.id,
                b_row.did
            )
        })
        .collect();
    for chunk in rows.chunks(500) {
        happyview::db::query(&format!(
            "INSERT INTO happyview_space_members (id, space_id, member_did, can_read, can_write, read_self, is_delegation, granted_by, created_at) VALUES {}",
            chunk.join(", ")
        ))
        .execute(&app.state.db)
        .await
        .unwrap();
    }
    remove_members(&app, &owner, &b, json!([member])).await;
}

/// The migration that records nested spaces' ids fills them in for the
/// references made before it, by address.
#[tokio::test]
#[serial]
async fn the_target_id_backfill_finds_each_nested_space() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let a = create(&app, &owner, memberspace(json!({}))).await;
    let b = create(&app, &owner, memberspace(json!({}))).await;
    nest(&app, &owner, &a, &b, true, false).await;
    let a_row = space_row(&app, &a).await;
    let b_row = space_row(&app, &b).await;
    let gone = format!(
        "at://did:plc:gone{}/space/com.example.forum/x",
        Uuid::new_v4().simple()
    );
    happyview::db::query(&happyview::db::adapt_sql(
        "INSERT INTO happyview_space_nested (id, space_id, target_space_uri, can_read, can_write, created_at) VALUES (?, ?, ?, 1, 0, ?)",
        app.state.db_backend,
    ))
    .bind(Uuid::new_v4().to_string())
    .bind(&a_row.id)
    .bind(&gone)
    .bind("2026-11-01T00:00:00Z")
    .execute(&app.state.db)
    .await
    .unwrap();
    happyview::db::query(&happyview::db::adapt_sql(
        "UPDATE happyview_space_nested SET target_space_id = NULL WHERE space_id = ?",
        app.state.db_backend,
    ))
    .bind(&a_row.id)
    .execute(&app.state.db)
    .await
    .unwrap();

    let migration = match app.state.db_backend {
        happyview::db::DatabaseBackend::Postgres => {
            include_str!("../migrations/postgres/20261121000001_space_nested_target_id.sql")
        }
        happyview::db::DatabaseBackend::Sqlite => {
            include_str!("../migrations/sqlite/20261121000001_space_nested_target_id.sql")
        }
    };
    let backfill = migration
        .split(';')
        .map(|statement| {
            statement
                .lines()
                .filter(|line| !line.trim_start().starts_with("--"))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .find(|statement| statement.trim_start().starts_with("UPDATE"))
        .expect("the backfill");
    happyview::db::query(&backfill)
        .execute(&app.state.db)
        .await
        .unwrap();

    let ids: Vec<(String, Option<String>)> = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT target_space_uri, target_space_id FROM happyview_space_nested WHERE space_id = ? ORDER BY target_space_uri",
        app.state.db_backend,
    ))
    .bind(&a_row.id)
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    let mut expected = vec![(b.clone(), Some(b_row.id.clone())), (gone, None)];
    expected.sort();
    assert_eq!(ids, expected);
}

//! Spaces nested in a memberspace (`dev.happyview.space.*NestedSpace`):
//! resolution with access caps, cycles and bounds, the cascade that revokes
//! credentials and updates published lists when membership changes, and the
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

/// The effective list, as `listMembers` gives it: (did, read, write).
async fn members(app: &TestApp, owner: &str, space: &str) -> Vec<(String, bool, bool)> {
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
async fn the_published_list_follows_nested_members() {
    common::require_db!();
    let app = setup().await;
    let owner = rand_did("owner");
    let member = rand_did("member");
    let a = create(&app, &owner, memberspace(json!({ "publishMembers": true }))).await;
    let b = create(&app, &owner, memberspace(json!({}))).await;
    let c = create(&app, &owner, memberspace(json!({}))).await;
    nest(&app, &owner, &a, &b, true, false).await;
    nest(&app, &owner, &b, &c, true, true).await;

    // A change two levels down reaches A's published list.
    put_members(
        &app,
        &owner,
        &c,
        json!([{ "did": member, "read": true, "write": true }]),
    )
    .await;
    let row = space_row(&app, &a).await;
    let record = |records: Vec<happyview::spaces::types::SpaceRecord>| {
        records
            .into_iter()
            .find(|r| r.rkey == member)
            .map(|r| (r.record["read"].clone(), r.record["write"].clone()))
    };
    let published = happyview::spaces::db::repo_collection_records(
        &app.state.db,
        app.state.db_backend,
        &row.id,
        &row.authority_did,
        MEMBER,
        None,
    )
    .await
    .unwrap();
    assert_eq!(record(published), Some((json!(true), json!(false))));

    unnest(&app, &owner, &a, &b).await;
    let published = happyview::spaces::db::repo_collection_records(
        &app.state.db,
        app.state.db_backend,
        &row.id,
        &row.authority_did,
        MEMBER,
        None,
    )
    .await
    .unwrap();
    assert_eq!(record(published), None);
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

    // A nested space that grows past it makes the list unreadable, by name.
    let c = create(&app, &owner, memberspace(json!({}))).await;
    nest(&app, &owner, &a, &c, true, false).await;
    bulk_members(&app, &c, happyview::spaces::members::MAX_RESOLVED, "grown").await;
    let (status, body) = get(
        &app,
        &owner,
        &format!(
            "/xrpc/com.atproto.memberspace.listMembers?space={}",
            urlencoding::encode(&a)
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], json!("MemberListTooLarge"));
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

/// A removal whose cascade didn't finish is finished by a retry: the retry
/// finds nothing left to remove, and still revokes what the first left.
#[tokio::test]
#[serial]
async fn a_retried_removal_revokes_what_the_first_attempt_left() {
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

    // The first attempt removed the reference and stopped there.
    let row = space_row(&app, &a).await;
    happyview::db::query(&format!(
        "DELETE FROM happyview_space_nested WHERE space_id = '{}'",
        row.id
    ))
    .execute(&app.state.db)
    .await
    .unwrap();
    assert_eq!(outstanding(&app, &a, &member).await, 1);

    let (status, body) = post(
        &app,
        &owner,
        "dev.happyview.space.removeNestedSpace",
        json!({ "space": a, "nested": b }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        outstanding(&app, &a, &member).await,
        0,
        "revoked on the retry"
    );
}

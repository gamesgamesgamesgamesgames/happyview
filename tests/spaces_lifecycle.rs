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

async fn get(app: &TestApp, path: &str, did: &str) -> (StatusCode, Value) {
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

/// A space that can sign commits and credentials.
async fn signing_setup() -> (TestApp, String) {
    let mut app = TestApp::new_with_encryption().await;
    app.setup_did_web().await;
    enable_spaces(&app).await;
    (app, rand_did("creator"))
}

async fn update(app: &TestApp, creator: &str, space: &str, policy: Value) {
    let (status, body) = post(
        app,
        "com.atproto.space.updateSpace",
        creator,
        json!({ "space": space, "policy": policy }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn stored(app: &TestApp, space: &str) -> happyview::spaces::types::Space {
    happyview::spaces::db::get_space_by_address(
        &app.state.db,
        app.state.db_backend,
        space.trim_start_matches("at://").split('/').next().unwrap(),
        "com.example.forum",
        "main",
    )
    .await
    .unwrap()
    .unwrap()
}

async fn member_dids(app: &TestApp, space: &str) -> Vec<String> {
    let row = stored(app, space).await;
    happyview::spaces::members::resolve_members(&app.state.db, app.state.db_backend, &row.id)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.did)
        .collect()
}

/// The creator writes and reads a space directly, as its owner.
async fn assert_creator_writes_and_reads(app: &TestApp, creator: &str, space: &str) {
    let (status, body) = post(
        app,
        "com.atproto.space.createRecord",
        creator,
        json!({
            "space": space,
            "collection": "com.example.note",
            "record": { "$type": "com.example.note", "text": "hi" },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = get(
        app,
        &format!(
            "/xrpc/com.atproto.space.listRecords?space={}&collection=com.example.note",
            urlencoding::encode(space)
        ),
        creator,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// Migrating a space discards its member list, the creator's row with it.
/// The creator stands in for the space's authority, and keeps direct access
/// after a migration either way.
#[tokio::test]
#[serial]
async fn the_creator_keeps_access_across_a_migration_each_way() {
    common::require_db!();
    let (app, creator) = signing_setup().await;
    let space = create(&app, &creator, "main", json!({ "$type": MEMBERSPACE })).await;

    update(
        &app,
        &creator,
        &space,
        json!({ "$type": MANAGEDSPACE, "managingApp": "did:plc:app" }),
    )
    .await;
    assert!(!member_dids(&app, &space).await.contains(&creator));
    assert_creator_writes_and_reads(&app, &creator, &space).await;

    update(&app, &creator, &space, json!({ "$type": MEMBERSPACE })).await;
    assert!(!member_dids(&app, &space).await.contains(&creator));
    assert_creator_writes_and_reads(&app, &creator, &space).await;
}

/// The creator is issued a credential with no member row, and under a
/// managing app that would refuse them: here `did:plc:app`, which resolves
/// to nothing.
#[tokio::test]
#[serial]
async fn the_creator_is_issued_a_credential_whatever_the_policy_says() {
    common::require_db!();
    let (app, creator) = signing_setup().await;
    let space = create(&app, &creator, "main", json!({ "$type": MEMBERSPACE })).await;

    for policy in [
        json!({ "$type": MANAGEDSPACE, "managingApp": "did:plc:app" }),
        json!({ "$type": MEMBERSPACE }),
    ] {
        update(&app, &creator, &space, policy.clone()).await;
        assert!(!member_dids(&app, &space).await.contains(&creator));

        let (status, body) = get(
            &app,
            &format!(
                "/xrpc/com.atproto.space.getDelegationToken?space={}",
                urlencoding::encode(&space)
            ),
            &creator,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{policy}: {body}");
        let key = {
            use rand::Rng;
            let mut bytes = [0u8; 32];
            rand::rng().fill_bytes(&mut bytes);
            p256::ecdsa::SigningKey::from_slice(&bytes).unwrap()
        };
        let req = Request::builder()
            .method("POST")
            .uri("/xrpc/com.atproto.space.getSpaceCredential")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "space": space,
                    "credentialKid": happyview::spaces::http_signature::did_key(key.verifying_key()),
                    "delegationToken": body["token"],
                })
                .to_string(),
            ))
            .unwrap();
        let resp = app.router.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let body = json_of(resp).await;
        assert_eq!(status, StatusCode::OK, "{policy}: {body}");
        assert!(body["credential"].is_string(), "{policy}: {body}");
    }
}

/// A managedspace has no member list, so none can be written to it.
#[tokio::test]
#[serial]
async fn a_managedspace_takes_no_members() {
    common::require_db!();
    let (app, creator) = setup().await;
    let space = create(
        &app,
        &creator,
        "main",
        json!({ "$type": MANAGEDSPACE, "managingApp": "did:plc:app" }),
    )
    .await;
    for method in [
        "com.atproto.simplespace.putMember",
        "com.atproto.simplespace.addMember",
    ] {
        let (status, body) = post(
            &app,
            method,
            &creator,
            json!({ "space": space, "did": rand_did("member"), "read": true, "write": false }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method}: {body}");
        assert_eq!(body["error"], json!("InvalidRequest"), "{method}");
    }
}

/// Access rules from before policies existed match no policy. Replacing them
/// with a managedspace discards the member list; with a memberspace, keeps it.
#[tokio::test]
#[serial]
async fn replacing_legacy_rules_keeps_the_member_list_only_for_a_memberspace() {
    common::require_db!();
    for (policy, kept) in [
        (
            json!({ "$type": MANAGEDSPACE, "managingApp": "did:plc:app" }),
            false,
        ),
        (json!({ "$type": MEMBERSPACE }), true),
    ] {
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

        let mut row = stored(&app, &space).await;
        row.read_policy = happyview::spaces::types::Policy::ManagingApp {
            managing_app: "did:plc:app".into(),
        };
        happyview::spaces::db::update_space(&app.state.db, app.state.db_backend, &row)
            .await
            .unwrap();

        update(&app, &creator, &space, policy.clone()).await;
        assert_eq!(
            member_dids(&app, &space).await.contains(&member),
            kept,
            "{policy}"
        );
    }
}

/// An invite made while a space was a memberspace adds no one once it is a
/// managedspace: migrating deletes the space's invites, and the invitee can
/// neither join nor write.
#[tokio::test]
#[serial]
async fn migrating_to_a_managedspace_withdraws_its_invites() {
    common::require_db!();
    let (app, creator) = signing_setup().await;
    let space = create(&app, &creator, "main", json!({ "$type": MEMBERSPACE })).await;
    let (status, body) = post(
        &app,
        "dev.happyview.space.createInvite",
        &creator,
        json!({ "space": space, "access": { "read": true, "write": true } }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let token = body["token"].as_str().unwrap().to_string();

    update(
        &app,
        &creator,
        &space,
        json!({ "$type": MANAGEDSPACE, "managingApp": "did:plc:app" }),
    )
    .await;

    use sha2::Digest;
    let hash = hex::encode(sha2::Sha256::digest(token.as_bytes()));
    let invite =
        happyview::spaces::db::get_invite_by_token_hash(&app.state.db, app.state.db_backend, &hash)
            .await
            .unwrap();
    assert!(invite.is_none(), "the invite outlived the migration");

    let invitee = rand_did("invitee");
    let (status, body) = post(
        &app,
        "dev.happyview.space.acceptInvite",
        &invitee,
        json!({ "token": token }),
    )
    .await;
    assert!(status.is_client_error(), "{status}: {body}");
    assert!(!member_dids(&app, &space).await.contains(&invitee));

    let (status, body) = post(
        &app,
        "com.atproto.space.createRecord",
        &invitee,
        json!({
            "space": space,
            "collection": "com.example.note",
            "record": { "$type": "com.example.note", "text": "hi" },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

/// A managedspace refuses to make an invite, and refuses to honour one that
/// exists anyway: accepting re-checks the policy where the member is added.
#[tokio::test]
#[serial]
async fn a_managedspace_makes_and_honours_no_invites() {
    common::require_db!();
    let (app, creator) = setup().await;
    let space = create(
        &app,
        &creator,
        "main",
        json!({ "$type": MANAGEDSPACE, "managingApp": "did:plc:app" }),
    )
    .await;
    let (status, body) = post(
        &app,
        "dev.happyview.space.createInvite",
        &creator,
        json!({ "space": space }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], json!("InvalidRequest"));

    use sha2::Digest;
    let token = Uuid::new_v4().simple().to_string();
    let invite = happyview::spaces::types::SpaceInvite {
        id: Uuid::new_v4().to_string(),
        space_id: stored(&app, &space).await.id,
        token_hash: hex::encode(sha2::Sha256::digest(token.as_bytes())),
        created_by: creator.clone(),
        access: happyview::spaces::types::MemberAccess::WRITE,
        max_uses: None,
        uses: 0,
        expires_at: None,
        revoked: false,
        created_at: happyview::db::now_rfc3339(),
    };
    happyview::spaces::db::create_invite(&app.state.db, app.state.db_backend, &invite)
        .await
        .unwrap();
    let invitee = rand_did("invitee");
    let (status, body) = post(
        &app,
        "dev.happyview.space.acceptInvite",
        &invitee,
        json!({ "token": token }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], json!("InvalidRequest"));
    assert!(!member_dids(&app, &space).await.contains(&invitee));
}

/// Give `space` rules from before policies: one of reads and writes through
/// a managing app, the other by its member list. No policy describes that.
async fn make_mixed_rules(app: &TestApp, space: &str, app_reads: bool) {
    use happyview::spaces::types::Policy;
    let managing_app = Policy::ManagingApp {
        managing_app: "did:plc:app".into(),
    };
    let mut row = stored(app, space).await;
    (row.read_policy, row.write_policy) = if app_reads {
        (managing_app, Policy::MemberList)
    } else {
        (Policy::MemberList, managing_app)
    };
    happyview::spaces::db::update_space(&app.state.db, app.state.db_backend, &row)
        .await
        .unwrap();
}

/// Reads through a managing app, writes by the member list.
async fn make_mixed(app: &TestApp, space: &str) {
    make_mixed_rules(app, space, true).await;
}

/// On a space whose read rule is a managing app, a member-list writer still
/// writes, but reads only with a credential the managing app decides on: a
/// direct read is refused. A delegation token, their way to that credential,
/// is still issued, and the creator still reads.
#[tokio::test]
#[serial]
async fn a_member_list_writer_on_an_app_read_space_is_refused_a_direct_read() {
    common::require_db!();
    let (app, creator) = signing_setup().await;
    let space = create(&app, &creator, "main", json!({ "$type": MEMBERSPACE })).await;
    let writer = rand_did("writer");
    let (status, _) = post(
        &app,
        "com.atproto.simplespace.putMember",
        &creator,
        json!({ "space": space, "did": writer, "read": true, "write": true }),
    )
    .await;
    assert!(status.is_success());
    make_mixed(&app, &space).await;

    let (status, body) = post(
        &app,
        "com.atproto.space.createRecord",
        &writer,
        json!({
            "space": space,
            "collection": "com.example.note",
            "record": { "$type": "com.example.note", "text": "hi" },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let rkey = body["uri"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_string();

    let encoded = urlencoding::encode(&space).into_owned();
    for path in [
        format!("/xrpc/com.atproto.space.listRecords?space={encoded}&collection=com.example.note"),
        format!(
            "/xrpc/com.atproto.space.getRecord?space={encoded}&collection=com.example.note&repo={}&rkey={rkey}",
            urlencoding::encode(&writer)
        ),
        format!("/xrpc/com.atproto.space.listRepos?space={encoded}"),
    ] {
        let (status, body) = get(&app, &path, &writer).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {body}");
        let (status, body) = get(&app, &path, &creator).await;
        assert_eq!(status, StatusCode::OK, "creator, {path}: {body}");
    }

    let (status, body) = get(
        &app,
        &format!("/xrpc/com.atproto.space.getDelegationToken?space={encoded}"),
        &writer,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// The rules of a memberspace whose read rule is not a managing app are
/// untouched: its members still read directly.
#[tokio::test]
#[serial]
async fn a_memberspace_member_still_reads_directly() {
    common::require_db!();
    let (app, creator) = signing_setup().await;
    let space = create(&app, &creator, "main", json!({ "$type": MEMBERSPACE })).await;
    let member = rand_did("member");
    post(
        &app,
        "com.atproto.simplespace.putMember",
        &creator,
        json!({ "space": space, "did": member, "read": true, "write": false }),
    )
    .await;
    let (status, body) = get(
        &app,
        &format!(
            "/xrpc/com.atproto.space.listRecords?space={}&collection=com.example.note",
            urlencoding::encode(&space)
        ),
        &member,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn enable_inspector(app: &TestApp) {
    let (name, value) = app.admin_cookie();
    let enable = Request::builder()
        .method("PUT")
        .uri("/admin/settings/feature.space_inspector_enabled")
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(json!({ "value": "true" }).to_string()))
        .unwrap();
    assert!(
        app.router
            .clone()
            .oneshot(enable)
            .await
            .unwrap()
            .status()
            .is_success()
    );
}

async fn retired_policies(app: &TestApp) -> Value {
    let (name, value) = app.admin_cookie();
    let req = Request::builder()
        .uri("/admin/spaces/retired-policies")
        .header(name, value)
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    json_of(resp).await
}

/// Spaces in a retired shape are counted for any operator, and named only
/// while the space inspector is on. A memberspace and a managedspace are not
/// retired.
#[tokio::test]
#[serial]
async fn spaces_in_a_retired_shape_are_listed_to_operators() {
    common::require_db!();
    let (app, creator) = setup().await;
    create(&app, &creator, "member", json!({ "$type": MEMBERSPACE })).await;
    create(
        &app,
        &creator,
        "managed",
        json!({ "$type": MANAGEDSPACE, "managingApp": "did:plc:app" }),
    )
    .await;
    let mixed = create(&app, &creator, "main", json!({ "$type": MEMBERSPACE })).await;
    make_mixed(&app, &mixed).await;

    let body = retired_policies(&app).await;
    assert_eq!(body["count"], json!(1), "{body}");
    assert!(
        body.get("spaces").is_none(),
        "named with the inspector off: {body}"
    );

    enable_inspector(&app).await;

    let body = retired_policies(&app).await;
    assert_eq!(body["count"], json!(1), "{body}");
    assert_eq!(body["spaces"][0]["uri"], json!(mixed), "{body}");
    assert_eq!(
        body["spaces"][0]["readPolicy"]["$type"],
        json!("com.atproto.simplespace.defs#managingAppPolicy")
    );
    assert_eq!(
        body["spaces"][0]["writePolicy"]["$type"],
        json!("com.atproto.simplespace.defs#memberListPolicy")
    );
}

/// On a space whose write rule is a managing app, a member-list reader still
/// reads directly, but writes only through their own PDS, which the managing
/// app admits or not: a direct write is refused. The creator still writes.
#[tokio::test]
#[serial]
async fn a_member_list_reader_on_an_app_write_space_is_refused_a_direct_write() {
    common::require_db!();
    let (app, creator) = signing_setup().await;
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
    make_mixed_rules(&app, &space, false).await;

    let record = json!({ "$type": "com.example.note", "text": "hi" });
    let writes = [
        (
            "com.atproto.space.createRecord",
            json!({ "space": space, "collection": "com.example.note", "record": record }),
        ),
        (
            "com.atproto.space.putRecord",
            json!({ "space": space, "collection": "com.example.note", "rkey": "a", "record": record }),
        ),
        (
            "com.atproto.space.applyWrites",
            json!({ "space": space, "writes": [
                { "action": "create", "collection": "com.example.note", "value": record },
            ] }),
        ),
    ];
    for (method, body) in &writes {
        let (status, reply) = post(&app, method, &member, body.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method}: {reply}");
    }
    let (status, reply) = post(&app, writes[0].0, &creator, writes[0].1.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "creator: {reply}");

    let (status, reply) = get(
        &app,
        &format!(
            "/xrpc/com.atproto.space.listRecords?space={}&collection=com.example.note",
            urlencoding::encode(&space)
        ),
        &member,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
}

/// Convert a space through the admin API, as an operator.
async fn convert(app: &TestApp, space: &str, body: Value) -> (StatusCode, Value) {
    let id = stored(app, space).await.id;
    let (name, value) = app.admin_cookie();
    let req = Request::builder()
        .method("POST")
        .uri(format!("/admin/spaces/{id}/convert"))
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    (status, json_of(resp).await)
}

async fn invite_count(app: &TestApp, space: &str) -> usize {
    let id = stored(app, space).await.id;
    happyview::spaces::db::list_invites(&app.state.db, app.state.db_backend, &id)
        .await
        .unwrap()
        .len()
}

/// A space in a retired shape, with a member-list writer and an invite.
async fn retired_with_member_and_invite(app: &TestApp, creator: &str) -> (String, String) {
    let space = create(app, creator, "main", json!({ "$type": MEMBERSPACE })).await;
    let member = rand_did("member");
    post(
        app,
        "com.atproto.simplespace.putMember",
        creator,
        json!({ "space": space, "did": member, "read": true, "write": true }),
    )
    .await;
    let (status, body) = post(
        app,
        "dev.happyview.space.createInvite",
        creator,
        json!({ "space": space }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    make_mixed(app, &space).await;
    (space, member)
}

/// Converted to a memberspace, a retired space keeps its members and
/// invites, and its members read it directly again.
#[tokio::test]
#[serial]
async fn converting_a_retired_space_to_a_memberspace_keeps_its_members() {
    common::require_db!();
    let (app, creator) = signing_setup().await;
    enable_inspector(&app).await;
    let (space, member) = retired_with_member_and_invite(&app, &creator).await;

    let (status, body) = convert(&app, &space, json!({ "to": "memberspace" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["policy"]["$type"], json!(MEMBERSPACE), "{body}");
    assert!(member_dids(&app, &space).await.contains(&member));
    assert_eq!(invite_count(&app, &space).await, 1);
    assert_eq!(retired_policies(&app).await["count"], json!(0));

    let (status, body) = get(
        &app,
        &format!(
            "/xrpc/com.atproto.space.listRecords?space={}&collection=com.example.note",
            urlencoding::encode(&space)
        ),
        &member,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// Converted to a managedspace, a retired space keeps its managing app and
/// discards its members and invites.
#[tokio::test]
#[serial]
async fn converting_a_retired_space_to_a_managedspace_discards_its_members() {
    common::require_db!();
    let (app, creator) = signing_setup().await;
    enable_inspector(&app).await;
    let (space, member) = retired_with_member_and_invite(&app, &creator).await;

    let (status, body) = convert(&app, &space, json!({ "to": "managedspace" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["policy"],
        json!({ "$type": MANAGEDSPACE, "managingApp": "did:plc:app", "publishPolicy": false }),
    );
    assert!(!member_dids(&app, &space).await.contains(&member));
    assert_eq!(invite_count(&app, &space).await, 0);
    assert_eq!(retired_policies(&app).await["count"], json!(0));
}

/// Only a space in a retired shape converts, and only with the inspector on.
#[tokio::test]
#[serial]
async fn conversion_is_for_retired_spaces_with_the_inspector_on() {
    common::require_db!();
    let (app, creator) = setup().await;
    let (space, _) = retired_with_member_and_invite(&app, &creator).await;
    let (status, body) = convert(&app, &space, json!({ "to": "memberspace" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], json!("SpaceInspectorDisabled"), "{body}");

    enable_inspector(&app).await;
    // `stored` finds a space by its key, `main`, so this one has another creator.
    let policy = create(
        &app,
        &rand_did("other"),
        "main",
        json!({ "$type": MEMBERSPACE }),
    )
    .await;
    let (status, body) = convert(&app, &policy, json!({ "to": "managedspace" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

/// Converting needs `spaces:update`: a session with only `spaces:read` is
/// refused, and the space keeps its rules, members and invites.
#[tokio::test]
#[serial]
async fn conversion_needs_spaces_update() {
    common::require_db!();
    let (app, creator) = signing_setup().await;
    enable_inspector(&app).await;
    let (space, member) = retired_with_member_and_invite(&app, &creator).await;
    let before = stored(&app, &space).await;

    let key = common::api_key(&app, &["spaces:read"]).await;
    let req = Request::builder()
        .method("POST")
        .uri(format!("/admin/spaces/{}/convert", before.id))
        .header("authorization", format!("Bearer {key}"))
        .header("content-type", "application/json")
        .body(Body::from(json!({ "to": "managedspace" }).to_string()))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let after = stored(&app, &space).await;
    assert_eq!(after.read_policy, before.read_policy);
    assert_eq!(after.write_policy, before.write_policy);
    assert!(member_dids(&app, &space).await.contains(&member));
    assert_eq!(invite_count(&app, &space).await, 1);
    assert_eq!(retired_policies(&app).await["count"], json!(1));
}

/// A managedspace has no app allow list, so converting a retired space that
/// has one says which apps it dropped.
#[tokio::test]
#[serial]
async fn a_managedspace_conversion_names_the_allow_list_it_drops() {
    common::require_db!();
    let (app, creator) = signing_setup().await;
    enable_inspector(&app).await;
    let space = create(&app, &creator, "main", json!({ "$type": MEMBERSPACE })).await;
    make_mixed(&app, &space).await;
    let mut row = stored(&app, &space).await;
    row.app_access = happyview::spaces::types::AppAccess::AllowList {
        allowed: vec!["https://app.example/client-metadata.json".into()],
    };
    happyview::spaces::db::update_space(&app.state.db, app.state.db_backend, &row)
        .await
        .unwrap();

    let (status, body) = convert(&app, &space, json!({ "to": "managedspace" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["droppedAllowedApps"],
        json!(["https://app.example/client-metadata.json"])
    );
}

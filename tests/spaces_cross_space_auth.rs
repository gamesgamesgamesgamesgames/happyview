mod common;

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Request, StatusCode};
use happyview::db::now_rfc3339;
use happyview::spaces::db as spaces_db;
use happyview::spaces::types::*;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

const ADMIN_A: &str = "did:plc:crossspace-admin-a";
const ADMIN_B: &str = "did:plc:crossspace-admin-b";
const OUTSIDER: &str = "did:plc:crossspace-outsider";
const READER: &str = "did:plc:crossspace-reader";
const SELF_READER: &str = "did:plc:crossspace-self-reader";
const NO_READ: &str = "did:plc:crossspace-no-read";
const AUTHOR: &str = "did:plc:crossspace-author";
const TYPE: &str = "com.example.crossspace";
const COLLECTION: &str = "com.example.crossspace.post";

fn space_uri(authority: &str, skey: &str) -> String {
    format!("at://{authority}/space/{TYPE}/{skey}")
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
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert!(resp.status().is_success(), "failed to enable spaces");
}

/// A space whose authority and creator is `authority`. Returns its id.
async fn create_space(app: &TestApp, authority: &str, skey: &str) -> String {
    let now = now_rfc3339();
    let id = Uuid::new_v4().to_string();
    let space = Space {
        id: id.clone(),
        did: authority.to_string(),
        authority_did: authority.to_string(),
        creator_did: authority.to_string(),
        type_nsid: TYPE.to_string(),
        skey: skey.to_string(),
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
        .expect("create_space failed");
    id
}

async fn add_member(app: &TestApp, space_id: &str, did: &str, access: MemberAccess) {
    spaces_db::add_member(
        &app.state.db,
        app.state.db_backend,
        &SpaceMember {
            id: Uuid::new_v4().to_string(),
            space_id: space_id.to_string(),
            did: did.to_string(),
            access,
            is_delegation: false,
            granted_by: None,
            created_at: now_rfc3339(),
        },
    )
    .await
    .expect("add_member failed");
}

async fn put_record(app: &TestApp, space_id: &str, space: &str, author: &str, rkey: &str) {
    spaces_db::insert_space_record(
        &app.state.db,
        app.state.db_backend,
        &SpaceRecord {
            uri: format!("{space}/{author}/{COLLECTION}/{rkey}"),
            space_id: space_id.to_string(),
            author_did: author.to_string(),
            collection: COLLECTION.to_string(),
            rkey: rkey.to_string(),
            record: json!({ "$type": COLLECTION, "by": author }),
            cid: String::new(),
            indexed_at: now_rfc3339(),
        },
    )
    .await
    .expect("insert_space_record failed");
}

fn cookie_for(app: &TestApp, did: &str) -> (HeaderName, HeaderValue) {
    common::auth::admin_cookie_header(did, &app.state.cookie_key)
}

async fn send(app: &TestApp, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn post(app: &TestApp, did: &str, method: &str, body: Value) -> Request<Body> {
    let (name, value) = cookie_for(app, did);
    Request::builder()
        .method("POST")
        .uri(format!("/xrpc/{method}"))
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn get(app: &TestApp, did: &str, method: &str, query: &[(&str, &str)]) -> Request<Body> {
    let (name, value) = cookie_for(app, did);
    let query = query
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    Request::builder()
        .method("GET")
        .uri(format!("/xrpc/{method}?{query}"))
        .header(name, value)
        .body(Body::empty())
        .unwrap()
}

/// An admin of one space names another space's invite while revoking in their
/// own. The invite must be untouched and the call must read as not found.
#[tokio::test]
#[serial]
async fn an_invite_is_revoked_only_through_its_own_space() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    let a = create_space(&app, ADMIN_A, "invites-a").await;
    let b = create_space(&app, ADMIN_B, "invites-b").await;
    assert_ne!(a, b);

    let invite_id = Uuid::new_v4().to_string();
    spaces_db::create_invite(
        &app.state.db,
        app.state.db_backend,
        &SpaceInvite {
            id: invite_id.clone(),
            space_id: b.clone(),
            token_hash: format!("hash-{invite_id}"),
            created_by: ADMIN_B.to_string(),
            access: MemberAccess::READ,
            max_uses: None,
            uses: 0,
            expires_at: None,
            revoked: false,
            created_at: now_rfc3339(),
        },
    )
    .await
    .expect("create_invite failed");

    let (status, body) = send(
        &app,
        post(
            &app,
            ADMIN_A,
            "dev.happyview.space.revokeInvite",
            json!({ "space": space_uri(ADMIN_A, "invites-a"), "inviteId": invite_id }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let invites = spaces_db::list_invites(&app.state.db, app.state.db_backend, &b)
        .await
        .unwrap();
    let invite = invites.iter().find(|i| i.id == invite_id).expect("invite");
    assert!(!invite.revoked, "another space's admin revoked the invite");

    let (status, body) = send(
        &app,
        post(
            &app,
            ADMIN_B,
            "dev.happyview.space.revokeInvite",
            json!({ "space": space_uri(ADMIN_B, "invites-b"), "inviteId": invite_id }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

fn register_body(space: &str) -> Value {
    json!({
        "space": space,
        "serviceDid": "did:web:notify.example.com",
        "endpoint": "https://notify.example.com/hook",
    })
}

/// A registration is told every writer's DID and revision, so only a caller
/// who may read the whole space can make one.
#[tokio::test]
#[serial]
async fn only_a_whole_space_reader_registers_for_notifications() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    let a = create_space(&app, ADMIN_A, "notify-a").await;
    let b = create_space(&app, ADMIN_B, "notify-b").await;
    add_member(&app, &b, OUTSIDER, MemberAccess::READ).await;
    add_member(&app, &a, SELF_READER, MemberAccess::READ_SELF).await;
    add_member(&app, &a, READER, MemberAccess::READ).await;
    let space = space_uri(ADMIN_A, "notify-a");

    for did in [OUTSIDER, SELF_READER] {
        let (status, body) = send(
            &app,
            post(
                &app,
                did,
                "com.atproto.space.registerNotify",
                register_body(&space),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{did}: {body}");
    }
    let registrations =
        spaces_db::list_notify_registrations(&app.state.db, app.state.db_backend, &a)
            .await
            .unwrap();
    assert!(registrations.is_empty(), "{registrations:?}");

    let (status, body) = send(
        &app,
        post(
            &app,
            READER,
            "com.atproto.space.registerNotify",
            register_body(&space),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// A member without read access gets nothing from listRecords, and a member
/// of another space is refused as a stranger.
#[tokio::test]
#[serial]
async fn list_records_needs_read_access_in_the_space_it_names() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    let a = create_space(&app, ADMIN_A, "list-a").await;
    let b = create_space(&app, ADMIN_B, "list-b").await;
    let space_a = space_uri(ADMIN_A, "list-a");
    add_member(&app, &a, NO_READ, MemberAccess::parse_wire("none").unwrap()).await;
    add_member(&app, &b, OUTSIDER, MemberAccess::READ).await;
    put_record(&app, &a, &space_a, NO_READ, "own").await;

    for did in [NO_READ, OUTSIDER] {
        let (status, body) = send(
            &app,
            get(
                &app,
                did,
                "com.atproto.space.listRecords",
                &[("space", &space_a)],
            ),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{did}: {body}");
    }
}

/// Two authors write the same collection and rkey. A read_self member asking
/// without a repo gets their own record, a reader naming a repo gets that
/// repo's, and a record in another space is not found.
#[tokio::test]
#[serial]
async fn get_record_reads_the_repo_it_names() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;
    let a = create_space(&app, ADMIN_A, "get-a").await;
    let b = create_space(&app, ADMIN_B, "get-b").await;
    let space_a = space_uri(ADMIN_A, "get-a");
    let space_b = space_uri(ADMIN_B, "get-b");
    add_member(&app, &a, SELF_READER, MemberAccess::READ_SELF).await;
    add_member(&app, &a, READER, MemberAccess::READ).await;
    // The other author's row goes in first, so a lookup by rkey alone finds it.
    put_record(&app, &a, &space_a, AUTHOR, "shared").await;
    put_record(&app, &a, &space_a, SELF_READER, "shared").await;
    put_record(&app, &b, &space_b, AUTHOR, "elsewhere").await;

    let by = |body: &Value| body["value"]["by"].as_str().map(str::to_string);

    let (status, body) = send(
        &app,
        get(
            &app,
            SELF_READER,
            "com.atproto.space.getRecord",
            &[
                ("space", &space_a),
                ("collection", COLLECTION),
                ("rkey", "shared"),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(by(&body).as_deref(), Some(SELF_READER));

    let (status, body) = send(
        &app,
        get(
            &app,
            SELF_READER,
            "com.atproto.space.getRecord",
            &[
                ("space", &space_a),
                ("repo", AUTHOR),
                ("collection", COLLECTION),
                ("rkey", "shared"),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, body) = send(
        &app,
        get(
            &app,
            READER,
            "com.atproto.space.getRecord",
            &[
                ("space", &space_a),
                ("repo", AUTHOR),
                ("collection", COLLECTION),
                ("rkey", "shared"),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(by(&body).as_deref(), Some(AUTHOR));

    let (status, body) = send(
        &app,
        get(
            &app,
            READER,
            "com.atproto.space.getRecord",
            &[
                ("space", &space_a),
                ("repo", AUTHOR),
                ("collection", COLLECTION),
                ("rkey", "elsewhere"),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

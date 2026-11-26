//! Reading a space with a credential: `Authorization: Atproto-Space-Credential`, signed by
//! the credential's bound key and addressed to the DID being read.

mod common;

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Request, StatusCode};
use happyview::spaces::http_signature;
use http_body_util::BodyExt;
use p256::ecdsa::SigningKey;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

const CREATOR: &str = "did:plc:creduse-creator";
const MEMBER: &str = "did:plc:creduse-member";

fn cookie_for(app: &TestApp, did: &str) -> (HeaderName, HeaderValue) {
    common::auth::admin_cookie_header(did, &app.state.cookie_key)
}

async fn json_of(resp: axum::http::Response<Body>) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or(json!(null))
}

async fn call(app: &TestApp, req: Request<Body>) -> axum::http::Response<Body> {
    app.router.clone().oneshot(req).await.unwrap()
}

fn post_as(app: &TestApp, did: &str, nsid: &str, body: Value) -> Request<Body> {
    let (name, value) = cookie_for(app, did);
    Request::builder()
        .method("POST")
        .uri(format!("/xrpc/{nsid}"))
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn key() -> SigningKey {
    use rand::Rng;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    SigningKey::from_slice(&bytes).unwrap()
}

/// A space on this instance, with `MEMBER` as a reader who has written a
/// record, and a credential for it bound to `key`. Returns the space and the
/// credential.
async fn setup(app: &mut TestApp, key: &SigningKey) -> (String, String) {
    app.setup_did_web().await;
    let (name, value) = app.admin_cookie();
    let enable = Request::builder()
        .method("PUT")
        .uri("/admin/settings/feature.spaces_enabled")
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(json!({ "value": "true" }).to_string()))
        .unwrap();
    assert!(call(app, enable).await.status().is_success());

    let created = call(
        app,
        post_as(
            app,
            CREATOR,
            "com.atproto.space.createSpace",
            json!({
                "spaceType": "com.example.creduse",
                "skey": format!("s{}", Uuid::new_v4().simple()),
                "policy": { "$type": "com.atproto.memberspace.policy" },
            }),
        ),
    )
    .await;
    assert!(created.status().is_success(), "createSpace failed");
    let space = json_of(created).await["uri"].as_str().unwrap().to_string();

    let put = call(
        app,
        post_as(
            app,
            CREATOR,
            "com.atproto.simplespace.putMember",
            json!({ "space": space, "did": MEMBER, "read": true, "write": true }),
        ),
    )
    .await;
    assert!(put.status().is_success(), "putMember failed");

    let wrote = call(
        app,
        post_as(
            app,
            MEMBER,
            "com.atproto.space.createRecord",
            json!({
                "space": space,
                "collection": "com.example.note",
                "record": { "$type": "com.example.note", "text": "hi" },
            }),
        ),
    )
    .await;
    assert_eq!(wrote.status(), StatusCode::CREATED);

    let (name, value) = cookie_for(app, MEMBER);
    let delegation = call(
        app,
        Request::builder()
            .uri(format!(
                "/xrpc/com.atproto.space.getDelegationToken?space={}",
                urlencoding::encode(&space)
            ))
            .header(name, value)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(delegation.status(), StatusCode::OK);
    let token = json_of(delegation).await["token"]
        .as_str()
        .unwrap()
        .to_string();

    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.getSpaceCredential")
        .header("content-type", "application/json");
    let minted = call(
        app,
        req.body(Body::from(
            json!({
                "space": space,
                "credentialKid": http_signature::did_key(key.verifying_key()),
                "delegationToken": token,
            })
            .to_string(),
        ))
        .unwrap(),
    )
    .await;
    assert_eq!(minted.status(), StatusCode::OK);
    let credential = json_of(minted).await["credential"]
        .as_str()
        .unwrap()
        .to_string();
    (space, credential)
}

/// A credential-authenticated GET, signed by `key` for `audience`.
fn read(path: &str, credential: &str, key: &SigningKey, audience: &str) -> Request<Body> {
    let signed = http_signature::sign(
        key,
        &format!("Atproto-Space-Credential {credential}"),
        audience,
    );
    let mut req = Request::builder().uri(path);
    for (name, value) in &signed {
        req = req.header(name, value);
    }
    req.body(Body::empty()).unwrap()
}

fn latest_commit_path(space: &str, repo: &str) -> String {
    format!(
        "/xrpc/com.atproto.space.getLatestCommit?space={}&did={}",
        urlencoding::encode(space),
        urlencoding::encode(repo)
    )
}

#[tokio::test]
#[serial]
async fn reads_a_repo_with_a_credential_addressed_to_it() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let key = key();
    let (space, credential) = setup(&mut app, &key).await;

    let resp = call(
        &app,
        read(
            &latest_commit_path(&space, MEMBER),
            &credential,
            &key,
            MEMBER,
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
#[serial]
async fn refuses_a_credential_addressed_to_another_repo() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let key = key();
    let (space, credential) = setup(&mut app, &key).await;

    let resp = call(
        &app,
        read(
            &latest_commit_path(&space, MEMBER),
            &credential,
            &key,
            CREATOR,
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(json_of(resp).await["error"], json!("BadSpaceAudience"));
}

#[tokio::test]
#[serial]
async fn refuses_a_credential_signed_by_another_key() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let key = key();
    let (space, credential) = setup(&mut app, &key).await;

    let resp = call(
        &app,
        read(
            &latest_commit_path(&space, MEMBER),
            &credential,
            &self::key(),
            MEMBER,
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(json_of(resp).await["error"], json!("BadSpaceSignature"));
}

#[tokio::test]
#[serial]
async fn lists_a_spaces_repos_with_a_credential_addressed_to_its_authority() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let key = key();
    let (space, credential) = setup(&mut app, &key).await;
    let authority = space
        .strip_prefix("at://")
        .unwrap()
        .split('/')
        .next()
        .unwrap()
        .to_string();

    let path = format!(
        "/xrpc/com.atproto.space.listRepos?space={}",
        urlencoding::encode(&space)
    );
    let resp = call(&app, read(&path, &credential, &key, &authority)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = call(&app, read(&path, &credential, &key, MEMBER)).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(json_of(resp).await["error"], json!("BadSpaceAudience"));
}

#[tokio::test]
#[serial]
async fn gets_a_space_with_a_credential_addressed_to_its_authority() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let key = key();
    let (space, credential) = setup(&mut app, &key).await;
    let authority = space
        .strip_prefix("at://")
        .unwrap()
        .split('/')
        .next()
        .unwrap()
        .to_string();

    let path = format!(
        "/xrpc/com.atproto.space.getSpace?space={}",
        urlencoding::encode(&space)
    );
    let resp = call(&app, read(&path, &credential, &key, &authority)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = call(&app, read(&path, &credential, &key, MEMBER)).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(json_of(resp).await["error"], json!("BadSpaceAudience"));
}

async fn space_admin(app: &TestApp, nsid: &str, body: Value) {
    let resp = call(app, post_as(app, CREATOR, nsid, body)).await;
    assert!(
        resp.status().is_success(),
        "{nsid} failed: {}",
        resp.status()
    );
}

#[tokio::test]
#[serial]
async fn a_credential_stops_working_when_its_member_loses_read_access() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let key = key();
    let (space, credential) = setup(&mut app, &key).await;

    space_admin(
        &app,
        "com.atproto.simplespace.putMember",
        json!({ "space": space, "did": MEMBER, "read": false, "write": true }),
    )
    .await;

    let resp = call(
        &app,
        read(
            &latest_commit_path(&space, MEMBER),
            &credential,
            &key,
            MEMBER,
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(json_of(resp).await["error"], json!("CredentialRevoked"));
}

#[tokio::test]
#[serial]
async fn a_credential_stops_working_when_its_member_is_removed() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let key = key();
    let (space, credential) = setup(&mut app, &key).await;

    space_admin(
        &app,
        "com.atproto.simplespace.removeMember",
        json!({ "space": space, "did": MEMBER }),
    )
    .await;

    let resp = call(
        &app,
        read(
            &latest_commit_path(&space, MEMBER),
            &credential,
            &key,
            MEMBER,
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(json_of(resp).await["error"], json!("CredentialRevoked"));
}

/// The PDSes hosting native repos verify this instance's credentials, so they
/// are told when one is revoked instead of honouring it until it expires.
#[tokio::test]
#[serial]
async fn revoking_a_credential_notifies_the_hosts_of_native_repos() {
    revocation_reaches_the_native_host(|space| {
        (
            "com.atproto.simplespace.putMember",
            json!({ "space": space, "did": MEMBER, "read": false, "write": true }),
        )
    })
    .await;
}

/// Withdrawing read access with `putMembers` tells the hosts too, once the
/// change and its revocations have committed.
#[tokio::test]
#[serial]
async fn withdrawing_read_with_put_members_notifies_the_hosts_of_native_repos() {
    revocation_reaches_the_native_host(|space| {
        (
            "com.atproto.memberspace.putMembers",
            json!({ "space": space, "members": [{ "did": MEMBER, "read": false, "write": false }] }),
        )
    })
    .await;
}

/// So does simplespace's `removeMember`.
#[tokio::test]
#[serial]
async fn removing_a_member_the_simplespace_way_notifies_the_hosts_of_native_repos() {
    revocation_reaches_the_native_host(|space| {
        (
            "com.atproto.simplespace.removeMember",
            json!({ "space": space, "did": MEMBER }),
        )
    })
    .await;
}

/// Moving a space to a policy that keeps no member list discards the list
/// and revokes its members' credentials, and tells the hosts too.
#[tokio::test]
#[serial]
async fn discarding_the_member_list_notifies_the_hosts_of_native_repos() {
    revocation_reaches_the_native_host(|space| {
        (
            "com.atproto.space.updateSpace",
            json!({ "space": space, "policy": {
                "$type": "com.atproto.managedspace.policy",
                "managingApp": "did:plc:app",
            }}),
        )
    })
    .await;
}

/// The credential itself is refused once the member list it was issued from
/// is discarded.
#[tokio::test]
#[serial]
async fn a_credential_stops_working_when_the_member_list_is_discarded() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let key = key();
    let (space, credential) = setup(&mut app, &key).await;

    space_admin(
        &app,
        "com.atproto.space.updateSpace",
        json!({ "space": space, "policy": {
            "$type": "com.atproto.managedspace.policy",
            "managingApp": "did:plc:app",
        }}),
    )
    .await;

    let resp = call(
        &app,
        read(
            &latest_commit_path(&space, MEMBER),
            &credential,
            &key,
            MEMBER,
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(json_of(resp).await["error"], json!("CredentialRevoked"));
}

/// Removing a member with `removeMembers` tells the hosts too, once the
/// removal and its revocations have committed.
#[tokio::test]
#[serial]
async fn removing_a_member_notifies_the_hosts_of_native_repos() {
    revocation_reaches_the_native_host(|space| {
        (
            "com.atproto.memberspace.removeMembers",
            json!({ "space": space, "dids": [MEMBER] }),
        )
    })
    .await;
}

/// Revoke `MEMBER`'s credential with the request `revoke` builds, and check
/// that the host of `MEMBER`'s native repo hears about it once, as the space.
async fn revocation_reaches_the_native_host(revoke: impl Fn(&str) -> (&'static str, Value)) {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let key = key();
    let (space, credential) = setup(&mut app, &key).await;

    let pds = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.space.notifyCredentialRevoked"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&pds)
        .await;
    let plc_store = common::plc::setup_mock_plc(&app.mock_server).await;
    plc_store.write().await.insert(
        MEMBER.to_string(),
        json!({
            "id": MEMBER,
            "verificationMethod": [],
            "service": [{ "id": "#atproto_pds", "type": "AtprotoPersonalDataServer", "serviceEndpoint": pds.uri() }],
        }),
    );

    let mut parts = space.strip_prefix("at://").unwrap().split('/');
    let (authority, _, space_type, skey) = (
        parts.next().unwrap(),
        parts.next().unwrap(),
        parts.next().unwrap(),
        parts.next().unwrap(),
    );
    let space_row = happyview::spaces::db::get_space_by_address(
        &app.state.db,
        app.state.db_backend,
        authority,
        space_type,
        skey,
    )
    .await
    .unwrap()
    .unwrap();
    let mut conn = app.state.db.acquire().await.unwrap();
    let mut repo_state = happyview::spaces::db::get_or_create_repo_state(
        &mut conn,
        app.state.db_backend,
        &space_row.id,
        MEMBER,
    )
    .await
    .unwrap();
    repo_state.host_mode = happyview::spaces::host_mode::HostMode::Native;
    happyview::spaces::db::update_repo_state(&mut *conn, app.state.db_backend, &repo_state)
        .await
        .unwrap();
    drop(conn);

    let (nsid, body) = revoke(&space);
    space_admin(&app, nsid, body).await;

    let mut received = Vec::new();
    for _ in 0..50 {
        received = pds.received_requests().await.unwrap_or_default();
        if !received.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(received.len(), 1, "the native repo's host should hear once");
    let body: Value = serde_json::from_slice(&received[0].body).unwrap();
    let jti = {
        use base64::Engine;
        let payload = credential.split('.').nth(1).unwrap();
        let claims: Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(payload)
                .unwrap(),
        )
        .unwrap();
        claims["jti"].clone()
    };
    // Sent in the current form, `at:space/`.
    assert_eq!(
        body["space"],
        json!(happyview::spaces::UriForm::AtSpace.write(&space))
    );
    assert_eq!(body["credentials"], json!([jti]));
    // The space has a DID of its own, and revokes as it.
    assert!(authority.starts_with("did:plc:"), "{authority}");
    let authorization = received[0]
        .headers
        .get("authorization")
        .expect("revocations carry service auth from the authority")
        .to_str()
        .unwrap();
    app.assert_signed_as_space(authorization, authority).await;
}

/// The bodies `MEMBER`'s native repo host is sent when `MEMBER` is removed,
/// with the host answering as `host` says, and the space's `at://` URI.
async fn revocations_sent_to(
    host: Vec<wiremock::Mock>,
    expected: usize,
) -> (String, Vec<Value>, TestApp, String) {
    use wiremock::MockServer;

    let mut app = TestApp::new_with_encryption().await;
    let key = key();
    let (space, credential) = setup(&mut app, &key).await;

    let pds = MockServer::start().await;
    for mock in host {
        mock.mount(&pds).await;
    }
    let plc_store = common::plc::setup_mock_plc(&app.mock_server).await;
    plc_store.write().await.insert(
        MEMBER.to_string(),
        json!({
            "id": MEMBER,
            "verificationMethod": [],
            "service": [{ "id": "#atproto_pds", "type": "AtprotoPersonalDataServer", "serviceEndpoint": pds.uri() }],
        }),
    );

    let mut parts = space.strip_prefix("at://").unwrap().split('/');
    let (authority, _, space_type, skey) = (
        parts.next().unwrap(),
        parts.next().unwrap(),
        parts.next().unwrap(),
        parts.next().unwrap(),
    );
    let space_row = happyview::spaces::db::get_space_by_address(
        &app.state.db,
        app.state.db_backend,
        authority,
        space_type,
        skey,
    )
    .await
    .unwrap()
    .unwrap();
    let mut conn = app.state.db.acquire().await.unwrap();
    let mut repo_state = happyview::spaces::db::get_or_create_repo_state(
        &mut conn,
        app.state.db_backend,
        &space_row.id,
        MEMBER,
    )
    .await
    .unwrap();
    repo_state.host_mode = happyview::spaces::host_mode::HostMode::Native;
    happyview::spaces::db::update_repo_state(&mut *conn, app.state.db_backend, &repo_state)
        .await
        .unwrap();
    drop(conn);

    space_admin(
        &app,
        "com.atproto.memberspace.removeMembers",
        json!({ "space": space, "dids": [MEMBER] }),
    )
    .await;
    let mut received = Vec::new();
    for _ in 0..50 {
        received = pds.received_requests().await.unwrap_or_default();
        if received.len() >= expected {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    // Anything further would have arrived with the rest.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let received = pds.received_requests().await.unwrap_or_default();
    let bodies = received
        .iter()
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    let _ = credential;
    (space, bodies, app, pds.uri())
}

/// A host that takes only the `at://…/space/…` form is sent a revocation once
/// more in it, and remembered; an unrelated refusal is neither retried nor
/// remembered.
#[tokio::test]
#[serial]
async fn a_revocation_falls_back_to_the_earlier_form_only_when_the_form_is_refused() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, ResponseTemplate};
    common::require_db!();
    let lxm = "/xrpc/com.atproto.space.notifyCredentialRevoked";
    let (space, bodies, app, pds) = revocations_sent_to(
        vec![
            Mock::given(method("POST"))
                .and(path(lxm))
                .and(body_string_contains("at:space/"))
                .respond_with(ResponseTemplate::new(400).set_body_json(
                    json!({ "error": "InvalidRequest", "message": "Invalid space-ref format: space" }),
                ))
                .with_priority(1),
            Mock::given(method("POST"))
                .and(path(lxm))
                .respond_with(ResponseTemplate::new(200))
                .with_priority(2),
        ],
        2,
    )
    .await;
    let spaces: Vec<_> = bodies.iter().map(|b| b["space"].clone()).collect();
    assert_eq!(
        spaces,
        [
            json!(happyview::spaces::UriForm::AtSpace.write(&space)),
            json!(space)
        ]
    );
    assert_eq!(
        happyview::spaces::pds_support::space_uri_form(&app.state.db, app.state.db_backend, &pds)
            .await
            .unwrap(),
        happyview::spaces::UriForm::Legacy
    );

    let (_, bodies, app, pds) = revocations_sent_to(
        vec![Mock::given(method("POST")).and(path(lxm)).respond_with(
            ResponseTemplate::new(400).set_body_json(
                json!({ "error": "InvalidRequest", "message": "too many credentials" }),
            ),
        )],
        1,
    )
    .await;
    assert_eq!(bodies.len(), 1, "{bodies:?}");
    assert_eq!(
        happyview::spaces::pds_support::space_uri_form(&app.state.db, app.state.db_backend, &pds)
            .await
            .unwrap(),
        happyview::spaces::UriForm::AtSpace
    );
}

fn get_as(app: &TestApp, did: &str, path: &str) -> Request<Body> {
    let (name, value) = cookie_for(app, did);
    Request::builder()
        .uri(path)
        .header(name, value)
        .body(Body::empty())
        .unwrap()
}

/// `simplespace.getSpace`, and its `dev.happyview` alias, give a space the
/// caller may not read the answer they give a space that does not exist.
#[tokio::test]
#[serial]
async fn simplespace_get_space_answers_a_refusal_as_not_found() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let key = key();
    let (space, credential) = setup(&mut app, &key).await;
    let space_did = space
        .trim_start_matches("at://")
        .split('/')
        .next()
        .unwrap()
        .to_string();
    let missing = format!("at://{space_did}/space/com.example.creduse/nothing-here");

    for nsid in [
        "com.atproto.simplespace.getSpace",
        "dev.happyview.space.getSpace",
    ] {
        let path = |space: &str| format!("/xrpc/{nsid}?space={}", urlencoding::encode(space));

        let resp = call(&app, get_as(&app, CREATOR, &path(&missing))).await;
        let status = resp.status();
        let absent = json_of(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{nsid}: {absent}");
        assert_eq!(absent["error"], json!("SpaceNotFound"), "{nsid}");

        let stranger = format!("did:plc:stranger{}", Uuid::new_v4().simple());
        let resp = call(&app, get_as(&app, &stranger, &path(&space))).await;
        assert_eq!(resp.status(), status, "{nsid}: a non-owner");
        assert_eq!(json_of(resp).await, absent, "{nsid}: a non-owner");

        let resp = call(&app, read(&path(&space), &credential, &key, MEMBER)).await;
        assert_eq!(resp.status(), status, "{nsid}: another audience");
        assert_eq!(json_of(resp).await, absent, "{nsid}: another audience");

        let resp = call(&app, read(&path(&space), &credential, &key, &space_did)).await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "{nsid}: addressed to the space"
        );
        let resp = call(&app, get_as(&app, CREATOR, &path(&space))).await;
        assert_eq!(resp.status(), StatusCode::OK, "{nsid}: the owner");
    }
}

/// `listSpaces` gives the space's revision to its owner, not to a member.
#[tokio::test]
#[serial]
async fn list_spaces_gives_the_space_revision_to_its_owner_only() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let (space, _) = setup(&mut app, &key()).await;
    let listed = |did: &'static str| {
        let app = &app;
        let space = space.clone();
        async move {
            let resp = call(app, get_as(app, did, "/xrpc/com.atproto.space.listSpaces")).await;
            assert_eq!(resp.status(), StatusCode::OK);
            json_of(resp).await["spaces"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["uri"] == json!(space))
                .cloned()
                .expect("listed")
        }
    };
    let as_member = listed(MEMBER).await;
    assert!(as_member["spaceRev"].is_null(), "{as_member}");
    assert!(
        as_member["repoRev"].is_string(),
        "the member wrote: {as_member}"
    );
    let as_owner = listed(CREATOR).await;
    assert!(as_owner["spaceRev"].is_string(), "{as_owner}");
}

/// Withdrawing a member's read access through `putMembers`, or removing the
/// member through `removeMembers`, revokes the credentials issued for it.
#[tokio::test]
#[serial]
async fn the_batch_member_methods_revoke_a_members_credentials() {
    common::require_db!();
    for (nsid, body) in [
        (
            "com.atproto.memberspace.putMembers",
            json!({ "members": [{ "did": MEMBER, "read": false, "write": false }] }),
        ),
        (
            "com.atproto.memberspace.removeMembers",
            json!({ "dids": [MEMBER] }),
        ),
    ] {
        let mut app = TestApp::new_with_encryption().await;
        let key = key();
        let (space, credential) = setup(&mut app, &key).await;
        let resp = call(
            &app,
            read(
                &latest_commit_path(&space, MEMBER),
                &credential,
                &key,
                MEMBER,
            ),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK, "{nsid}: before");

        let mut body = body.clone();
        body["space"] = json!(space);
        let resp = call(&app, post_as(&app, CREATOR, nsid, body)).await;
        assert_eq!(resp.status(), StatusCode::OK, "{nsid}");

        let resp = call(
            &app,
            read(
                &latest_commit_path(&space, MEMBER),
                &credential,
                &key,
                MEMBER,
            ),
        )
        .await;
        assert!(
            !resp.status().is_success(),
            "{nsid}: revoked, got {}",
            resp.status()
        );
    }
}

/// A credential is accepted on space routes only, under either scheme: a
/// presentation that a space route would take is refused anywhere else.
#[tokio::test]
#[serial]
async fn a_credential_is_refused_off_the_space_routes_under_either_scheme() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let key = key();
    let (space, credential) = setup(&mut app, &key).await;

    let presentations = [
        (
            "Atproto-Space-Credential",
            http_signature::sign(
                &key,
                &format!("Atproto-Space-Credential {credential}"),
                MEMBER,
            ),
        ),
        (
            "Atproto-Space",
            http_signature::sign_legacy(&key, &format!("Atproto-Space {credential}"), Some(MEMBER)),
        ),
    ];
    for (scheme, signed) in presentations {
        let request = |path: &str| {
            let mut req = Request::builder().uri(path);
            for (name, value) in &signed {
                req = req.header(name, value);
            }
            req.body(Body::empty()).unwrap()
        };
        // The presentation itself is good: a space route takes it.
        let resp = call(&app, request(&latest_commit_path(&space, MEMBER))).await;
        assert_eq!(resp.status(), StatusCode::OK, "{scheme} on a space route");

        let resp = call(
            &app,
            request(&format!("/xrpc/app.bsky.actor.getProfile?actor={MEMBER}")),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{scheme}");
        let body = json_of(resp).await;
        assert!(
            body.to_string().contains("only accepted on space routes"),
            "{scheme}: {body}"
        );
    }
}

/// `did`'s credential for `space`, bound to `key`.
async fn credential_for(app: &TestApp, did: &str, space: &str, key: &SigningKey) -> String {
    let (status, body) = try_credential(app, did, space, key).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{did}: credential for {space}: {body}"
    );
    body["credential"].as_str().unwrap().to_string()
}

/// Ask for `did`'s credential for `space`, bound to `key`.
async fn try_credential(
    app: &TestApp,
    did: &str,
    space: &str,
    key: &SigningKey,
) -> (StatusCode, Value) {
    let (name, value) = cookie_for(app, did);
    let delegation = call(
        app,
        Request::builder()
            .uri(format!(
                "/xrpc/com.atproto.space.getDelegationToken?space={}",
                urlencoding::encode(space)
            ))
            .header(name, value)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    // Refused a delegation token, the member is refused a credential.
    if delegation.status() != StatusCode::OK {
        let status = delegation.status();
        return (status, json_of(delegation).await);
    }
    let token = json_of(delegation).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let minted = call(
        app,
        Request::builder()
            .method("POST")
            .uri("/xrpc/com.atproto.space.getSpaceCredential")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "space": space,
                    "credentialKid": http_signature::did_key(key.verifying_key()),
                    "delegationToken": token,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    let status = minted.status();
    (status, json_of(minted).await)
}

/// A new memberspace of `CREATOR`'s that takes `inner`'s members through a
/// delegation entry, as simplespace's `isDelegation` adds one.
async fn delegating_space(app: &TestApp, inner: &str) -> String {
    delegating_space_with(
        app,
        inner,
        json!({ "$type": "com.atproto.memberspace.policy" }),
    )
    .await
}

/// [`delegating_space`] with the given policy.
async fn delegating_space_with(app: &TestApp, inner: &str, policy: Value) -> String {
    let created = call(
        app,
        post_as(
            app,
            CREATOR,
            "com.atproto.space.createSpace",
            json!({
                "spaceType": "com.example.creduse",
                "skey": format!("d{}", Uuid::new_v4().simple()),
                "policy": policy,
            }),
        ),
    )
    .await;
    assert!(created.status().is_success(), "createSpace failed");
    let outer = json_of(created).await["uri"].as_str().unwrap().to_string();
    space_admin(
        app,
        "com.atproto.simplespace.putMember",
        json!({ "space": outer, "did": inner, "read": true, "write": false, "isDelegation": true }),
    )
    .await;
    outer
}

/// The space's authority, which a credential for the space itself is
/// addressed to.
fn authority_of(space: &str) -> String {
    space
        .strip_prefix("at://")
        .unwrap()
        .split('/')
        .next()
        .unwrap()
        .to_string()
}

/// `getSpace` on `space` with `credential`, addressed to the space.
async fn get_space_with(
    app: &TestApp,
    space: &str,
    credential: &str,
    key: &SigningKey,
) -> (StatusCode, Value) {
    let path = format!(
        "/xrpc/com.atproto.space.getSpace?space={}",
        urlencoding::encode(space)
    );
    let resp = call(app, read(&path, credential, key, &authority_of(space))).await;
    let status = resp.status();
    (status, json_of(resp).await)
}

/// `MEMBER`'s credential for a space that takes `inner`'s members through a
/// delegation entry, `MEMBER` being in that space only through `inner`.
/// Returns the inner space, the outer one and the credential for the outer.
async fn setup_delegated(app: &mut TestApp, key: &SigningKey) -> (String, String, String) {
    let (inner, _) = setup(app, key).await;
    let outer = delegating_space(app, &inner).await;
    let credential = credential_for(app, MEMBER, &outer, key).await;
    let (status, body) = get_space_with(app, &outer, &credential, key).await;
    assert_eq!(status, StatusCode::OK, "before: {body}");
    (inner, outer, credential)
}

async fn assert_revoked(
    app: &TestApp,
    space: &str,
    credential: &str,
    key: &SigningKey,
    case: &str,
) {
    let (status, body) = get_space_with(app, space, credential, key).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{case}: {body}");
    assert_eq!(body["error"], json!("CredentialRevoked"), "{case}");
}

/// A member removed from a space, by any of the member methods, loses the
/// credentials it was issued for the spaces that take that space's members
/// through a delegation entry.
#[tokio::test]
#[serial]
async fn a_member_change_revokes_credentials_issued_through_a_delegation_entry() {
    common::require_db!();
    for (nsid, body) in [
        (
            "com.atproto.memberspace.removeMembers",
            json!({ "dids": [MEMBER] }),
        ),
        (
            "com.atproto.memberspace.putMembers",
            json!({ "members": [{ "did": MEMBER, "read": false, "write": true }] }),
        ),
        (
            "com.atproto.simplespace.removeMember",
            json!({ "did": MEMBER }),
        ),
        (
            "com.atproto.simplespace.putMember",
            json!({ "did": MEMBER, "read": false, "write": true }),
        ),
    ] {
        let mut app = TestApp::new_with_encryption().await;
        let key = key();
        let (inner, outer, credential) = setup_delegated(&mut app, &key).await;

        let mut body = body.clone();
        body["space"] = json!(inner);
        space_admin(&app, nsid, body).await;

        assert_revoked(&app, &outer, &credential, &key, nsid).await;
    }
}

/// So do the members of a space when its delegation entry is removed, when
/// either space's member list is discarded, and when the space delegated to
/// is deleted.
#[tokio::test]
#[serial]
async fn credentials_issued_through_a_delegation_entry_go_with_it() {
    common::require_db!();
    let managed = json!({
        "$type": "com.atproto.managedspace.policy",
        "managingApp": "did:plc:app",
    });
    for case in [
        "remove the entry",
        "discard the outer list",
        "discard the inner list",
        "delete the inner space",
    ] {
        let mut app = TestApp::new_with_encryption().await;
        let key = key();
        let (inner, outer, credential) = setup_delegated(&mut app, &key).await;

        match case {
            "remove the entry" => {
                space_admin(
                    &app,
                    "com.atproto.simplespace.removeMember",
                    json!({ "space": outer, "did": inner }),
                )
                .await
            }
            "discard the outer list" => {
                space_admin(
                    &app,
                    "com.atproto.space.updateSpace",
                    json!({ "space": outer, "policy": managed }),
                )
                .await
            }
            "discard the inner list" => {
                space_admin(
                    &app,
                    "com.atproto.space.updateSpace",
                    json!({ "space": inner, "policy": managed }),
                )
                .await
            }
            _ => {
                space_admin(
                    &app,
                    "com.atproto.space.deleteSpace",
                    json!({ "space": inner }),
                )
                .await
            }
        }

        assert_revoked(&app, &outer, &credential, &key, case).await;
    }
}

/// The id of the space at `uri`.
async fn space_id(app: &TestApp, uri: &str) -> String {
    let uri = happyview::spaces::SpaceUri::parse(uri).unwrap();
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
    .id
}

/// A new memberspace of `CREATOR`'s that takes `inner`'s members through a
/// member row naming it, as `simplespace` stored an `isDelegation` entry
/// before spaces nested, added by `inner`'s administrator.
async fn older_delegating_space(app: &TestApp, inner: &str) -> String {
    older_delegating_space_by(app, inner, CREATOR).await
}

/// [`older_delegating_space`], created by `owner`, with the entry consented
/// as `inner`'s administrator would have.
async fn older_delegating_space_by(app: &TestApp, inner: &str, owner: &str) -> String {
    let created = call(
        app,
        post_as(
            app,
            owner,
            "com.atproto.space.createSpace",
            json!({
                "spaceType": "com.example.creduse",
                "skey": format!("o{}", Uuid::new_v4().simple()),
                "policy": { "$type": "com.atproto.memberspace.policy" },
            }),
        ),
    )
    .await;
    assert!(created.status().is_success(), "createSpace failed");
    let outer = json_of(created).await["uri"].as_str().unwrap().to_string();
    let (outer_id, inner_id) = (space_id(app, &outer).await, space_id(app, inner).await);
    assert!(
        happyview::spaces::db::insert_delegation_if_allowed(
            &app.state.db,
            app.state.db_backend,
            &happyview::spaces::types::SpaceMember {
                id: Uuid::new_v4().to_string(),
                space_id: outer_id,
                did: inner.to_string(),
                access: happyview::spaces::types::MemberAccess::READ,
                is_delegation: true,
                granted_by: Some(CREATOR.to_string()),
                created_at: happyview::db::now_rfc3339(),
            },
            happyview::spaces::db::DelegationConsent::Granted(&inner_id),
        )
        .await
        .unwrap()
    );
    outer
}

/// Credentials issued through a member row naming a space go when the
/// member leaves that space, when the row goes, and when the space it names
/// is deleted.
#[tokio::test]
#[serial]
async fn credentials_issued_through_an_older_delegation_entry_go_with_it() {
    common::require_db!();
    for case in [
        "remove the member",
        "remove the entry",
        "delete the inner space",
    ] {
        let mut app = TestApp::new_with_encryption().await;
        let key = key();
        let (inner, _) = setup(&mut app, &key).await;
        let outer = older_delegating_space(&app, &inner).await;
        let credential = credential_for(&app, MEMBER, &outer, &key).await;
        let (status, body) = get_space_with(&app, &outer, &credential, &key).await;
        assert_eq!(status, StatusCode::OK, "{case}, before: {body}");

        let (nsid, body) = match case {
            "remove the member" => (
                "com.atproto.simplespace.removeMember",
                json!({ "space": inner, "did": MEMBER }),
            ),
            "remove the entry" => (
                "com.atproto.simplespace.removeMember",
                json!({ "space": outer, "did": inner }),
            ),
            _ => ("com.atproto.space.deleteSpace", json!({ "space": inner })),
        };
        space_admin(&app, nsid, body).await;

        assert_revoked(&app, &outer, &credential, &key, case).await;
    }
}

/// Put the entry in `outer` naming `inner` with `read` and `write`, as the
/// administrator of both.
async fn put_entry(app: &TestApp, outer: &str, inner: &str, read: bool, write: bool) {
    space_admin(
        app,
        "com.atproto.simplespace.putMember",
        json!({ "space": outer, "did": inner, "read": read, "write": write, "isDelegation": true }),
    )
    .await;
}

/// A member row naming a space grants that space's members no more than its
/// own access, so withdrawing its read revokes the credentials issued
/// through it, here and in the spaces that take this one's members, through
/// entries or nesting; and a member it no longer lets read is issued none.
#[tokio::test]
#[serial]
async fn withdrawing_an_older_entrys_read_revokes_the_credentials_issued_through_it() {
    common::require_db!();
    for case in [
        "here",
        "a level up",
        "in a space nesting it",
        "above a nested space",
    ] {
        let mut app = TestApp::new_with_encryption().await;
        let key = key();
        let (inner, _) = setup(&mut app, &key).await;
        // The entry lowered is `outer`'s, naming `inner`; `checked` holds
        // the credential.
        let (outer, inner, checked) = match case {
            "here" => {
                let outer = older_delegating_space(&app, &inner).await;
                (outer.clone(), inner, outer)
            }
            "a level up" => {
                let outer = older_delegating_space(&app, &inner).await;
                let up = older_delegating_space(&app, &outer).await;
                (outer, inner, up)
            }
            "in a space nesting it" => {
                let outer = older_delegating_space(&app, &inner).await;
                let nesting = delegating_space(&app, &outer).await;
                (outer, inner, nesting)
            }
            _ => {
                let nested = delegating_space(&app, &inner).await;
                let outer = older_delegating_space(&app, &nested).await;
                (outer.clone(), nested, outer)
            }
        };
        let credential = credential_for(&app, MEMBER, &checked, &key).await;
        let (status, body) = get_space_with(&app, &checked, &credential, &key).await;
        assert_eq!(status, StatusCode::OK, "{case}, before: {body}");

        put_entry(&app, &outer, &inner, false, true).await;

        assert_revoked(&app, &checked, &credential, &key, case).await;
        let (status, body) = try_credential(&app, MEMBER, &checked, &key).await;
        assert_ne!(status, StatusCode::OK, "{case}, after: {body}");
    }
}

/// The delegating space's administrator alone, who doesn't administer the
/// named space, may withdraw an older entry's read, and that revokes the
/// credentials issued through it. Raising it back is refused.
#[tokio::test]
#[serial]
async fn the_delegating_admin_alone_lowering_an_entry_revokes_its_credentials() {
    common::require_db!();
    const OWNER: &str = "did:plc:creduse-outer-owner";
    let mut app = TestApp::new_with_encryption().await;
    let key = key();
    let (inner, _) = setup(&mut app, &key).await;
    let outer = older_delegating_space_by(&app, &inner, OWNER).await;
    let credential = credential_for(&app, MEMBER, &outer, &key).await;
    let entry = |read: bool, write: bool| json!({ "space": outer, "did": inner, "read": read, "write": write, "isDelegation": true });

    let lowered = call(
        &app,
        post_as(
            &app,
            OWNER,
            "com.atproto.simplespace.putMember",
            entry(false, false),
        ),
    )
    .await;
    assert!(lowered.status().is_success(), "lower: {}", lowered.status());
    assert_revoked(&app, &outer, &credential, &key, "lowered").await;

    let raised = call(
        &app,
        post_as(
            &app,
            OWNER,
            "com.atproto.simplespace.putMember",
            entry(true, false),
        ),
    )
    .await;
    assert_eq!(raised.status(), StatusCode::FORBIDDEN, "raise");
    let (status, body) = try_credential(&app, MEMBER, &outer, &key).await;
    assert_ne!(status, StatusCode::OK, "after the refused raise: {body}");
}

/// Withdrawing only an older entry's write leaves its members reading, and
/// their credentials working.
#[tokio::test]
#[serial]
async fn withdrawing_an_older_entrys_write_keeps_its_members_credentials() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let key = key();
    let (inner, _) = setup(&mut app, &key).await;
    let outer = older_delegating_space(&app, &inner).await;
    put_entry(&app, &outer, &inner, true, true).await;
    let credential = credential_for(&app, MEMBER, &outer, &key).await;

    put_entry(&app, &outer, &inner, true, false).await;

    let (status, body) = get_space_with(&app, &outer, &credential, &key).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// Discarding a member list keeps the creator's credentials.
#[tokio::test]
#[serial]
async fn discarding_a_member_list_keeps_the_creators_credentials() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let key = key();
    let (_, outer, member_credential) = setup_delegated(&mut app, &key).await;
    let creator_credential = credential_for(&app, CREATOR, &outer, &key).await;

    space_admin(
        &app,
        "com.atproto.space.updateSpace",
        json!({ "space": outer, "policy": {
            "$type": "com.atproto.managedspace.policy",
            "managingApp": "did:plc:app",
        }}),
    )
    .await;

    assert_revoked(&app, &outer, &member_credential, &key, "member").await;
    let (status, body) = get_space_with(&app, &outer, &creator_credential, &key).await;
    assert_eq!(status, StatusCode::OK, "creator: {body}");
}

/// A member who still reaches the space another way keeps its credential:
/// directly, or through another delegation entry.
#[tokio::test]
#[serial]
async fn a_member_who_still_reaches_the_space_keeps_its_credential() {
    common::require_db!();
    for other_way in ["directly", "through another space"] {
        let mut app = TestApp::new_with_encryption().await;
        let key = key();
        let (inner, outer, credential) = setup_delegated(&mut app, &key).await;
        if other_way == "directly" {
            space_admin(
                &app,
                "com.atproto.simplespace.putMember",
                json!({ "space": outer, "did": MEMBER, "read": true, "write": false }),
            )
            .await;
        } else {
            let (second, _) = setup(&mut app, &key).await;
            space_admin(
                &app,
                "com.atproto.simplespace.putMember",
                json!({ "space": outer, "did": second, "read": true, "write": false, "isDelegation": true }),
            )
            .await;
        }

        space_admin(
            &app,
            "com.atproto.memberspace.removeMembers",
            json!({ "space": inner, "dids": [MEMBER] }),
        )
        .await;

        let (status, body) = get_space_with(&app, &outer, &credential, &key).await;
        assert_eq!(status, StatusCode::OK, "{other_way}: {body}");
    }
}

/// The revocation follows delegation entries more than one level up.
#[tokio::test]
#[serial]
async fn a_removal_revokes_credentials_two_delegation_entries_up() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let key = key();
    let (inner, outer, _) = setup_delegated(&mut app, &key).await;
    let outermost = delegating_space(&app, &outer).await;
    let credential = credential_for(&app, MEMBER, &outermost, &key).await;
    let (status, body) = get_space_with(&app, &outermost, &credential, &key).await;
    assert_eq!(status, StatusCode::OK, "before: {body}");

    space_admin(
        &app,
        "com.atproto.memberspace.removeMembers",
        json!({ "space": inner, "dids": [MEMBER] }),
    )
    .await;

    assert_revoked(&app, &outermost, &credential, &key, "two levels up").await;
}

/// The hosts of a delegating space's native repos are told of the
/// credentials revoked there, as that space.
#[tokio::test]
#[serial]
async fn a_revocation_through_a_delegation_entry_notifies_the_hosts_of_native_repos() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let key = key();
    let (inner, outer, credential) = setup_delegated(&mut app, &key).await;

    let pds = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.space.notifyCredentialRevoked"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&pds)
        .await;
    let plc_store = common::plc::setup_mock_plc(&app.mock_server).await;
    plc_store.write().await.insert(
        MEMBER.to_string(),
        json!({
            "id": MEMBER,
            "verificationMethod": [],
            "service": [{ "id": "#atproto_pds", "type": "AtprotoPersonalDataServer", "serviceEndpoint": pds.uri() }],
        }),
    );
    let mut parts = outer.strip_prefix("at://").unwrap().split('/');
    let (authority, _, space_type, skey) = (
        parts.next().unwrap(),
        parts.next().unwrap(),
        parts.next().unwrap(),
        parts.next().unwrap(),
    );
    let outer_row = happyview::spaces::db::get_space_by_address(
        &app.state.db,
        app.state.db_backend,
        authority,
        space_type,
        skey,
    )
    .await
    .unwrap()
    .unwrap();
    let mut conn = app.state.db.acquire().await.unwrap();
    let mut repo_state = happyview::spaces::db::get_or_create_repo_state(
        &mut conn,
        app.state.db_backend,
        &outer_row.id,
        MEMBER,
    )
    .await
    .unwrap();
    repo_state.host_mode = happyview::spaces::host_mode::HostMode::Native;
    happyview::spaces::db::update_repo_state(&mut *conn, app.state.db_backend, &repo_state)
        .await
        .unwrap();
    drop(conn);

    space_admin(
        &app,
        "com.atproto.memberspace.removeMembers",
        json!({ "space": inner, "dids": [MEMBER] }),
    )
    .await;

    let mut received = Vec::new();
    for _ in 0..50 {
        received = pds.received_requests().await.unwrap_or_default();
        if !received.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(received.len(), 1, "the native repo's host should hear once");
    let body: Value = serde_json::from_slice(&received[0].body).unwrap();
    let jti = {
        use base64::Engine;
        let payload = credential.split('.').nth(1).unwrap();
        let claims: Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(payload)
                .unwrap(),
        )
        .unwrap();
        claims["jti"].clone()
    };
    assert_eq!(
        body["space"],
        json!(happyview::spaces::UriForm::AtSpace.write(&outer))
    );
    assert_eq!(body["credentials"], json!([jti]));
    let authorization = received[0]
        .headers
        .get("authorization")
        .expect("revocations carry service auth from the authority")
        .to_str()
        .unwrap();
    app.assert_signed_as_space(authorization, authority).await;
}

/// A space's creator keeps its credential for it when it leaves a space
/// that one delegates to: the creator is admitted apart from the member list.
#[tokio::test]
#[serial]
async fn the_creator_keeps_its_credential_when_it_leaves_a_delegated_space() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let key = key();
    let (inner, outer, _) = setup_delegated(&mut app, &key).await;
    space_admin(
        &app,
        "com.atproto.simplespace.putMember",
        json!({ "space": inner, "did": CREATOR, "read": true, "write": false }),
    )
    .await;
    // `createSpace` lists the creator as a member; without that row it reads
    // the outer space only as its creator.
    space_admin(
        &app,
        "com.atproto.memberspace.removeMembers",
        json!({ "space": outer, "dids": [CREATOR] }),
    )
    .await;
    let credential = credential_for(&app, CREATOR, &outer, &key).await;

    space_admin(
        &app,
        "com.atproto.memberspace.removeMembers",
        json!({ "space": inner, "dids": [CREATOR, MEMBER] }),
    )
    .await;

    let (status, body) = get_space_with(&app, &outer, &credential, &key).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// A space that reads publicly keeps the credential it issued to a member
/// of a space it delegates to when that member leaves: its readers are not
/// its member list.
#[tokio::test]
#[serial]
async fn a_public_space_keeps_its_credential_when_a_delegated_member_leaves() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let key = key();
    let (inner, _) = setup(&mut app, &key).await;
    let outer = delegating_space_with(
        &app,
        &inner,
        json!({ "$type": "com.atproto.memberspace.policy", "publicRead": true }),
    )
    .await;
    let credential = credential_for(&app, MEMBER, &outer, &key).await;

    space_admin(
        &app,
        "com.atproto.memberspace.removeMembers",
        json!({ "space": inner, "dids": [MEMBER] }),
    )
    .await;

    let (status, body) = get_space_with(&app, &outer, &credential, &key).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// Wait until a request on another connection is waiting on a lock, on
/// Postgres; on SQLite, give it time to reach its `BEGIN IMMEDIATE`.
async fn until_waiting_on_a_lock(app: &TestApp) {
    if app.state.db_backend != happyview::db::DatabaseBackend::Postgres {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        return;
    }
    for _ in 0..100 {
        let waiting: i64 = happyview::db::query_scalar(
            "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock'",
        )
        .fetch_one(&app.state.db)
        .await
        .unwrap();
        if waiting > 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }
}

/// A removal in a delegated space waits for a member change committing in
/// the space that delegates to it, and reads that space's members after it:
/// a member added there directly meanwhile keeps its credential.
#[tokio::test]
#[serial]
async fn a_removal_below_waits_for_a_member_change_committing_above() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let backend = app.state.db_backend;
    let key = key();
    let (inner, outer, credential) = setup_delegated(&mut app, &key).await;
    let uri = happyview::spaces::SpaceUri::parse(&outer).unwrap();
    let outer_row = happyview::spaces::db::get_space_by_address(
        &app.state.db,
        backend,
        &uri.did,
        &uri.type_nsid,
        &uri.skey,
    )
    .await
    .unwrap()
    .unwrap();

    // MEMBER added to the outer space directly, as `putMember` makes it:
    // the space's row first, then the member row, not yet committed.
    let mut change = happyview::db::begin_write(&app.state.db, backend)
        .await
        .unwrap();
    assert!(
        happyview::spaces::db::lock_space_for_members(&mut *change, backend, &outer_row.id)
            .await
            .unwrap()
    );
    happyview::db::query(&happyview::db::adapt_sql(
        "INSERT INTO happyview_space_members (id, space_id, member_did, can_read, can_write, read_self, is_delegation, granted_by, created_at) VALUES (?, ?, ?, 1, 0, 0, 0, ?, ?)",
        backend,
    ))
    .bind(Uuid::new_v4().to_string())
    .bind(&outer_row.id)
    .bind(MEMBER)
    .bind(CREATOR)
    .bind(happyview::db::now_rfc3339())
    .execute(&mut *change)
    .await
    .unwrap();

    let router = app.router.clone();
    let (name, value) = cookie_for(&app, CREATOR);
    let body = json!({ "space": inner, "dids": [MEMBER] });
    let removal = tokio::spawn(async move {
        let req = Request::builder()
            .method("POST")
            .uri("/xrpc/com.atproto.memberspace.removeMembers")
            .header(name, value)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        router.oneshot(req).await.unwrap().status()
    });
    until_waiting_on_a_lock(&app).await;
    change.commit().await.unwrap();
    assert_eq!(removal.await.unwrap(), StatusCode::OK);

    let (status, body) = get_space_with(&app, &outer, &credential, &key).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

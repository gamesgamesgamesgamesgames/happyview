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
    assert_eq!(body["space"], json!(space));
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
                .find(|v| v["space"] == json!(space))
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
    assert_eq!(
        delegation.status(),
        StatusCode::OK,
        "{did}: delegation token"
    );
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
    let body = json_of(minted).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{did}: credential for {space}: {body}"
    );
    body["credential"].as_str().unwrap().to_string()
}

/// A new memberspace of `CREATOR`'s that takes `inner`'s members through a
/// delegation entry, as simplespace's `isDelegation` adds one.
async fn delegating_space(app: &TestApp, inner: &str) -> String {
    let created = call(
        app,
        post_as(
            app,
            CREATOR,
            "com.atproto.space.createSpace",
            json!({
                "spaceType": "com.example.creduse",
                "skey": format!("d{}", Uuid::new_v4().simple()),
                "policy": { "$type": "com.atproto.memberspace.policy" },
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
    assert_eq!(body["space"], json!(outer));
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

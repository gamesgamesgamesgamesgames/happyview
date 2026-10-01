//! Reading a space with a credential: `Authorization: Atproto-Space`, signed by
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
            "com.atproto.simplespace.createSpace",
            json!({
                "type": "com.example.creduse",
                "skey": format!("s{}", Uuid::new_v4().simple()),
                "readPolicy": { "$type": "com.atproto.simplespace.defs#memberListPolicy" },
                "writePolicy": { "$type": "com.atproto.simplespace.defs#memberListPolicy" },
                "appAccess": { "$type": "com.atproto.simplespace.defs#open" },
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
    let token = json_of(delegation).await["delegationToken"]
        .as_str()
        .unwrap()
        .to_string();

    let authorization = format!("Bearer {token}");
    let signed = http_signature::sign(key, &authorization, None);
    let mut req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.getSpaceCredential")
        .header("content-type", "application/json")
        .header("authorization", &authorization);
    for name in ["signature-input", "signature"] {
        req = req.header(name, signed[name].clone());
    }
    let minted = call(
        app,
        req.body(Body::from(json!({ "space": space }).to_string()))
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
    let signed = http_signature::sign(key, &format!("Atproto-Space {credential}"), Some(audience));
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

    space_admin(
        &app,
        "com.atproto.simplespace.putMember",
        json!({ "space": space, "did": MEMBER, "read": false, "write": true }),
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
    assert_eq!(body["space"], json!(space));
    assert_eq!(body["credentials"], json!([jti]));
    assert!(
        received[0]
            .headers
            .get("authorization")
            .is_some_and(|v| v.to_str().unwrap().starts_with("Bearer ")),
        "revocations carry service auth from the authority"
    );
}

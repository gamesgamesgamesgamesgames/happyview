mod common;

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Request, StatusCode};
use happyview::db::now_rfc3339;
use happyview::spaces::db as spaces_db;
use happyview::spaces::types::*;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

const AUTHORITY: &str = "did:plc:mint-authority";
const MEMBER: &str = "did:plc:mint-member";

fn space_uri() -> String {
    format!("at://{AUTHORITY}/space/com.example.mint/main")
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

fn cookie_for(app: &TestApp, did: &str) -> (HeaderName, HeaderValue) {
    common::auth::admin_cookie_header(did, &app.state.cookie_key)
}

async fn json_of(resp: axum::http::Response<Body>) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or(json!(null))
}

/// Set up a space with `MEMBER` as a read member, and return a fresh delegation
/// token issued to `MEMBER`.
async fn setup_and_get_delegation_token(app: &TestApp) -> String {
    enable_spaces(app).await;

    let space_id = Uuid::new_v4().to_string();
    let now = now_rfc3339();
    let space = Space {
        id: space_id.clone(),
        did: AUTHORITY.to_string(),
        authority_did: AUTHORITY.to_string(),
        creator_did: AUTHORITY.to_string(),
        type_nsid: "com.example.mint".to_string(),
        skey: "main".to_string(),
        display_name: None,
        description: None,
        read_policy: Policy::MemberList,
        write_policy: Policy::MemberList,
        app_access: AppAccess::Open,
        config: SpaceConfig::default(),
        revision: None,
        created_at: now.clone(),
        updated_at: now.clone(),
    };
    spaces_db::create_space(&app.state.db, app.state.db_backend, &space)
        .await
        .unwrap();
    spaces_db::add_member(
        &app.state.db,
        app.state.db_backend,
        &SpaceMember {
            id: Uuid::new_v4().to_string(),
            space_id,
            did: MEMBER.to_string(),
            access: MemberAccess::READ,
            is_delegation: false,
            granted_by: Some(AUTHORITY.to_string()),
            created_at: now,
        },
    )
    .await
    .unwrap();

    // MEMBER obtains a delegation token (proof of membership).
    let (name, value) = cookie_for(app, MEMBER);
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/xrpc/com.atproto.space.getDelegationToken?space={}",
            urlencoding::encode(&space_uri())
        ))
        .header(name, value)
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "getDelegationToken failed");
    let body = json_of(resp).await;
    // `delegationToken` is the earlier name, returned until v3.
    assert_eq!(body["delegationToken"], body["token"]);
    body["token"].as_str().expect("token").to_string()
}

/// A fresh key for a syncer to bind a credential to.
fn syncer_key() -> p256::ecdsa::SigningKey {
    use rand::Rng;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    p256::ecdsa::SigningKey::from_slice(&bytes).unwrap()
}

/// `getSpaceCredential` as the spec shapes it: the delegation token as the
/// authorization token, signed by the key to bind the credential to.
fn credential_req(delegation_token: &str, key: Option<&p256::ecdsa::SigningKey>) -> Request<Body> {
    let authorization = format!("Bearer {delegation_token}");
    let mut req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.getSpaceCredential")
        .header("content-type", "application/json")
        .header("authorization", &authorization);
    if let Some(key) = key {
        let signed = happyview::spaces::http_signature::sign(key, &authorization, None);
        for name in ["signature-input", "signature"] {
            req = req.header(name, signed[name].clone());
        }
    }
    req.body(Body::from(json!({ "space": space_uri() }).to_string()))
        .unwrap()
}

/// The claims of a JWT, unverified.
fn claims_of(jwt: &str) -> Value {
    use base64::Engine;
    let payload = jwt.split('.').nth(1).unwrap();
    serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
#[serial]
async fn mints_a_credential_bound_to_the_key_that_signed_for_it() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let token = setup_and_get_delegation_token(&app).await;
    let key = syncer_key();

    let resp = app
        .router
        .clone()
        .oneshot(credential_req(&token, Some(&key)))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let credential = json_of(resp).await["credential"]
        .as_str()
        .expect("a credential")
        .to_string();
    assert_eq!(
        claims_of(&credential)["cnf"]["kid"],
        json!(happyview::spaces::http_signature::did_key(
            key.verifying_key()
        ))
    );
}

/// A delegation token alone is not enough: without a signature there is no key
/// to bind, and the credential would be a bearer token.
#[tokio::test]
#[serial]
async fn refuses_a_delegation_token_without_a_signature() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let token = setup_and_get_delegation_token(&app).await;

    let resp = app
        .router
        .clone()
        .oneshot(credential_req(&token, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(json_of(resp).await["error"], json!("BadSpaceSignature"));
}

/// A PDS that serves spaces mints delegation tokens with the account's own
/// signing key, which is found through the account's DID document.
#[tokio::test]
#[serial]
async fn accepts_a_delegation_token_signed_by_the_account() {
    use happyview::spaces::credential::{DelegationTokenClaims, sign_delegation_token};

    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    setup_and_get_delegation_token(&app).await;

    let mut bytes = [0u8; 32];
    {
        use rand::Rng;
        rand::rng().fill_bytes(&mut bytes);
    }
    let account_key = k256::ecdsa::SigningKey::from_slice(&bytes).unwrap();
    let mut multikey = vec![0xe7, 0x01];
    multikey.extend_from_slice(account_key.verifying_key().to_sec1_point(true).as_bytes());
    let plc_store = common::plc::setup_mock_plc(&app.mock_server).await;
    plc_store.write().await.insert(
        MEMBER.to_string(),
        json!({
            "id": MEMBER,
            "verificationMethod": [{
                "id": format!("{MEMBER}#atproto"),
                "type": "Multikey",
                "controller": MEMBER,
                "publicKeyMultibase": multibase::encode(multibase::Base::Base58Btc, multikey),
            }],
            "service": [],
        }),
    );

    let now = chrono::Utc::now().timestamp() as u64;
    let token = sign_delegation_token(
        &DelegationTokenClaims {
            iss: MEMBER.to_string(),
            sub: space_uri(),
            aud: format!("{AUTHORITY}#atproto_space_host"),
            iat: now,
            exp: now + 60,
            jti: Uuid::new_v4().to_string(),
        },
        &account_key,
    )
    .unwrap();

    let resp = app
        .router
        .clone()
        .oneshot(credential_req(&token, Some(&syncer_key())))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// A delegation token is single-use, so one captured in transit cannot be
/// exchanged again for a credential bound to someone else's key.
#[tokio::test]
#[serial]
async fn refuses_a_delegation_token_that_was_already_exchanged() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let token = setup_and_get_delegation_token(&app).await;

    let first = app
        .router
        .clone()
        .oneshot(credential_req(&token, Some(&syncer_key())))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let replay = app
        .router
        .clone()
        .oneshot(credential_req(&token, Some(&syncer_key())))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_of(replay).await["error"],
        json!("InvalidDelegationToken")
    );
}

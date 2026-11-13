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

/// `getSpaceCredential` as Proposal 0016 shapes it: the space, the key to
/// bind the credential to, and the delegation token, all in the body.
fn credential_req(delegation_token: &str, key: Option<&p256::ecdsa::SigningKey>) -> Request<Body> {
    let mut body = json!({ "space": space_uri(), "delegationToken": delegation_token });
    if let Some(key) = key {
        body["credentialKid"] = json!(happyview::spaces::http_signature::did_key(
            key.verifying_key()
        ));
    }
    Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.getSpaceCredential")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
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

/// A delegation token alone is not enough: without a key to bind, the
/// credential would be a bearer token. A key that is not P-256 cannot sign
/// the requests that use it.
#[tokio::test]
#[serial]
async fn refuses_a_request_that_names_no_p256_key() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let token = setup_and_get_delegation_token(&app).await;

    let resp = app
        .router
        .clone()
        .oneshot(credential_req(&token, None))
        .await
        .unwrap();
    assert!(resp.status().is_client_error(), "{}", resp.status());

    let mut req = credential_req(&token, Some(&syncer_key()));
    let body = json!({
        "space": space_uri(),
        "delegationToken": token,
        "credentialKid": "did:key:zQ3shokFTS3brHcDQrn82RUDfCZESWL1ZdCEJwekUDPQiYBme",
    });
    *req.body_mut() = Body::from(body.to_string());
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_of(resp).await["error"], json!("InvalidRequest"));
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
            aud: format!("{AUTHORITY}#atproto_pds"),
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

/// HappyView signs its delegation tokens with a key of its own, stored sealed,
/// not with the encryption key used as a signing key. A token signed that way,
/// as every token was before, is refused, and asking again gets a good one.
#[tokio::test]
#[serial]
async fn refuses_a_delegation_token_signed_with_the_encryption_key() {
    use happyview::spaces::credential::{DelegationTokenClaims, sign_delegation_token};

    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let fresh = setup_and_get_delegation_token(&app).await;

    let legacy_key = k256::ecdsa::SigningKey::from_slice(&common::db::TEST_ENCRYPTION_KEY).unwrap();
    let now = chrono::Utc::now().timestamp() as u64;
    let token = sign_delegation_token(
        &DelegationTokenClaims {
            iss: MEMBER.to_string(),
            sub: space_uri(),
            aud: format!("{AUTHORITY}#atproto_pds"),
            iat: now,
            exp: now + 60,
            jti: Uuid::new_v4().to_string(),
        },
        &legacy_key,
    )
    .unwrap();

    let resp = app
        .router
        .clone()
        .oneshot(credential_req(&token, Some(&syncer_key())))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_of(resp).await["error"],
        json!("InvalidDelegationToken")
    );

    let retry = app
        .router
        .clone()
        .oneshot(credential_req(&fresh, Some(&syncer_key())))
        .await
        .unwrap();
    assert_eq!(retry.status(), StatusCode::OK);
}

/// A space created here has a DID of its own. A credential for it is signed
/// with that DID's `#atproto` key, names `#atproto` as its `kid` and the space
/// DID as its issuer, as Proposal 0016 requires, and verifies against the key
/// the DID's genesis operation published. A delegation token for it is
/// addressed to the space DID's `#atproto_pds`.
#[tokio::test]
#[serial]
async fn a_credential_for_a_space_on_its_own_did_is_signed_as_that_did() {
    use base64::Engine;
    use p256::ecdsa::signature::Verifier;

    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    enable_spaces(&app).await;
    let creator = format!("did:plc:creator{}", Uuid::new_v4().simple());

    let (name, value) = cookie_for(&app, &creator);
    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.createSpace")
        .header(name.clone(), value.clone())
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "spaceType": "com.example.mint", "skey": "own", "policy": { "$type": "com.atproto.memberspace.policy" } }).to_string(),
        ))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let space = json_of(resp).await["uri"].as_str().unwrap().to_string();
    let space_did = space
        .trim_start_matches("at://")
        .split('/')
        .next()
        .unwrap()
        .to_string();
    assert!(space_did.starts_with("did:plc:"));

    let genesis: Value = app
        .mock_server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.method.as_str() == "POST" && r.url.path() == format!("/{space_did}"))
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .expect("the space's DID was published");
    assert_eq!(happyview_plc::derive_did(&genesis).unwrap(), space_did);

    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/xrpc/com.atproto.space.getDelegationToken?space={}",
            urlencoding::encode(&space)
        ))
        .header(name, value)
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let token = json_of(resp).await["token"].as_str().unwrap().to_string();
    assert_eq!(claims_of(&token)["aud"], format!("{space_did}#atproto_pds"));

    let key = syncer_key();
    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.getSpaceCredential")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "space": space,
                "credentialKid": happyview::spaces::http_signature::did_key(key.verifying_key()),
                "delegationToken": token,
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let credential = json_of(resp).await["credential"]
        .as_str()
        .unwrap()
        .to_string();

    let parts: Vec<&str> = credential.split('.').collect();
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let header: Value = serde_json::from_slice(&b64.decode(parts[0]).unwrap()).unwrap();
    assert_eq!(header["kid"], "#atproto");
    assert_eq!(claims_of(&credential)["iss"], space_did);

    let published = genesis["verificationMethods"]["atproto"].as_str().unwrap();
    let (_, multikey) = multibase::decode(published.trim_start_matches("did:key:")).unwrap();
    assert_eq!(&multikey[..2], &[0x80, 0x24], "a P-256 key");
    let verifying = p256::ecdsa::VerifyingKey::from_sec1_bytes(&multikey[2..]).unwrap();
    let signature = p256::ecdsa::Signature::from_slice(&b64.decode(parts[2]).unwrap()).unwrap();
    verifying
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .expect("the credential verifies against the space DID's published #atproto key");
}

/// Seed a space under `AUTHORITY` whose read policy is `read_policy`.
async fn seed_space(app: &TestApp, skey: &str, read_policy: Policy) -> String {
    seed_space_with_apps(app, skey, read_policy, AppAccess::Open).await
}

/// As [`seed_space`], with `app_access`.
async fn seed_space_with_apps(
    app: &TestApp,
    skey: &str,
    read_policy: Policy,
    app_access: AppAccess,
) -> String {
    let now = now_rfc3339();
    spaces_db::create_space(
        &app.state.db,
        app.state.db_backend,
        &Space {
            id: Uuid::new_v4().to_string(),
            did: AUTHORITY.to_string(),
            authority_did: AUTHORITY.to_string(),
            creator_did: AUTHORITY.to_string(),
            type_nsid: "com.example.mint".to_string(),
            skey: skey.to_string(),
            display_name: None,
            description: None,
            read_policy,
            write_policy: Policy::MemberList,
            app_access,
            config: SpaceConfig::default(),
            revision: None,
            created_at: now.clone(),
            updated_at: now,
        },
    )
    .await
    .unwrap();
    format!("at://{AUTHORITY}/space/com.example.mint/{skey}")
}

fn bare_credential_req(space: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.getSpaceCredential")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "space": space,
                "credentialKid": happyview::spaces::http_signature::did_key(
                    syncer_key().verifying_key()
                ),
            })
            .to_string(),
        ))
        .unwrap()
}

/// A space anyone may read issues a credential with no delegation token, so a
/// syncer can read it without acting for a user.
#[tokio::test]
#[serial]
async fn a_public_space_issues_a_credential_without_a_delegation_token() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    enable_spaces(&app).await;
    let space = seed_space(&app, "public", Policy::Public).await;

    let resp = app
        .router
        .clone()
        .oneshot(bare_credential_req(&space))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(json_of(resp).await["credential"].is_string());
}

/// Any other space needs to know who it is issuing to, and answers as if it
/// did not exist.
#[tokio::test]
#[serial]
async fn a_member_space_refuses_a_request_without_a_delegation_token() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    enable_spaces(&app).await;
    let space = seed_space(&app, "members", Policy::MemberList).await;

    let resp = app
        .router
        .clone()
        .oneshot(bare_credential_req(&space))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_of(resp).await["error"], json!("SpaceNotFound"));
}

/// The key to bind is named `credentialKey` since the Nov 12 revision and
/// `credentialKid` before it; both are accepted. A request naming neither is
/// the simplespace exchange, which is refused by name.
#[tokio::test]
#[serial]
async fn the_key_to_bind_is_named_either_way() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    enable_spaces(&app).await;
    let space = seed_space(&app, "named", Policy::Public).await;
    let key = happyview::spaces::http_signature::did_key(syncer_key().verifying_key());
    let ask = |body: Value| {
        Request::builder()
            .method("POST")
            .uri("/xrpc/com.atproto.space.getSpaceCredential")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    for field in ["credentialKey", "credentialKid"] {
        let resp = app
            .router
            .clone()
            .oneshot(ask(json!({ "space": space, field: key })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{field}");
        assert!(json_of(resp).await["credential"].is_string(), "{field}");
    }
    let resp = app
        .router
        .clone()
        .oneshot(ask(
            json!({ "space": space, "credentialKey": "did:key:nope" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(
        json_of(resp).await["message"]
            .as_str()
            .unwrap()
            .contains("credentialKey"),
        "the error names the field the request used"
    );

    // Naming two different keys is refused rather than resolved.
    let resp = app
        .router
        .clone()
        .oneshot(ask(json!({
            "space": space,
            "credentialKey": key,
            "credentialKid": happyview::spaces::http_signature::did_key(syncer_key().verifying_key()),
        })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(
        json_of(resp).await["message"]
            .as_str()
            .unwrap()
            .contains("different keys")
    );

    let resp = app
        .router
        .clone()
        .oneshot(ask(json!({ "space": space })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = json_of(resp).await;
    assert_eq!(body["error"], json!("InvalidRequest"));
    assert!(body["message"].as_str().unwrap().contains("simplespace"));
}

/// An account the space does not admit gets the same answer whichever app it
/// comes through: the app is only weighed for an account the space admits.
#[tokio::test]
#[serial]
async fn the_account_is_settled_before_the_app() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    enable_spaces(&app).await;
    let space = seed_space_with_apps(
        &app,
        "apps",
        Policy::MemberList,
        AppAccess::AllowList {
            allowed: vec!["https://allowed.example/client-metadata.json".into()],
        },
    )
    .await;

    let resp = app
        .router
        .clone()
        .oneshot(bare_credential_req(&space))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_of(resp).await["error"], json!("SpaceNotFound"));
}

/// A delegation token is addressed to the space host as the authority's
/// `#atproto_pds`; one addressed to the earlier `#atproto_space_host` is
/// refused.
#[tokio::test]
#[serial]
async fn refuses_a_delegation_token_addressed_to_the_earlier_service() {
    use happyview::spaces::credential::{DelegationTokenClaims, sign_delegation_token};

    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    setup_and_get_delegation_token(&app).await;
    let key = happyview::spaces::delegation_key::load(
        &app.state.db,
        app.state.db_backend,
        &app.state.config.keyring().unwrap(),
    )
    .await
    .unwrap()
    .expect("a delegation key, minted by getDelegationToken");
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
        &key,
    )
    .unwrap();

    let resp = app
        .router
        .clone()
        .oneshot(credential_req(&token, Some(&syncer_key())))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_of(resp).await["error"],
        json!("InvalidDelegationToken")
    );
}

/// An authority revokes credentials by `jti` with `notifyCredentialRevoked`.
/// Only the space's authority may, a repeat changes nothing, and a revoked
/// credential is refused from then on.
#[tokio::test]
#[serial]
async fn the_authority_revokes_a_credential_by_jti() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let plc_store = common::plc::setup_mock_plc(&app.mock_server).await;
    let instance_did = app.setup_did_web().await;
    let token = setup_and_get_delegation_token(&app).await;
    let resp = app
        .router
        .clone()
        .oneshot(credential_req(&token, Some(&syncer_key())))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let credential = json_of(resp).await["credential"]
        .as_str()
        .unwrap()
        .to_string();
    let jti = claims_of(&credential)["jti"].as_str().unwrap().to_string();
    let space = spaces_db::get_space_by_address(
        &app.state.db,
        app.state.db_backend,
        AUTHORITY,
        "com.example.mint",
        "main",
    )
    .await
    .unwrap()
    .unwrap();

    let revoke = |auth: String| {
        Request::builder()
            .method("POST")
            .uri("/xrpc/com.atproto.space.notifyCredentialRevoked")
            .header("content-type", "application/json")
            .header("authorization", auth)
            .header("host", "127.0.0.1:0")
            .body(Body::from(
                json!({ "space": space_uri(), "credentials": [jti] }).to_string(),
            ))
            .unwrap()
    };
    let lxm = Some("com.atproto.space.notifyCredentialRevoked");

    let stranger = app
        .service_auth_jwt_for(&plc_store, "did:plc:stranger", &instance_did, "", lxm)
        .await;
    let resp = app.router.clone().oneshot(revoke(stranger)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(
        !spaces_db::is_space_credential_jti_revoked(
            &app.state.db,
            app.state.db_backend,
            &space.id,
            &jti
        )
        .await
        .unwrap()
    );

    for _ in 0..2 {
        let auth = app
            .service_auth_jwt_for(&plc_store, AUTHORITY, &instance_did, "", lxm)
            .await;
        let resp = app.router.clone().oneshot(revoke(auth)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
    assert!(
        spaces_db::is_space_credential_jti_revoked(
            &app.state.db,
            app.state.db_backend,
            &space.id,
            &jti
        )
        .await
        .unwrap()
    );
}

/// A client attestation is addressed to the space host as the authority's
/// `#atproto_pds`, and is single use.
#[tokio::test]
#[serial]
async fn a_client_attestation_is_addressed_to_the_space_host_and_used_once() {
    use base64::Engine;
    use p256::ecdsa::signature::Signer;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};

    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    enable_spaces(&app).await;
    let space = seed_space(&app, "attested", Policy::Public).await;

    let app_key = syncer_key();
    let point = app_key.verifying_key().to_sec1_point(false);
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    Mock::given(method("GET"))
        .and(path("/client-metadata.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jwks": { "keys": [{
                "kty": "EC", "crv": "P-256", "kid": "k1",
                "x": b64.encode(point.x().unwrap()),
                "y": b64.encode(point.y().unwrap()),
            }]},
        })))
        .mount(&app.mock_server)
        .await;
    let client_id = format!("{}/client-metadata.json", app.mock_server.uri());

    let attestation = |aud: String| {
        let now = chrono::Utc::now().timestamp() as u64;
        let header = b64.encode(
            json!({ "typ": "atproto-client-attestation+jwt", "alg": "ES256", "kid": "k1" })
                .to_string(),
        );
        let payload = b64.encode(
            json!({
                "iss": client_id, "sub": client_id, "aud": aud,
                "iat": now, "exp": now + 60, "jti": Uuid::new_v4().to_string(),
            })
            .to_string(),
        );
        let signature: p256::ecdsa::Signature =
            app_key.sign(format!("{header}.{payload}").as_bytes());
        format!("{header}.{payload}.{}", b64.encode(signature.to_bytes()))
    };
    let request = |attestation: &str| {
        Request::builder()
            .method("POST")
            .uri("/xrpc/com.atproto.space.getSpaceCredential")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "space": space,
                    "credentialKid": happyview::spaces::http_signature::did_key(
                        syncer_key().verifying_key()
                    ),
                    "clientAttestation": attestation,
                })
                .to_string(),
            ))
            .unwrap()
    };

    let bare = attestation(AUTHORITY.to_string());
    let resp = app.router.clone().oneshot(request(&bare)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_of(resp).await["error"],
        json!("InvalidClientAttestation")
    );

    let addressed = attestation(format!("{AUTHORITY}#atproto_pds"));
    let resp = app
        .router
        .clone()
        .oneshot(request(&addressed))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .router
        .clone()
        .oneshot(request(&addressed))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_of(resp).await["error"],
        json!("InvalidClientAttestation")
    );
}

/// Create a space on its own DID through `createSpace` as `creator`,
/// returning its uri and DID.
async fn create_on_its_own_did(app: &TestApp, creator: &str, skey: &str) -> (String, String) {
    let (name, value) = cookie_for(app, creator);
    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.createSpace")
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "spaceType": "com.example.mint", "skey": skey, "policy": { "$type": "com.atproto.memberspace.policy" } }).to_string(),
        ))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let uri = json_of(resp).await["uri"].as_str().unwrap().to_string();
    let did = uri
        .trim_start_matches("at://")
        .split('/')
        .next()
        .unwrap()
        .to_string();
    (uri, did)
}

/// A revocation's service auth is bound to its space: one addressed to
/// another space's DID is refused, though this instance holds both and the
/// caller is the authority it names.
#[tokio::test]
#[serial]
async fn a_revocation_addressed_to_another_space_is_refused() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let plc_store = common::plc::setup_mock_plc(&app.mock_server).await;
    enable_spaces(&app).await;
    let creator = format!("did:plc:creator{}", Uuid::new_v4().simple());
    let (space, space_did) = create_on_its_own_did(&app, &creator, "revoked").await;
    let (_, other_did) = create_on_its_own_did(&app, &creator, "elsewhere").await;

    let revoke = |auth: String| {
        Request::builder()
            .method("POST")
            .uri("/xrpc/com.atproto.space.notifyCredentialRevoked")
            .header("content-type", "application/json")
            .header("authorization", auth)
            .body(Body::from(
                json!({ "space": space, "credentials": ["some-jti"] }).to_string(),
            ))
            .unwrap()
    };
    let lxm = Some("com.atproto.space.notifyCredentialRevoked");

    let misaddressed = app
        .service_auth_jwt_for(&plc_store, &space_did, &other_did, "#atproto_pds", lxm)
        .await;
    let resp = app
        .router
        .clone()
        .oneshot(revoke(misaddressed))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(json_of(resp).await["error"], json!("BadServiceAudience"));

    let addressed = app
        .service_auth_jwt_for(&plc_store, &space_did, &space_did, "#atproto_pds", lxm)
        .await;
    let resp = app.router.clone().oneshot(revoke(addressed)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// Anonymous credential requests, which public spaces allow, spend the
/// anonymous rate-limit bucket.
#[tokio::test]
#[serial]
async fn anonymous_credential_requests_are_rate_limited() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    enable_spaces(&app).await;
    let space = seed_space(&app, "limited", Policy::Public).await;
    app.state.rate_limiter.register_client_limit(
        "anonymous".into(),
        Some(1),
        Some(0.000_1),
        100,
        2.0,
    );

    let first = app
        .router
        .clone()
        .oneshot(bare_credential_req(&space))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let second = app
        .router
        .clone()
        .oneshot(bare_credential_req(&space))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
}

/// The anonymous bucket is charged before a delegation token is looked at, so
/// a forged token cannot skip it, nor buy an issuer lookup per request.
#[tokio::test]
#[serial]
async fn a_delegation_token_does_not_skip_the_rate_limit() {
    use happyview::spaces::credential::{DelegationTokenClaims, sign_delegation_token};

    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    enable_spaces(&app).await;
    let space = seed_space(&app, "forged", Policy::MemberList).await;
    app.state.rate_limiter.register_client_limit(
        "anonymous".into(),
        Some(1),
        Some(0.000_1),
        100,
        2.0,
    );

    let mut bytes = [0u8; 32];
    {
        use rand::Rng;
        rand::rng().fill_bytes(&mut bytes);
    }
    let now = chrono::Utc::now().timestamp() as u64;
    let forged = sign_delegation_token(
        &DelegationTokenClaims {
            iss: "did:web:forged.example.com".to_string(),
            sub: space.clone(),
            aud: format!("{AUTHORITY}#atproto_pds"),
            iat: now,
            exp: now + 60,
            jti: Uuid::new_v4().to_string(),
        },
        &k256::ecdsa::SigningKey::from_slice(&bytes).unwrap(),
    )
    .unwrap();
    let request = |token: &str| {
        Request::builder()
            .method("POST")
            .uri("/xrpc/com.atproto.space.getSpaceCredential")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "space": space,
                    "credentialKid": happyview::spaces::http_signature::did_key(
                        syncer_key().verifying_key()
                    ),
                    "delegationToken": token,
                })
                .to_string(),
            ))
            .unwrap()
    };

    let first = app.router.clone().oneshot(request("")).await.unwrap();
    assert_ne!(first.status(), StatusCode::TOO_MANY_REQUESTS);
    for token in ["", forged.as_str()] {
        let resp = app.router.clone().oneshot(request(token)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS, "{token:?}");
    }
}

/// An account the space no longer admits gets the same answer as one it never
/// knew, even with a delegation token it obtained while it was a member.
#[tokio::test]
#[serial]
async fn an_account_the_space_no_longer_admits_is_told_it_was_not_found() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let token = setup_and_get_delegation_token(&app).await;
    let space = spaces_db::get_space_by_address(
        &app.state.db,
        app.state.db_backend,
        AUTHORITY,
        "com.example.mint",
        "main",
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        spaces_db::remove_member(&app.state.db, app.state.db_backend, &space.id, MEMBER)
            .await
            .unwrap()
    );

    let resp = app
        .router
        .clone()
        .oneshot(credential_req(&token, Some(&syncer_key())))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_of(resp).await["error"], json!("SpaceNotFound"));
}

/// The assertions in a request are checked against its address before the
/// space is looked up, so a bad delegation token or client attestation is
/// answered alike for a space that exists and one that does not.
#[tokio::test]
#[serial]
async fn bad_assertions_are_answered_alike_for_any_address() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    enable_spaces(&app).await;
    let existing = seed_space(&app, "assertions", Policy::MemberList).await;
    let missing = format!("at://{AUTHORITY}/space/com.example.mint/no-such-space");
    let key = happyview::spaces::http_signature::did_key(syncer_key().verifying_key());

    for field in ["delegationToken", "clientAttestation"] {
        let mut answers = Vec::new();
        for space in [&existing, &missing] {
            let req = Request::builder()
                .method("POST")
                .uri("/xrpc/com.atproto.space.getSpaceCredential")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "space": space,
                        "credentialKid": key,
                        field: "garbage.garbage.garbage",
                    })
                    .to_string(),
                ))
                .unwrap();
            let resp = app.router.clone().oneshot(req).await.unwrap();
            answers.push((resp.status(), json_of(resp).await["error"].clone()));
        }
        assert_eq!(answers[0], answers[1], "{field}: {answers:?}");
        assert_eq!(answers[0].0, StatusCode::BAD_REQUEST, "{field}");
    }
}

/// An account the space admits, coming through an app its allow list does
/// not name, is told the app is not authorized.
#[tokio::test]
#[serial]
async fn an_admitted_account_through_another_app_is_told_the_app_is_not_authorized() {
    use base64::Engine;
    use p256::ecdsa::signature::Signer;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};

    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    enable_spaces(&app).await;
    // A space anyone may read admits every account.
    let space = seed_space_with_apps(
        &app,
        "other-app",
        Policy::Public,
        AppAccess::AllowList {
            allowed: vec!["https://allowed.example/client-metadata.json".into()],
        },
    )
    .await;

    let app_key = syncer_key();
    let point = app_key.verifying_key().to_sec1_point(false);
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    Mock::given(method("GET"))
        .and(path("/client-metadata.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jwks": { "keys": [{
                "kty": "EC", "crv": "P-256", "kid": "k1",
                "x": b64.encode(point.x().unwrap()),
                "y": b64.encode(point.y().unwrap()),
            }]},
        })))
        .mount(&app.mock_server)
        .await;
    let client_id = format!("{}/client-metadata.json", app.mock_server.uri());
    let now = chrono::Utc::now().timestamp() as u64;
    let header = b64.encode(
        json!({ "typ": "atproto-client-attestation+jwt", "alg": "ES256", "kid": "k1" }).to_string(),
    );
    let payload = b64.encode(
        json!({
            "iss": client_id, "sub": client_id, "aud": format!("{AUTHORITY}#atproto_pds"),
            "iat": now, "exp": now + 60, "jti": Uuid::new_v4().to_string(),
        })
        .to_string(),
    );
    let signature: p256::ecdsa::Signature = app_key.sign(format!("{header}.{payload}").as_bytes());
    let attestation = format!("{header}.{payload}.{}", b64.encode(signature.to_bytes()));

    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.getSpaceCredential")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "space": space,
                "credentialKid": happyview::spaces::http_signature::did_key(
                    syncer_key().verifying_key()
                ),
                "clientAttestation": attestation,
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(json_of(resp).await["error"], json!("AppNotAuthorized"));
}

/// A client attestation from an app whose metadata this test serves,
/// addressed to `aud`. Returns the attestation and the app's client id.
async fn attestation_from_an_app(app: &TestApp, aud: &str) -> (String, String) {
    use base64::Engine;
    use p256::ecdsa::signature::Signer;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};

    let app_key = syncer_key();
    let point = app_key.verifying_key().to_sec1_point(false);
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    Mock::given(method("GET"))
        .and(path("/client-metadata.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jwks": { "keys": [{
                "kty": "EC", "crv": "P-256", "kid": "k1",
                "x": b64.encode(point.x().unwrap()),
                "y": b64.encode(point.y().unwrap()),
            }]},
        })))
        .mount(&app.mock_server)
        .await;
    let client_id = format!("{}/client-metadata.json", app.mock_server.uri());
    let now = chrono::Utc::now().timestamp() as u64;
    let header = b64.encode(
        json!({ "typ": "atproto-client-attestation+jwt", "alg": "ES256", "kid": "k1" }).to_string(),
    );
    let payload = b64.encode(
        json!({
            "iss": client_id, "sub": client_id, "aud": aud,
            "iat": now, "exp": now + 60, "jti": Uuid::new_v4().to_string(),
        })
        .to_string(),
    );
    let signature: p256::ecdsa::Signature = app_key.sign(format!("{header}.{payload}").as_bytes());
    (
        format!("{header}.{payload}.{}", b64.encode(signature.to_bytes())),
        client_id,
    )
}

/// An account the space does not admit is answered `SpaceNotFound` even when
/// it comes through an app the space does not allow: the account is settled
/// first.
#[tokio::test]
#[serial]
async fn a_refused_account_through_another_app_is_told_the_space_was_not_found() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let token = setup_and_get_delegation_token(&app).await;
    let mut space = spaces_db::get_space_by_address(
        &app.state.db,
        app.state.db_backend,
        AUTHORITY,
        "com.example.mint",
        "main",
    )
    .await
    .unwrap()
    .unwrap();
    space.app_access = AppAccess::AllowList {
        allowed: vec!["https://allowed.example/client-metadata.json".into()],
    };
    spaces_db::update_space(&app.state.db, app.state.db_backend, &space)
        .await
        .unwrap();
    assert!(
        spaces_db::remove_member(&app.state.db, app.state.db_backend, &space.id, MEMBER)
            .await
            .unwrap()
    );
    let (attestation, _) = attestation_from_an_app(&app, &format!("{AUTHORITY}#atproto_pds")).await;

    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.getSpaceCredential")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "space": space_uri(),
                "credentialKid": happyview::spaces::http_signature::did_key(
                    syncer_key().verifying_key()
                ),
                "delegationToken": token,
                "clientAttestation": attestation,
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_of(resp).await["error"], json!("SpaceNotFound"));
}

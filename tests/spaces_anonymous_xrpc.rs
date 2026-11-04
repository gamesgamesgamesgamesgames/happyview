//! A space credential at the `anonymous_xrpc` limit: it counts as a caller
//! only on the space routes, which verify it. On every other XRPC method, a
//! call that carries only a space credential is anonymous.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Request, StatusCode};
use happyview::spaces::credential::{
    Confirmation, SpaceCredentialClaims, make_jti, sign_credential_with_key,
};
use happyview::spaces::http_signature;
use happyview_provider::{FixedLimits, Gate, Limits, Wording};
use http_body_util::BodyExt;
use p256::ecdsa::SigningKey;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

const CREATOR: &str = "did:plc:anongate-creator";
const MEMBER: &str = "did:plc:anongate-member";

/// A method under a space namespace that no route serves, so the catch-all
/// answers it.
const UNROUTED: &str = "com.atproto.space.notARealMethod";

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

fn close_the_gate(app: &mut TestApp) {
    app.state.limits = Arc::new(FixedLimits::new(Limits {
        anonymous_xrpc: Some(Gate {
            allowed: false,
            wording: Wording::default(),
        }),
        ..Limits::default()
    }));
    app.rebuild_router();
}

/// A credential bound to `key` and signed by it too, rather than by any
/// space's key: well formed, but issued by nobody.
fn self_signed_credential(key: &SigningKey) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = SpaceCredentialClaims {
        iss: "did:plc:anongate-nobody".into(),
        sub: "at://did:plc:anongate-nobody/space/com.example.anongate/s1".into(),
        iat: now,
        exp: now + 600,
        jti: make_jti(),
        cnf: Some(Confirmation {
            kid: http_signature::did_key(key.verifying_key()),
        }),
    };
    sign_credential_with_key(&claims, key).unwrap()
}

/// A credential-authenticated request, signed by `key` for `audience`.
fn with_credential(
    method: &str,
    path: &str,
    credential: &str,
    key: &SigningKey,
    audience: &str,
) -> Request<Body> {
    let signed = http_signature::sign(key, &format!("Atproto-Space {credential}"), Some(audience));
    let mut req = Request::builder().method(method).uri(path);
    for (name, value) in &signed {
        req = req.header(name, value);
    }
    if method == "POST" {
        req = req.header("content-type", "application/json");
        return req.body(Body::from("{}")).unwrap();
    }
    req.body(Body::empty()).unwrap()
}

/// A space on this instance, with `MEMBER` as a reader who has written a
/// record, and a credential for it bound to `key`, issued the way a syncer
/// gets one. Returns the space and the credential.
async fn setup_space(app: &mut TestApp, key: &SigningKey) -> (String, String) {
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
                "type": "com.example.anongate",
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
    let token = json_of(delegation).await["token"]
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

/// With the gate closed, a call to a method the catch-all serves, carrying
/// only a space credential, is refused like any other anonymous call, on
/// both the query and the procedure handler. The request is properly signed
/// by the credential's bound key, so the extractor accepts it.
#[tokio::test]
#[serial]
async fn a_credential_alone_is_anonymous_on_the_catch_all_when_the_gate_is_closed() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    close_the_gate(&mut app);
    let key = key();
    let credential = self_signed_credential(&key);

    for method in ["GET", "POST"] {
        let resp = call(
            &app,
            with_credential(
                method,
                &format!("/xrpc/{UNROUTED}"),
                &credential,
                &key,
                "did:plc:anongate-nobody",
            ),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{method}");
        assert_eq!(
            json_of(resp).await,
            json!({
                "error": "LimitReached",
                "limit": "anonymous_xrpc",
                "message": "This instance only answers XRPC calls made with an API client.",
            }),
            "{method}"
        );
    }
}

/// With the gate open, which is the default, the same calls go on to the
/// method, unchanged from before. No lexicon serves it, so they reach the
/// proxy, which can't resolve the NSID's authority and answers 502.
#[tokio::test]
#[serial]
async fn a_credential_alone_reaches_the_catch_all_when_the_gate_is_open() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let key = key();
    let credential = self_signed_credential(&key);

    for method in ["GET", "POST"] {
        let resp = call(
            &app,
            with_credential(
                method,
                &format!("/xrpc/{UNROUTED}"),
                &credential,
                &key,
                "did:plc:anongate-nobody",
            ),
        )
        .await;
        let status = resp.status();
        let body = json_of(resp).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{method}: {body}");
        assert_ne!(body["error"], "LimitReached", "{method}: {body}");
    }
}

/// The space routes verify their own credentials and aren't behind the
/// gate, so a real credential still reads a space with the gate closed.
#[tokio::test]
#[serial]
async fn a_real_credential_still_reads_a_space_when_the_gate_is_closed() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    close_the_gate(&mut app);
    let key = key();
    let (space, credential) = setup_space(&mut app, &key).await;

    let path = format!(
        "/xrpc/com.atproto.space.getLatestCommit?space={}&did={}",
        urlencoding::encode(&space),
        urlencoding::encode(MEMBER)
    );
    let resp = call(
        &app,
        with_credential("GET", &path, &credential, &key, MEMBER),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

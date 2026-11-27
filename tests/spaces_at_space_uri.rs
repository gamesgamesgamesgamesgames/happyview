//! Space URIs in Proposal 0016's `at:space/` form (Nov 24, 2026), beside the
//! `at://…/space/…` form before it: either is read, and a request is answered
//! in the form it used.

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

const CREATOR: &str = "did:plc:atspace-creator";
const MEMBER: &str = "did:plc:atspace-member";
const TYPE_NSID: &str = "com.example.atspace";
const NOTE: &str = "com.example.note";

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

async fn post(app: &TestApp, did: &str, nsid: &str, body: Value) -> (StatusCode, Value) {
    let (name, value) = cookie_for(app, did);
    let req = Request::builder()
        .method("POST")
        .uri(format!("/xrpc/{nsid}"))
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = call(app, req).await;
    (resp.status(), json_of(resp).await)
}

async fn get(app: &TestApp, did: &str, path: &str) -> (StatusCode, Value) {
    let (name, value) = cookie_for(app, did);
    let req = Request::builder()
        .uri(path)
        .header(name, value)
        .body(Body::empty())
        .unwrap();
    let resp = call(app, req).await;
    (resp.status(), json_of(resp).await)
}

fn key() -> SigningKey {
    use rand::Rng;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    SigningKey::from_slice(&bytes).unwrap()
}

/// `uri` (`at://<did>/space/<type>/<skey>[/<rest>]`) in the `at:space/`
/// form, written out here rather than by the code under test.
fn at_space(uri: &str) -> String {
    let rest = uri.strip_prefix("at://").expect("an at:// URI");
    let (did, rest) = rest.split_once("/space/").expect("a space URI");
    format!("at:space/{did}/{rest}")
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

/// A memberspace on this instance with `MEMBER` reading and writing. Returns
/// the `createSpace` answer.
async fn setup(app: &mut TestApp) -> Value {
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

    let (status, created) = post(
        app,
        CREATOR,
        "com.atproto.space.createSpace",
        json!({
            "spaceType": TYPE_NSID,
            "skey": format!("s{}", Uuid::new_v4().simple()),
            "policy": { "$type": "com.atproto.memberspace.policy" },
        }),
    )
    .await;
    assert!(status.is_success(), "createSpace: {created}");
    let (status, body) = post(
        app,
        CREATOR,
        "com.atproto.memberspace.putMembers",
        json!({ "space": created["space"], "members": [{ "did": MEMBER, "read": true, "write": true }] }),
    )
    .await;
    assert!(status.is_success(), "putMembers: {body}");
    created
}

#[tokio::test]
#[serial]
async fn a_new_space_is_named_in_both_forms() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let created = setup(&mut app).await;
    let legacy = created["uri"].as_str().unwrap().to_string();
    // `uri` is the Oct 29 name, in the form its clients read; `space` is the
    // current form.
    assert!(
        legacy.starts_with("at://") && legacy.contains("/space/"),
        "{created}"
    );
    assert_eq!(created["space"], at_space(&legacy), "{created}");

    let (status, listed) = get(&app, CREATOR, "/xrpc/com.atproto.space.listSpaces").await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let view = listed["spaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["uri"] == legacy.as_str())
        .unwrap_or_else(|| panic!("{listed}"));
    assert_eq!(view["space"], at_space(&legacy), "{view}");

    for asked in [legacy.clone(), at_space(&legacy)] {
        let (status, body) = get(
            &app,
            CREATOR,
            &format!(
                "/xrpc/com.atproto.space.getSpace?space={}",
                urlencoding::encode(&asked)
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{asked}: {body}");
        assert_eq!(body["space"], asked.as_str(), "answered as asked: {body}");
        assert_eq!(body["uri"], legacy.as_str(), "{body}");
    }
}

#[tokio::test]
#[serial]
async fn records_are_named_in_the_form_asked() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let created = setup(&mut app).await;
    let legacy = created["uri"].as_str().unwrap().to_string();

    for (asked, rkey) in [(at_space(&legacy), "a"), (legacy.clone(), "b")] {
        let (status, body) = post(
            &app,
            MEMBER,
            "com.atproto.space.createRecord",
            json!({ "space": asked, "collection": NOTE, "rkey": rkey, "record": { "$type": NOTE, "text": rkey } }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let uri = body["uri"].as_str().unwrap();
        assert_eq!(uri, format!("{asked}/{MEMBER}/{NOTE}/{rkey}"), "{body}");

        let (status, body) = get(
            &app,
            MEMBER,
            &format!(
                "/xrpc/com.atproto.space.getRecord?space={}&repo={}&collection={NOTE}&rkey={rkey}",
                urlencoding::encode(&asked),
                urlencoding::encode(MEMBER)
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["uri"], uri, "{body}");
    }

    let (status, body) = post(
        &app,
        MEMBER,
        "com.atproto.space.applyWrites",
        json!({
            "space": at_space(&legacy),
            "writes": [{ "$type": "com.atproto.space.applyWrites#create", "collection": NOTE, "rkey": "c", "value": { "$type": NOTE, "text": "c" } }],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["results"][0]["uri"],
        format!("{}/{MEMBER}/{NOTE}/c", at_space(&legacy)),
        "{body}"
    );
}

#[tokio::test]
#[serial]
async fn a_commit_names_its_space_as_asked() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let created = setup(&mut app).await;
    let legacy = created["uri"].as_str().unwrap().to_string();
    let (status, body) = post(
        &app,
        MEMBER,
        "com.atproto.space.createRecord",
        json!({ "space": legacy, "collection": NOTE, "record": { "$type": NOTE, "text": "x" } }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    for asked in [at_space(&legacy), legacy.clone()] {
        let (status, body) = get(
            &app,
            MEMBER,
            &format!(
                "/xrpc/com.atproto.space.getLatestCommit?space={}&repo={}",
                urlencoding::encode(&asked),
                urlencoding::encode(MEMBER)
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["commit"]["space"], asked.as_str(), "{body}");

        let (status, body) = get(
            &app,
            MEMBER,
            &format!(
                "/xrpc/com.atproto.space.listRepoOps?space={}&repo={}",
                urlencoding::encode(&asked),
                urlencoding::encode(MEMBER)
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["commit"]["space"], asked.as_str(), "{body}");
    }
}

/// A delegation token for `asked`, as `MEMBER`.
async fn delegation_token(app: &TestApp, asked: &str) -> String {
    let (status, body) = get(
        app,
        MEMBER,
        &format!(
            "/xrpc/com.atproto.space.getDelegationToken?space={}",
            urlencoding::encode(asked)
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["token"].as_str().unwrap().to_string()
}

async fn authorize(
    app: &TestApp,
    asked: &str,
    token: &str,
    key: &SigningKey,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.space.authorizeSpaceSync")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "space": asked,
                "credentialKey": http_signature::did_key(key.verifying_key()),
                "delegationToken": token,
            })
            .to_string(),
        ))
        .unwrap();
    let resp = call(app, req).await;
    (resp.status(), json_of(resp).await)
}

#[tokio::test]
#[serial]
async fn tokens_and_credentials_cross_forms() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let created = setup(&mut app).await;
    let legacy = created["uri"].as_str().unwrap().to_string();
    let new = at_space(&legacy);

    // Each token names the space as it was asked for, and is taken whichever
    // form the exchange names it in.
    for (token_form, exchange_form) in [(&new, &legacy), (&legacy, &new)] {
        let token = delegation_token(&app, token_form).await;
        assert_eq!(claims_of(&token)["sub"], token_form.as_str());

        let key = key();
        let (status, body) = authorize(&app, exchange_form, &token, &key).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{token_form} → {exchange_form}: {body}"
        );
        let credential = body["credential"].as_str().unwrap().to_string();
        assert_eq!(
            claims_of(&credential)["sub"],
            exchange_form.as_str(),
            "the credential names the space as the exchange did"
        );

        // And the credential reads the space named either way.
        for read_form in [&new, &legacy] {
            let signed = http_signature::sign(
                &key,
                &format!("Atproto-Space-Credential {credential}"),
                MEMBER,
            );
            let mut req = Request::builder().uri(format!(
                "/xrpc/com.atproto.space.listRecords?space={}&repo={}",
                urlencoding::encode(read_form),
                urlencoding::encode(MEMBER)
            ));
            for (name, value) in &signed {
                req = req.header(name, value);
            }
            let resp = call(&app, req.body(Body::empty()).unwrap()).await;
            assert_eq!(resp.status(), StatusCode::OK, "read as {read_form}");
        }
    }
}

#[tokio::test]
#[serial]
async fn a_space_named_as_a_member_is_kept_in_the_form_compared() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let parent = setup(&mut app).await;
    let (status, child) = post(
        &app,
        CREATOR,
        "com.atproto.space.createSpace",
        json!({
            "spaceType": TYPE_NSID,
            "skey": format!("c{}", Uuid::new_v4().simple()),
            "policy": { "$type": "com.atproto.memberspace.policy" },
        }),
    )
    .await;
    assert!(status.is_success(), "{child}");

    // The child named in the at:space/ form nests it, as the at:// form does.
    let (status, body) = post(
        &app,
        CREATOR,
        "com.atproto.simplespace.putMember",
        json!({ "space": parent["uri"], "did": child["space"], "read": true, "write": false }),
    )
    .await;
    assert!(status.is_success(), "{body}");
    assert_eq!(
        body["member"]["did"], child["uri"],
        "answered in the at:// form it is stored in: {body}"
    );

    let (status, listed) = get(
        &app,
        CREATOR,
        &format!(
            "/xrpc/dev.happyview.space.listNestedSpaces?space={}",
            urlencoding::encode(parent["uri"].as_str().unwrap())
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let nested = listed.to_string();
    assert!(nested.contains(child["uri"].as_str().unwrap()), "{listed}");
    assert!(
        !nested.contains("at:space/"),
        "stored in the at:// form: {listed}"
    );

    // Asked in the at:space/ form, nested spaces are named in that form.
    let (status, listed) = get(
        &app,
        CREATOR,
        &format!(
            "/xrpc/dev.happyview.space.listNestedSpaces?space={}",
            urlencoding::encode(&at_space(parent["uri"].as_str().unwrap()))
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let nested = listed.to_string();
    assert!(
        nested.contains(&at_space(child["uri"].as_str().unwrap())),
        "{listed}"
    );
    assert!(!nested.contains("at://"), "{listed}");

    // Naming it again in the other form is the same entry.
    let (status, body) = post(
        &app,
        CREATOR,
        "com.atproto.simplespace.putMember",
        json!({ "space": parent["uri"], "did": child["uri"], "read": true, "write": false }),
    )
    .await;
    assert!(status.is_success(), "{body}");
    let (_, listed) = get(
        &app,
        CREATOR,
        &format!(
            "/xrpc/dev.happyview.space.listNestedSpaces?space={}",
            urlencoding::encode(parent["uri"].as_str().unwrap())
        ),
    )
    .await;
    let count = listed
        .to_string()
        .matches(child["uri"].as_str().unwrap())
        .count();
    assert_eq!(count, 1, "{listed}");
}

/// A space that isn't a memberspace, named as a member, is stored as an entry
/// naming it, in the form HappyView compares, so naming it again in the other
/// form is the same entry.
#[tokio::test]
#[serial]
async fn an_entry_naming_a_space_is_stored_in_the_form_compared() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    let parent = setup(&mut app).await;
    let (status, managed) = post(
        &app,
        CREATOR,
        "com.atproto.space.createSpace",
        json!({
            "spaceType": TYPE_NSID,
            "skey": format!("m{}", Uuid::new_v4().simple()),
            "policy": { "$type": "com.atproto.managedspace.policy", "managingApp": "did:plc:someapp#app" },
        }),
    )
    .await;
    assert!(status.is_success(), "{managed}");

    let (status, body) = post(
        &app,
        CREATOR,
        "com.atproto.simplespace.addMember",
        json!({ "space": parent["uri"], "did": managed["space"], "isDelegation": true }),
    )
    .await;
    assert!(status.is_success(), "{body}");
    assert_eq!(
        body["member"]["did"], managed["uri"],
        "stored in the at:// form: {body}"
    );

    let (status, body) = post(
        &app,
        CREATOR,
        "com.atproto.simplespace.addMember",
        json!({ "space": parent["uri"], "did": managed["uri"], "isDelegation": true }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "the same entry: {body}");
}

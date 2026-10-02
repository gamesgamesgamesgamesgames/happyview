//! Service-proxy routing: an unrecognized XRPC method is forwarded to the
//! *caller's own PDS*, authenticated as the caller.
//!
//! This is the path that was reported broken. Under the previous routing an
//! unrecognized method was resolved via `_lexicon` DNS to whoever publishes its
//! lexicon and sent there with **no credentials at all**, so
//! `com.atproto.repo.createRecord` arrived at a Bluesky-run lexicon-publishing
//! account and came back `AuthMissing`. These tests pin the two halves of the
//! fix: the request reaches the caller's own PDS, and it carries their
//! authorization when it does.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::oauth::pds_write::generate_dpop_proof;
use happyview::proxy_config::{ProxyConfig, ProxyMode, ProxyRouting};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use std::sync::Arc;
use tower::ServiceExt;
use wiremock::matchers::{header, method as http_method, path};
use wiremock::{Mock, ResponseTemplate};

use common::app::TestApp;

const DID: &str = "did:plc:proxyuser";
const TOKEN: &str = "proxy-access-token";

async fn response_json(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or(Value::Null)
}

fn set_routing(app: &TestApp, routing: ProxyRouting) {
    app.state.proxy_config.store(Arc::new(ProxyConfig {
        mode: ProxyMode::Open,
        nsids: vec![],
        routing,
    }));
}

/// Provision a DPoP key and register a session whose PDS is the mock server.
/// Returns `(client_key, private_jwk)` for signing proofs.
async fn register_session(app: &TestApp, scopes: &str) -> (String, Value) {
    let (client_key, client_secret, client_id) = app.create_api_client("confidential", None).await;

    // A session can only carry scopes its API client is registered for — the
    // fixture registers `atproto` alone, so widen it to whatever this session
    // asks for. Skipping this is a 400 at registration, not at use.
    let sql = happyview::db::adapt_sql(
        "UPDATE happyview_api_clients SET scopes = ? WHERE id = ?",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(scopes)
        .bind(&client_id)
        .execute(&app.state.db)
        .await
        .expect("failed to widen test client scopes");

    let key_req = Request::builder()
        .method("POST")
        .uri("/oauth/dpop-keys")
        .header("content-type", "application/json")
        .header("host", "127.0.0.1")
        .header("x-client-key", &client_key)
        .header("x-client-secret", &client_secret)
        .body(Body::from("{}"))
        .unwrap();
    let key_resp = app.router.clone().oneshot(key_req).await.unwrap();
    assert_eq!(key_resp.status(), StatusCode::CREATED);
    let key_body = response_json(key_resp).await;
    let provision_id = key_body["provision_id"].as_str().unwrap().to_string();
    let dpop_key = key_body["dpop_key"].clone();

    // Registration resolves the PDS from the DID document, never from the
    // client-supplied `pds_url` — so both must point at the mock.
    app.mock_session_verification(DID, DID).await;
    let pds_url = format!("{}/pds/{DID}", app.mock_server.uri());

    let session_req = Request::builder()
        .method("POST")
        .uri("/oauth/sessions")
        .header("content-type", "application/json")
        .header("host", "127.0.0.1")
        .header("x-client-key", &client_key)
        .header("x-client-secret", &client_secret)
        .body(Body::from(
            serde_json::to_vec(&json!({
                "provision_id": provision_id,
                "did": DID,
                "access_token": TOKEN,
                "scopes": scopes,
                "pds_url": pds_url,
            }))
            .unwrap(),
        ))
        .unwrap();
    let session_resp = app.router.clone().oneshot(session_req).await.unwrap();
    let status = session_resp.status();
    if status != StatusCode::CREATED {
        panic!(
            "session registration failed ({status}): {}",
            response_json(session_resp).await
        );
    }

    (client_key, dpop_key)
}

fn dpop_request(
    http_verb: &str,
    uri: &str,
    client_key: &str,
    dpop_key: &Value,
    body: Option<&Value>,
    extra: &[(&str, &str)],
) -> Request<Body> {
    // A DPoP proof's `htu` is the target URI **without** query or fragment
    // (RFC 9449 §4.2), and `resolve_dpop_claims` builds what it expects from
    // `uri.path()`. Signing over the query string produces a mismatch that
    // surfaces only as a bare 401.
    let path_only = uri.split('?').next().unwrap_or(uri);
    let request_url = format!("http://127.0.0.1:0{path_only}");
    let proof = generate_dpop_proof(dpop_key, http_verb, &request_url, TOKEN, None)
        .expect("failed to generate DPoP proof");

    let mut builder = Request::builder()
        .method(http_verb)
        .uri(uri)
        .header("host", "127.0.0.1:0")
        .header("x-client-key", client_key)
        .header("authorization", format!("DPoP {TOKEN}"))
        .header("dpop", proof);
    for (name, value) in extra {
        builder = builder.header(*name, *value);
    }
    if let Some(body) = body {
        builder = builder.header("content-type", "application/json");
        builder
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap()
    } else {
        builder.body(Body::empty()).unwrap()
    }
}

/// The headline fix. The mock only matches when the `Authorization` header is
/// present, so a 200 here *is* the assertion that the request arrived
/// authenticated — the exact thing whose absence produced `AuthMissing`.
#[tokio::test]
#[serial]
async fn forwards_to_the_callers_own_pds_with_authorization() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let (client_key, dpop_key) = register_session(&app, "atproto transition:generic").await;
    set_routing(&app, ProxyRouting::ServiceProxy);

    Mock::given(http_method("POST"))
        .and(path(format!(
            "/pds/{DID}/xrpc/com.atproto.repo.createRecord"
        )))
        .and(header("authorization", format!("DPoP {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "uri": format!("at://{DID}/com.example.post/abc"),
            "cid": "bafyreiabc",
        })))
        .mount(&app.mock_server)
        .await;

    let req = dpop_request(
        "POST",
        "/xrpc/com.atproto.repo.createRecord",
        &client_key,
        &dpop_key,
        Some(&json!({
            "repo": DID,
            "collection": "com.example.post",
            "record": { "text": "hello" },
        })),
        &[],
    );

    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "createRecord should reach the caller's PDS authenticated"
    );
    let body = response_json(resp).await;
    assert_eq!(body["cid"], "bafyreiabc");
}

/// `atproto-proxy` is relayed rather than resolved locally — HappyView cannot
/// mint the inter-service token, which is signed by the user's own identity key.
#[tokio::test]
#[serial]
async fn forwards_the_atproto_proxy_header_untouched() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let (client_key, dpop_key) = register_session(&app, "atproto transition:generic").await;
    set_routing(&app, ProxyRouting::ServiceProxy);

    Mock::given(http_method("POST"))
        .and(path(format!("/pds/{DID}/xrpc/com.example.doThing")))
        .and(header("atproto-proxy", "did:web:api.bsky.app#bsky_appview"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .mount(&app.mock_server)
        .await;

    let req = dpop_request(
        "POST",
        "/xrpc/com.example.doThing",
        &client_key,
        &dpop_key,
        Some(&json!({})),
        &[("atproto-proxy", "did:web:api.bsky.app#bsky_appview")],
    );

    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// A query reaches the PDS too, and its query string survives.
#[tokio::test]
#[serial]
async fn forwards_a_query_with_its_parameters() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let (client_key, dpop_key) = register_session(&app, "atproto transition:generic").await;
    set_routing(&app, ProxyRouting::ServiceProxy);

    Mock::given(http_method("GET"))
        .and(path(format!("/pds/{DID}/xrpc/com.example.getThing")))
        .and(wiremock::matchers::query_param("limit", "5"))
        .and(header("authorization", format!("DPoP {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [] })))
        .mount(&app.mock_server)
        .await;

    let req = dpop_request(
        "GET",
        "/xrpc/com.example.getThing?limit=5",
        &client_key,
        &dpop_key,
        None,
        &[],
    );

    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// Under service-proxy routing the destination is a function of who is asking,
/// so there is nobody to forward an anonymous request to. This is the
/// deliberate break that makes the routing opt-in.
#[tokio::test]
#[serial]
async fn refuses_an_anonymous_query() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    set_routing(&app, ProxyRouting::ServiceProxy);

    let req = Request::builder()
        .method("GET")
        .uri("/xrpc/com.example.getThing")
        .header("host", "127.0.0.1:0")
        .body(Body::empty())
        .unwrap();

    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// A proxied write acts as the caller, so naming someone else's repo can only
/// fail. It used to fail deep in session lookup as an error about the caller's
/// login rather than the repo they asked for — the message that cost a reporter
/// several days.
#[tokio::test]
#[serial]
async fn refuses_a_write_to_another_accounts_repo() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let (client_key, dpop_key) = register_session(&app, "atproto transition:generic").await;
    set_routing(&app, ProxyRouting::ServiceProxy);

    let req = dpop_request(
        "POST",
        "/xrpc/com.atproto.repo.createRecord",
        &client_key,
        &dpop_key,
        Some(&json!({
            "repo": "did:plc:someoneelse",
            "collection": "com.example.post",
            "record": {},
        })),
        &[],
    );

    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = response_json(resp).await;
    let message = body["error"].as_str().unwrap_or_default();
    assert!(
        message.contains("did:plc:someoneelse") && message.contains("linked repos"),
        "the refusal should name the repo and where to go instead, got: {message}"
    );
}

/// Forward-time scope enforcement: a session scoped to one collection cannot
/// write another. The PDS would refuse it too — this fails earlier, with a
/// message that says which scope is missing.
#[tokio::test]
#[serial]
async fn refuses_a_write_outside_the_sessions_scopes() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let (client_key, dpop_key) =
        register_session(&app, "atproto repo:com.example.allowed?action=create").await;
    set_routing(&app, ProxyRouting::ServiceProxy);

    let req = dpop_request(
        "POST",
        "/xrpc/com.atproto.repo.createRecord",
        &client_key,
        &dpop_key,
        Some(&json!({
            "repo": DID,
            "collection": "com.example.forbidden",
            "record": {},
        })),
        &[],
    );

    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = response_json(resp).await;
    let message = body["error"].as_str().unwrap_or_default();
    assert!(
        message.contains("repo:com.example.forbidden"),
        "the refusal should name the scope needed, got: {message}"
    );
}

/// The granted collection still works, so the check above is not simply
/// refusing everything.
#[tokio::test]
#[serial]
async fn allows_a_write_within_the_sessions_scopes() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let (client_key, dpop_key) =
        register_session(&app, "atproto repo:com.example.allowed?action=create").await;
    set_routing(&app, ProxyRouting::ServiceProxy);

    Mock::given(http_method("POST"))
        .and(path(format!(
            "/pds/{DID}/xrpc/com.atproto.repo.createRecord"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "cid": "bafyok" })))
        .mount(&app.mock_server)
        .await;

    let req = dpop_request(
        "POST",
        "/xrpc/com.atproto.repo.createRecord",
        &client_key,
        &dpop_key,
        Some(&json!({
            "repo": DID,
            "collection": "com.example.allowed",
            "record": {},
        })),
        &[],
    );

    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// An upstream error is relayed with its status and body intact — which is how
/// the original `AuthMissing` surfaced. Worth pinning so the relaying itself
/// does not regress while the routing around it changes.
#[tokio::test]
#[serial]
async fn relays_an_upstream_error_verbatim() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let (client_key, dpop_key) = register_session(&app, "atproto transition:generic").await;
    set_routing(&app, ProxyRouting::ServiceProxy);

    Mock::given(http_method("POST"))
        .and(path(format!("/pds/{DID}/xrpc/com.example.doThing")))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "error": "InvalidSwap",
            "message": "Record was at a different CID",
        })))
        .mount(&app.mock_server)
        .await;

    let req = dpop_request(
        "POST",
        "/xrpc/com.example.doThing",
        &client_key,
        &dpop_key,
        Some(&json!({})),
        &[],
    );

    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = response_json(resp).await;
    assert_eq!(body["error"], "InvalidSwap");
}

// ---------------------------------------------------------------------------
// The pieces underneath: scope lookup, GET proof shape, and nonce negotiation
// ---------------------------------------------------------------------------

/// `get_dpop_session_scopes` is hand-written SQL on the hot path of every
/// forwarded request. A wrong column name compiles fine and fails at runtime.
#[tokio::test]
#[serial]
async fn session_scopes_are_readable_without_decrypting_tokens() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let scopes = "atproto repo:com.example.allowed?action=create";
    register_session(&app, scopes).await;

    let sql = happyview::db::adapt_sql(
        "SELECT api_client_id, dpop_key_id FROM happyview_dpop_sessions WHERE user_did = ?",
        app.state.db_backend,
    );
    let (api_client_id, dpop_key_id): (String, String) = happyview::db::query_as(&sql)
        .bind(DID)
        .fetch_one(&app.state.db)
        .await
        .expect("session row should exist");

    let found = happyview::oauth::sessions::get_dpop_session_scopes(
        &app.state.db,
        app.state.db_backend,
        &api_client_id,
        &dpop_key_id,
    )
    .await
    .expect("scope lookup should succeed");

    assert_eq!(found.as_deref(), Some(scopes));
}

#[tokio::test]
#[serial]
async fn unknown_session_scopes_are_none_not_an_error() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;

    let found = happyview::oauth::sessions::get_dpop_session_scopes(
        &app.state.db,
        app.state.db_backend,
        "no-such-client",
        "no-such-key",
    )
    .await
    .expect("a missing session is not an error");

    assert!(found.is_none());
}

/// The outbound proof's `htu` excludes the query string (RFC 9449 §4.2). If it
/// did not, every forwarded query would fail as a signature mismatch — and the
/// PDS would report it as a bare 401, which reads as a login problem.
///
/// The mock asserts the shape by matching on the decoded `htu`.
#[tokio::test]
#[serial]
async fn a_forwarded_query_signs_a_proof_without_the_query_string() {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let (client_key, dpop_key) = register_session(&app, "atproto transition:generic").await;
    set_routing(&app, ProxyRouting::ServiceProxy);

    let expected_htu = format!(
        "{}/pds/{DID}/xrpc/com.example.getThing",
        app.mock_server.uri()
    );

    Mock::given(http_method("GET"))
        .and(path(format!("/pds/{DID}/xrpc/com.example.getThing")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .mount(&app.mock_server)
        .await;

    let req = dpop_request(
        "GET",
        "/xrpc/com.example.getThing?limit=5&limit=6",
        &client_key,
        &dpop_key,
        None,
        &[],
    );
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Inspect the proof the *outbound* request carried.
    let requests = app
        .mock_server
        .received_requests()
        .await
        .expect("mock server should record requests");
    let forwarded = requests
        .iter()
        .find(|r| r.url.path().ends_with("/xrpc/com.example.getThing"))
        .expect("the query should have been forwarded");

    // Repeated parameters must survive the round trip through the forward path;
    // a map-based rewrite would have collapsed these to one.
    let limits: Vec<_> = forwarded
        .url
        .query_pairs()
        .filter(|(k, _)| k == "limit")
        .map(|(_, v)| v.into_owned())
        .collect();
    assert_eq!(limits, vec!["5", "6"], "repeated query params must survive");

    let proof = forwarded
        .headers
        .get("dpop")
        .expect("forwarded request must carry a DPoP proof")
        .to_str()
        .unwrap();
    let payload = proof.split('.').nth(1).expect("proof should be a JWT");
    let decoded: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap();

    assert_eq!(
        decoded["htu"], expected_htu,
        "htu must be the bare target URI, with no query string"
    );
    assert_eq!(decoded["htm"], "GET");
}

/// A PDS may answer the first attempt with a nonce challenge. The retry has to
/// carry the nonce it was given — and the challenge is only a challenge when
/// the response says so, not merely because a `dpop-nonce` header is present.
#[tokio::test]
#[serial]
async fn a_nonce_challenge_is_retried_with_the_nonce() {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let (client_key, dpop_key) = register_session(&app, "atproto transition:generic").await;
    set_routing(&app, ProxyRouting::ServiceProxy);

    let endpoint = format!("/pds/{DID}/xrpc/com.example.doThing");

    // First attempt: an explicit nonce challenge. `up_to_n_times(1)` makes this
    // mock answer once, after which the second mock takes over.
    Mock::given(http_method("POST"))
        .and(path(endpoint.clone()))
        .respond_with(
            ResponseTemplate::new(401)
                .append_header("dpop-nonce", "server-nonce-1")
                .append_header("www-authenticate", r#"DPoP error="use_dpop_nonce""#)
                .set_body_json(json!({ "error": "use_dpop_nonce" })),
        )
        .up_to_n_times(1)
        .mount(&app.mock_server)
        .await;

    Mock::given(http_method("POST"))
        .and(path(endpoint.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .mount(&app.mock_server)
        .await;

    let req = dpop_request(
        "POST",
        "/xrpc/com.example.doThing",
        &client_key,
        &dpop_key,
        Some(&json!({})),
        &[],
    );
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "the nonce challenge should have been retried and succeeded"
    );

    let requests = app.mock_server.received_requests().await.unwrap();
    let attempts: Vec<_> = requests
        .iter()
        .filter(|r| r.url.path() == endpoint)
        .collect();
    assert_eq!(attempts.len(), 2, "expected one challenge and one retry");

    let nonce_of = |r: &wiremock::Request| -> Option<String> {
        let proof = r.headers.get("dpop")?.to_str().ok()?;
        let payload = proof.split('.').nth(1)?;
        let decoded: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
        decoded["nonce"].as_str().map(str::to_string)
    };

    assert_eq!(
        nonce_of(attempts[0]),
        None,
        "the first attempt has no nonce"
    );
    assert_eq!(
        nonce_of(attempts[1]).as_deref(),
        Some("server-nonce-1"),
        "the retry must carry the nonce the server supplied"
    );
}

/// The bug the buffered-response rewrite fixed: PDS implementations attach a
/// `dpop-nonce` header to *every* error, so keying off the header alone made a
/// validation failure look like a nonce challenge — and the write was resent.
#[tokio::test]
#[serial]
async fn a_validation_error_carrying_a_nonce_is_not_retried() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let (client_key, dpop_key) = register_session(&app, "atproto transition:generic").await;
    set_routing(&app, ProxyRouting::ServiceProxy);

    let endpoint = format!("/pds/{DID}/xrpc/com.example.doThing");

    Mock::given(http_method("POST"))
        .and(path(endpoint.clone()))
        .respond_with(
            ResponseTemplate::new(400)
                .append_header("dpop-nonce", "server-nonce-1")
                .set_body_json(json!({
                    "error": "InvalidRequest",
                    "message": "Invalid record",
                })),
        )
        .mount(&app.mock_server)
        .await;

    let req = dpop_request(
        "POST",
        "/xrpc/com.example.doThing",
        &client_key,
        &dpop_key,
        Some(&json!({})),
        &[],
    );
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let requests = app.mock_server.received_requests().await.unwrap();
    let attempts = requests.iter().filter(|r| r.url.path() == endpoint).count();
    assert_eq!(
        attempts, 1,
        "a validation error must not be resent as if it were a nonce challenge"
    );
}

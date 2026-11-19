//! End to end: a DPoP-signed-in caller gets a caller
//! assertion naming a browser key it proved, a stand-in for the ticket issuer
//! turns that into a ticket, and the ticket opens an instance's admin
//! API, with proofs made by that browser key and nothing else.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use common::app::TestApp;
use happyview::auth::caller_assertion::{CallerAssertionConfig, assert_caller, issuer};
use happyview::oauth::pds_write::generate_dpop_proof;
use http_body_util::BodyExt;
use p256::ecdsa::{Signature, SigningKey, VerifyingKey, signature::Signer, signature::Verifier};
use rand::Rng;
use serde_json::{Value, json};
use serial_test::serial;
use sha2::{Digest, Sha256};
use tower::ServiceExt;

const PROVISIONER: &str = "https://provisioner.example";
const TICKET_AUD: &str = "hpy:instance:managed";
const METHOD: &str = "at.happyproto.server.getManagementTicket";
const ORIGIN: &str = "http://127.0.0.1:0";

fn new_key() -> SigningKey {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    SigningKey::from_slice(&bytes).unwrap()
}

fn public_jwk(key: &SigningKey) -> Value {
    let point = key.verifying_key().to_sec1_point(false);
    json!({
        "kty": "EC", "crv": "P-256",
        "x": URL_SAFE_NO_PAD.encode(point.x().unwrap()),
        "y": URL_SAFE_NO_PAD.encode(point.y().unwrap()),
    })
}

fn thumbprint_of(jwk: &Value) -> String {
    let canonical = format!(
        r#"{{"crv":"P-256","kty":"EC","x":"{}","y":"{}"}}"#,
        jwk["x"].as_str().unwrap(),
        jwk["y"].as_str().unwrap()
    );
    URL_SAFE_NO_PAD.encode(Sha256::digest(canonical.as_bytes()))
}

fn sign(key: &SigningKey, header: &Value, claims: &Value) -> String {
    let h = URL_SAFE_NO_PAD.encode(serde_json::to_vec(header).unwrap());
    let c = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
    let input = format!("{h}.{c}");
    let sig: Signature = key.sign(input.as_bytes());
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig.to_bytes()))
}

fn b64_sha256(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(bytes))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn decode(part: &str) -> Value {
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
}

/// The ticket issuer, as the provisioner does it: verify the assertion
/// against the pinned key, insist on the dashboard's client, the ticket
/// method and a proven key, then sign a ticket for that key. `Err` with the
/// refused claim otherwise.
struct FakeIssuer {
    key: SigningKey,
    pinned: Value,
    assertion_issuer: String,
    client_id: String,
}

impl FakeIssuer {
    fn issue(&self, assertion: &str) -> Result<String, &'static str> {
        let parts: Vec<&str> = assertion.split('.').collect();
        let (header, claims) = (decode(parts[0]), decode(parts[1]));
        let jwk = self.pinned["keys"]
            .as_array()
            .unwrap()
            .iter()
            .find(|k| k["kid"] == header["kid"])
            .ok_or("kid")?;
        let mut sec1 = vec![0x04];
        sec1.extend(URL_SAFE_NO_PAD.decode(jwk["x"].as_str().unwrap()).unwrap());
        sec1.extend(URL_SAFE_NO_PAD.decode(jwk["y"].as_str().unwrap()).unwrap());
        let sig = Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
        VerifyingKey::from_sec1_bytes(&sec1)
            .unwrap()
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
            .map_err(|_| "signature")?;
        if header["typ"] != "hv-caller+jwt" {
            return Err("typ");
        }
        for (claim, want) in [
            ("iss", self.assertion_issuer.as_str()),
            ("aud", PROVISIONER),
            ("azp", self.client_id.as_str()),
            ("lxm", METHOD),
            ("cnf_src", "key_proof"),
        ] {
            if claims[claim] != want {
                return Err(claim);
            }
        }
        let iat = now();
        Ok(sign(
            &self.key,
            &json!({ "alg": "ES256", "typ": "hv-ticket+jwt", "kid": "t1" }),
            &json!({
                "iss": PROVISIONER, "sub": claims["sub"], "aud": TICKET_AUD,
                "cnf": { "jkt": claims["cnf"]["jkt"] },
                "iat": iat, "exp": iat + 60,
                "jti": uuid::Uuid::new_v4().to_string(), "v": 1,
            }),
        ))
    }

    fn trust(&self) -> happyview::auth::ticket::TicketTrust {
        let mut jwk = public_jwk(&self.key);
        jwk["kid"] = json!("t1");
        let jwks = json!({ "keys": [jwk] }).to_string();
        happyview::auth::ticket::TicketTrust::parse(
            Some(PROVISIONER),
            Some(TICKET_AUD),
            Some(&jwks),
        )
        .unwrap()
        .unwrap()
    }
}

/// A GET to `path` on the managed instance with `ticket` and a proof signed by
/// `key`.
fn ticket_request(ticket: &str, key: &SigningKey, path: &str) -> Request<Body> {
    let proof = sign(
        key,
        &json!({ "alg": "ES256", "typ": "hv-ticket-proof+jwt", "jwk": public_jwk(key) }),
        &json!({
            "htm": "GET", "htu": format!("{ORIGIN}{path}"),
            "hq": b64_sha256(b""), "bh": b64_sha256(b""),
            "ath": b64_sha256(ticket.as_bytes()),
            "iat": now(), "jti": uuid::Uuid::new_v4().to_string(),
        }),
    );
    Request::builder()
        .method("GET")
        .uri(path)
        .header("host", "127.0.0.1")
        .header("authorization", format!("HV-Ticket {ticket}"))
        .header("hv-ticket-proof", proof)
        .body(Body::empty())
        .unwrap()
}

async fn json_body(resp: axum::http::Response<Body>) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

fn post(uri: &str, body: &Value, headers: &[(&str, &str)]) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    b.body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

#[tokio::test]
#[serial]
async fn a_dpop_session_becomes_a_ticket_that_only_the_browser_key_can_use() {
    common::require_db!();

    // One instance plays both parts: HappyProto's, where the owner signs in
    // with DPoP and gets a caller assertion, and the managed one, whose admin
    // API the ticket opens. (Test apps share one database, one at a time.)
    let mut hp = TestApp::new_with_encryption().await;
    let owner = hp.admin_did.clone();
    let assertion_key = new_key();
    let mut private = public_jwk(&assertion_key);
    private["kid"] = json!("a1");
    private["d"] = json!(URL_SAFE_NO_PAD.encode(assertion_key.to_bytes()));
    let config = CallerAssertionConfig::parse(Some(&private.to_string()), Some(PROVISIONER))
        .unwrap()
        .unwrap();
    let pinned = config.public_jwks();
    hp.state.config.caller_assertions = Some(config);
    hp.rebuild_router();

    // The owner signs in to HappyProto's instance with DPoP.
    let (client_key, client_secret, _) = hp.create_api_client("confidential", None).await;
    let (client_id_url,): (String,) = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT client_id_url FROM happyview_api_clients WHERE client_key = ?",
        hp.state.db_backend,
    ))
    .bind(&client_key)
    .fetch_one(&hp.state.db)
    .await
    .unwrap();
    let auth = [
        ("x-client-key", client_key.as_str()),
        ("x-client-secret", client_secret.as_str()),
    ];
    let key_resp = hp
        .router
        .clone()
        .oneshot(post("/oauth/dpop-keys", &json!({}), &auth))
        .await
        .unwrap();
    assert_eq!(key_resp.status(), StatusCode::CREATED);
    let key_body = json_body(key_resp).await;
    let dpop_key = key_body["dpop_key"].clone();
    let access_token = "e2e-access-token";
    hp.mock_session_verification(&owner, &owner).await;
    // The browser made its ticket key before signing in, and pins it to the
    // sign-in as the session is registered.
    let browser = new_key();
    let pin = |key: &SigningKey, dpop_jkt: &str| {
        sign(
            key,
            &json!({ "alg": "ES256", "typ": "hv-key-proof+jwt", "jwk": public_jwk(key) }),
            &json!({
                "aud": issuer(&hp.state), "lxm": happyview::auth::caller_assertion::SESSION_PIN_LXM,
                "dpop_jkt": dpop_jkt, "iat": now(), "jti": uuid::Uuid::new_v4().to_string(),
            }),
        )
    };
    let register = |proof: String| {
        post(
            "/oauth/sessions",
            &json!({
                "provision_id": key_body["provision_id"], "did": owner,
                "access_token": access_token, "scopes": "atproto",
                "pds_url": "https://pds.example.com",
                "ticket_key_proof": proof,
            }),
            &auth,
        )
    };
    let session = register(pin(&browser, &thumbprint_of(&dpop_key)));
    let resp = hp.router.clone().oneshot(session).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert_eq!(json_body(resp).await["ticket_pin"], "pinned");

    // The getManagementTicket request, authenticated as core does it.
    let path = format!("/xrpc/{METHOD}");
    let dpop = generate_dpop_proof(
        &dpop_key,
        "POST",
        &format!("http://127.0.0.1:0{path}"),
        access_token,
        None,
    )
    .unwrap();
    let (parts, _) = Request::builder()
        .method("POST")
        .uri(&path)
        .header("host", "127.0.0.1:0")
        .header("x-client-key", &client_key)
        .header("authorization", format!("DPoP {access_token}"))
        .header("dpop", &dpop)
        .body(())
        .unwrap()
        .into_parts();
    let claims = happyview::auth::middleware::resolve_dpop_claims(&hp.state, &parts, access_token)
        .await
        .expect("a DPoP session");

    // The browser's ticket key proves itself for this request.
    let key_proof = sign(
        &browser,
        &json!({ "alg": "ES256", "typ": "hv-key-proof+jwt", "jwk": public_jwk(&browser) }),
        &json!({
            "aud": issuer(&hp.state), "lxm": METHOD,
            "dpop_jkt": thumbprint_of(&dpop_key),
            "iat": now(), "jti": uuid::Uuid::new_v4().to_string(),
        }),
    );
    let trigger = format!("xrpc.procedure:{METHOD}");
    let assertion = assert_caller(&hp.state, &claims, &trigger, PROVISIONER, Some(&key_proof))
        .await
        .unwrap();

    // The issuer turns it into a ticket for the browser key.
    let ticket_issuer = FakeIssuer {
        key: new_key(),
        pinned,
        assertion_issuer: issuer(&hp.state),
        client_id: client_id_url,
    };
    let ticket = ticket_issuer.issue(&assertion.token).expect("a ticket");
    assert_eq!(
        decode(ticket.split('.').nth(1).unwrap())["cnf"]["jkt"],
        thumbprint_of(&public_jwk(&browser))
    );

    // Without a key proof the assertion names the DPoP key, and gets no ticket.
    let dpop_bound = assert_caller(&hp.state, &claims, &trigger, PROVISIONER, None)
        .await
        .unwrap();
    assert_eq!(ticket_issuer.issue(&dpop_bound.token), Err("cnf_src"));

    // The managed instance trusts the issuer: the browser key's proofs open
    // its admin API as the owner.
    hp.state.config.management_tickets = Some(ticket_issuer.trust());
    hp.rebuild_router();
    let resp = hp
        .unmarked_router()
        .oneshot(ticket_request(&ticket, &browser, "/admin/stats/counts"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let counts = json_body(resp).await;
    assert!(counts["users"].as_i64().unwrap() >= 1, "{counts}");

    // The same ticket with proofs from any other key, the DPoP key included,
    // is refused.
    let dpop_signing = SigningKey::from_slice(
        &URL_SAFE_NO_PAD
            .decode(dpop_key["d"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    for other in [dpop_signing, new_key()] {
        let resp = hp
            .unmarked_router()
            .oneshot(ticket_request(&ticket, &other, "/admin/stats/counts"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // A key the sign-in didn't pin gets no assertion, even with a valid proof.
    let stranger = new_key();
    let stranger_proof = sign(
        &stranger,
        &json!({ "alg": "ES256", "typ": "hv-key-proof+jwt", "jwk": public_jwk(&stranger) }),
        &json!({
            "aud": issuer(&hp.state), "lxm": METHOD,
            "dpop_jkt": thumbprint_of(&dpop_key),
            "iat": now(), "jti": uuid::Uuid::new_v4().to_string(),
        }),
    );
    assert!(matches!(
        assert_caller(
            &hp.state,
            &claims,
            &trigger,
            PROVISIONER,
            Some(&stranger_proof)
        )
        .await,
        Err(happyview::auth::caller_assertion::AssertionError::KeyProof(
            _
        ))
    ));

    // Signing out ends the session, and its pin with it.
    let url = format!("http://127.0.0.1:0/oauth/sessions/{owner}");
    let dpop = generate_dpop_proof(&dpop_key, "DELETE", &url, access_token, None).unwrap();
    let delete = Request::builder()
        .method("DELETE")
        .uri(format!("/oauth/sessions/{owner}"))
        .header("host", "127.0.0.1:0")
        .header("x-client-key", &client_key)
        .header("authorization", format!("DPoP {access_token}"))
        .header("dpop", &dpop)
        .body(Body::empty())
        .unwrap();
    assert!(
        hp.router
            .clone()
            .oneshot(delete)
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let (pins,): (i64,) = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT COUNT(*) FROM happyview_dpop_keys WHERE ticket_jkt IS NOT NULL",
        hp.state.db_backend,
    ))
    .fetch_one(&hp.state.db)
    .await
    .unwrap();
    assert_eq!(pins, 0);
}

#[tokio::test]
#[serial]
async fn a_sign_in_without_a_pinned_key_gets_no_key_bound_assertion() {
    common::require_db!();
    let mut hp = TestApp::new_with_encryption().await;
    let owner = hp.admin_did.clone();
    let key = new_key();
    let mut private = public_jwk(&key);
    private["kid"] = json!("a1");
    private["d"] = json!(URL_SAFE_NO_PAD.encode(key.to_bytes()));
    hp.state.config.caller_assertions = Some(
        CallerAssertionConfig::parse(Some(&private.to_string()), Some(PROVISIONER))
            .unwrap()
            .unwrap(),
    );
    hp.rebuild_router();

    let (client_key, client_secret, _) = hp.create_api_client("confidential", None).await;
    let auth = [
        ("x-client-key", client_key.as_str()),
        ("x-client-secret", client_secret.as_str()),
    ];
    let key_resp = hp
        .router
        .clone()
        .oneshot(post("/oauth/dpop-keys", &json!({}), &auth))
        .await
        .unwrap();
    let key_body = json_body(key_resp).await;
    let dpop_key = key_body["dpop_key"].clone();
    let access_token = "unpinned-token";
    hp.mock_session_verification(&owner, &owner).await;
    // A pin that doesn't verify (made for another DPoP key) never blocks the
    // sign-in: the session is registered, unpinned, and says so.
    let wrong = new_key();
    let bad_pin = sign(
        &wrong,
        &json!({ "alg": "ES256", "typ": "hv-key-proof+jwt", "jwk": public_jwk(&wrong) }),
        &json!({
            "aud": issuer(&hp.state), "lxm": happyview::auth::caller_assertion::SESSION_PIN_LXM,
            "dpop_jkt": "another-dpop-key", "iat": now(), "jti": uuid::Uuid::new_v4().to_string(),
        }),
    );
    let session = post(
        "/oauth/sessions",
        &json!({
            "provision_id": key_body["provision_id"], "did": owner,
            "access_token": access_token, "scopes": "atproto",
            "pds_url": "https://pds.example.com",
            "ticket_key_proof": bad_pin,
        }),
        &auth,
    );
    let resp = hp.router.clone().oneshot(session).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert_eq!(json_body(resp).await["ticket_pin"], "refused");
    let (pins,): (i64,) = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT COUNT(*) FROM happyview_dpop_keys WHERE ticket_jkt IS NOT NULL",
        hp.state.db_backend,
    ))
    .fetch_one(&hp.state.db)
    .await
    .unwrap();
    assert_eq!(pins, 0);

    let path = format!("/xrpc/{METHOD}");
    let dpop = generate_dpop_proof(
        &dpop_key,
        "POST",
        &format!("http://127.0.0.1:0{path}"),
        access_token,
        None,
    )
    .unwrap();
    let (parts, _) = Request::builder()
        .method("POST")
        .uri(&path)
        .header("host", "127.0.0.1:0")
        .header("x-client-key", &client_key)
        .header("authorization", format!("DPoP {access_token}"))
        .header("dpop", &dpop)
        .body(())
        .unwrap()
        .into_parts();
    let claims = happyview::auth::middleware::resolve_dpop_claims(&hp.state, &parts, access_token)
        .await
        .unwrap();
    let browser = new_key();
    let proof = sign(
        &browser,
        &json!({ "alg": "ES256", "typ": "hv-key-proof+jwt", "jwk": public_jwk(&browser) }),
        &json!({
            "aud": issuer(&hp.state), "lxm": METHOD,
            "dpop_jkt": thumbprint_of(&dpop_key),
            "iat": now(), "jti": uuid::Uuid::new_v4().to_string(),
        }),
    );
    let trigger = format!("xrpc.procedure:{METHOD}");
    assert_eq!(
        assert_caller(&hp.state, &claims, &trigger, PROVISIONER, Some(&proof))
            .await
            .unwrap_err(),
        happyview::auth::caller_assertion::AssertionError::NotPinned
    );
}

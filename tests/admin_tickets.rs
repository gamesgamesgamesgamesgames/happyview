//! Management tickets on the admin API: a trusted issuer's ticket naming a
//! user, presented with a proof for the request, authenticates that user with
//! their own permissions, and nothing else.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use common::app::TestApp;
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use rand::Rng;
use serde_json::{Value, json};
use serial_test::serial;
use sha2::{Digest, Sha256};
use tower::ServiceExt;

const ISS: &str = "https://issuer.example";
const AUD: &str = "hpy:instance:test";
const ORIGIN: &str = "http://127.0.0.1:0";

fn new_key() -> SigningKey {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    SigningKey::from_slice(&bytes).unwrap()
}

fn public_jwk(key: &SigningKey) -> Value {
    let point = key.verifying_key().to_sec1_point(false);
    json!({
        "kty": "EC",
        "crv": "P-256",
        "x": URL_SAFE_NO_PAD.encode(point.x().unwrap()),
        "y": URL_SAFE_NO_PAD.encode(point.y().unwrap()),
    })
}

fn thumbprint(key: &SigningKey) -> String {
    let jwk = public_jwk(key);
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

struct Issuer {
    key: SigningKey,
    caller: SigningKey,
}

impl Issuer {
    fn new() -> Self {
        Issuer {
            key: new_key(),
            caller: new_key(),
        }
    }

    fn trust(&self) -> happyview::auth::ticket::TicketTrust {
        let mut jwk = public_jwk(&self.key);
        jwk["kid"] = json!("k1");
        let jwks = json!({ "keys": [jwk] }).to_string();
        happyview::auth::ticket::TicketTrust::parse(Some(ISS), Some(AUD), Some(&jwks))
            .unwrap()
            .unwrap()
    }

    fn ticket(&self, did: &str) -> String {
        let iat = now();
        sign(
            &self.key,
            &json!({ "alg": "ES256", "typ": "hv-ticket+jwt", "kid": "k1" }),
            &json!({
                "iss": ISS, "sub": did, "aud": AUD,
                "cnf": { "jkt": thumbprint(&self.caller) },
                "iat": iat, "exp": iat + 60,
                "jti": uuid::Uuid::new_v4().to_string(), "v": 1,
            }),
        )
    }

    /// A request to `path` with a ticket for `did`, its proof claiming
    /// `proof_body` as the body while the request sends `body`.
    fn request(
        &self,
        did: &str,
        method: &str,
        path: &str,
        body: &[u8],
        proof_body: &[u8],
        origin: &str,
    ) -> axum::http::request::Builder {
        let ticket = self.ticket(did);
        let has_body = !body.is_empty();
        let mut claims = json!({
            "htm": method,
            "htu": format!("{origin}{path}"),
            "hq": b64_sha256(b""),
            "bh": b64_sha256(proof_body),
            "ath": b64_sha256(ticket.as_bytes()),
            "iat": now(),
            "jti": uuid::Uuid::new_v4().to_string(),
        });
        if has_body {
            claims["ct"] = json!("application/json");
        }
        let proof = sign(
            &self.caller,
            &json!({ "alg": "ES256", "typ": "hv-ticket-proof+jwt", "jwk": public_jwk(&self.caller) }),
            &claims,
        );
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1")
            .header("authorization", format!("HV-Ticket {ticket}"))
            .header("hv-ticket-proof", proof);
        if has_body {
            builder = builder.header("content-type", "application/json");
        }
        builder
    }
}

async fn app_trusting(issuer: &Issuer) -> TestApp {
    let mut app = TestApp::new().await;
    app.state.config.management_tickets = Some(issuer.trust());
    app.rebuild_router();
    app
}

async fn send(app: &TestApp, req: Request<Body>) -> StatusCode {
    app.unmarked_router().oneshot(req).await.unwrap().status()
}

async fn user_count(app: &TestApp) -> i64 {
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM happyview_users")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    n
}

#[tokio::test]
#[serial]
async fn a_ticket_authenticates_its_user() {
    let issuer = Issuer::new();
    let app = app_trusting(&issuer).await;
    let req = issuer
        .request(&app.admin_did, "GET", "/admin/stats", b"", b"", ORIGIN)
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, req).await, StatusCode::OK);
}

#[tokio::test]
#[serial]
async fn the_proof_covers_the_body() {
    let issuer = Issuer::new();
    let app = app_trusting(&issuer).await;
    let body = serde_json::to_vec(&json!({ "value": "x" })).unwrap();

    let req = issuer
        .request(
            &app.admin_did,
            "PUT",
            "/admin/settings/app_name",
            &body,
            &body,
            ORIGIN,
        )
        .body(Body::from(body.clone()))
        .unwrap();
    assert!(send(&app, req).await.is_success());

    let other = serde_json::to_vec(&json!({ "value": "y" })).unwrap();
    let req = issuer
        .request(
            &app.admin_did,
            "PUT",
            "/admin/settings/app_name",
            &other,
            &body,
            ORIGIN,
        )
        .body(Body::from(other))
        .unwrap();
    assert_eq!(send(&app, req).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[serial]
async fn a_ticket_for_another_origin_is_refused() {
    let issuer = Issuer::new();
    let app = app_trusting(&issuer).await;
    let req = issuer
        .request(
            &app.admin_did,
            "GET",
            "/admin/stats",
            b"",
            b"",
            "https://elsewhere.example",
        )
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, req).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[serial]
async fn a_ticket_with_a_cookie_is_refused() {
    let issuer = Issuer::new();
    let app = app_trusting(&issuer).await;
    let cookie = app.admin_cookie();
    let req = issuer
        .request(&app.admin_did, "GET", "/admin/stats", b"", b"", ORIGIN)
        .header(cookie.0, cookie.1)
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, req).await, StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[serial]
async fn tickets_are_off_unless_configured() {
    let issuer = Issuer::new();
    let app = TestApp::new().await;
    let req = issuer
        .request(&app.admin_did, "GET", "/admin/stats", b"", b"", ORIGIN)
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, req).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[serial]
async fn a_ticket_never_creates_a_user() {
    let issuer = Issuer::new();
    let app = app_trusting(&issuer).await;

    let req = issuer
        .request("did:plc:stranger", "GET", "/admin/stats", b"", b"", ORIGIN)
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, req).await, StatusCode::FORBIDDEN);

    // Not even on an instance with no users yet, where the first sign-in
    // would otherwise become the super user.
    sqlx::query("DELETE FROM happyview_user_permissions")
        .execute(&app.state.db)
        .await
        .unwrap();
    sqlx::query("DELETE FROM happyview_users")
        .execute(&app.state.db)
        .await
        .unwrap();
    let req = issuer
        .request("did:plc:stranger", "GET", "/admin/stats", b"", b"", ORIGIN)
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, req).await, StatusCode::FORBIDDEN);
    assert_eq!(user_count(&app).await, 0);
}

/// A job that inherits its creator's auth runs with that DID's stored OAuth
/// session (`jobs::worker`). Admin routes authenticate callers who may not
/// have one of their own, a management ticket's included, so no admin route
/// may create such a job.
#[test]
fn no_admin_route_creates_a_job_that_inherits_auth() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/admin");
    let mut calls = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let src = std::fs::read_to_string(&path).unwrap();
        let mut rest = src.as_str();
        while let Some(at) = rest.find("create_job(") {
            let args = call_args(&rest[at + "create_job(".len()..]);
            // create_job(state, job_type, input, created_by, inherit_auth, ...)
            assert_eq!(
                args.get(4).map(String::as_str),
                Some("false"),
                "{} creates a job with inherit_auth = {:?}",
                path.display(),
                args.get(4)
            );
            calls += 1;
            rest = &rest[at + 1..];
        }
    }
    assert!(
        calls > 0,
        "found no create_job calls; has the function moved?"
    );
}

/// The top-level, comma-separated arguments of a call whose `(` was just
/// consumed, trimmed.
fn call_args(src: &str) -> Vec<String> {
    let (mut depth, mut args, mut current) = (0i32, Vec::new(), String::new());
    for c in src.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth == 0 => break,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                args.push(current.trim().to_string());
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    if !current.trim().is_empty() {
        args.push(current.trim().to_string());
    }
    args
}

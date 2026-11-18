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
        self.ticket_and_jti(did).0
    }

    /// A ticket for `did`, and its `jti`.
    fn ticket_and_jti(&self, did: &str) -> (String, String) {
        let iat = now();
        let jti = uuid::Uuid::new_v4().to_string();
        let ticket = sign(
            &self.key,
            &json!({ "alg": "ES256", "typ": "hv-ticket+jwt", "kid": "k1" }),
            &json!({
                "iss": ISS, "sub": did, "aud": AUD,
                "cnf": { "jkt": thumbprint(&self.caller) },
                "iat": iat, "exp": iat + 60,
                "jti": jti, "v": 1,
            }),
        );
        (ticket, jti)
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
        self.request_with(&ticket, method, path, body, proof_body, origin)
    }

    /// As [`Issuer::request`], with a ticket already issued.
    fn request_with(
        &self,
        ticket: &str,
        method: &str,
        path: &str,
        body: &[u8],
        proof_body: &[u8],
        origin: &str,
    ) -> axum::http::request::Builder {
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
/// may create such a job. `create_job` also refuses one at run time while a
/// ticket request is being handled; this keeps the admin routes from relying
/// on that.
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

#[tokio::test]
#[serial]
async fn a_proof_is_used_once() {
    let issuer = Issuer::new();
    let app = app_trusting(&issuer).await;
    let first = issuer
        .request(&app.admin_did, "GET", "/admin/stats", b"", b"", ORIGIN)
        .body(Body::empty())
        .unwrap();
    let mut again = Request::builder().method("GET").uri("/admin/stats");
    for (name, value) in first.headers() {
        again = again.header(name, value);
    }
    assert_eq!(send(&app, first).await, StatusCode::OK);
    assert_eq!(
        send(&app, again.body(Body::empty()).unwrap()).await,
        StatusCode::UNAUTHORIZED
    );
}

/// A request whose ticket fails is refused on its headers, before its body is
/// read: an oversized body gets the ticket's 401, not the 413 that reading it
/// would.
#[tokio::test]
#[serial]
async fn a_failing_ticket_is_refused_before_its_body_is_read() {
    let issuer = Issuer::new();
    let app = app_trusting(&issuer).await;
    let huge = vec![b' '; happyview::auth::ticket::MAX_BODY_BYTES + 1];
    let req = issuer
        .request(
            &app.admin_did,
            "PUT",
            "/admin/settings/app_name",
            b"{}",
            b"{}",
            "https://elsewhere.example",
        )
        .body(Body::from(huge.clone()))
        .unwrap();
    assert_eq!(send(&app, req).await, StatusCode::UNAUTHORIZED);

    // With a valid ticket the same body is read, and is too large.
    let req = issuer
        .request(
            &app.admin_did,
            "PUT",
            "/admin/settings/app_name",
            b"{}",
            b"{}",
            ORIGIN,
        )
        .body(Body::from(huge))
        .unwrap();
    assert_eq!(send(&app, req).await, StatusCode::PAYLOAD_TOO_LARGE);
}

/// Copy a request's method, URI and headers, to send it again with `body`.
fn resend_with(req: &Request<Body>, body: &[u8]) -> Request<Body> {
    let mut again = Request::builder()
        .method(req.method().clone())
        .uri(req.uri().clone());
    for (name, value) in req.headers() {
        again = again.header(name, value);
    }
    again.body(Body::from(body.to_vec())).unwrap()
}

fn resend(req: &Request<Body>) -> Request<Body> {
    resend_with(req, b"")
}

#[tokio::test]
#[serial]
async fn a_ticket_request_carrying_another_credential_or_an_upgrade_is_refused() {
    let issuer = Issuer::new();
    let app = app_trusting(&issuer).await;
    for (name, value) in [
        ("dpop", "eyJ.e30.x"),
        ("x-client-key", "hvc_test"),
        ("upgrade", "websocket"),
    ] {
        let req = issuer
            .request(&app.admin_did, "GET", "/admin/stats", b"", b"", ORIGIN)
            .header(name, value)
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(&app, req).await, StatusCode::BAD_REQUEST, "{name}");
    }
}

/// One ticket serves many requests, each with its own proof; each proof is
/// accepted once.
#[tokio::test]
#[serial]
async fn one_ticket_serves_many_requests_with_a_proof_each() {
    let issuer = Issuer::new();
    let app = app_trusting(&issuer).await;
    let ticket = issuer.ticket(&app.admin_did);
    let first = issuer
        .request_with(&ticket, "GET", "/admin/stats", b"", b"", ORIGIN)
        .body(Body::empty())
        .unwrap();
    let second = issuer
        .request_with(&ticket, "GET", "/admin/stats", b"", b"", ORIGIN)
        .body(Body::empty())
        .unwrap();
    let (first_again, second_again) = (resend(&first), resend(&second));

    assert_eq!(send(&app, first).await, StatusCode::OK);
    assert_eq!(send(&app, second).await, StatusCode::OK);
    assert_eq!(send(&app, second_again).await, StatusCode::UNAUTHORIZED);
    assert_eq!(send(&app, first_again).await, StatusCode::UNAUTHORIZED);
}

/// With tickets off, a ticket request is refused without its body being read,
/// however large.
#[tokio::test]
#[serial]
async fn tickets_off_refuse_a_large_body_without_reading_it() {
    let issuer = Issuer::new();
    let app = TestApp::new().await;
    let huge = vec![b' '; happyview::auth::ticket::MAX_BODY_BYTES + 1];
    let req = issuer
        .request(
            &app.admin_did,
            "PUT",
            "/admin/settings/app_name",
            b"{}",
            b"{}",
            ORIGIN,
        )
        .body(Body::from(huge))
        .unwrap();
    assert_eq!(send(&app, req).await, StatusCode::UNAUTHORIZED);
}

/// `(detail, actor_did)` of each event of `event_type`, oldest first.
async fn events(app: &TestApp, event_type: &str) -> Vec<(Value, Option<String>)> {
    let rows: Vec<(String, Option<String>)> = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT detail, actor_did FROM happyview_event_logs WHERE event_type = ? ORDER BY created_at",
        app.state.db_backend,
    ))
    .bind(event_type)
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(d, a)| (serde_json::from_str(&d).unwrap(), a))
        .collect()
}

#[tokio::test]
#[serial]
async fn the_event_log_records_the_ticket() {
    let issuer = Issuer::new();
    let app = app_trusting(&issuer).await;
    let (ticket, jti) = issuer.ticket_and_jti(&app.admin_did);
    let body = serde_json::to_vec(&json!({ "value": "Ticketed" })).unwrap();
    let req = issuer
        .request_with(
            &ticket,
            "PUT",
            "/admin/settings/app_name",
            &body,
            &body,
            ORIGIN,
        )
        .body(Body::from(body.clone()))
        .unwrap();
    let again = resend_with(&req, &body);
    assert!(send(&app, req).await.is_success());

    let updated = events(&app, "setting.updated").await;
    let (detail, actor) = updated.last().expect("a setting.updated event");
    assert_eq!(actor.as_deref(), Some(app.admin_did.as_str()));
    assert_eq!(detail["auth"], "ticket");
    assert_eq!(detail["ticket"]["jti"], jti.as_str());
    assert_eq!(detail["ticket"]["iss"], ISS);
    assert_eq!(detail["ticket"]["jkt"], thumbprint(&issuer.caller).as_str());

    // A replayed proof is refused, and recorded as a refused ticket.
    assert_eq!(send(&app, again).await, StatusCode::UNAUTHORIZED);
    let refused = events(&app, "ticket.refused").await;
    let (detail, actor) = refused.last().expect("a ticket.refused event");
    assert_eq!(actor.as_deref(), Some(app.admin_did.as_str()));
    assert_eq!(detail["step"], "replay");
    assert_eq!(detail["path"], "/admin/settings/app_name");
    assert_eq!(detail["ticket"]["jti"], jti.as_str());

    // So is a ticket for someone who isn't a user here.
    let req = issuer
        .request("did:plc:stranger", "GET", "/admin/stats", b"", b"", ORIGIN)
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, req).await, StatusCode::FORBIDDEN);
    let refused = events(&app, "ticket.refused").await;
    let (detail, actor) = refused.last().unwrap();
    assert_eq!(actor.as_deref(), Some("did:plc:stranger"));
    assert_eq!(detail["step"], "not_a_user");
    assert_eq!(detail["auth"], "ticket");

    // A ticket the issuer didn't sign is only traced.
    let before = events(&app, "ticket.refused").await.len();
    let stranger = Issuer::new();
    let req = stranger
        .request(&app.admin_did, "GET", "/admin/stats", b"", b"", ORIGIN)
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, req).await, StatusCode::UNAUTHORIZED);
    assert_eq!(events(&app, "ticket.refused").await.len(), before);
}

async fn add_user(app: &TestApp, did: &str, permissions: &[&str]) {
    let s = &app.state;
    let user_id = uuid::Uuid::new_v4().to_string();
    let now = happyview::db::now_rfc3339();
    happyview::db::query(&happyview::db::adapt_sql(
        "INSERT INTO happyview_users (id, did, is_super, created_at) VALUES (?, ?, 0, ?)",
        s.db_backend,
    ))
    .bind(&user_id)
    .bind(did)
    .bind(&now)
    .execute(&s.db)
    .await
    .unwrap();
    for permission in permissions {
        happyview::db::query(&happyview::db::adapt_sql(
            "INSERT INTO happyview_user_permissions (user_id, permission, granted_at) VALUES (?, ?, ?)",
            s.db_backend,
        ))
        .bind(&user_id)
        .bind(*permission)
        .bind(&now)
        .execute(&s.db)
        .await
        .unwrap();
    }
}

/// On a route the hosting platform's key may also call, a ticket is still
/// its user, with that user's permissions.
#[tokio::test]
#[serial]
async fn a_ticket_on_a_platform_route_is_its_user() {
    let issuer = Issuer::new();
    let app = app_trusting(&issuer).await;
    add_user(&app, "did:plc:viewer", &[]).await;
    add_user(&app, "did:plc:metrics", &["metrics:read"]).await;

    let req = issuer
        .request(
            "did:plc:viewer",
            "GET",
            "/admin/metrics/scripts",
            b"",
            b"",
            ORIGIN,
        )
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, req).await, StatusCode::FORBIDDEN);

    let req = issuer
        .request(
            "did:plc:metrics",
            "GET",
            "/admin/metrics/scripts",
            b"",
            b"",
            ORIGIN,
        )
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, req).await, StatusCode::OK);
}

/// A proof already used is refused on its headers: replaying it with a large
/// body gets the replay's 401, not the 413 that reading the body would.
#[tokio::test]
#[serial]
async fn a_replayed_proof_is_refused_before_its_body_is_read() {
    let issuer = Issuer::new();
    let app = app_trusting(&issuer).await;
    let body = serde_json::to_vec(&json!({ "value": "Once" })).unwrap();
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
    let huge = vec![b' '; happyview::auth::ticket::MAX_BODY_BYTES + 1];
    let again = resend_with(&req, &huge);
    assert!(send(&app, req).await.is_success());
    assert_eq!(send(&app, again).await, StatusCode::UNAUTHORIZED);
    let refused = events(&app, "ticket.refused").await;
    assert_eq!(refused.last().unwrap().0["step"], "replay");
}

/// Repeating a refused request adds one event row per ticket and step, not
/// one per attempt.
#[tokio::test]
#[serial]
async fn a_refusal_is_logged_once_per_ticket_and_step() {
    let issuer = Issuer::new();
    let app = app_trusting(&issuer).await;
    let (ticket, jti) = issuer.ticket_and_jti(&app.admin_did);
    let before = events(&app, "ticket.refused").await.len();
    for _ in 0..3 {
        let req = issuer
            .request_with(
                &ticket,
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
    let refused = events(&app, "ticket.refused").await;
    assert_eq!(refused.len(), before + 1);
    assert_eq!(refused.last().unwrap().0["ticket"]["jti"], jti.as_str());

    // Another step for the same ticket is its own row.
    let body = serde_json::to_vec(&json!({ "value": "x" })).unwrap();
    let other = serde_json::to_vec(&json!({ "value": "y" })).unwrap();
    for _ in 0..2 {
        let req = issuer
            .request_with(
                &ticket,
                "PUT",
                "/admin/settings/app_name",
                &other,
                &body,
                ORIGIN,
            )
            .body(Body::from(other.clone()))
            .unwrap();
        assert_eq!(send(&app, req).await, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(events(&app, "ticket.refused").await.len(), before + 2);

    // A new ticket starts its own count.
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
    assert_eq!(events(&app, "ticket.refused").await.len(), before + 3);
}

//! The admin API accepts an app's DPoP session only when the authorization
//! server granted it `dev.happyview.adminFull`, read from a token response
//! HappyView received itself, and a service-auth JWT only when it is bound to
//! the method that set grants. The built-in hpy client is seeded at boot.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use happyview::auth::{admin_scope, cli_client};
use happyview::oauth::pds_write::generate_dpop_proof;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ADMIN_GRANT: &str = "atproto rpc:dev.happyview.admin.manage?aud=*";

async fn response_json(resp: axum::http::Response<Body>) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or(json!(null))
}

fn post_json(uri: &str, body: &Value, headers: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("host", "127.0.0.1");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

/// An authorization server for `did`, listed by the DID's PDS when `listed`.
/// Its token endpoint answers each refresh with the next of `responses`,
/// repeating the last.
async fn authorization_server(
    app: &common::app::TestApp,
    did: &str,
    listed: bool,
    responses: &[Value],
) -> MockServer {
    let server = MockServer::start().await;
    let issuer = server.uri();
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": issuer,
            "token_endpoint": format!("{issuer}/oauth/token"),
        })))
        .mount(&server)
        .await;
    let (last, first) = responses.split_last().unwrap();
    for response in first {
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .up_to_n_times(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(last))
        .mount(&server)
        .await;

    app.mock_session_verification(did, did).await;
    let named = if listed {
        issuer.clone()
    } else {
        "https://elsewhere.example".to_string()
    };
    Mock::given(method("GET"))
        .and(path(format!(
            "/pds/{did}/.well-known/oauth-protected-resource"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "authorization_servers": [named] })),
        )
        .mount(&app.mock_server)
        .await;
    server
}

fn token_response(did: &str, scope: &str, expires_in: u64) -> Value {
    json!({
        "access_token": "refreshed-access-token",
        "refresh_token": "refreshed-refresh-token",
        "token_type": "DPoP",
        "expires_in": expires_in,
        "sub": did,
        "scope": scope,
    })
}

async fn token_requests(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/oauth/token")
        .count()
}

/// The client an app signs in through: its key, and the Origin it sends, if any.
struct Client {
    key: String,
    origin: Option<String>,
}

impl Client {
    fn hpy() -> Self {
        Self {
            key: cli_client::CLIENT_KEY.to_string(),
            origin: None,
        }
    }

    fn headers(&self) -> Vec<(&str, &str)> {
        let mut headers = vec![("x-client-key", self.key.as_str())];
        if let Some(origin) = &self.origin {
            headers.push(("origin", origin.as_str()));
        }
        headers
    }
}

/// Signs in through the built-in client as hpy does: a DPoP key provisioned
/// with PKCE and no Origin, then a session registered with the verifier and
/// whatever scope the app claims. Returns the provision id and the DPoP key.
async fn sign_in(
    app: &common::app::TestApp,
    did: &str,
    issuer: &str,
    claimed_scope: &str,
    access_token: &str,
) -> (String, Value) {
    sign_in_as(
        app,
        &Client::hpy(),
        did,
        issuer,
        claimed_scope,
        access_token,
    )
    .await
}

/// [`sign_in`] through any public client.
async fn sign_in_as(
    app: &common::app::TestApp,
    client: &Client,
    did: &str,
    issuer: &str,
    claimed_scope: &str,
    access_token: &str,
) -> (String, Value) {
    let verifier = "hpy-test-verifier-0123456789abcdef0123456789";
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let resp = app
        .router
        .clone()
        .oneshot(post_json(
            "/oauth/dpop-keys",
            &json!({ "pkce_challenge": challenge }),
            &client.headers(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = response_json(resp).await;

    let resp = app
        .router
        .clone()
        .oneshot(post_json(
            "/oauth/sessions",
            &json!({
                "provision_id": body["provision_id"],
                "pkce_verifier": verifier,
                "did": did,
                "access_token": access_token,
                "refresh_token": "registered-refresh-token",
                "expires_at": (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
                "scopes": claimed_scope,
                "issuer": issuer,
            }),
            &client.headers(),
        ))
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(status, StatusCode::CREATED, "{}", response_json(resp).await);
    (
        body["provision_id"].as_str().unwrap().to_string(),
        body["dpop_key"].clone(),
    )
}

async fn dpop_get(
    app: &common::app::TestApp,
    path: &str,
    dpop_key: &Value,
    access_token: &str,
) -> (StatusCode, Value) {
    dpop_get_as(app, &Client::hpy(), path, dpop_key, access_token).await
}

async fn dpop_get_as(
    app: &common::app::TestApp,
    client: &Client,
    path: &str,
    dpop_key: &Value,
    access_token: &str,
) -> (StatusCode, Value) {
    let url = format!("http://127.0.0.1{path}");
    let proof = generate_dpop_proof(dpop_key, "GET", &url, access_token, None).unwrap();
    let req = Request::builder()
        .method("GET")
        .uri(path)
        .header("host", "127.0.0.1")
        .header("x-client-key", &client.key)
        .header("authorization", format!("DPoP {access_token}"))
        .header("dpop", proof)
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    (status, response_json(resp).await)
}

async fn seeded_app() -> common::app::TestApp {
    let app = common::app::TestApp::new_with_encryption().await;
    cli_client::seed(&app.state.db, app.state.db_backend)
        .await
        .unwrap();
    app
}

#[tokio::test]
#[serial]
async fn a_granted_session_reaches_admin_with_the_users_permissions() {
    common::require_db!();
    let app = seeded_app().await;
    let did = app.admin_did.clone();
    let server =
        authorization_server(&app, &did, true, &[token_response(&did, ADMIN_GRANT, 3600)]).await;
    let (_, key) = sign_in(&app, &did, &server.uri(), ADMIN_GRANT, "app-token").await;

    // The admin is a super user, and the grant carries all of it.
    for path in ["/admin/plugins", "/admin/users"] {
        let (status, body) = dpop_get(&app, path, &key, "app-token").await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
    }
    // Read once and reused while the token it came with is live.
    assert_eq!(token_requests(&server).await, 1);
}

#[tokio::test]
#[serial]
async fn the_scope_an_app_claims_grants_nothing() {
    common::require_db!();
    let app = seeded_app().await;
    let did = app.admin_did.clone();
    let server =
        authorization_server(&app, &did, true, &[token_response(&did, "atproto", 3600)]).await;
    let (_, key) = sign_in(&app, &did, &server.uri(), ADMIN_GRANT, "app-token").await;

    let (status, body) = dpop_get(&app, "/admin/plugins", &key, "app-token").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

/// Nothing about the rule is particular to hpy: an ordinary registered app's
/// session reaches the admin API only with the grant its authorization server
/// reports, and `transition:generic` is not it.
#[tokio::test]
#[serial]
async fn any_apps_session_is_held_to_the_grant() {
    common::require_db!();
    let app = seeded_app().await;
    let did = app.admin_did.clone();
    let origin = "https://app.example".to_string();
    let (key, _, _) = app
        .create_api_client("public", Some(vec![origin.clone()]))
        .await;
    let client = Client {
        key,
        origin: Some(origin),
    };

    // One session per grant, in order: the server answers each one's first
    // refresh with the next response.
    let cases = [
        ("atproto transition:generic", StatusCode::FORBIDDEN),
        (ADMIN_GRANT, StatusCode::OK),
    ];
    let responses: Vec<Value> = cases
        .iter()
        .map(|(granted, _)| token_response(&did, granted, 3600))
        .collect();
    let server = authorization_server(&app, &did, true, &responses).await;
    for (i, (granted, expected)) in cases.into_iter().enumerate() {
        let token = format!("app-token-{i}");
        let (_, dpop_key) = sign_in_as(&app, &client, &did, &server.uri(), "atproto", &token).await;
        let (status, body) = dpop_get_as(&app, &client, "/admin/plugins", &dpop_key, &token).await;
        assert_eq!(status, expected, "granted {granted}: {body}");
    }
}

#[tokio::test]
#[serial]
async fn an_issuer_the_pds_does_not_name_is_never_asked() {
    common::require_db!();
    let app = seeded_app().await;
    let did = app.admin_did.clone();
    let server = authorization_server(
        &app,
        &did,
        false,
        &[token_response(&did, ADMIN_GRANT, 3600)],
    )
    .await;
    let (_, key) = sign_in(&app, &did, &server.uri(), ADMIN_GRANT, "app-token").await;

    let (status, body) = dpop_get(&app, "/admin/plugins", &key, "app-token").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(token_requests(&server).await, 0);
}

#[tokio::test]
#[serial]
async fn a_grant_for_another_account_counts_for_nothing() {
    common::require_db!();
    let app = seeded_app().await;
    let did = app.admin_did.clone();
    let server = authorization_server(
        &app,
        &did,
        true,
        &[token_response("did:plc:someoneelse", ADMIN_GRANT, 3600)],
    )
    .await;
    let (_, key) = sign_in(&app, &did, &server.uri(), ADMIN_GRANT, "app-token").await;

    let (status, body) = dpop_get(&app, "/admin/plugins", &key, "app-token").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
#[serial]
async fn an_expired_token_reads_the_grant_again() {
    common::require_db!();
    let app = seeded_app().await;
    let did = app.admin_did.clone();
    // The first refresh grants admin and expires at once; the next one finds
    // the grant gone.
    let server = authorization_server(
        &app,
        &did,
        true,
        &[
            token_response(&did, ADMIN_GRANT, 0),
            token_response(&did, "atproto", 3600),
        ],
    )
    .await;
    let (_, key) = sign_in(&app, &did, &server.uri(), ADMIN_GRANT, "app-token").await;

    let (status, body) = dpop_get(&app, "/admin/plugins", &key, "app-token").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = dpop_get(&app, "/admin/plugins", &key, "app-token").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(token_requests(&server).await, 2);
}

#[tokio::test]
#[serial]
async fn signing_in_again_forgets_the_previous_grant() {
    common::require_db!();
    let app = seeded_app().await;
    let did = app.admin_did.clone();
    let server = authorization_server(
        &app,
        &did,
        true,
        &[
            token_response(&did, ADMIN_GRANT, 3600),
            token_response(&did, "atproto", 3600),
        ],
    )
    .await;
    let (provision_id, key) = sign_in(&app, &did, &server.uri(), ADMIN_GRANT, "app-token").await;
    let (status, body) = dpop_get(&app, "/admin/plugins", &key, "app-token").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The same device registers fresh tokens: what was verified for the old
    // ones no longer stands.
    let resp = app
        .router
        .clone()
        .oneshot(post_json(
            "/oauth/sessions",
            &json!({
                "provision_id": provision_id,
                "pkce_verifier": "hpy-test-verifier-0123456789abcdef0123456789",
                "did": did,
                "access_token": "second-token",
                "refresh_token": "second-refresh-token",
                "expires_at": (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
                "scopes": ADMIN_GRANT,
                "issuer": server.uri(),
            }),
            &[("x-client-key", cli_client::CLIENT_KEY)],
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let (status, body) = dpop_get(&app, "/admin/plugins", &key, "second-token").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
#[serial]
async fn a_service_auth_token_must_name_the_admin_method() {
    common::require_db!();
    let mut app = common::app::TestApp::new().await;
    let plc_store = common::plc::setup_mock_plc(&app.mock_server).await;
    let instance = app.setup_did_web().await;
    let did = app.admin_did.clone();

    for (lxm, expected) in [
        (None, StatusCode::FORBIDDEN),
        (Some("com.example.other"), StatusCode::FORBIDDEN),
        (Some(admin_scope::ADMIN_LXM), StatusCode::OK),
    ] {
        let auth = app
            .service_auth_jwt_for(&plc_store, &did, &instance, "#happyview", lxm)
            .await;
        let req = Request::builder()
            .uri("/admin/plugins")
            .header("authorization", &auth)
            .header("host", "127.0.0.1:0")
            .body(Body::empty())
            .unwrap();
        let resp = app.router.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        assert_eq!(status, expected, "{lxm:?}: {}", response_json(resp).await);
    }
}

#[tokio::test]
#[serial]
async fn seeding_twice_restores_the_built_in_client() {
    common::require_db!();
    let app = common::app::TestApp::new().await;
    let db = &app.state.db;
    let backend = app.state.db_backend;

    cli_client::seed(db, backend).await.unwrap();
    happyview::db::query(&happyview::db::adapt_sql(
        "UPDATE happyview_api_clients SET is_active = 0, scopes = 'atproto' WHERE id = ?",
        backend,
    ))
    .bind(cli_client::CLIENT_ID)
    .execute(db)
    .await
    .unwrap();
    cli_client::seed(db, backend).await.unwrap();

    let row: (String, String, String, String, i32) = happyview::db::query_as(
        &happyview::db::adapt_sql(
            "SELECT client_key, client_id_url, client_type, scopes, is_active FROM happyview_api_clients WHERE id = ?",
            backend,
        ),
    )
    .bind(cli_client::CLIENT_ID)
    .fetch_one(db)
    .await
    .unwrap();
    assert_eq!(
        row,
        (
            cli_client::CLIENT_KEY.to_string(),
            cli_client::CLIENT_ID_URL.to_string(),
            "public".to_string(),
            format!("atproto include:{}", admin_scope::PERMISSION_SET),
            1
        )
    );
}

#[tokio::test]
#[serial]
async fn the_built_in_client_cannot_be_edited_or_deleted() {
    common::require_db!();
    let app = common::app::TestApp::new().await;
    cli_client::seed(&app.state.db, app.state.db_backend)
        .await
        .unwrap();
    let path = format!("/admin/api-clients/{}", cli_client::CLIENT_ID);

    let mut requests = vec![
        app.authed_request()
            .method("PUT")
            .uri(&path)
            .header("content-type", "application/json")
            .body(Body::from(json!({ "name": "renamed" }).to_string()))
            .unwrap(),
        app.authed_request()
            .method("DELETE")
            .uri(&path)
            .body(Body::empty())
            .unwrap(),
    ];
    // A key would make it a confidential client that no instance can sign for.
    for suffix in ["/auth-key", "/auth-key/rotate"] {
        requests.push(
            app.authed_request()
                .method("POST")
                .uri(format!("{path}{suffix}"))
                .body(Body::empty())
                .unwrap(),
        );
    }
    for req in requests {
        let label = format!("{} {}", req.method(), req.uri());
        let resp = app.router.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "{label}: {}",
            response_json(resp).await
        );
    }
}

#[tokio::test]
#[serial]
async fn config_names_the_cli_client_key() {
    common::require_db!();
    let app = common::app::TestApp::new().await;
    let body = app.get_json("/config").await;
    assert_eq!(body["cli_client_key"], cli_client::CLIENT_KEY);
}

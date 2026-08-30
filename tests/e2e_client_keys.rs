mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

fn admin_get(
    uri: &str,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(cookie.0, cookie.1)
        .body(Body::empty())
        .unwrap()
}

fn admin_post(
    uri: &str,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(cookie.0, cookie.1)
        .body(Body::empty())
        .unwrap()
}

fn sample_api_client_body() -> Value {
    json!({
        "name": "Test App",
        "client_id_url": "https://app.example.com/client-metadata.json",
        "client_uri": "https://app.example.com",
        "redirect_uris": ["https://happyview.example.com/auth/callback"],
        "scopes": "atproto"
    })
}

async fn create_api_client(app: &TestApp) -> String {
    let resp = app
        .router
        .clone()
        .oneshot(admin_post_json(
            "/admin/api-clients",
            app.admin_cookie(),
            &sample_api_client_body(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let created = json_body(resp).await;
    created["id"].as_str().unwrap().to_string()
}

fn admin_post_json(
    uri: &str,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
    body: &Value,
) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(cookie.0, cookie.1)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

fn admin_delete(
    uri: &str,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .header(cookie.0, cookie.1)
        .body(Body::empty())
        .unwrap()
}

/// No cookie: this is the request shape a stranger's PDS actually sends.
fn anon_get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

/// Create a confidential API client with a specific `client_id_url`, the way
/// a real app registers one. `create_api_client` hardcodes a placeholder
/// `client_id_url`; the assertion tests need to control it, since it becomes
/// the assertion's `iss`/`sub`.
///
/// Returns (client_id, client_key, client_secret).
async fn create_confidential_api_client(
    app: &TestApp,
    client_id_url: &str,
) -> (String, String, String) {
    let (client_key, client_secret, id) = app.create_api_client("confidential", None).await;

    let sql = happyview::db::adapt_sql(
        "UPDATE happyview_api_clients SET client_id_url = ? WHERE id = ?",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(client_id_url)
        .bind(&id)
        .execute(&app.state.db)
        .await
        .expect("failed to set client_id_url");

    (id, client_key, client_secret)
}

/// POST to `uri`, authenticated with a confidential client's own credentials
/// (`X-Client-Key` + `X-Client-Secret`) rather than the admin cookie.
async fn post_with_client_credentials(
    app: &TestApp,
    uri: &str,
    client_key: &str,
    client_secret: &str,
    body: Value,
) -> axum::response::Response {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-client-key", client_key)
        .header("x-client-secret", client_secret)
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    app.router.clone().oneshot(req).await.unwrap()
}

/// Decode one dot-separated segment of a compact JWS/JWT as JSON. Each file
/// under `tests/` compiles as its own binary, so `client_assertion.rs`'s
/// private `decode_part` test helper isn't reachable here — this is a local
/// equivalent.
fn decode_jwt_part(jwt: &str, index: usize) -> Value {
    let part = jwt.split('.').nth(index).unwrap();
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn provisioning_an_auth_key_is_idempotent() {
    common::require_db!();
    let app = TestApp::new().await;
    let client_id = create_api_client(&app).await;

    let first = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/api-clients/{client_id}/auth-key"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first_kid = json_body(first).await["kid"].as_str().unwrap().to_string();

    let second = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/api-clients/{client_id}/auth-key"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    let second_kid = json_body(second).await["kid"].as_str().unwrap().to_string();
    assert_eq!(second_kid, first_kid);
}

#[tokio::test]
#[serial]
async fn auth_key_response_names_the_jwks_uri() {
    common::require_db!();
    let app = TestApp::new().await;
    let client_id = create_api_client(&app).await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/api-clients/{client_id}/auth-key"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let uri = json_body(resp).await["jwks_uri"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(uri.ends_with(&format!("/oauth/clients/{client_id}/jwks.json")));
}

#[tokio::test]
#[serial]
async fn getting_an_unprovisioned_key_is_404() {
    common::require_db!();
    let app = TestApp::new().await;
    let client_id = create_api_client(&app).await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_get(
            &format!("/admin/api-clients/{client_id}/auth-key"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[serial]
async fn jwks_is_public_and_omits_private_material() {
    common::require_db!();
    let app = TestApp::new().await;
    let client_id = create_api_client(&app).await;

    let provisioned = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/api-clients/{client_id}/auth-key"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    let kid = json_body(provisioned).await["kid"]
        .as_str()
        .unwrap()
        .to_string();

    // No admin cookie: this endpoint is public key material.
    let resp = app
        .router
        .clone()
        .oneshot(anon_get(&format!("/oauth/clients/{client_id}/jwks.json")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = json_body(resp).await;
    let keys = body["keys"].as_array().unwrap().clone();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0]["kid"].as_str().unwrap(), kid);
    assert_eq!(keys[0]["alg"].as_str().unwrap(), "ES256");
    assert!(keys[0]["d"].is_null());
}

#[tokio::test]
#[serial]
async fn deleting_a_client_revokes_its_keys() {
    common::require_db!();
    let app = TestApp::new().await;
    let client_id = create_api_client(&app).await;

    app.router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/api-clients/{client_id}/auth-key"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();

    let delete_resp = app
        .router
        .clone()
        .oneshot(admin_delete(
            &format!("/admin/api-clients/{client_id}"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(delete_resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .router
        .clone()
        .oneshot(anon_get(&format!("/oauth/clients/{client_id}/jwks.json")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json_body(resp).await["keys"].as_array().unwrap().len(), 0);
}

#[tokio::test]
#[serial]
async fn jwks_for_an_unknown_client_is_an_empty_set() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(anon_get("/oauth/clients/does-not-exist/jwks.json"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json_body(resp).await["keys"].as_array().unwrap().len(), 0);
}

#[tokio::test]
#[serial]
async fn assertion_endpoint_signs_for_the_authenticated_client() {
    common::require_db!();
    let app = TestApp::new().await;
    let client_id_url = "https://app.example.com/client-metadata.json";
    let (client_id, client_key, client_secret) =
        create_confidential_api_client(&app, client_id_url).await;

    let provisioned = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/api-clients/{client_id}/auth-key"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    let provisioned_kid = json_body(provisioned).await["kid"]
        .as_str()
        .unwrap()
        .to_string();

    let resp = post_with_client_credentials(
        &app,
        "/oauth/client-assertion",
        &client_key,
        &client_secret,
        json!({ "issuer": "https://pds.example.com" }),
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(
        body["client_assertion_type"].as_str().unwrap(),
        "urn:ietf:params:oauth:client-assertion-type:jwt-bearer"
    );
    assert_eq!(body["expires_in"].as_u64().unwrap(), 60);
    let jwt = body["client_assertion"].as_str().unwrap().to_string();
    assert_eq!(jwt.split('.').count(), 3);

    // Decode and check what mint_client_assertion actually wired together —
    // the unit tests in client_assertion.rs already prove `build()` is
    // correct given correct inputs, but only this endpoint assembles those
    // inputs (the `Current` key via `load_keys`, the `iss`/`sub` value via
    // `lookup_client_id_url`), and nothing else observes the result.
    let header = decode_jwt_part(&jwt, 0);
    assert_eq!(
        header["kid"].as_str().unwrap(),
        provisioned_kid,
        "assertion must be signed with this client's own key, not another client's"
    );

    let claims = decode_jwt_part(&jwt, 1);
    assert_eq!(
        claims["iss"].as_str().unwrap(),
        client_id_url,
        "iss must be the client's published client_id_url, not its internal db id"
    );
    assert_eq!(
        claims["sub"].as_str().unwrap(),
        client_id_url,
        "sub must be the client's published client_id_url, not its internal db id"
    );
    assert_eq!(claims["aud"].as_str().unwrap(), "https://pds.example.com");
    assert!(claims["jti"].as_str().is_some_and(|s| !s.is_empty()));
    let iat = claims["iat"].as_u64().unwrap();
    let exp = claims["exp"].as_u64().unwrap();
    assert_eq!(exp - iat, 60);
}

#[tokio::test]
#[serial]
async fn revoking_all_auth_keys_empties_the_jwks_and_un_delegates_the_client() {
    common::require_db!();
    let app = TestApp::new().await;
    let client_id_url = "https://undelegate.example.com/oauth-client-metadata.json";
    let (client_id, client_key, client_secret) =
        create_confidential_api_client(&app, client_id_url).await;

    app.router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/api-clients/{client_id}/auth-key"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    app.router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/api-clients/{client_id}/auth-key/rotate"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();

    // Two live keys now: one current, one retiring. Revoking all takes both,
    // including `current` — which the single-key endpoint refuses.
    let resp = app
        .router
        .clone()
        .oneshot(admin_delete(
            &format!("/admin/api-clients/{client_id}/auth-keys"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(
        body["revoked"].as_u64().unwrap(),
        2,
        "both the current and the retiring key must be revoked"
    );

    // The JWKS must now be empty — that is what "un-delegated" means.
    let jwks = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/oauth/clients/{client_id}/jwks.json"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let published = json_body(jwks).await;
    assert_eq!(
        published["keys"].as_array().unwrap().len(),
        0,
        "revoking every key must leave an empty JWKS, got {published}"
    );

    // And the client can no longer mint assertions.
    let resp = post_with_client_credentials(
        &app,
        "/oauth/client-assertion",
        &client_key,
        &client_secret,
        json!({ "issuer": "https://pds.example.com" }),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "a client with no keys must not be able to mint an assertion"
    );

    // Revoking again is a 400, not a silent success on an empty set.
    let again = app
        .router
        .clone()
        .oneshot(admin_delete(
            &format!("/admin/api-clients/{client_id}/auth-keys"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[serial]
async fn a_retiring_client_auth_key_can_be_revoked_but_the_current_one_cannot() {
    common::require_db!();
    let app = TestApp::new().await;
    let client_id_url = "https://revoke.example.com/oauth-client-metadata.json";
    let (client_id, _client_key, _client_secret) =
        create_confidential_api_client(&app, client_id_url).await;

    let provisioned = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/api-clients/{client_id}/auth-key"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    let old_kid = json_body(provisioned).await["kid"]
        .as_str()
        .unwrap()
        .to_string();

    let rotated = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/api-clients/{client_id}/auth-key/rotate"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    let new_kid = json_body(rotated).await["kid"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(old_kid, new_kid);

    // The listing must expose the retiring key — an operator cannot revoke a
    // leaked key they cannot see.
    let listed = app
        .router
        .clone()
        .oneshot(admin_get(
            &format!("/admin/api-clients/{client_id}/auth-keys"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let body = json_body(listed).await;
    let statuses: Vec<(String, String)> = body["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| {
            (
                k["kid"].as_str().unwrap().to_string(),
                k["status"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(statuses.contains(&(new_kid.clone(), "current".to_string())));
    assert!(statuses.contains(&(old_kid.clone(), "retiring".to_string())));

    // Revoking the current key must be refused — it would leave the client
    // unable to authenticate at all.
    let refused = app
        .router
        .clone()
        .oneshot(admin_delete(
            &format!("/admin/api-clients/{client_id}/auth-key/{new_kid}"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(
        refused.status(),
        StatusCode::BAD_REQUEST,
        "revoking the current key must be refused, not silently accepted"
    );

    // An unknown kid is a 404, not a silent success.
    let unknown = app
        .router
        .clone()
        .oneshot(admin_delete(
            &format!("/admin/api-clients/{client_id}/auth-key/not-a-real-kid"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

    // The retiring key can be revoked.
    let revoked = app
        .router
        .clone()
        .oneshot(admin_delete(
            &format!("/admin/api-clients/{client_id}/auth-key/{old_kid}"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::OK);
    let body = json_body(revoked).await;
    assert_eq!(body["kid"].as_str().unwrap(), old_kid);
    assert!(body["sessions_destroyed"].as_u64().is_some());

    // And it leaves the published JWKS immediately — that is what revoking
    // is for. Assert the end state, not just the DB row.
    let jwks = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/oauth/clients/{client_id}/jwks.json"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let published: Vec<String> = json_body(jwks).await["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["kid"].as_str().unwrap().to_string())
        .collect();
    assert!(
        !published.contains(&old_kid),
        "a revoked key must disappear from the published JWKS, got {published:?}"
    );
    assert!(published.contains(&new_kid));
}

#[tokio::test]
#[serial]
async fn assertion_endpoint_signs_with_an_explicitly_requested_kid() {
    common::require_db!();
    let app = TestApp::new().await;
    let client_id_url = "https://kidpick.example.com/oauth-client-metadata.json";
    let (client_id, client_key, client_secret) =
        create_confidential_api_client(&app, client_id_url).await;

    let provisioned = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/api-clients/{client_id}/auth-key"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    let old_kid = json_body(provisioned).await["kid"]
        .as_str()
        .unwrap()
        .to_string();

    let rotated = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/api-clients/{client_id}/auth-key/rotate"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(rotated.status(), StatusCode::OK);
    let new_kid = json_body(rotated).await["kid"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(old_kid, new_kid, "rotation must mint a distinct key");

    // No kid: whatever is current. Correct for an initial authorization.
    let resp = post_with_client_credentials(
        &app,
        "/oauth/client-assertion",
        &client_key,
        &client_secret,
        json!({ "issuer": "https://pds.example.com" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(
        decode_jwt_part(body["client_assertion"].as_str().unwrap(), 0)["kid"]
            .as_str()
            .unwrap(),
        new_kid
    );
    assert_eq!(
        body["kid"].as_str().unwrap(),
        new_kid,
        "the response must name the signing key, or a caller has no way to ask for it again"
    );

    // Explicit retiring kid: a refresh for a session established before the
    // rotation. Signing that with the current key would present a kid the
    // authorization server never bound the session to, and a conforming
    // server destroys the session rather than refusing the request.
    let resp = post_with_client_credentials(
        &app,
        "/oauth/client-assertion",
        &client_key,
        &client_secret,
        json!({ "issuer": "https://pds.example.com", "kid": old_kid }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(
        decode_jwt_part(body["client_assertion"].as_str().unwrap(), 0)["kid"]
            .as_str()
            .unwrap(),
        old_kid,
        "an explicitly requested kid must sign the assertion, not the current key"
    );
    assert_eq!(body["kid"].as_str().unwrap(), old_kid);

    // An unknown or revoked kid must fail loudly rather than silently
    // falling back to the current key.
    let resp = post_with_client_credentials(
        &app,
        "/oauth/client-assertion",
        &client_key,
        &client_secret,
        json!({ "issuer": "https://pds.example.com", "kid": "not-a-real-kid" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[serial]
async fn assertion_endpoint_refuses_a_client_with_no_key() {
    common::require_db!();
    let app = TestApp::new().await;
    let (_id, client_key, client_secret) =
        create_confidential_api_client(&app, "https://app.example.com/client-metadata.json").await;

    let resp = post_with_client_credentials(
        &app,
        "/oauth/client-assertion",
        &client_key,
        &client_secret,
        json!({ "issuer": "https://pds.example.com" }),
    )
    .await;

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[serial]
async fn recheck_reports_not_confidential_for_an_unpublished_document() {
    common::require_db!();
    let app = TestApp::new().await;
    let client_id_url =
        "https://client-that-does-not-exist.e2e-client-keys-test.invalid/client-metadata.json";
    let (client_id, _client_key, _client_secret) =
        create_confidential_api_client(&app, client_id_url).await;

    app.router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/api-clients/{client_id}/auth-key"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();

    // The client's document lives at an address that does not resolve, so the
    // probe cannot possibly find `private_key_jwt` there. This must come back
    // as a normal 200 verdict, not a 500 — an unreachable third-party server
    // is not a HappyView error.
    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/api-clients/{client_id}/auth-key/recheck"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = json_body(resp).await;
    assert_eq!(body["confidential"].as_bool(), Some(false));
    assert!(
        !body["reason"].as_str().unwrap_or_default().is_empty(),
        "reason must explain why the verdict is not confidential"
    );
    assert!(body["checked_at"].as_str().is_some());
}

#[tokio::test]
#[serial]
async fn assertion_still_mints_when_the_probe_fails() {
    common::require_db!();
    let app = TestApp::new().await;
    let client_id_url =
        "https://client-that-does-not-exist.e2e-client-keys-test.invalid/client-metadata.json";
    let (client_id, client_key, client_secret) =
        create_confidential_api_client(&app, client_id_url).await;

    app.router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/api-clients/{client_id}/auth-key"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();

    // mint_client_assertion re-probes the client's confidentiality on the way
    // in. That probe cannot reach this client's (deliberately unreachable)
    // metadata document — the probe informs registration, it does not gate
    // signing, so a client that holds a key must still get an assertion.
    let resp = post_with_client_credentials(
        &app,
        "/oauth/client-assertion",
        &client_key,
        &client_secret,
        json!({ "issuer": "https://pds.example.com" }),
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert!(body["client_assertion"].as_str().is_some());
}

#[tokio::test]
#[serial]
async fn recheck_reports_public_for_a_loopback_client_even_when_its_document_declares_confidential()
{
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};

    common::require_db!();
    let app = TestApp::new().await;

    let client_id_url = format!(
        "{}/recheck-transition/client-metadata.json",
        app.mock_server.uri()
    );

    let create_body = json!({
        "name": "Recheck Transition Test",
        "client_id_url": client_id_url,
        "client_uri": "https://app.example.com",
        "redirect_uris": ["https://happyview.example.com/auth/callback"],
        "scopes": "atproto",
    });
    let created = app
        .router
        .clone()
        .oneshot(admin_post_json(
            "/admin/api-clients",
            app.admin_cookie(),
            &create_body,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let client_id = json_body(created).await["id"].as_str().unwrap().to_string();

    assert_eq!(
        app.state
            .oauth
            .get(&client_id_url)
            .expect("client registered at creation")
            .client_metadata
            .token_endpoint_auth_method,
        Some("none".to_string()),
        "sanity check: a freshly created client registers public"
    );

    let provisioned = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/api-clients/{client_id}/auth-key"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(provisioned.status(), StatusCode::OK);
    let jwks_uri = json_body(provisioned).await["jwks_uri"]
        .as_str()
        .unwrap()
        .to_string();

    Mock::given(method("GET"))
        .and(path("/recheck-transition/client-metadata.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token_endpoint_auth_method": "private_key_jwt",
            "jwks_uri": jwks_uri,
        })))
        .mount(&app.mock_server)
        .await;

    let recheck = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/api-clients/{client_id}/auth-key/recheck"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();
    assert_eq!(recheck.status(), StatusCode::OK);
    let body = json_body(recheck).await;

    let raw_probe = happyview::oauth::client_probe::cached(&app.state, &client_id, &client_id_url)
        .await
        .unwrap();
    assert!(
        raw_probe.confidential,
        "the probe must have genuinely evaluated the document as confidential"
    );
    assert_eq!(
        raw_probe.reason,
        "published document declares private_key_jwt against this instance"
    );
    assert_eq!(
        body["confidential"].as_bool(),
        Some(false),
        "a loopback client_id_url can never be registered confidential, \
         no matter what its document says"
    );
    assert_eq!(
        body["reason"].as_str(),
        Some(
            "this app's client_id_url is a loopback address (localhost/127.0.0.1). \
             Loopback clients always register as public OAuth clients — no published \
             document can make one confidential — so this is expected and there is \
             nothing to fix in the document."
        ),
        "reason must explain the registration-side constraint, not the document, \
         since the document already passed every check"
    );
    assert_ne!(
        body["reason"].as_str(),
        Some("published document declares private_key_jwt against this instance"),
        "reason must never go back to describing the document when confidential=false \
         contradicts what the document says"
    );

    assert_eq!(
        app.state
            .oauth
            .get(&client_id_url)
            .expect("client still registered")
            .client_metadata
            .token_endpoint_auth_method,
        Some("none".to_string()),
        "the registry must stay public for a loopback client_id_url"
    );
}

#[tokio::test]
#[serial]
async fn assertion_endpoint_rejects_bad_credentials() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = post_with_client_credentials(
        &app,
        "/oauth/client-assertion",
        "hvc_nonexistent",
        "wrong",
        json!({ "issuer": "https://pds.example.com" }),
    )
    .await;

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

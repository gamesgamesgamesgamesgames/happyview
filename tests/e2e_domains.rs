mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
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
    body: &Value,
) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
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
        .method(Method::DELETE)
        .uri(uri)
        .header(cookie.0, cookie.1)
        .body(Body::empty())
        .unwrap()
}

fn get_with_host(uri: &str, host: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("host", host)
        .body(Body::empty())
        .unwrap()
}

async fn seed_domain(app: &TestApp, id: &str, url: &str, is_primary: bool) {
    let now = happyview::db::now_rfc3339();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_domains (id, url, is_primary, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(id)
        .bind(url)
        .bind(if is_primary { 1i32 } else { 0i32 })
        .bind(&now)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .unwrap();
    app.state
        .domain_cache
        .insert(happyview::domain::Domain {
            id: id.into(),
            url: url.into(),
            is_primary,
            created_at: now.clone(),
            updated_at: now,
        })
        .await;
}

// ---------------------------------------------------------------------------
// Domains tests
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn domains_list_returns_seeded_domain() {
    common::require_db!();
    let app = TestApp::new().await;

    seed_domain(&app, "primary-id", "http://127.0.0.1:0", true).await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/domains", app.admin_cookie()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = json_body(resp).await;
    let domains = json.as_array().expect("expected array");
    assert_eq!(domains.len(), 1, "expected 1 domain, got {}", domains.len());
    assert_eq!(domains[0]["url"], "http://127.0.0.1:0");
    assert_eq!(domains[0]["is_primary"], true);
}

#[tokio::test]
#[serial]
async fn domains_create_and_delete() {
    common::require_db!();
    let app = TestApp::new().await;

    seed_domain(&app, "primary-id", "http://127.0.0.1:0", true).await;

    // Create a new domain
    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/domains",
            app.admin_cookie(),
            &json!({ "url": "http://127.0.0.1:9999" }),
        ))
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "expected 201 on create, got {}",
        resp.status()
    );
    let json = json_body(resp).await;
    assert!(
        json["id"].is_string(),
        "expected id in response, got {:?}",
        json
    );
    assert_eq!(json["url"], "http://127.0.0.1:9999");
    assert_eq!(json["is_primary"], false);

    let new_id = json["id"].as_str().unwrap().to_string();

    // Delete the newly created domain
    let resp = app
        .router
        .clone()
        .oneshot(admin_delete(
            &format!("/admin/domains/{new_id}"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "expected 204 on delete, got {}",
        resp.status()
    );
}

#[tokio::test]
#[serial]
async fn domains_duplicate_url_returns_400() {
    common::require_db!();
    let app = TestApp::new().await;

    seed_domain(&app, "primary-id", "http://127.0.0.1:0", true).await;

    // Attempt to create a domain with the same URL
    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/domains",
            app.admin_cookie(),
            &json!({ "url": "http://127.0.0.1:0" }),
        ))
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "expected 400 on duplicate URL, got {}",
        resp.status()
    );
}

#[tokio::test]
#[serial]
async fn domains_cannot_delete_primary() {
    common::require_db!();
    let app = TestApp::new().await;

    seed_domain(&app, "primary-id", "http://127.0.0.1:0", true).await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_delete(
            "/admin/domains/primary-id",
            app.admin_cookie(),
        ))
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "expected 400 when deleting primary domain, got {}",
        resp.status()
    );
}

#[tokio::test]
#[serial]
async fn domains_set_primary() {
    common::require_db!();
    let app = TestApp::new().await;

    seed_domain(&app, "id-a", "http://127.0.0.1:0", true).await;
    seed_domain(&app, "id-b", "http://127.0.0.1:9999", false).await;

    // Set domain b as primary
    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/domains/id-b/primary",
            app.admin_cookie(),
            &json!({}),
        ))
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "expected 204 on set primary, got {}",
        resp.status()
    );

    // Verify domain b is now primary
    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/domains", app.admin_cookie()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = json_body(resp).await;
    let domains = json.as_array().expect("expected array");
    let domain_b = domains
        .iter()
        .find(|d| d["id"] == "id-b")
        .expect("domain b not found");
    assert_eq!(
        domain_b["is_primary"], true,
        "expected domain b to be primary"
    );
}

#[tokio::test]
#[serial]
async fn unknown_host_returns_421_on_domain_scoped_routes() {
    common::require_db!();
    let app = TestApp::new().await;

    // No domains seeded — cache is empty
    let resp = app
        .router
        .clone()
        .oneshot(get_with_host("/config", "unknown.example.com"))
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::MISDIRECTED_REQUEST,
        "expected 421 for unknown host, got {}",
        resp.status()
    );
}

#[tokio::test]
#[serial]
async fn health_check_bypasses_domain_resolution() {
    common::require_db!();
    let app = TestApp::new().await;

    // No domains seeded — cache is empty
    let resp = app
        .router
        .clone()
        .oneshot(get_with_host("/health", "unknown.example.com"))
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 on /health regardless of host, got {}",
        resp.status()
    );
}

#[tokio::test]
#[serial]
async fn domain_scoped_route_works_with_known_host() {
    common::require_db!();
    let app = TestApp::new().await;

    // Domain.host() for "http://localhost:3000" is "localhost:3000"
    seed_domain(&app, "local-id", "http://localhost:3000", true).await;

    let resp = app
        .router
        .clone()
        .oneshot(get_with_host("/config", "localhost:3000"))
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 on /config with known host, got {}",
        resp.status()
    );

    let json = json_body(resp).await;
    assert_eq!(
        json["public_url"], "http://localhost:3000",
        "expected public_url to match domain URL, got {:?}",
        json["public_url"]
    );
}

#[tokio::test]
#[serial]
async fn set_primary_carries_the_promoted_clients_signing_kid() {
    common::require_db!();
    let mut app = TestApp::new().await;

    let instance_key = happyview::oauth::client_keys::ensure_instance_key(
        &app.state.db,
        app.state.db_backend,
        None,
    )
    .await
    .expect("ensure instance key");
    let jwk =
        happyview::oauth::client_keys::to_atrium_jwk(&instance_key).expect("convert to atrium jwk");
    app.state.client_jwks = vec![jwk];
    app.rebuild_router();

    seed_domain(&app, "primary-id", "http://127.0.0.1:0", true).await;

    let domain_url = "https://promote.e2e-domains-test.invalid";
    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/domains",
            app.admin_cookie(),
            &json!({ "url": domain_url }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let domain_id = created["id"].as_str().expect("created domain id");

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            &format!("/admin/domains/{domain_id}/primary"),
            app.admin_cookie(),
            &json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "expected 204 on set primary, got {}",
        resp.status()
    );

    let (_client, kid) = app.state.oauth.primary_client_and_kid();
    assert_eq!(
        kid.as_deref(),
        Some(instance_key.kid.as_str()),
        "promoting a domain must carry the kid its client signs with. Recording None here \
         makes the login callback write NULL over every session's pin, and a later rotation \
         then re-pins those sessions to a key that never established them — which a strict \
         authorization server answers by destroying them"
    );
}

#[tokio::test]
#[serial]
async fn domain_create_registers_domain_specific_client_not_primary() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let key = happyview::oauth::client_keys::generate_client_key("test-owner")
        .expect("generate client key");
    let jwk = happyview::oauth::client_keys::to_atrium_jwk(&key).expect("convert to atrium jwk");
    app.state.client_jwks = vec![jwk];
    app.rebuild_router();

    seed_domain(&app, "primary-id", "http://127.0.0.1:0", true).await;

    let domain_url = "https://confidential.e2e-domains-test.invalid";
    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/domains",
            app.admin_cookie(),
            &json!({ "url": domain_url }),
        ))
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "expected 201 on create, got {}",
        resp.status()
    );

    let domain_client = app.state.oauth.get_for_domain(domain_url);
    let primary_client = app.state.oauth.primary_client();

    assert!(
        !std::sync::Arc::ptr_eq(&domain_client, &primary_client),
        "domain client fell back to the primary client instead of building its own"
    );
    assert_eq!(
        domain_client.client_metadata.token_endpoint_auth_method,
        Some("private_key_jwt".to_string()),
        "expected the domain's own confidential client, got metadata {:?}",
        domain_client.client_metadata
    );
}

#[tokio::test]
#[serial]
async fn domain_create_fails_and_rolls_back_on_client_id_collision() {
    common::require_db!();
    let app = TestApp::new().await;

    seed_domain(&app, "primary-id", "http://127.0.0.1:0", true).await;

    let colliding_url = "https://colliding.e2e-domains-test.invalid";
    let client_id_url = format!(
        "{}/oauth-client-metadata.json",
        app.state
            .config
            .url_with_base_path(colliding_url)
            .trim_end_matches('/')
    );

    app.state.oauth.register_domain_client(
        colliding_url.to_string(),
        client_id_url.clone(),
        app.state.oauth.primary_client(),
        None,
    );

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/domains",
            app.admin_cookie(),
            &json!({ "url": colliding_url }),
        ))
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "expected 500 when the OAuth client collides, got {}",
        resp.status()
    );

    // No row was left behind.
    let row: Option<(String,)> = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT id FROM happyview_domains WHERE url = ?",
        app.state.db_backend,
    ))
    .bind(colliding_url)
    .fetch_optional(&app.state.db)
    .await
    .unwrap();
    assert!(
        row.is_none(),
        "expected the failed domain's row to be rolled back, found {:?}",
        row
    );

    // No cache entry was left behind either.
    let host = "colliding.e2e-domains-test.invalid";
    assert!(
        app.state.domain_cache.get(host).await.is_none(),
        "expected no domain_cache entry for a domain whose client failed to register"
    );

    assert!(
        std::sync::Arc::ptr_eq(
            &app.state.oauth.get_domain_client(colliding_url).unwrap(),
            &app.state.oauth.primary_client()
        ),
        "the pre-existing domain client registration should be unaffected by the failed create"
    );
}

#[tokio::test]
#[serial]
async fn domain_create_compensates_registry_on_commit_failure() {
    common::require_db!();
    let mut app = TestApp::new().await;

    if app.state.db_backend != happyview::db::DatabaseBackend::Postgres {
        eprintln!("skipped (commit-failure injection requires a real Postgres COMMIT)");
        return;
    }

    let key = happyview::oauth::client_keys::generate_client_key("test-owner")
        .expect("generate client key");
    let jwk = happyview::oauth::client_keys::to_atrium_jwk(&key).expect("convert to atrium jwk");
    app.state.client_jwks = vec![jwk];
    app.rebuild_router();

    seed_domain(&app, "primary-id", "http://127.0.0.1:0", true).await;

    happyview::db::query("DROP TRIGGER IF EXISTS test17_fail_commit_trigger ON happyview_domains")
        .execute(&app.state.db)
        .await
        .unwrap();
    happyview::db::query(
        "CREATE OR REPLACE FUNCTION test17_fail_commit() RETURNS trigger AS $$
         BEGIN
             IF NEW.url = 'https://forced-commit-failure.e2e-domains-test.invalid' THEN
                 RAISE EXCEPTION 'test17: forced commit failure';
             END IF;
             RETURN NEW;
         END;
         $$ LANGUAGE plpgsql",
    )
    .execute(&app.state.db)
    .await
    .unwrap();
    happyview::db::query(
        "CREATE CONSTRAINT TRIGGER test17_fail_commit_trigger
         AFTER INSERT ON happyview_domains
         DEFERRABLE INITIALLY DEFERRED
         FOR EACH ROW EXECUTE FUNCTION test17_fail_commit()",
    )
    .execute(&app.state.db)
    .await
    .unwrap();

    let domain_url = "https://forced-commit-failure.e2e-domains-test.invalid";
    let client_id_url = format!(
        "{}/oauth-client-metadata.json",
        app.state
            .config
            .url_with_base_path(domain_url)
            .trim_end_matches('/')
    );

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/domains",
            app.admin_cookie(),
            &json!({ "url": domain_url }),
        ))
        .await
        .unwrap();

    happyview::db::query("DROP TRIGGER IF EXISTS test17_fail_commit_trigger ON happyview_domains")
        .execute(&app.state.db)
        .await
        .unwrap();
    happyview::db::query("DROP FUNCTION IF EXISTS test17_fail_commit()")
        .execute(&app.state.db)
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "expected 500 when the commit itself fails, got {}",
        resp.status()
    );

    let row: Option<(String,)> = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT id FROM happyview_domains WHERE url = ?",
        app.state.db_backend,
    ))
    .bind(domain_url)
    .fetch_optional(&app.state.db)
    .await
    .unwrap();
    assert!(
        row.is_none(),
        "expected no row after a forced commit failure, found {:?}",
        row
    );

    // No cache entry.
    let host = "forced-commit-failure.e2e-domains-test.invalid";
    assert!(
        app.state.domain_cache.get(host).await.is_none(),
        "expected no domain_cache entry after a forced commit failure"
    );

    assert!(
        app.state.oauth.get_domain_client(domain_url).is_none(),
        "expected the domain client registration to be undone after a commit failure"
    );
    assert!(
        app.state.oauth.get(&client_id_url).is_none(),
        "expected the client_id_url registry entry to be undone after a commit failure"
    );
}

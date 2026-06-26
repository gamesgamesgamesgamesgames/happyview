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
    sqlx::query(&sql)
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

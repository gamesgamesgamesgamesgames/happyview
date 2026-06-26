mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::db::{adapt_sql, now_rfc3339};
use http_body_util::BodyExt;
use serde_json::Value;
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;
use common::fixtures;

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

async fn seed_network_lexicon(app: &TestApp, nsid: &str, authority_did: &str) {
    let lexicon_json = fixtures::game_record_lexicon();
    let backend = app.state.db_backend;
    let sql = adapt_sql(
        r#"
        INSERT INTO happyview_lexicons (id, lexicon_json, backfill, source, authority_did, last_fetched_at, created_at)
        VALUES (?, ?, 0, 'network', ?, ?, ?)
        ON CONFLICT (id) DO NOTHING
        "#,
        backend,
    );
    let now = now_rfc3339();
    sqlx::query(&sql)
        .bind(nsid)
        .bind(serde_json::to_string(&lexicon_json).unwrap_or_default())
        .bind(authority_did)
        .bind(&now)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .expect("failed to seed network lexicon");
}

// ---------------------------------------------------------------------------
// Network lexicon CRUD
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn network_lexicon_list_empty() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/network-lexicons", app.admin_cookie()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = json_body(resp).await;
    assert!(json.as_array().unwrap().is_empty());
}

#[tokio::test]
#[serial]
async fn network_lexicon_list_returns_seeded() {
    common::require_db!();
    let app = TestApp::new().await;

    seed_network_lexicon(&app, "games.gamesgamesgamesgames.game", "did:plc:authority").await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_get("/admin/network-lexicons", app.admin_cookie()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = json_body(resp).await;
    let arr = json.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["nsid"], "games.gamesgamesgamesgames.game");
    assert_eq!(arr[0]["authority_did"], "did:plc:authority");
}

#[tokio::test]
#[serial]
async fn network_lexicon_delete_removes_tracking_and_lexicon() {
    common::require_db!();
    let app = TestApp::new().await;
    let backend = app.state.db_backend;

    let nsid = "games.gamesgamesgamesgames.game";
    seed_network_lexicon(&app, nsid, "did:plc:authority").await;

    let resp = app
        .router
        .clone()
        .clone()
        .oneshot(admin_delete(
            &format!("/admin/network-lexicons/{nsid}"),
            app.admin_cookie(),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Verify lexicon is removed.
    let sql = adapt_sql(
        "SELECT COUNT(*) FROM happyview_lexicons WHERE id = ? AND source = 'network'",
        backend,
    );
    let count: (i64,) = sqlx::query_as(&sql)
        .bind(nsid)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(count.0, 0);
}

#[tokio::test]
#[serial]
async fn network_lexicon_delete_not_found() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(admin_delete(
            "/admin/network-lexicons/nonexistent.lexicon",
            app.admin_cookie(),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[serial]
async fn network_lexicon_no_auth_returns_401() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/network-lexicons")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::db::{adapt_sql, now_rfc3339};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

use common::app::TestApp;
use common::fixtures;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

fn admin_post(
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

async fn seed_lexicons(app: &TestApp) {
    // Record lexicon
    app.router
        .clone()
        .clone()
        .oneshot(admin_post(
            "/admin/lexicons",
            app.admin_cookie(),
            &json!({
                "lexicon_json": fixtures::game_record_lexicon(),
                "backfill": false
            }),
        ))
        .await
        .unwrap();

    // Query lexicon
    app.router
        .clone()
        .clone()
        .oneshot(admin_post(
            "/admin/lexicons",
            app.admin_cookie(),
            &json!({
                "lexicon_json": fixtures::list_games_query_lexicon(),
            }),
        ))
        .await
        .unwrap();

    // Procedure lexicon
    app.router
        .clone()
        .clone()
        .oneshot(admin_post(
            "/admin/lexicons",
            app.admin_cookie(),
            &json!({
                "lexicon_json": fixtures::create_game_procedure_lexicon(),
            }),
        ))
        .await
        .unwrap();
}

async fn seed_record(app: &TestApp, uri: &str, did: &str, collection: &str, record: &Value) {
    let rkey = uri.split('/').next_back().unwrap_or("1");
    let backend = app.state.db_backend;
    let sql = adapt_sql(
        "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        backend,
    );
    happyview::db::query(&sql)
        .bind(uri)
        .bind(did)
        .bind(collection)
        .bind(rkey)
        .bind(serde_json::to_string(record).unwrap_or_default())
        .bind("bafytest")
        .bind(now_rfc3339())
        .execute(&app.state.db)
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// Profile
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn profile_no_auth_returns_401() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/xrpc/app.bsky.actor.getProfile")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// Catch-all GET (queries)
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn xrpc_get_unknown_method_proxies_and_returns_bad_gateway() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/xrpc/nonexistent.method")
                .header("x-client-key", "hvc_test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // Unknown methods are proxied to the resolved authority; DNS lookup
    // failure for a nonsense NSID results in a 502 Bad Gateway.
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
#[serial]
async fn xrpc_get_non_query_returns_400() {
    common::require_db!();
    let app = TestApp::new().await;
    seed_lexicons(&app).await;

    // game is a record, not a query
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/xrpc/games.gamesgamesgamesgames.game")
                .header("x-client-key", "hvc_test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[serial]
async fn xrpc_get_single_record_by_uri() {
    common::require_db!();
    let app = TestApp::new().await;
    seed_lexicons(&app).await;

    let did = "did:plc:test";
    let uri = "at://did:plc:test/games.gamesgamesgamesgames.game/abc123";
    let record = json!({"title": "Test Game", "$type": "games.gamesgamesgamesgames.game"});
    seed_record(&app, uri, did, "games.gamesgamesgamesgames.game", &record).await;

    // Mock PLC for PDS resolution
    Mock::given(method("GET"))
        .and(path(format!("/{did}")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(fixtures::did_document(did, "https://pds.example.com")),
        )
        .mount(&app.mock_server)
        .await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/xrpc/games.gamesgamesgamesgames.listGames?uri={}",
                    urlencoding::encode(uri)
                ))
                .header("x-client-key", "hvc_test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = json_body(resp).await;
    assert_eq!(json["record"]["title"], "Test Game");
    assert_eq!(json["record"]["uri"], uri);
}

#[tokio::test]
#[serial]
async fn xrpc_get_record_not_found() {
    common::require_db!();
    let app = TestApp::new().await;
    seed_lexicons(&app).await;

    let resp = app
        .router.clone()
        .oneshot(
            Request::builder()
                .uri("/xrpc/games.gamesgamesgamesgames.listGames?uri=at%3A%2F%2Fdid%3Aplc%3Anone%2Fgames.gamesgamesgamesgames.game%2Fmissing")
                .header("x-client-key", "hvc_test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[serial]
async fn a_list_query_without_a_script_names_the_missing_script() {
    common::require_db!();
    let app = TestApp::new().await;
    seed_lexicons(&app).await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/xrpc/games.gamesgamesgamesgames.listGames?limit=2")
                .header("x-client-key", "hvc_test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    let json = json_body(resp).await;
    assert_eq!(json["error"], "MethodNotImplemented");
    let message = json["message"].as_str().unwrap();
    assert!(
        message.contains("xrpc.query:games.gamesgamesgamesgames.listGames"),
        "{message}"
    );
}

// ---------------------------------------------------------------------------
// Catch-all POST (procedures)
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn xrpc_post_no_auth_returns_401() {
    common::require_db!();
    let app = TestApp::new().await;
    seed_lexicons(&app).await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/xrpc/games.gamesgamesgamesgames.createGame")
                .header("content-type", "application/json")
                .body(Body::from(b"{}".to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[serial]
async fn upload_lexicon_with_empty_id_returns_400() {
    common::require_db!();
    let app = TestApp::new().await;

    let mut lexicon = fixtures::game_record_lexicon();
    lexicon["id"] = json!("");

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/lexicons",
            app.admin_cookie(),
            &json!({ "lexicon_json": lexicon }),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[serial]
async fn upload_lexicon_with_malformed_nsid_returns_400() {
    common::require_db!();
    let app = TestApp::new().await;

    let mut lexicon = fixtures::game_record_lexicon();
    lexicon["id"] = json!("not-an-nsid");

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/lexicons",
            app.admin_cookie(),
            &json!({ "lexicon_json": lexicon }),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

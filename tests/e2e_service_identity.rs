mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;
use common::fixtures;
use common::plc;
use common::tls;

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

async fn seed_query_lexicon(app: &TestApp) {
    app.router
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

    app.router
        .clone()
        .oneshot(admin_post(
            "/admin/lexicons",
            app.admin_cookie(),
            &json!({
                "lexicon_json": fixtures::list_games_query_lexicon(),
                "target_collection": "games.gamesgamesgamesgames.game"
            }),
        ))
        .await
        .unwrap();
}

async fn seed_procedure_lexicon(app: &TestApp) {
    app.router
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

    app.router
        .clone()
        .oneshot(admin_post(
            "/admin/lexicons",
            app.admin_cookie(),
            &json!({
                "lexicon_json": fixtures::create_game_procedure_lexicon(),
                "target_collection": "games.gamesgamesgamesgames.game"
            }),
        ))
        .await
        .unwrap();
}

async fn seed_procedure_script(app: &TestApp, body: &str) {
    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/scripts",
            app.admin_cookie(),
            &json!({
                "id": "xrpc.procedure:games.gamesgamesgamesgames.createGame",
                "script_type": "lua",
                "body": body,
                "description": "test procedure"
            }),
        ))
        .await
        .unwrap();

    assert!(
        resp.status().is_success(),
        "seed_procedure_script failed with status {}",
        resp.status(),
    );
}

// ---------------------------------------------------------------------------
// Setup status endpoint
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn setup_status_unconfigured() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/setup/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["identity_configured"], false);
    assert!(body["identity_mode"].is_null());
}

#[tokio::test]
#[serial]
async fn setup_status_after_did_web() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.setup_did_web().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/setup/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["identity_configured"], true);
    assert_eq!(body["identity_mode"], "did_web");
}

#[tokio::test]
#[serial]
async fn setup_status_after_not_exposed() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.setup_not_exposed().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/setup/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["identity_mode"], "not_exposed");
}

// ---------------------------------------------------------------------------
// DID document generation
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn did_doc_returns_404_when_no_identity() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/.well-known/did.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[serial]
async fn did_doc_empty_services() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let did = app.setup_did_web().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/.well-known/did.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let doc = json_body(resp).await;
    assert_eq!(doc["id"], did);
    assert!(!doc["verificationMethod"].as_array().unwrap().is_empty());
    assert_eq!(doc["service"].as_array().unwrap().len(), 0);
}

#[tokio::test]
#[serial]
async fn did_doc_with_entries() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let _did = app.setup_did_web().await;

    let _id1 = app
        .create_service_entry("#chess", "ChessAppView", "all")
        .await;
    let _id2 = app
        .create_service_entry("#checkers", "CheckersAppView", "all")
        .await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/.well-known/did.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let doc = json_body(resp).await;
    let services = doc["service"].as_array().unwrap();
    assert_eq!(services.len(), 2);
    assert_eq!(services[0]["id"], "#chess");
    assert_eq!(services[0]["type"], "ChessAppView");
    assert_eq!(services[1]["id"], "#checkers");

    happyview::service_entries::delete_entry(&app.state.db, app.state.db_backend, _id1)
        .await
        .unwrap();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/.well-known/did.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let doc = json_body(resp).await;
    let services = doc["service"].as_array().unwrap();
    assert_eq!(services.len(), 1);
    assert_eq!(services[0]["id"], "#checkers");
}

// ---------------------------------------------------------------------------
// Service auth — queries
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn service_auth_query_allowed() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = plc::setup_mock_plc(&app.mock_server).await;
    let did = app.setup_did_web().await;

    seed_query_lexicon(&app).await;

    app.create_service_entry("#chess", "ChessAppView", "all")
        .await;

    let auth = app
        .service_auth_jwt(&plc_store, "did:plc:caller123", &did, "#chess")
        .await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/xrpc/games.gamesgamesgamesgames.listGames")
                .header("authorization", &auth)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
#[serial]
async fn service_auth_query_denied() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = plc::setup_mock_plc(&app.mock_server).await;
    let did = app.setup_did_web().await;

    seed_query_lexicon(&app).await;

    app.create_service_entry("#chess", "ChessAppView", "specific")
        .await;

    let auth = app
        .service_auth_jwt(&plc_store, "did:plc:caller456", &did, "#chess")
        .await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/xrpc/games.gamesgamesgamesgames.listGames")
                .header("authorization", &auth)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = json_body(resp).await;
    assert!(body["error"].as_str().unwrap().contains("not authorized"));
}

#[tokio::test]
#[serial]
async fn service_auth_specific_xrpc_allowed() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = plc::setup_mock_plc(&app.mock_server).await;
    let did = app.setup_did_web().await;

    seed_query_lexicon(&app).await;

    let entry_id = app
        .create_service_entry("#chess", "ChessAppView", "specific")
        .await;
    app.add_entry_xrpcs(entry_id, &["games.gamesgamesgamesgames.listGames"])
        .await;

    let auth = app
        .service_auth_jwt(&plc_store, "did:plc:caller789", &did, "#chess")
        .await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/xrpc/games.gamesgamesgamesgames.listGames")
                .header("authorization", &auth)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Service auth — procedures
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn service_auth_procedure_allowed() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = plc::setup_mock_plc(&app.mock_server).await;
    let did = app.setup_did_web().await;

    seed_procedure_lexicon(&app).await;
    seed_procedure_script(&app, "function handle(input, params)\nreturn { uri = 'at://test/games.gamesgamesgamesgames.game/1' }\nend").await;

    let entry_id = app
        .create_service_entry("#chess", "ChessAppView", "specific")
        .await;
    app.add_entry_xrpcs(entry_id, &["games.gamesgamesgamesgames.createGame"])
        .await;

    let auth = app
        .service_auth_jwt(&plc_store, "did:plc:procallowed", &did, "#chess")
        .await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/xrpc/games.gamesgamesgamesgames.createGame")
                .header("authorization", &auth)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"title": "test"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
#[serial]
async fn service_auth_procedure_denied() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = plc::setup_mock_plc(&app.mock_server).await;
    let did = app.setup_did_web().await;

    seed_procedure_lexicon(&app).await;
    seed_procedure_script(&app, "function handle(input, params)\nreturn { uri = 'at://test/games.gamesgamesgamesgames.game/1' }\nend").await;

    app.create_service_entry("#chess", "ChessAppView", "specific")
        .await;

    let auth = app
        .service_auth_jwt(&plc_store, "did:plc:procdenied", &did, "#chess")
        .await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/xrpc/games.gamesgamesgamesgames.createGame")
                .header("authorization", &auth)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"title": "test"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = json_body(resp).await;
    assert!(body["error"].as_str().unwrap().contains("not authorized"));
}

#[tokio::test]
#[serial]
async fn token_scope_enforcement() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = plc::setup_mock_plc(&app.mock_server).await;
    let did = app.setup_did_web().await;

    seed_procedure_lexicon(&app).await;

    seed_procedure_script(
        &app,
        "function handle(input, params)\nlocal x = xrpc.query('games.birb.chess.getGame', {})\nreturn { uri = 'at://test/games.gamesgamesgamesgames.game/1' }\nend",
    ).await;

    let entry_id = app
        .create_service_entry("#chess", "ChessAppView", "specific")
        .await;
    app.add_entry_xrpcs(entry_id, &["games.gamesgamesgamesgames.createGame"])
        .await;

    let auth = app
        .service_auth_jwt(&plc_store, "did:plc:scopecheck", &did, "#chess")
        .await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/xrpc/games.gamesgamesgamesgames.createGame")
                .header("authorization", &auth)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"title": "test"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = json_body(resp).await;
    let msg = body["error"].as_str().unwrap();
    assert!(
        msg.contains("games.birb.chess.getGame"),
        "error should list the missing scope XRPC"
    );
}

// ---------------------------------------------------------------------------
// Edge cases — identity modes, invalid JWTs, missing fragments
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn not_exposed_rejects_service_auth() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = plc::setup_mock_plc(&app.mock_server).await;
    app.setup_not_exposed().await;

    seed_procedure_lexicon(&app).await;
    seed_procedure_script(&app, "function handle(input, params)\nreturn { uri = 'at://test/games.gamesgamesgamesgames.game/1' }\nend").await;

    let auth = app
        .raw_service_auth_jwt(
            &plc_store,
            "did:plc:notexposed",
            "did:plc:fake#chess",
            chrono::Utc::now().timestamp() as u64 + 60,
        )
        .await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/xrpc/games.gamesgamesgamesgames.createGame")
                .header("authorization", &auth)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"title": "test"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "not_exposed should reject service auth"
    );
}

#[tokio::test]
#[serial]
async fn wrong_aud_rejects_service_auth() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = plc::setup_mock_plc(&app.mock_server).await;
    let _did = app.setup_did_web().await;

    seed_procedure_lexicon(&app).await;
    seed_procedure_script(&app, "function handle(input, params)\nreturn { uri = 'at://test/games.gamesgamesgamesgames.game/1' }\nend").await;

    app.create_service_entry("#chess", "ChessAppView", "all")
        .await;

    let auth = app
        .raw_service_auth_jwt(
            &plc_store,
            "did:plc:wrongaud",
            "did:web:wrong.example.com#chess",
            chrono::Utc::now().timestamp() as u64 + 60,
        )
        .await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/xrpc/games.gamesgamesgamesgames.createGame")
                .header("authorization", &auth)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"title": "test"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "wrong aud should reject service auth"
    );
}

#[tokio::test]
#[serial]
async fn expired_jwt_rejects_service_auth() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = plc::setup_mock_plc(&app.mock_server).await;
    let did = app.setup_did_web().await;

    seed_procedure_lexicon(&app).await;
    seed_procedure_script(&app, "function handle(input, params)\nreturn { uri = 'at://test/games.gamesgamesgamesgames.game/1' }\nend").await;

    app.create_service_entry("#chess", "ChessAppView", "all")
        .await;

    let auth = app
        .raw_service_auth_jwt(
            &plc_store,
            "did:plc:expired",
            &format!("{}#chess", did),
            1000,
        )
        .await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/xrpc/games.gamesgamesgamesgames.createGame")
                .header("authorization", &auth)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"title": "test"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "expired JWT should reject service auth"
    );
}

#[tokio::test]
#[serial]
async fn nonexistent_fragment_denies_access() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = plc::setup_mock_plc(&app.mock_server).await;
    let did = app.setup_did_web().await;

    seed_query_lexicon(&app).await;

    app.create_service_entry("#chess", "ChessAppView", "all")
        .await;

    let auth = app
        .service_auth_jwt(&plc_store, "did:plc:nofragment", &did, "#doesNotExist")
        .await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/xrpc/games.gamesgamesgamesgames.listGames")
                .header("authorization", &auth)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = json_body(resp).await;
    assert!(body["error"].as_str().unwrap().contains("not authorized"));
}

#[tokio::test]
#[serial]
async fn did_plc_returns_404_for_did_json() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let _did = app.setup_did_plc().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/.well-known/did.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[serial]
async fn multiple_entries_matched_by_fragment() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = plc::setup_mock_plc(&app.mock_server).await;
    let did = app.setup_did_web().await;

    seed_query_lexicon(&app).await;

    app.create_service_entry("#chess", "ChessAppView", "all")
        .await;
    let checkers_id = app
        .create_service_entry("#checkers", "CheckersAppView", "specific")
        .await;
    app.add_entry_xrpcs(checkers_id, &["games.gamesgamesgamesgames.otherGame"])
        .await;

    let auth_chess = app
        .service_auth_jwt(&plc_store, "did:plc:multi1", &did, "#chess")
        .await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/xrpc/games.gamesgamesgamesgames.listGames")
                .header("authorization", &auth_chess)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let auth_checkers = app
        .service_auth_jwt(&plc_store, "did:plc:multi2", &did, "#checkers")
        .await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/xrpc/games.gamesgamesgamesgames.listGames")
                .header("authorization", &auth_checkers)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[serial]
async fn scope_check_applies_with_access_mode_all() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = plc::setup_mock_plc(&app.mock_server).await;
    let did = app.setup_did_web().await;

    seed_procedure_lexicon(&app).await;
    seed_procedure_script(
        &app,
        "function handle(input, params)\nlocal x = xrpc.query('games.birb.chess.getGame', {})\nreturn { uri = 'at://test/games.gamesgamesgamesgames.game/1' }\nend",
    ).await;

    app.create_service_entry("#chess", "ChessAppView", "all")
        .await;

    let auth = app
        .service_auth_jwt(&plc_store, "did:plc:scopeall", &did, "#chess")
        .await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/xrpc/games.gamesgamesgamesgames.createGame")
                .header("authorization", &auth)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"title": "test"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = json_body(resp).await;
    let msg = body["error"].as_str().unwrap();
    assert!(
        msg.contains("games.birb.chess.getGame"),
        "scope check should apply even with access_mode=all"
    );
}

#[tokio::test]
#[serial]
async fn aud_missing_fragment_rejects() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = plc::setup_mock_plc(&app.mock_server).await;
    let did = app.setup_did_web().await;

    seed_procedure_lexicon(&app).await;
    seed_procedure_script(&app, "function handle(input, params)\nreturn { uri = 'at://test/games.gamesgamesgamesgames.game/1' }\nend").await;

    app.create_service_entry("#chess", "ChessAppView", "all")
        .await;

    // aud = instance DID with no fragment
    let auth = app
        .raw_service_auth_jwt(
            &plc_store,
            "did:plc:nofrag",
            &did,
            chrono::Utc::now().timestamp() as u64 + 60,
        )
        .await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/xrpc/games.gamesgamesgamesgames.createGame")
                .header("authorization", &auth)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"title": "test"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "aud without fragment should reject"
    );
}

// ---------------------------------------------------------------------------
// Auth regression — existing auth paths still work
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn anonymous_access_still_works() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.setup_did_web().await;

    seed_query_lexicon(&app).await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/xrpc/games.gamesgamesgamesgames.listGames")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Static analysis — outbound_xrpcs persistence
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn static_analysis_persistence() {
    common::require_db!();
    let app = TestApp::new().await;

    app.router
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

    app.router
        .clone()
        .oneshot(admin_post(
            "/admin/lexicons",
            app.admin_cookie(),
            &json!({
                "lexicon_json": fixtures::create_game_procedure_lexicon(),
                "target_collection": "games.gamesgamesgamesgames.game"
            }),
        ))
        .await
        .unwrap();

    let resp = app
        .router
        .clone()
        .oneshot(admin_post(
            "/admin/scripts",
            app.admin_cookie(),
            &json!({
                "id": "xrpc.procedure:games.gamesgamesgamesgames.createGame",
                "script_type": "lua",
                "body": "function handle(input, params)\nreturn { uri = 'at://test/games.gamesgamesgamesgames.game/1' }\nend",
                "description": "test procedure"
            }),
        ))
        .await
        .unwrap();

    assert!(
        resp.status().is_success(),
        "POST /admin/scripts returned {}",
        resp.status()
    );
    let body = json_body(resp).await;
    assert!(
        body["outbound_xrpcs"].is_null()
            || body["outbound_xrpcs"]
                .as_array()
                .is_some_and(|a| a.is_empty()),
        "expected null or empty outbound_xrpcs for script with no XRPC calls"
    );

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/admin/scripts/xrpc.procedure%3Agames.gamesgamesgamesgames.createGame")
                .header(app.admin_cookie().0, app.admin_cookie().1)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "body": "function handle(input, params)\nlocal x = xrpc.query('games.birb.chess.getGame', {})\nreturn { uri = 'at://test/games.gamesgamesgamesgames.game/1' }\nend"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        resp.status().is_success(),
        "PATCH script returned {}",
        resp.status()
    );
    let body = json_body(resp).await;
    let xrpcs = body["outbound_xrpcs"]
        .as_array()
        .expect("expected outbound_xrpcs array");
    assert_eq!(xrpcs.len(), 1);
    assert_eq!(xrpcs[0], "games.birb.chess.getGame");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/admin/scripts/xrpc.procedure%3Agames.gamesgamesgamesgames.createGame")
                .header(app.admin_cookie().0, app.admin_cookie().1)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "body": "function handle(input, params)\n-- local x = xrpc.query('games.birb.chess.getGame', {})\nreturn { uri = 'at://test/games.gamesgamesgamesgames.game/1' }\nend"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        resp.status().is_success(),
        "second PATCH returned {}",
        resp.status()
    );
    let body = json_body(resp).await;
    assert!(
        body["outbound_xrpcs"].is_null()
            || body["outbound_xrpcs"]
                .as_array()
                .is_some_and(|a| a.is_empty()),
        "expected null or empty outbound_xrpcs when only commented-out calls exist"
    );
}

// ---------------------------------------------------------------------------
// JWT edge cases — forbidden typ, unsupported DID, missing aud
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn forbidden_jwt_typ_rejected() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = plc::setup_mock_plc(&app.mock_server).await;
    let did = app.setup_did_web().await;

    seed_procedure_lexicon(&app).await;
    seed_procedure_script(&app, "function handle(input, params)\nreturn { uri = 'at://test/games.gamesgamesgamesgames.game/1' }\nend").await;

    app.create_service_entry("#chess", "ChessAppView", "all")
        .await;

    for forbidden_typ in ["at+jwt", "refresh+jwt", "dpop+jwt"] {
        let auth = app
            .custom_service_auth_jwt(
                &plc_store,
                &format!("did:plc:typ{}", forbidden_typ.replace('+', "")),
                json!({"alg": "ES256", "typ": forbidden_typ}),
                json!({
                    "iss": format!("did:plc:typ{}", forbidden_typ.replace('+', "")),
                    "aud": format!("{}#chess", did),
                    "exp": chrono::Utc::now().timestamp() as u64 + 60,
                }),
            )
            .await;

        let resp = app
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/xrpc/games.gamesgamesgamesgames.createGame")
                    .header("authorization", &auth)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({"title": "test"})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "JWT with typ={} should be rejected",
            forbidden_typ
        );
    }
}

#[tokio::test]
#[serial]
async fn unsupported_did_method_rejected() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = plc::setup_mock_plc(&app.mock_server).await;
    let did = app.setup_did_web().await;

    seed_procedure_lexicon(&app).await;
    seed_procedure_script(&app, "function handle(input, params)\nreturn { uri = 'at://test/games.gamesgamesgamesgames.game/1' }\nend").await;

    app.create_service_entry("#chess", "ChessAppView", "all")
        .await;

    // Use did:key: which is not supported by resolve_signing_key
    let auth = app
        .custom_service_auth_jwt(
            &plc_store,
            "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK",
            json!({"alg": "ES256"}),
            json!({
                "iss": "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK",
                "aud": format!("{}#chess", did),
                "exp": chrono::Utc::now().timestamp() as u64 + 60,
            }),
        )
        .await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/xrpc/games.gamesgamesgamesgames.createGame")
                .header("authorization", &auth)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"title": "test"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "unsupported DID method should be rejected"
    );
}

#[tokio::test]
#[serial]
async fn jwt_without_aud_field_rejected() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = plc::setup_mock_plc(&app.mock_server).await;
    let _did = app.setup_did_web().await;

    seed_procedure_lexicon(&app).await;
    seed_procedure_script(&app, "function handle(input, params)\nreturn { uri = 'at://test/games.gamesgamesgamesgames.game/1' }\nend").await;

    app.create_service_entry("#chess", "ChessAppView", "all")
        .await;

    // JWT payload with no aud field — JwtPayload deserialization fails
    let auth = app
        .custom_service_auth_jwt(
            &plc_store,
            "did:plc:noaud",
            json!({"alg": "ES256"}),
            json!({
                "iss": "did:plc:noaud",
                "exp": chrono::Utc::now().timestamp() as u64 + 60,
            }),
        )
        .await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/xrpc/games.gamesgamesgamesgames.createGame")
                .header("authorization", &auth)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"title": "test"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "JWT without aud should be rejected"
    );
}

// ---------------------------------------------------------------------------
// Setup identity — AttachAccount mode stores attached DID
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn set_identity_attach_account_mode() {
    common::require_db!();
    let app = TestApp::new().await;
    let cookie = app.admin_cookie();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/setup/identity")
                .header(cookie.0.clone(), cookie.1.clone())
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "mode": "attach_account",
                        "attached_account_did": "did:plc:testaccount"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Verify status reflects the mode
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/setup/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = json_body(resp).await;
    assert_eq!(body["identity_mode"], "attach_account");
}

// ---------------------------------------------------------------------------
// Setup HTTP flow — full endpoint-driven setup produces working identity
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn setup_http_flow_did_web_produces_valid_did_doc() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.state.config.token_encryption_key = Some([0x42u8; 32]);
    app.rebuild_router();

    let cookie = app.admin_cookie();

    // Step 1: POST /api/setup/identity with mode=did_web
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/setup/identity")
                .header(cookie.0.clone(), cookie.1.clone())
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"mode": "did_web"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Step 2: POST /api/setup/complete
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/setup/complete")
                .header(cookie.0.clone(), cookie.1.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Step 3: Rebuild router to pick up identity changes, then verify DID doc
    app.rebuild_router();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/.well-known/did.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let doc = json_body(resp).await;
    assert!(doc["id"].as_str().unwrap().starts_with("did:web:"));
    assert!(!doc["verificationMethod"].as_array().unwrap().is_empty());

    // Step 4: Verify status shows complete
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/setup/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let status = json_body(resp).await;
    assert_eq!(status["identity_mode"], "did_web");
    assert_eq!(status["identity_configured"], true);
    assert_eq!(status["setup_complete"], true);
}

// ---------------------------------------------------------------------------
// setup_complete reset — mode change resets setup_complete flag
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn mode_change_resets_setup_complete() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let _did = app.setup_did_web().await;
    let cookie = app.admin_cookie();

    // Verify setup_complete is true after setup_did_web
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/service-identity")
                .header(cookie.0.clone(), cookie.1.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = json_body(resp).await;
    assert_eq!(body["setup_complete"], true);

    // Change mode via PUT — this should reset setup_complete
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/admin/service-identity")
                .header(cookie.0.clone(), cookie.1.clone())
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"mode": "not_exposed"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Verify setup_complete was reset to false
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/service-identity")
                .header(cookie.0.clone(), cookie.1.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = json_body(resp).await;
    assert_eq!(body["mode"], "not_exposed");
    assert_eq!(
        body["setup_complete"], false,
        "mode change should reset setup_complete"
    );
}

// ---------------------------------------------------------------------------
// did:web issuer resolution via TLS
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn did_web_issuer_resolved_via_https() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let instance_did = app.setup_did_web().await;

    seed_query_lexicon(&app).await;

    app.create_service_entry("#appview", "TestAppView", "all")
        .await;

    let mut key_bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut key_bytes);
    let issuer_key = p256::ecdsa::SigningKey::from_bytes((&key_bytes[..]).into()).unwrap();
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    let public_key = p256::PublicKey::from(issuer_key.verifying_key());
    let compressed = public_key.to_encoded_point(true);
    let pub_bytes = compressed.as_bytes().to_vec();

    let server =
        tls::start_did_web_server(move |did| plc::test_did_document(did, &pub_bytes)).await;
    let issuer_did = server.issuer_did().to_string();

    app.use_permissive_http_client();

    let auth = app.did_web_service_auth_jwt(&issuer_key, &issuer_did, &instance_did, "#appview");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/xrpc/games.gamesgamesgamesgames.listGames")
                .header("authorization", &auth)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "did:web issuer should resolve via HTTPS and be allowed with access_mode=all"
    );
}

// ---------------------------------------------------------------------------
// Service auth with no service entries at all
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn service_auth_rejected_when_no_entries_exist() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = plc::setup_mock_plc(&app.mock_server).await;
    let did = app.setup_did_web().await;

    seed_query_lexicon(&app).await;

    let auth = app
        .service_auth_jwt(&plc_store, "did:plc:noentries", &did, "#chess")
        .await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/xrpc/games.gamesgamesgamesgames.listGames")
                .header("authorization", &auth)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = json_body(resp).await;
    assert!(body["error"].as_str().unwrap().contains("not authorized"));
}

// ---------------------------------------------------------------------------
// Service auth with did:plc identity mode
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn service_auth_works_with_did_plc_identity() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let plc_store = plc::setup_mock_plc(&app.mock_server).await;
    let did = app.setup_did_plc().await;

    seed_query_lexicon(&app).await;

    app.create_service_entry("#chess", "ChessAppView", "all")
        .await;

    let auth = app
        .service_auth_jwt(&plc_store, "did:plc:plccaller", &did, "#chess")
        .await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/xrpc/games.gamesgamesgamesgames.listGames")
                .header("authorization", &auth)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "service auth should work when instance uses did:plc identity"
    );
}

// ---------------------------------------------------------------------------
// Anonymous POST to procedure is rejected
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn anonymous_procedure_rejected() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.setup_did_web().await;

    seed_procedure_lexicon(&app).await;
    seed_procedure_script(&app, "function handle(input, params)\nreturn { uri = 'at://test/games.gamesgamesgamesgames.game/1' }\nend").await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/xrpc/games.gamesgamesgamesgames.createGame")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({"title": "test"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "anonymous POST to procedure should be rejected"
    );
}

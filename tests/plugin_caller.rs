//! The `sdk_caller` fixture end to end: every caller-acting and local-index
//! host import, exercised through a real DPoP-authenticated procedure script
//! against a mocked PDS.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::oauth::pds_write::generate_dpop_proof;
use happyview::plugin::library::LibraryCallContext;
use happyview::plugin::loader;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, ResponseTemplate};

use common::app::TestApp;

// ---------------------------------------------------------------------------
// Helpers — copied from `tests/e2e_delegation.rs`, which is not a shared module.
// ---------------------------------------------------------------------------

async fn response_json(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or(json!(null))
}

fn post_json_with_headers(uri: &str, body: &Value, headers: Vec<(&str, &str)>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("host", "127.0.0.1:0");
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

/// Set up a full DPoP session and return `(client_key, dpop_key, access_token)`.
async fn setup_dpop_session(app: &TestApp, user_did: &str) -> (String, Value, String) {
    let (client_key, client_secret, _id) = app.create_api_client("confidential", None).await;

    let key_req = post_json_with_headers(
        "/oauth/dpop-keys",
        &json!({}),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let key_resp = app.router.clone().oneshot(key_req).await.unwrap();
    assert_eq!(key_resp.status(), StatusCode::CREATED);
    let key_body = response_json(key_resp).await;
    let provision_id = key_body["provision_id"].as_str().unwrap().to_string();
    let dpop_key = key_body["dpop_key"].clone();

    let access_token = format!("test-access-{}", uuid::Uuid::new_v4());
    app.mock_session_verification(user_did, user_did).await;
    let session_req = post_json_with_headers(
        "/oauth/sessions",
        &json!({
            "provision_id": provision_id,
            "did": user_did,
            "access_token": &access_token,
            "scopes": "atproto",
            "pds_url": "https://pds.example.com",
        }),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let session_resp = app.router.clone().oneshot(session_req).await.unwrap();
    assert_eq!(session_resp.status(), StatusCode::CREATED);

    (client_key, dpop_key, access_token)
}

/// Build DPoP auth headers for a request.
fn dpop_auth_headers<'a>(
    client_key: &'a str,
    dpop_key: &Value,
    access_token: &'a str,
    method: &str,
    url: &str,
) -> Vec<(&'static str, String)> {
    // DPoP htu must not include query/fragment
    let htu = url.split('?').next().unwrap_or(url);
    let proof = generate_dpop_proof(dpop_key, method, htu, access_token, None)
        .expect("failed to generate DPoP proof");
    vec![
        ("x-client-key", client_key.to_string()),
        ("authorization", format!("DPoP {}", access_token)),
        ("dpop", proof),
    ]
}

/// Make an authenticated POST request.
async fn dpop_post(
    app: &TestApp,
    path: &str,
    body: &Value,
    client_key: &str,
    dpop_key: &Value,
    access_token: &str,
) -> axum::response::Response {
    let url = format!("http://127.0.0.1:0{}", path);
    let headers = dpop_auth_headers(client_key, dpop_key, access_token, "POST", &url);
    let str_headers: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let req = post_json_with_headers(path, body, str_headers);
    app.router.clone().oneshot(req).await.unwrap()
}

async fn update_session_pds_url(app: &TestApp, user_did: &str, pds_url: &str) {
    let sql = happyview::db::adapt_sql(
        "UPDATE happyview_dpop_sessions SET pds_url = ? WHERE user_did = ?",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(pds_url)
        .bind(user_did)
        .execute(&app.state.db)
        .await
        .expect("failed to update session pds_url");
}

async fn seed_procedure_lexicon(app: &TestApp) {
    let (status, _) = app
        .post_json_status(
            "/admin/lexicons",
            json!({
                "lexicon_json": common::fixtures::create_game_procedure_lexicon(),
                "target_collection": "games.gamesgamesgamesgames.game",
            }),
        )
        .await;
    assert!(status < 300, "failed to seed procedure lexicon: {status}");
}

async fn seed_script(app: &TestApp, id: &str, body: &str) {
    let (status, resp) = app
        .post_json_status(
            "/admin/scripts",
            json!({
                "id": id,
                "body": body,
            }),
        )
        .await;
    assert!(
        status < 300,
        "failed to seed script {id} ({status}): {resp}"
    );
}

/// Load and install the `sdk_caller` fixture, built at
/// `cargo build --manifest-path tests/fixtures/sdk_caller/Cargo.toml --target wasm32-unknown-unknown --release`.
async fn install_caller_fixture(app: &TestApp) {
    let plugin = loader::load_from_file(std::path::Path::new("tests/fixtures/sdk_caller"))
        .await
        .expect(
            "sdk_caller fixture not built. Run: cargo build --manifest-path \
             tests/fixtures/sdk_caller/Cargo.toml --target wasm32-unknown-unknown --release",
        );
    app.state.plugin_registry.install(plugin).await.unwrap();
}

const CREATE_GAME: &str = "games.gamesgamesgamesgames.createGame";

fn create_record_script() -> String {
    "function handle(input, ctx)\n\
       local c = require(\"caller\")\n\
       return c.create_record({ collection = \"com.example.post\", record = { text = input.text }, validate = false })\n\
     end"
        .to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn create_record_through_a_procedure_script() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    install_caller_fixture(&app).await;
    seed_procedure_lexicon(&app).await;
    seed_script(
        &app,
        &format!("xrpc.procedure:{CREATE_GAME}"),
        &create_record_script(),
    )
    .await;

    const DID: &str = "did:plc:sdkcallerwriter";
    let (client_key, dpop_key, access_token) = setup_dpop_session(&app, DID).await;
    let pds_url = format!("{}/pds/{DID}", app.mock_server.uri());
    update_session_pds_url(&app, DID, &pds_url).await;

    Mock::given(method("POST"))
        .and(path(format!(
            "/pds/{DID}/xrpc/com.atproto.repo.createRecord"
        )))
        .and(header(
            "authorization",
            format!("DPoP {access_token}").as_str(),
        ))
        .and(body_partial_json(json!({ "repo": DID })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "uri": format!("at://{DID}/com.example.post/abc"),
            "cid": "bafyreicaller",
        })))
        .expect(1)
        .mount(&app.mock_server)
        .await;

    let resp = dpop_post(
        &app,
        &format!("/xrpc/{CREATE_GAME}"),
        &json!({ "text": "hello" }),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    let status = resp.status();
    let body = response_json(resp).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["uri"], format!("at://{DID}/com.example.post/abc"));
    assert_eq!(body["cid"], "bafyreicaller");
}

#[tokio::test]
#[serial]
async fn no_session_is_a_plugin_error() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    install_caller_fixture(&app).await;

    let err = app
        .state
        .plugin_executor()
        .call_library(
            "sdk_caller",
            "create_record",
            &[json!({
                "collection": "com.example.post",
                "record": { "text": "hi" },
            })],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("NO_SESSION"), "{err}");
}

#[tokio::test]
#[serial]
async fn writable_repo_is_enforced() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    install_caller_fixture(&app).await;
    seed_procedure_lexicon(&app).await;
    seed_script(
        &app,
        &format!("xrpc.procedure:{CREATE_GAME}"),
        "function handle(input, ctx)\n\
           local c = require(\"caller\")\n\
           return c.create_record({ collection = \"com.example.post\", repo = \"did:plc:someoneelse\", record = { text = input.text }, validate = false })\n\
         end",
    )
    .await;

    const DID: &str = "did:plc:sdkcallerwriter2";
    let (client_key, dpop_key, access_token) = setup_dpop_session(&app, DID).await;

    let resp = dpop_post(
        &app,
        &format!("/xrpc/{CREATE_GAME}"),
        &json!({ "text": "hello" }),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = response_json(resp).await;
    let message = body["message"].as_str().unwrap_or_default().to_string();
    assert!(message.contains("WRITABLE_REPO"), "{message}");

    // `setup_dpop_session` itself talks to `app.mock_server` for DID
    // resolution, so check the write endpoint specifically rather than
    // asserting the mock saw nothing at all.
    let create_record_hits = app
        .mock_server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path().contains("createRecord"))
        .count();
    assert_eq!(
        create_record_hits, 0,
        "a write refused for pointing at the wrong repo must never reach the PDS"
    );
}

/// `upload_blob_to_pds`'s DPoP branch used to collapse every non-2xx PDS
/// response into a statusless `AppError::Internal`, so a plugin could not
/// tell a rejected upload from an unreachable PDS. It now carries the real
/// status through `AppError::PdsError`, and this pins that at the WASM
/// boundary rather than only at the `pds_failure` unit level.
#[tokio::test]
#[serial]
async fn blob_upload_errors_carry_the_pds_status() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    install_caller_fixture(&app).await;
    seed_procedure_lexicon(&app).await;
    seed_script(
        &app,
        &format!("xrpc.procedure:{CREATE_GAME}"),
        "function handle(input, ctx)\n\
           local c = require(\"caller\")\n\
           return c.upload_blob({ bytes = \"hi\", mime_type = \"image/png\" })\n\
         end",
    )
    .await;

    const DID: &str = "did:plc:sdkcalleruploader";
    let (client_key, dpop_key, access_token) = setup_dpop_session(&app, DID).await;
    let pds_url = format!("{}/pds/{DID}", app.mock_server.uri());
    update_session_pds_url(&app, DID, &pds_url).await;

    Mock::given(method("POST"))
        .and(path(format!("/pds/{DID}/xrpc/com.atproto.repo.uploadBlob")))
        .and(header(
            "authorization",
            format!("DPoP {access_token}").as_str(),
        ))
        .respond_with(ResponseTemplate::new(413).set_body_string("BlobTooLarge"))
        .expect(1)
        .mount(&app.mock_server)
        .await;

    let resp = dpop_post(
        &app,
        &format!("/xrpc/{CREATE_GAME}"),
        &json!({ "text": "hello" }),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = response_json(resp).await;
    let message = body["message"].as_str().unwrap_or_default().to_string();
    assert!(message.contains("PDS_ERROR"), "{message}");
    assert!(message.contains("413"), "{message}");
}

#[tokio::test]
#[serial]
async fn xrpc_query_reaches_a_local_handler() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    install_caller_fixture(&app).await;
    seed_procedure_lexicon(&app).await;

    const LIST_GAMES: &str = "games.gamesgamesgamesgames.listGames";
    let (status, _) = app
        .post_json_status(
            "/admin/lexicons",
            json!({
                "lexicon_json": common::fixtures::list_games_query_lexicon(),
                "target_collection": "games.gamesgamesgamesgames.game",
            }),
        )
        .await;
    assert!(status < 300, "failed to seed query lexicon: {status}");
    seed_script(
        &app,
        &format!("xrpc.query:{LIST_GAMES}"),
        "function handle(params, ctx)\n  return { ok = true }\nend",
    )
    .await;
    seed_script(
        &app,
        &format!("xrpc.procedure:{CREATE_GAME}"),
        &format!(
            "function handle(input, ctx)\n\
               local c = require(\"caller\")\n\
               return c.xrpc_query({{ method = \"{LIST_GAMES}\", params = {{}} }})\n\
             end"
        ),
    )
    .await;

    const DID: &str = "did:plc:sdkcallerreader";
    let (client_key, dpop_key, access_token) = setup_dpop_session(&app, DID).await;

    let resp = dpop_post(
        &app,
        &format!("/xrpc/{CREATE_GAME}"),
        &json!({}),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    let status = resp.status();
    let body = response_json(resp).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);
}

/// The Lua `xrpc.query` global needs no PDS auth for a query, record-event or
/// label script, and `host_caller_xrpc_query` has to reach the same local
/// handler with no `CallerSession` at all — calling through `call_library`
/// (not `call_library_as`) with the default, session-less context is exactly
/// that runner.
#[tokio::test]
#[serial]
async fn xrpc_query_reaches_a_local_handler_with_no_session() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    install_caller_fixture(&app).await;

    const LIST_GAMES: &str = "games.gamesgamesgamesgames.listGames";
    let (status, _) = app
        .post_json_status(
            "/admin/lexicons",
            json!({
                "lexicon_json": common::fixtures::list_games_query_lexicon(),
                "target_collection": "games.gamesgamesgamesgames.game",
            }),
        )
        .await;
    assert!(status < 300, "failed to seed query lexicon: {status}");
    seed_script(
        &app,
        &format!("xrpc.query:{LIST_GAMES}"),
        "function handle(params, ctx)\n  return { ok = true }\nend",
    )
    .await;

    let result = app
        .state
        .plugin_executor()
        .call_library(
            "sdk_caller",
            "xrpc_query",
            &[json!({ "method": LIST_GAMES, "params": {} })],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap();
    assert_eq!(result["ok"], true);
}

#[tokio::test]
#[serial]
async fn index_put_and_lexicon_get_round_trip() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    install_caller_fixture(&app).await;

    let lexicon = common::fixtures::game_record_lexicon();
    let (status, _) = app
        .post_json_status(
            "/admin/lexicons",
            json!({
                "lexicon_json": lexicon,
                "target_collection": "games.gamesgamesgamesgames.game",
            }),
        )
        .await;
    assert!(status < 300, "failed to seed record lexicon: {status}");

    let ctx = LibraryCallContext::default();
    let exec = app.state.plugin_executor();

    let got = exec
        .call_library(
            "sdk_caller",
            "lexicon_get",
            &[json!("games.gamesgamesgamesgames.game")],
            &ctx,
            0,
        )
        .await
        .unwrap();
    assert_eq!(
        got, lexicon,
        "lexicon_get should return the raw uploaded JSON"
    );

    const DID: &str = "did:plc:sdkcallerindexer";
    let record_ref = exec
        .call_library(
            "sdk_caller",
            "index_put",
            &[json!({
                "collection": "games.gamesgamesgamesgames.game",
                "rkey": "abc123",
                "did": DID,
                "record": { "title": "Indexed" },
            })],
            &ctx,
            0,
        )
        .await
        .unwrap();
    let uri = record_ref["uri"].as_str().unwrap().to_string();
    assert_eq!(
        uri,
        format!("at://{DID}/games.gamesgamesgamesgames.game/abc123")
    );

    let row = happyview::plugin::host::records_get(&app.state.db, app.state.db_backend, &uri)
        .await
        .unwrap()
        .expect("index_put should have made the record readable through records_get");
    assert_eq!(row["title"], "Indexed");
}

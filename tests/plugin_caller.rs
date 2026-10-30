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
    let (client_key, _secret, dpop_key, access_token) =
        setup_dpop_session_with_secret(app, user_did).await;
    (client_key, dpop_key, access_token)
}

/// [`setup_dpop_session`], also returning the API client's secret, which
/// registering a second account's session under the same client needs.
async fn setup_dpop_session_with_secret(
    app: &TestApp,
    user_did: &str,
) -> (String, String, Value, String) {
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

    (client_key, client_secret, dpop_key, access_token)
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

/// Seed a script row directly, bypassing `POST /admin/scripts`, and install
/// the interpreter that will run it. The bodies below are directives for the
/// echo fixture rather than source in the language the row names, and that
/// endpoint validates what it stores.
async fn seed_script(app: &TestApp, id: &str, body: &str) {
    let now = happyview::db::now_rfc3339();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_scripts (id, body, script_type, created_at, updated_at) \
         VALUES (?, ?, 'lua', ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(id)
        .bind(body)
        .bind(&now)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .expect("seed a script row");
    app.state
        .plugin_registry
        .register(common::echo_interpreter::plugin("lua"))
        .await;
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

/// `require`'s contract: the library's value on success, a raise on an error
/// envelope. The call's arguments ride in on the request rather than sitting in
/// the body, since the echo fixture reads a directive and not a script.
const REQUIRE: &str = "host:require";

fn call(function: &str, args: Value) -> Value {
    json!({ "library": "sdk_caller", "function": function, "args": [args] })
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
    seed_script(&app, &format!("xrpc.procedure:{CREATE_GAME}"), REQUIRE).await;

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
        &call("create_record", json!({ "collection": "com.example.post", "record": { "text": "hello" }, "validate": false })),
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
    seed_script(&app, &format!("xrpc.procedure:{CREATE_GAME}"), REQUIRE).await;

    const DID: &str = "did:plc:sdkcallerwriter2";
    let (client_key, dpop_key, access_token) = setup_dpop_session(&app, DID).await;

    let resp = dpop_post(
        &app,
        &format!("/xrpc/{CREATE_GAME}"),
        &call("create_record", json!({ "collection": "com.example.post", "repo": "did:plc:someoneelse", "record": { "text": "hello" }, "validate": false })),
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
    seed_script(&app, &format!("xrpc.procedure:{CREATE_GAME}"), REQUIRE).await;

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
        &call(
            "upload_blob",
            json!({ "bytes": "hi", "mime_type": "image/png" }),
        ),
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

/// Answer `getSession` at `did`'s mocked PDS, only for the session holding
/// `access_token`. Mounted ahead of the mock `setup_dpop_session` mounts for
/// session verification, which answers with the DID alone.
async fn mount_get_session(
    app: &TestApp,
    did: &str,
    access_token: &str,
    response: ResponseTemplate,
    times: u64,
) {
    Mock::given(method("GET"))
        .and(path(format!(
            "/pds/{did}/xrpc/com.atproto.server.getSession"
        )))
        .and(header(
            "authorization",
            format!("DPoP {access_token}").as_str(),
        ))
        .respond_with(response)
        .with_priority(1)
        .expect(times)
        .mount(&app.mock_server)
        .await;
}

/// A DPoP caller with a procedure script that calls the fixture, whose PDS
/// is mocked at `/pds/{did}`.
async fn session_caller(app: &TestApp, did: &str) -> (String, Value, String) {
    install_caller_fixture(app).await;
    seed_procedure_lexicon(app).await;
    seed_script(app, &format!("xrpc.procedure:{CREATE_GAME}"), REQUIRE).await;
    let session = setup_dpop_session(app, did).await;
    update_session_pds_url(app, did, &format!("{}/pds/{did}", app.mock_server.uri())).await;
    session
}

async fn call_as(
    app: &TestApp,
    (client_key, dpop_key, access_token): &(String, Value, String),
    function: &str,
) -> (StatusCode, Value) {
    let resp = dpop_post(
        app,
        &format!("/xrpc/{CREATE_GAME}"),
        &call(function, json!({})),
        client_key,
        dpop_key,
        access_token,
    )
    .await;
    let status = resp.status();
    (status, response_json(resp).await)
}

fn full_session(did: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "did": did,
        "handle": "session.test",
        "email": "session@example.com",
        "emailConfirmed": false,
        "active": true,
    }))
}

/// The caller's account comes from their own PDS's getSession, as them, and
/// only the DID and handle reach a plugin through `caller:read`, even when the
/// PDS reports the address too.
#[tokio::test]
#[serial]
async fn get_session_reads_the_callers_pds_account() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    const DID: &str = "did:plc:sdkcallersession";
    let session = session_caller(&app, DID).await;
    mount_get_session(&app, DID, &session.2, full_session(DID), 1).await;

    let (status, body) = call_as(&app, &session, "get_session").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"did": DID, "handle": "session.test"}));
}

/// The address comes only through `host_caller_get_email`, behind
/// `caller:email`.
#[tokio::test]
#[serial]
async fn get_email_reads_the_callers_address() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    const DID: &str = "did:plc:sdkcalleremail";
    let session = session_caller(&app, DID).await;
    mount_get_session(&app, DID, &session.2, full_session(DID), 1).await;

    let (status, body) = call_as(&app, &session, "get_email").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({"email": "session@example.com", "email_confirmed": false})
    );
}

/// A sign-in granted neither `transition:email` nor `account:email` gets no
/// address from the PDS, and that is an empty answer, not an error.
#[tokio::test]
#[serial]
async fn get_email_is_empty_when_the_pds_withholds_it() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    const DID: &str = "did:plc:sdkcallernoemail";
    let session = session_caller(&app, DID).await;
    let withheld = ResponseTemplate::new(200)
        .set_body_json(json!({"did": DID, "handle": "session.test", "active": true}));
    mount_get_session(&app, DID, &session.2, withheld, 1).await;

    let (status, body) = call_as(&app, &session, "get_email").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({}));
}

/// A PDS answering about another account is refused by both calls, and
/// nothing it said reaches the guest.
#[tokio::test]
#[serial]
async fn a_session_naming_another_account_is_refused() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    const DID: &str = "did:plc:sdkcallerimpostor";
    let session = session_caller(&app, DID).await;
    let other = ResponseTemplate::new(200).set_body_json(json!({
        "did": "did:plc:someoneelse", "handle": "else.test", "email": "else@example.com",
    }));
    mount_get_session(&app, DID, &session.2, other, 2).await;

    for function in ["get_session", "get_email"] {
        let (status, body) = call_as(&app, &session, function).await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{function}: {body}"
        );
        let message = body["message"].as_str().unwrap_or_default();
        assert!(message.contains("PDS_ERROR"), "{function}: {message}");
        assert!(
            !message.contains("else@example.com"),
            "{function}: {message}"
        );
        assert!(!message.contains("else.test"), "{function}: {message}");
    }
}

/// A PDS's refusal reaches the guest with its status.
#[tokio::test]
#[serial]
async fn get_session_errors_carry_the_pds_status() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    const DID: &str = "did:plc:sdkcallerpdsdown";
    let session = session_caller(&app, DID).await;
    mount_get_session(
        &app,
        DID,
        &session.2,
        ResponseTemplate::new(503).set_body_string("Unavailable"),
        2,
    )
    .await;

    for function in ["get_session", "get_email"] {
        let (status, body) = call_as(&app, &session, function).await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{function}: {body}"
        );
        let message = body["message"].as_str().unwrap_or_default();
        assert!(message.contains("PDS_ERROR"), "{function}: {message}");
        assert!(message.contains("503"), "{function}: {message}");
    }
}

/// No caller, as for a query, record-event or label script or a job without
/// `auth`: nothing to read the account with.
#[tokio::test]
#[serial]
async fn account_calls_with_no_session_are_refused() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    install_caller_fixture(&app).await;

    for function in ["get_session", "get_email"] {
        let err = app
            .state
            .plugin_executor()
            .call_library(
                "sdk_caller",
                function,
                &[json!({})],
                &LibraryCallContext::default(),
                0,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("NO_SESSION"), "{function}: {err}");
    }
}

/// A plugin that imports the email call without declaring `caller:email`
/// is refused at load, so an update can't start reading addresses under the
/// consent an operator gave to `caller:read`.
#[tokio::test]
async fn the_email_call_needs_caller_email_declared() {
    let fixture = std::path::Path::new("tests/fixtures/sdk_caller");
    let mut manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(fixture.join("manifest.json")).unwrap())
            .unwrap();
    let wasm = std::fs::canonicalize(fixture.join(manifest["wasm_file"].as_str().unwrap())).expect(
        "sdk_caller fixture not built. Run: cargo build --manifest-path \
             tests/fixtures/sdk_caller/Cargo.toml --target wasm32-unknown-unknown --release",
    );
    manifest["wasm_file"] = json!(wasm.to_str().unwrap());
    manifest["capabilities"]
        .as_array_mut()
        .unwrap()
        .retain(|c| c != "caller:email");
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();

    let err = loader::load_from_file(dir.path())
        .await
        .err()
        .expect("loading an undeclared email import must fail")
        .to_string();
    assert!(err.contains("host_caller_get_email"), "{err}");
    assert!(err.contains("caller:email"), "{err}");
}

/// A delegated request acts for another account, but the session it reads is
/// still the actor's own: the account and address handed out are theirs, and
/// the delegated account's PDS is never asked.
#[tokio::test]
#[serial]
async fn a_delegated_request_reads_the_actors_account() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    const OWNER: &str = "did:plc:sdkcallerowner";
    const STUDIO: &str = "did:plc:sdkcallerstudio";
    install_caller_fixture(&app).await;
    seed_procedure_lexicon(&app).await;
    seed_script(&app, &format!("xrpc.procedure:{CREATE_GAME}"), REQUIRE).await;
    let (client_key, client_secret, dpop_key, access_token) =
        setup_dpop_session_with_secret(&app, OWNER).await;
    update_session_pds_url(
        &app,
        OWNER,
        &format!("{}/pds/{OWNER}", app.mock_server.uri()),
    )
    .await;
    let (client_key, dpop_key, access_token) = (&client_key, &dpop_key, &access_token);
    link_account(
        &app,
        client_key,
        &client_secret,
        dpop_key,
        access_token,
        STUDIO,
    )
    .await;
    update_session_pds_url(
        &app,
        STUDIO,
        &format!("{}/pds/{STUDIO}", app.mock_server.uri()),
    )
    .await;
    mount_get_session(&app, OWNER, access_token, full_session(OWNER), 2).await;
    Mock::given(path(format!(
        "/pds/{STUDIO}/xrpc/com.atproto.server.getSession"
    )))
    .respond_with(full_session(STUDIO))
    .with_priority(1)
    .expect(0)
    .mount(&app.mock_server)
    .await;

    for (function, expected) in [
        (
            "get_session",
            json!({"did": OWNER, "handle": "session.test"}),
        ),
        (
            "get_email",
            json!({"email": "session@example.com", "email_confirmed": false}),
        ),
    ] {
        let mut body = call(function, json!({}));
        body["delegateDid"] = json!(STUDIO);
        let resp = dpop_post(
            &app,
            &format!("/xrpc/{CREATE_GAME}"),
            &body,
            client_key,
            dpop_key,
            access_token,
        )
        .await;
        let status = resp.status();
        let body = response_json(resp).await;
        assert_eq!(status, StatusCode::OK, "{function}: {body}");
        assert_eq!(body, expected, "{function}");
    }
}

/// Register a session for `target_did` under the caller's API client and link
/// it to the caller, as `tests/e2e_delegation.rs`'s `setup_linked_account`
/// does.
async fn link_account(
    app: &TestApp,
    client_key: &str,
    client_secret: &str,
    dpop_key: &Value,
    access_token: &str,
    target_did: &str,
) {
    let key_req = post_json_with_headers(
        "/oauth/dpop-keys",
        &json!({}),
        vec![
            ("x-client-key", client_key),
            ("x-client-secret", client_secret),
        ],
    );
    let key_resp = app.router.clone().oneshot(key_req).await.unwrap();
    assert_eq!(key_resp.status(), StatusCode::CREATED);
    let provision_id = response_json(key_resp).await["provision_id"]
        .as_str()
        .unwrap()
        .to_string();
    app.mock_session_verification(target_did, target_did).await;
    let session_req = post_json_with_headers(
        "/oauth/sessions",
        &json!({
            "provision_id": provision_id,
            "did": target_did,
            "access_token": format!("test-target-access-{}", uuid::Uuid::new_v4()),
            "scopes": "atproto",
            "pds_url": "https://pds.example.com",
        }),
        vec![
            ("x-client-key", client_key),
            ("x-client-secret", client_secret),
        ],
    );
    let session_resp = app.router.clone().oneshot(session_req).await.unwrap();
    assert_eq!(session_resp.status(), StatusCode::CREATED);

    let resp = dpop_post(
        app,
        "/xrpc/dev.happyview.delegation.linkAccount",
        &json!({ "did": target_did }),
        client_key,
        dpop_key,
        access_token,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED, "linkAccount failed");
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
    seed_script(&app, &format!("xrpc.query:{LIST_GAMES}"), "value:other").await;
    seed_script(&app, &format!("xrpc.procedure:{CREATE_GAME}"), REQUIRE).await;

    const DID: &str = "did:plc:sdkcallerreader";
    let (client_key, dpop_key, access_token) = setup_dpop_session(&app, DID).await;

    let resp = dpop_post(
        &app,
        &format!("/xrpc/{CREATE_GAME}"),
        &call(
            "xrpc_query",
            json!({ "method": LIST_GAMES, "params": { "ok": true } }),
        ),
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
    seed_script(&app, &format!("xrpc.query:{LIST_GAMES}"), "value:other").await;

    let result = app
        .state
        .plugin_executor()
        .call_library(
            "sdk_caller",
            "xrpc_query",
            &[json!({ "method": LIST_GAMES, "params": { "ok": true } })],
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
    assert_eq!(row["record"]["title"], "Indexed");
}

// ---------------------------------------------------------------------------
// Mirroring: a write through the caller lands in the index at once
// ---------------------------------------------------------------------------

async fn indexed(app: &TestApp, uri: &str) -> Option<Value> {
    happyview::plugin::host::records_get(&app.state.db, app.state.db_backend, uri)
        .await
        .unwrap()
}

async fn ref_targets(app: &TestApp, source_uri: &str) -> Vec<String> {
    let sql = happyview::db::adapt_sql(
        "SELECT target_uri FROM happyview_record_refs WHERE source_uri = ? ORDER BY target_uri",
        app.state.db_backend,
    );
    let rows: Vec<(String,)> = happyview::db::query_as(&sql)
        .bind(source_uri)
        .fetch_all(&app.state.db)
        .await
        .unwrap();
    rows.into_iter().map(|(t,)| t).collect()
}

async fn set_indexed_at(app: &TestApp, uri: &str, indexed_at: &str) {
    let sql = happyview::db::adapt_sql(
        "UPDATE happyview_records SET indexed_at = ? WHERE uri = ?",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(indexed_at)
        .bind(uri)
        .execute(&app.state.db)
        .await
        .unwrap();
}

/// Mount a PDS write endpoint for `did` that answers `response` once.
async fn mount_pds_write(
    app: &TestApp,
    did: &str,
    method: &str,
    access_token: &str,
    response: Value,
) {
    Mock::given(wiremock::matchers::method("POST"))
        .and(path(format!("/pds/{did}/xrpc/com.atproto.repo.{method}")))
        .and(header(
            "authorization",
            format!("DPoP {access_token}").as_str(),
        ))
        .and(body_partial_json(json!({ "repo": did })))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .expect(1)
        .mount(&app.mock_server)
        .await;
}

/// Make the index refuse rows in `collection`, so a PDS write can succeed
/// while its mirror cannot. `allow_collection` undoes it; both run at the
/// start of the test that needs them so an earlier panic cannot leave the
/// probe behind.
async fn refuse_collection(app: &TestApp, collection: &str) {
    let sql = match app.state.db_backend {
        happyview::db::DatabaseBackend::Postgres => {
            // A CHECK cannot hold a subquery: intern the collection here and
            // name its id.
            happyview::db::query(
                "INSERT INTO happyview_collections (nsid) VALUES ($1) ON CONFLICT (nsid) DO NOTHING",
            )
            .bind(collection)
            .execute(&app.state.db)
            .await
            .unwrap();
            let (id,): (i64,) =
                happyview::db::query_as("SELECT id FROM happyview_collections WHERE nsid = $1")
                    .bind(collection)
                    .fetch_one(&app.state.db)
                    .await
                    .unwrap();
            format!(
                "ALTER TABLE happyview_records_v2 ADD CONSTRAINT mirror_probe CHECK (collection_id <> {id})"
            )
        }
        happyview::db::DatabaseBackend::Sqlite => format!(
            "CREATE TRIGGER mirror_probe BEFORE INSERT ON happyview_records_v2 \
             WHEN NEW.collection_id = (SELECT id FROM happyview_collections WHERE nsid = '{collection}') \
             BEGIN SELECT RAISE(ABORT, 'mirror probe'); END"
        ),
    };
    happyview::db::query(&sql)
        .execute(&app.state.db)
        .await
        .unwrap();
}

async fn allow_collection(app: &TestApp) {
    let sql = match app.state.db_backend {
        happyview::db::DatabaseBackend::Postgres => {
            "ALTER TABLE happyview_records_v2 DROP CONSTRAINT IF EXISTS mirror_probe"
        }
        happyview::db::DatabaseBackend::Sqlite => "DROP TRIGGER IF EXISTS mirror_probe",
    };
    happyview::db::query(sql)
        .execute(&app.state.db)
        .await
        .unwrap();
}

#[tokio::test]
#[serial]
async fn a_created_record_is_mirrored_into_the_index() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    install_caller_fixture(&app).await;
    seed_procedure_lexicon(&app).await;
    seed_script(&app, &format!("xrpc.procedure:{CREATE_GAME}"), REQUIRE).await;

    const DID: &str = "did:plc:sdkcallermirror";
    let (client_key, dpop_key, access_token) = setup_dpop_session(&app, DID).await;
    update_session_pds_url(&app, DID, &format!("{}/pds/{DID}", app.mock_server.uri())).await;
    let uri = format!("at://{DID}/com.example.post/abc");
    mount_pds_write(
        &app,
        DID,
        "createRecord",
        &access_token,
        json!({ "uri": uri, "cid": "bafyreicaller" }),
    )
    .await;

    let resp = dpop_post(
        &app,
        &format!("/xrpc/{CREATE_GAME}"),
        &call("create_record", json!({ "collection": "com.example.post", "record": { "text": "hello", "subject": "at://did:plc:other/com.example.post/parent" }, "validate": false })),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    let status = resp.status();
    let body = response_json(resp).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let row = indexed(&app, &uri)
        .await
        .expect("the write is in the index");
    assert_eq!(row["did"], DID);
    assert_eq!(row["collection"], "com.example.post");
    assert_eq!(row["rkey"], "abc");
    assert_eq!(row["cid"], "bafyreicaller");
    assert!(row["indexed_at"].is_null(), "{row}");
    assert_eq!(
        row["record"],
        json!({
            "$type": "com.example.post",
            "text": "hello",
            "subject": "at://did:plc:other/com.example.post/parent",
        })
    );
    assert_eq!(
        ref_targets(&app, &uri).await,
        vec!["at://did:plc:other/com.example.post/parent"]
    );
}

#[tokio::test]
#[serial]
async fn a_put_record_replaces_the_mirrored_cid_and_clears_indexed_at() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    install_caller_fixture(&app).await;
    seed_procedure_lexicon(&app).await;
    seed_script(&app, &format!("xrpc.procedure:{CREATE_GAME}"), REQUIRE).await;

    const DID: &str = "did:plc:sdkcallerputter";
    let (client_key, dpop_key, access_token) = setup_dpop_session(&app, DID).await;
    update_session_pds_url(&app, DID, &format!("{}/pds/{DID}", app.mock_server.uri())).await;
    let uri = format!("at://{DID}/com.example.post/abc");
    happyview::plugin::host::index_put(
        &app.state.records,
        happyview_plugin_sdk::wire::IndexPut {
            collection: "com.example.post".into(),
            rkey: "abc".into(),
            did: Some(DID.into()),
            record: json!({ "text": "first" }),
            cid: Some("bafyold".into()),
        },
    )
    .await
    .unwrap();
    set_indexed_at(&app, &uri, "2026-01-01T00:00:00+00:00").await;
    mount_pds_write(
        &app,
        DID,
        "putRecord",
        &access_token,
        json!({ "uri": uri, "cid": "bafynew" }),
    )
    .await;

    let resp = dpop_post(
        &app,
        &format!("/xrpc/{CREATE_GAME}"),
        &call(
            "put_record",
            json!({ "uri": uri, "record": { "text": "second" }, "validate": false }),
        ),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    let status = resp.status();
    let body = response_json(resp).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["cid"], "bafynew");

    let row = indexed(&app, &uri).await.expect("the row is still there");
    assert_eq!(row["cid"], "bafynew");
    // The PDS write comes back over Jetstream, and that echo stamps the
    // arrival time. Until it does, the mirrored row has none.
    assert!(row["indexed_at"].is_null(), "{row}");
    assert_eq!(
        row["record"],
        json!({ "$type": "com.example.post", "text": "second" })
    );
}

#[tokio::test]
#[serial]
async fn a_deleted_record_leaves_the_index_with_its_refs() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    install_caller_fixture(&app).await;
    seed_procedure_lexicon(&app).await;
    seed_script(&app, &format!("xrpc.procedure:{CREATE_GAME}"), REQUIRE).await;

    const DID: &str = "did:plc:sdkcallerdeleter";
    let (client_key, dpop_key, access_token) = setup_dpop_session(&app, DID).await;
    update_session_pds_url(&app, DID, &format!("{}/pds/{DID}", app.mock_server.uri())).await;
    let uri = format!("at://{DID}/com.example.post/abc");
    happyview::plugin::host::index_put(
        &app.state.records,
        happyview_plugin_sdk::wire::IndexPut {
            collection: "com.example.post".into(),
            rkey: "abc".into(),
            did: Some(DID.into()),
            record: json!({ "text": "bye", "subject": "at://did:plc:other/com.example.post/parent" }),
            cid: Some("bafyold".into()),
        },
    )
    .await
    .unwrap();
    assert_eq!(ref_targets(&app, &uri).await.len(), 1);
    mount_pds_write(&app, DID, "deleteRecord", &access_token, json!({})).await;

    let resp = dpop_post(
        &app,
        &format!("/xrpc/{CREATE_GAME}"),
        &call("delete_record", json!({ "uri": uri })),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    let status = resp.status();
    let body = response_json(resp).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert!(indexed(&app, &uri).await.is_none());
    assert!(ref_targets(&app, &uri).await.is_empty());
}

/// The PDS accepted the write, so the script gets its reference whatever the
/// index did with it: a mirror that cannot land is the index's problem until
/// Jetstream catches it up, never the script's.
#[tokio::test]
#[serial]
async fn a_mirror_failure_leaves_the_write_successful() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    install_caller_fixture(&app).await;
    seed_procedure_lexicon(&app).await;
    seed_script(&app, &format!("xrpc.procedure:{CREATE_GAME}"), REQUIRE).await;

    const DID: &str = "did:plc:sdkcallerunmirrored";
    let (client_key, dpop_key, access_token) = setup_dpop_session(&app, DID).await;
    update_session_pds_url(&app, DID, &format!("{}/pds/{DID}", app.mock_server.uri())).await;
    let uri = format!("at://{DID}/com.example.unmirrorable/abc");
    mount_pds_write(
        &app,
        DID,
        "createRecord",
        &access_token,
        json!({ "uri": uri, "cid": "bafyreicaller" }),
    )
    .await;

    allow_collection(&app).await;
    refuse_collection(&app, "com.example.unmirrorable").await;
    let resp = dpop_post(
        &app,
        &format!("/xrpc/{CREATE_GAME}"),
        &call("create_record", json!({ "collection": "com.example.unmirrorable", "record": { "text": "hello" }, "validate": false })),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    let status = resp.status();
    let body = response_json(resp).await;
    let row = indexed(&app, &uri).await;
    allow_collection(&app).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["uri"], uri);
    assert_eq!(body["cid"], "bafyreicaller");
    assert!(
        row.is_none(),
        "the probe should have refused the mirror: {row:?}"
    );
}

// ---------------------------------------------------------------------------
// A test run: every write answers from the in-process PDS
// ---------------------------------------------------------------------------

/// Give `app` a test harness. The session's PDS points at the mock server with
/// nothing mounted, so a write that leaves the process fails the test.
async fn into_test_run(app: &mut TestApp) -> std::sync::Arc<happyview::testing::TestHarness> {
    let harness = std::sync::Arc::new(happyview::testing::TestHarness::new("a.test.lua"));
    app.state.test = Some(harness.clone());
    app.rebuild_router();
    harness
}

#[tokio::test]
#[serial]
async fn a_test_runs_procedure_script_writes_through_the_in_process_pds() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    install_caller_fixture(&app).await;
    seed_procedure_lexicon(&app).await;
    seed_script(&app, &format!("xrpc.procedure:{CREATE_GAME}"), REQUIRE).await;

    const DID: &str = "did:plc:sdkcallertestrun";
    let (client_key, dpop_key, access_token) = setup_dpop_session(&app, DID).await;
    update_session_pds_url(&app, DID, &format!("{}/pds/{DID}", app.mock_server.uri())).await;
    let harness = into_test_run(&mut app).await;

    let resp = dpop_post(
        &app,
        &format!("/xrpc/{CREATE_GAME}"),
        &call(
            "create_record",
            json!({ "collection": "com.example.post", "rkey": "abc", "record": { "text": "hello" }, "validate": false }),
        ),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    let status = resp.status();
    let body = response_json(resp).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let writes = harness.pds().writes();
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert_eq!(writes[0].repo, DID);
    assert_eq!(body["cid"].as_str(), writes[0].cid.as_deref());
    let row = indexed(&app, &format!("at://{DID}/com.example.post/abc"))
        .await
        .expect("the write is in the index");
    assert_eq!(row["cid"], body["cid"]);
}

/// A procedure with a lexicon and no script writes through the in-process
/// PDS and mirrors what it answered, as the OAuth path does.
#[tokio::test]
#[serial]
async fn a_test_runs_scriptless_procedure_writes_through_the_in_process_pds() {
    common::require_db!();
    let mut app = TestApp::new_with_encryption().await;
    seed_procedure_lexicon(&app).await;

    const DID: &str = "did:plc:scriptlesstestrun";
    let (client_key, dpop_key, access_token) = setup_dpop_session(&app, DID).await;
    update_session_pds_url(&app, DID, &format!("{}/pds/{DID}", app.mock_server.uri())).await;
    let harness = into_test_run(&mut app).await;

    let resp = dpop_post(
        &app,
        &format!("/xrpc/{CREATE_GAME}"),
        &json!({ "name": "Tetris" }),
        &client_key,
        &dpop_key,
        &access_token,
    )
    .await;
    let status = resp.status();
    let body = response_json(resp).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let writes = harness.pds().writes();
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert_eq!(writes[0].action, "create");
    assert_eq!(writes[0].repo, DID);
    assert_eq!(writes[0].collection, "games.gamesgamesgamesgames.game");
    let uri = body["uri"].as_str().expect("the PDS's uri is answered");
    let row = indexed(&app, uri).await.expect("the write is in the index");
    assert_eq!(row["cid"], body["cid"]);
    assert_eq!(row["record"]["name"], "Tetris");
}

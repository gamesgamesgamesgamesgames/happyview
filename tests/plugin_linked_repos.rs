//! End-to-end coverage of the seven linked-repo/jobs host imports, through
//! the executor and the `sdk_linked_repos` fixture rather than the bare Rust
//! functions in `src/plugin/host/linked_repos.rs` and
//! `src/plugin/host/jobs.rs` (already unit-tested there). A successful
//! linked-repo write against a mocked PDS is out of scope: `linked_repos::pds`
//! has no test harness today, and this file does not change that. What it
//! pins instead is that the capability gate and the grant/session lookups
//! are reachable from a real WASM module, and that `jobs_create` carries a
//! real DPoP session's identifiers through a real procedure script.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;

use happyview::linked_repos::db;
use happyview::oauth::pds_write::generate_dpop_proof;
use happyview::plugin::library::LibraryCallContext;
use happyview::plugin::loader;
use happyview::plugin::{LoadedPlugin, PluginInfo, PluginManifest, PluginSource};

const FIXTURE: &str =
    "tests/fixtures/sdk_linked_repos/target/wasm32-unknown-unknown/release/sdk_linked_repos.wasm";

fn fixture_bytes() -> Vec<u8> {
    std::fs::read(FIXTURE).expect(
        "linked repos fixture not built. Run: cargo build --manifest-path tests/fixtures/sdk_linked_repos/Cargo.toml --target wasm32-unknown-unknown --release",
    )
}

/// The fixture registered as a library under `id`, with `capabilities`.
/// `plugin_registry.register` never runs the loader's import check, so this
/// also registers manifests missing a capability the fixture still imports,
/// to pin that the host function itself refuses the call rather than
/// trusting whatever the module happened to import.
fn linked_repos_plugin(id: &str, capabilities: &[&str]) -> LoadedPlugin {
    let manifest: PluginManifest = serde_json::from_value(json!({
        "id": id, "name": id, "version": "1.0.0", "api_version": "2",
        "plugin_type": "library", "namespace": id,
        "capabilities": capabilities,
    }))
    .unwrap();
    LoadedPlugin {
        info: PluginInfo {
            id: id.into(),
            name: id.into(),
            version: "1.0.0".into(),
            api_version: "2".into(),
            icon_url: None,
            required_secrets: vec![],
            auth_type: "oauth2".into(),
            config_schema: None,
        },
        source: PluginSource::File {
            path: "tests/fixtures/sdk_linked_repos".into(),
        },
        wasm_bytes: fixture_bytes(),
        manifest: Some(manifest),
    }
}

/// Create a grant, bind it to `did`, and optionally push it into
/// `needs_reauth` — the same sequence a stale linked-repo session goes
/// through in production.
async fn seed_grant(app: &TestApp, did: &str, scopes: &str, needs_reauth: bool) {
    let grant = db::create(&app.state, None, None, None, scopes, "admin")
        .await
        .expect("create grant");
    db::bind_did(&app.state, &grant.id, did, None)
        .await
        .expect("bind did");
    if needs_reauth {
        db::mark_needs_reauth(&app.state, &grant.id, "test reauth")
            .await
            .expect("mark needs_reauth");
    }
}

// ---------------------------------------------------------------------------
// `list`, `create_record` grant-lookup outcomes — no session needed, since
// every spec here names its own DID rather than acting as the script runner.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_returns_seeded_grants() {
    common::require_db!();
    let app = TestApp::new().await;
    seed_grant(&app, "did:plc:seeded", "repo:*", false).await;

    app.state
        .plugin_registry
        .register(linked_repos_plugin(
            "sdk_linked_repos",
            &["linked_repos:use", "jobs:create"],
        ))
        .await;
    let executor = app.state.plugin_executor();

    let out = executor
        .call_library(
            "sdk_linked_repos",
            "list",
            &[],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap();
    let grants = out.as_array().expect("list returns an array");
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0]["did"], "did:plc:seeded");
    assert_eq!(grants[0]["scopes"], "repo:*");
}

#[tokio::test]
async fn create_record_for_an_unlinked_did_is_not_linked() {
    common::require_db!();
    let app = TestApp::new().await;

    app.state
        .plugin_registry
        .register(linked_repos_plugin(
            "sdk_linked_repos",
            &["linked_repos:use", "jobs:create"],
        ))
        .await;
    let executor = app.state.plugin_executor();

    let err = executor
        .call_library(
            "sdk_linked_repos",
            "create_record",
            &[json!({
                "did": "did:plc:nobody",
                "collection": "com.example.note",
                "record": {},
            })],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("NOT_LINKED"), "{err}");
}

#[tokio::test]
async fn create_record_without_the_collection_scope_is_a_scope_error() {
    common::require_db!();
    let app = TestApp::new().await;
    seed_grant(&app, "did:plc:scoped", "repo:com.example.other", false).await;

    app.state
        .plugin_registry
        .register(linked_repos_plugin(
            "sdk_linked_repos",
            &["linked_repos:use", "jobs:create"],
        ))
        .await;
    let executor = app.state.plugin_executor();

    let err = executor
        .call_library(
            "sdk_linked_repos",
            "create_record",
            &[json!({
                "did": "did:plc:scoped",
                "collection": "com.example.note",
                "record": {},
            })],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("SCOPE"), "{err}");
}

#[tokio::test]
async fn create_record_on_a_needs_reauth_grant_needs_reauth() {
    common::require_db!();
    let app = TestApp::new().await;
    seed_grant(&app, "did:plc:stale", "repo:*", true).await;

    app.state
        .plugin_registry
        .register(linked_repos_plugin(
            "sdk_linked_repos",
            &["linked_repos:use", "jobs:create"],
        ))
        .await;
    let executor = app.state.plugin_executor();

    let err = executor
        .call_library(
            "sdk_linked_repos",
            "create_record",
            &[json!({
                "did": "did:plc:stale",
                "collection": "com.example.note",
                "record": {},
            })],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("NEEDS_REAUTH"), "{err}");
}

// ---------------------------------------------------------------------------
// `jobs_create` outcomes reachable straight through the executor, with no
// script and no real DPoP session in play.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn jobs_create_with_a_reserved_job_type_is_bad_input() {
    common::require_db!();
    let app = TestApp::new().await;

    app.state
        .plugin_registry
        .register(linked_repos_plugin(
            "sdk_linked_repos",
            &["linked_repos:use", "jobs:create"],
        ))
        .await;
    let executor = app.state.plugin_executor();

    let ctx = LibraryCallContext {
        caller_did: Some("did:plc:caller".into()),
        ..LibraryCallContext::default()
    };
    let err = executor
        .call_library(
            "sdk_linked_repos",
            "jobs_create",
            &[json!({"job_type": "happyview.x", "input": {}, "auth": false})],
            &ctx,
            0,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("BAD_INPUT"), "{err}");
}

#[tokio::test]
async fn jobs_create_with_auth_and_no_session_has_no_session() {
    common::require_db!();
    let app = TestApp::new().await;

    app.state
        .plugin_registry
        .register(linked_repos_plugin(
            "sdk_linked_repos",
            &["linked_repos:use", "jobs:create"],
        ))
        .await;
    let executor = app.state.plugin_executor();

    // `call_library` (as opposed to `call_library_as`) always passes `None`
    // for the caller session, so a runner identified only by `caller_did` in
    // the call context — no `CallerSession` behind it — is exactly what a
    // library sees when it has no session to lend, even though it knows who
    // is asking.
    let ctx = LibraryCallContext {
        caller_did: Some("did:plc:caller".into()),
        ..LibraryCallContext::default()
    };
    let err = executor
        .call_library(
            "sdk_linked_repos",
            "jobs_create",
            &[json!({"job_type": "test.no-session", "input": {}, "auth": true})],
            &ctx,
            0,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("NO_SESSION"), "{err}");
}

// ---------------------------------------------------------------------------
// Capability gate: each import needs its own capability, checked at the host
// function itself. Registering the fixture with only one of the two
// capabilities pins that the other import is refused regardless of what the
// module actually imports.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_record_without_linked_repos_use_is_forbidden() {
    common::require_db!();
    let app = TestApp::new().await;
    seed_grant(&app, "did:plc:scoped", "repo:*", false).await;

    app.state
        .plugin_registry
        .register(linked_repos_plugin(
            "sdk_linked_repos_jobs_only",
            &["jobs:create"],
        ))
        .await;
    let executor = app.state.plugin_executor();

    let err = executor
        .call_library(
            "sdk_linked_repos_jobs_only",
            "create_record",
            &[json!({
                "did": "did:plc:scoped",
                "collection": "com.example.note",
                "record": {},
            })],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("FORBIDDEN"), "{err}");
}

#[tokio::test]
async fn jobs_create_without_jobs_create_capability_is_forbidden() {
    common::require_db!();
    let app = TestApp::new().await;

    app.state
        .plugin_registry
        .register(linked_repos_plugin(
            "sdk_linked_repos_repos_only",
            &["linked_repos:use"],
        ))
        .await;
    let executor = app.state.plugin_executor();

    let ctx = LibraryCallContext {
        caller_did: Some("did:plc:caller".into()),
        ..LibraryCallContext::default()
    };
    let err = executor
        .call_library(
            "sdk_linked_repos_repos_only",
            "jobs_create",
            &[json!({"job_type": "test.forbidden", "input": {}, "auth": false})],
            &ctx,
            0,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("FORBIDDEN"), "{err}");
}

// ---------------------------------------------------------------------------
// `jobs_create` through a real DPoP-authenticated procedure script — the only
// path that has a real `CallerSession` (and therefore real
// `api_client_id`/`dpop_key_id`) to carry into the job row.
//
// Helpers — copied from `tests/plugin_caller.rs`, which is not a shared module.
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

/// The `happyview_dpop_keys.id` row that `/oauth/sessions` resolves
/// `provision_id` into (`keys::get_dpop_key`) and stores as
/// `happyview_dpop_sessions.dpop_key_id` — a different string from
/// `provision_id` itself, and never returned over HTTP, so a test that wants
/// to assert equality against a stored `dpop_key_id` has to read it here.
async fn dpop_key_id_for_provision(app: &TestApp, provision_id: &str) -> String {
    let sql = happyview::db::adapt_sql(
        "SELECT id FROM happyview_dpop_keys WHERE provision_id = ?",
        app.state.db_backend,
    );
    let (id,): (String,) = happyview::db::query_as(&sql)
        .bind(provision_id)
        .fetch_one(&app.state.db)
        .await
        .expect("dpop key row should exist");
    id
}

/// Set up a full DPoP session and return `(client_key, dpop_key, access_token,
/// api_client_id, dpop_key_id)` — the last two are the identifiers the
/// session was actually registered under, so a caller can assert a job row
/// carries *this* session's ids rather than merely carrying some ids.
async fn setup_dpop_session(
    app: &TestApp,
    user_did: &str,
) -> (String, Value, String, String, String) {
    let (client_key, client_secret, api_client_id) =
        app.create_api_client("confidential", None).await;

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
    let dpop_key_id = dpop_key_id_for_provision(app, &provision_id).await;

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

    (
        client_key,
        dpop_key,
        access_token,
        api_client_id,
        dpop_key_id,
    )
}

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

/// Load and install the `sdk_linked_repos` fixture through the loader, so
/// its manifest's declared capabilities are cross-checked against its actual
/// imports the way an operator's upload would be.
async fn install_linked_repos_fixture(app: &TestApp) {
    let plugin = loader::load_from_file(std::path::Path::new("tests/fixtures/sdk_linked_repos"))
        .await
        .expect(
            "sdk_linked_repos fixture not built. Run: cargo build --manifest-path \
             tests/fixtures/sdk_linked_repos/Cargo.toml --target wasm32-unknown-unknown --release",
        );
    app.state.plugin_registry.install(plugin).await.unwrap();
}

const CREATE_GAME: &str = "games.gamesgamesgamesgames.createGame";

/// `jobs::db` is crate-private, so this reads `happyview_jobs` directly
/// rather than through it — the same thing `src/plugin/host/jobs.rs`'s own
/// unit tests do, since those run inside the crate and could reach it.
async fn job_dpop_fields(app: &TestApp, id: &str) -> (bool, Option<String>, Option<String>) {
    let sql = happyview::db::adapt_sql(
        "SELECT inherit_auth, api_client_id, dpop_key_id FROM happyview_jobs WHERE id = ?",
        app.state.db_backend,
    );
    happyview::db::query_as(&sql)
        .bind(id)
        .fetch_one(&app.state.db)
        .await
        .expect("job row should exist")
}

#[tokio::test]
#[serial]
async fn jobs_create_with_auth_carries_the_sessions_dpop_ids() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    install_linked_repos_fixture(&app).await;
    seed_procedure_lexicon(&app).await;
    seed_script(
        &app,
        &format!("xrpc.procedure:{CREATE_GAME}"),
        "function handle(input, ctx)\n\
           local lr = require(\"linked_fixture\")\n\
           local job_id = lr.jobs_create({ job_type = \"test.with-auth\", input = {}, auth = true })\n\
           return { job_id = job_id }\n\
         end",
    )
    .await;

    const DID: &str = "did:plc:linkedreposjobcaller";
    let (client_key, dpop_key, access_token, session_api_client_id, session_dpop_key_id) =
        setup_dpop_session(&app, DID).await;

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
    let job_id = body["job_id"].as_str().expect("job_id in response");

    let (inherit_auth, api_client_id, dpop_key_id) = job_dpop_fields(&app, job_id).await;
    assert!(inherit_auth);
    // Equality, not just presence — pins that the job row carries *this*
    // session's identifiers rather than merely some non-null ones.
    assert_eq!(api_client_id, Some(session_api_client_id));
    assert_eq!(dpop_key_id, Some(session_dpop_key_id));
}

#[tokio::test]
#[serial]
async fn jobs_create_without_auth_leaves_dpop_ids_null() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    install_linked_repos_fixture(&app).await;
    seed_procedure_lexicon(&app).await;
    seed_script(
        &app,
        &format!("xrpc.procedure:{CREATE_GAME}"),
        "function handle(input, ctx)\n\
           local lr = require(\"linked_fixture\")\n\
           local job_id = lr.jobs_create({ job_type = \"test.without-auth\", input = {}, auth = false })\n\
           return { job_id = job_id }\n\
         end",
    )
    .await;

    const DID: &str = "did:plc:linkedreposjobcaller2";
    let (client_key, dpop_key, access_token, _session_api_client_id, _session_dpop_key_id) =
        setup_dpop_session(&app, DID).await;

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
    let job_id = body["job_id"].as_str().expect("job_id in response");

    let (inherit_auth, api_client_id, dpop_key_id) = job_dpop_fields(&app, job_id).await;
    assert!(!inherit_auth);
    assert!(api_client_id.is_none(), "{api_client_id:?}");
    assert!(dpop_key_id.is_none(), "{dpop_key_id:?}");
}

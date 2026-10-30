//! `HAPPYVIEW_DEFAULT_PLUGINS`: each source is installed once, ever, with
//! the operator's consent to its capabilities, and a failure is retried on
//! the next boot. A "boot" here is `default_plugins::apply`, which is what
//! boot spawns; `run_entrypoint.rs` checks that boot does spawn it.

mod common;

use serde_json::{Value, json};
use serial_test::serial;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

use common::app::TestApp;
use happyview::admin::default_plugins::{APPLIED_KEY, apply};
use happyview::plugin::official_registry::RegistryConfig;

fn module() -> Vec<u8> {
    wat::parse_str(r#"(module (memory (export "memory") 1))"#).unwrap()
}

/// Serve a library plugin `id` at `version` under `/<dir>/`, declaring
/// `wasi:clock`, and answer its manifest URL.
async fn serve(app: &TestApp, dir: &str, id: &str, version: &str) -> String {
    Mock::given(method("GET"))
        .and(path(format!("/{dir}/manifest.json")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": id, "name": id, "version": version, "api_version": "2",
            "plugin_type": "library", "capabilities": ["wasi:clock"],
            "wasm_file": "plugin.wasm",
        })))
        .mount(&app.mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/{dir}/plugin.wasm")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(module(), "application/wasm"))
        .mount(&app.mock_server)
        .await;
    manifest_url(app, dir)
}

fn manifest_url(app: &TestApp, dir: &str) -> String {
    format!("{}/{dir}/manifest.json", app.mock_server.uri())
}

async fn new_app() -> TestApp {
    TestApp::new_with_persisted_plugins(RegistryConfig::production()).await
}

/// Set `HAPPYVIEW_DEFAULT_PLUGINS`, once the app's mock server is there to
/// name.
fn configure(app: &mut TestApp, sources: &[String]) {
    app.state.config.default_plugins = sources.to_vec();
}

async fn applied(app: &TestApp) -> Option<Vec<String>> {
    happyview::admin::settings::get_setting(&app.state.db, APPLIED_KEY, app.state.db_backend)
        .await
        .map(|v| serde_json::from_str(&v).expect("a JSON list"))
}

async fn installed_version(app: &TestApp, id: &str) -> Option<String> {
    app.state
        .plugin_registry
        .get(id)
        .await
        .map(|p| p.info.version.clone())
}

async fn persisted(app: &TestApp, id: &str) -> bool {
    let sql = happyview::db::adapt_sql(
        "SELECT id FROM happyview_plugins WHERE id = ?",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(id)
        .fetch_optional(&app.state.db)
        .await
        .unwrap()
        .is_some()
}

async fn requests_to(app: &TestApp, dir: &str) -> usize {
    app.mock_server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.url.path().starts_with(&format!("/{dir}/")))
        .count()
}

async fn added_event(app: &TestApp, id: &str) -> (Option<String>, Value) {
    let sql = happyview::db::adapt_sql(
        "SELECT actor_did, detail FROM happyview_event_logs \
         WHERE event_type = 'plugin.added' AND subject = ?",
        app.state.db_backend,
    );
    let (actor, detail): (Option<String>, String) = happyview::db::query_as(&sql)
        .bind(id)
        .fetch_one(&app.state.db)
        .await
        .expect("a plugin.added event");
    (actor, serde_json::from_str(&detail).unwrap())
}

#[tokio::test]
#[serial]
async fn a_fresh_instance_installs_its_default_plugins_and_records_them() {
    common::require_db!();
    let mut app = new_app().await;
    let url = serve(&app, "lua", "lua", "1.0.0").await;
    configure(&mut app, std::slice::from_ref(&url));

    apply(&app.state).await;

    assert_eq!(
        installed_version(&app, "lua").await.as_deref(),
        Some("1.0.0")
    );
    assert!(persisted(&app, "lua").await, "the install is persisted");
    assert_eq!(applied(&app).await, Some(vec![url.clone()]));
    // The operator's consent is recorded as an admin's would be.
    let (actor, detail) = added_event(&app, "lua").await;
    assert_eq!(actor, None);
    assert_eq!(detail["url"], url);
    assert_eq!(detail["capabilities"], json!(["wasi:clock"]));
}

#[tokio::test]
#[serial]
async fn a_second_boot_does_nothing() {
    common::require_db!();
    let mut app = new_app().await;
    let url = serve(&app, "lua", "lua", "1.0.0").await;
    configure(&mut app, std::slice::from_ref(&url));

    apply(&app.state).await;
    let fetched = requests_to(&app, "lua").await;
    apply(&app.state).await;

    assert_eq!(
        requests_to(&app, "lua").await,
        fetched,
        "nothing is fetched"
    );
    assert_eq!(applied(&app).await, Some(vec![url]));
}

#[tokio::test]
#[serial]
async fn an_uninstalled_default_plugin_stays_uninstalled() {
    common::require_db!();
    let mut app = new_app().await;
    let url = serve(&app, "lua", "lua", "1.0.0").await;
    configure(&mut app, std::slice::from_ref(&url));
    apply(&app.state).await;

    let (status, body) = app.delete_json("/admin/plugins/lua").await;
    assert_eq!(status, 204, "{body}");
    apply(&app.state).await;

    assert_eq!(installed_version(&app, "lua").await, None);
    assert!(!persisted(&app, "lua").await);
    assert_eq!(applied(&app).await, Some(vec![url]));
}

#[tokio::test]
#[serial]
async fn a_source_that_fails_is_retried_on_the_next_boot() {
    common::require_db!();
    let mut app = new_app().await;
    // Nothing listens on port 1, and nothing is served at `later` yet.
    let unreachable = "http://127.0.0.1:1/lua/manifest.json".to_string();
    let later = manifest_url(&app, "later");
    configure(&mut app, &[unreachable.clone(), later.clone()]);

    apply(&app.state).await;
    assert_eq!(installed_version(&app, "later").await, None);
    assert_eq!(applied(&app).await, None, "nothing is recorded");

    serve(&app, "later", "later", "1.0.0").await;
    apply(&app.state).await;

    assert_eq!(
        installed_version(&app, "later").await.as_deref(),
        Some("1.0.0")
    );
    assert_eq!(applied(&app).await, Some(vec![later]));
    // The instance still answers.
    let (status, _) = app
        .post_json_status("/admin/plugins/preview", json!({}))
        .await;
    assert_eq!(status, 400);
}

#[tokio::test]
#[serial]
async fn a_plugin_the_owner_already_installed_is_only_marked_applied() {
    common::require_db!();
    let mut app = new_app().await;
    let own = serve(&app, "own", "lua", "1.0.0").await;
    let (status, body) = app
        .post_json_status("/admin/plugins", json!({ "url": own }))
        .await;
    assert_eq!(status, 200, "{body}");

    let default = serve(&app, "default", "lua", "2.0.0").await;
    configure(&mut app, std::slice::from_ref(&default));
    apply(&app.state).await;

    assert_eq!(
        installed_version(&app, "lua").await.as_deref(),
        Some("1.0.0"),
        "the owner's install is left alone"
    );
    assert_eq!(applied(&app).await, Some(vec![default]));
}

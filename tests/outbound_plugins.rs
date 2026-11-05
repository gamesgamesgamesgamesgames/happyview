//! `ALLOW_PRIVATE_HOSTS` exempts hosts for the requests plugins and scripts
//! send themselves, and for nothing else. A target of its own, because the
//! policies are installed for the whole process.

use happyview::db::adapt_sql;
use happyview::outbound::{Policy, install_with_plugins};
use happyview::plugin::library::LibraryCallContext;
use happyview::plugin::{LoadedPlugin, PluginManifest, PluginSource};
use happyview::test_support::{migrated_memory_pool, test_state_with_pool};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FIXTURE: &str = "tests/fixtures/sdk_http/target/wasm32-unknown-unknown/release/sdk_http.wasm";

/// The SDK `http` fixture under `capability`, with `allowed_hosts` stored for
/// it when the capability is the restricted one.
async fn http_library(capability: &str, allowed_hosts: &[&str]) -> happyview::AppState {
    let wasm_bytes = tokio::fs::read(FIXTURE)
        .await
        .expect("sdk_http fixture not built. Run: cargo build --manifest-path tests/fixtures/sdk_http/Cargo.toml --target wasm32-unknown-unknown --release");
    let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
        "id": "sdk_http", "name": "SDK HTTP fixture", "version": "1.0.0", "api_version": "2",
        "plugin_type": "library", "namespace": "http",
        "capabilities": [capability], "allowed_hosts": [],
    }))
    .unwrap();
    let plugin = LoadedPlugin {
        info: manifest.clone().into(),
        source: PluginSource::File {
            path: "tests/fixtures/sdk_http".into(),
        },
        wasm_bytes,
        manifest: Some(manifest),
    };
    let pool = migrated_memory_pool().await;
    happyview::db::query(&adapt_sql(
        "INSERT INTO happyview_plugins (id, source, api_version) VALUES (?, 'file', '2')",
        happyview::db::DatabaseBackend::Sqlite,
    ))
    .bind("sdk_http")
    .execute(&pool)
    .await
    .unwrap();
    let state = test_state_with_pool(pool);
    if !allowed_hosts.is_empty() {
        happyview::plugin::config::store_allowed_hosts(
            &state.db,
            state.db_backend,
            "sdk_http",
            &allowed_hosts
                .iter()
                .map(|h| h.to_string())
                .collect::<Vec<_>>(),
        )
        .await
        .unwrap();
    }
    state.plugin_registry.install(plugin).await.unwrap();
    state
}

async fn get_through_plugin(
    state: &happyview::AppState,
    url: &str,
) -> Result<serde_json::Value, String> {
    state
        .plugin_executor()
        .call_library(
            "sdk_http",
            "get",
            &[serde_json::json!(url)],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .map_err(|e| e.to_string())
}

async fn get_through_shared_client(url: &str) -> String {
    let err = happyview::http_retry::shared_client()
        .get(url)
        .send()
        .await
        .expect_err("the shared client must refuse a private host");
    let mut chain = err.to_string();
    let mut source = std::error::Error::source(&err);
    while let Some(s) = source {
        chain.push_str(": ");
        chain.push_str(&s.to_string());
        source = s.source();
    }
    chain
}

#[tokio::test]
async fn a_listed_host_is_reachable_by_plugins_and_by_nothing_else() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/hello"))
        .respond_with(ResponseTemplate::new(200).set_body_string("hi"))
        .mount(&server)
        .await;
    let port = server.address().port();
    let listed = format!("http://localhost:{port}/hello");

    // As `install_for` does for `ALLOW_PRIVATE_HOSTS=localhost` with nothing
    // else configured.
    install_with_plugins(
        Policy::default(),
        Some(Policy::trusting(
            happyview::outbound::private_hosts(Some(" localhost , "))
                .iter()
                .map(String::as_str),
        )),
    );

    // An unrestricted plugin reaches it.
    let unrestricted = http_library("network:request:unrestricted", &[]).await;
    let out = get_through_plugin(&unrestricted, &listed)
        .await
        .expect("a listed host is reachable from an unrestricted plugin");
    assert_eq!(out["status"], 200);

    // A restricted plugin reaches it only when `allowed_hosts` names it too.
    let restricted = http_library("network:request:defined", &["localhost"]).await;
    let out = get_through_plugin(&restricted, &listed)
        .await
        .expect("a listed and allowed host is reachable from a restricted plugin");
    assert_eq!(out["status"], 200);
    let not_allowed = http_library("network:request:defined", &["example.com"]).await;
    get_through_plugin(&not_allowed, &listed)
        .await
        .expect_err("allowed_hosts still applies");

    // Everything else keeps refusing it, by name and by the address the
    // plugin policy resolved it to.
    let by_name = get_through_shared_client(&listed).await;
    assert!(
        by_name.contains("localhost resolves to no public address"),
        "{by_name}"
    );
    let by_address = get_through_shared_client(&format!("http://127.0.0.1:{port}/hello")).await;
    assert!(
        by_address.contains("127.0.0.1 is not a public address"),
        "{by_address}"
    );
    let no_redirect = happyview::http_retry::build_no_redirect_client("test")
        .get(&listed)
        .send()
        .await
        .expect_err("the no-redirect client blob_download uses must refuse it too");
    assert!(
        format!("{no_redirect:?}").contains("no public address"),
        "{no_redirect:?}"
    );

    assert_eq!(
        server.received_requests().await.unwrap().len(),
        2,
        "only the two plugin requests that were allowed arrive"
    );
}

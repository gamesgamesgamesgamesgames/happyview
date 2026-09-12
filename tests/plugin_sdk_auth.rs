//! The SDK-built auth fixture, end to end: loaded from `tests/fixtures/sdk_auth`
//! and driven through all four of the host's auth entry points. This is what
//! pins `auth_plugin!`'s wire shapes against the host's — a mismatch in either
//! direction fails here rather than in production against a real provider.

use std::collections::HashMap;

use happyview::plugin::TokenSetExt;
use happyview::plugin::loader;
use happyview::test_support::{memory_pool, test_state_with_pool};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn state_with_auth() -> happyview::AppState {
    let plugin = loader::load_from_file(std::path::Path::new("tests/fixtures/sdk_auth"))
        .await
        .expect("sdk_auth fixture not built. Run: cargo build --manifest-path tests/fixtures/sdk_auth/Cargo.toml --target wasm32-unknown-unknown --release");
    let state = test_state_with_pool(memory_pool().await);
    state.plugin_registry.install(plugin).await.unwrap();
    state
}

fn secrets() -> HashMap<String, String> {
    HashMap::from([("API_KEY".to_string(), "k".to_string())])
}

#[tokio::test]
async fn authorize_url_carries_the_state_and_redirect_uri() {
    let state = state_with_auth().await;
    let mut instance = state
        .plugin_executor()
        .instantiate(
            "sdk_auth",
            "user:did:plc:test",
            secrets(),
            serde_json::Value::Null,
        )
        .await
        .unwrap();

    let url = instance
        .call_get_authorize_url("s1", "https://app.test/cb", &serde_json::json!({}))
        .await
        .unwrap();

    assert!(url.contains("state=s1"), "{url}");
    assert!(url.contains("redirect=https://app.test/cb"), "{url}");
}

#[tokio::test]
async fn handle_callback_reads_a_flattened_query_parameter() {
    let state = state_with_auth().await;
    let mut instance = state
        .plugin_executor()
        .instantiate(
            "sdk_auth",
            "user:did:plc:test",
            secrets(),
            serde_json::Value::Null,
        )
        .await
        .unwrap();

    let params = HashMap::from([
        ("code".to_string(), "abc123".to_string()),
        ("state".to_string(), "s1".to_string()),
    ]);
    let tokens = instance
        .call_handle_callback(&params, &serde_json::json!({}))
        .await
        .unwrap();

    assert_eq!(tokens.access_token, "abc123");
    assert_eq!(tokens.refresh_token.as_deref(), Some("refresh-abc123"));
    assert_eq!(tokens.token_type, "Bearer");
    // The fixture sets no expiry, and an omitted field must decode as absent
    // rather than failing the host's `chrono` parse.
    assert_eq!(tokens.expires_at, None);
}

#[tokio::test]
async fn refresh_tokens_round_trips_the_refresh_token() {
    let state = state_with_auth().await;
    let mut instance = state
        .plugin_executor()
        .instantiate(
            "sdk_auth",
            "user:did:plc:test",
            secrets(),
            serde_json::Value::Null,
        )
        .await
        .unwrap();

    let tokens = instance
        .call_refresh_tokens("r1", &serde_json::json!({}))
        .await
        .unwrap();

    assert_eq!(tokens.access_token, "access-r1");
    assert_eq!(tokens.token_type, "Bearer");
    // The fixture returns `expires_in` rather than an absolute `expires_at`,
    // so `expires_at` itself comes back absent...
    assert_eq!(tokens.expires_at, None);
    assert_eq!(tokens.expires_in, Some(3600));
    // ...but the host can still derive an absolute, future instant from it.
    let resolved = tokens
        .resolved_expires_at()
        .expect("a duration should parse")
        .expect("expires_in should derive an expiry");
    assert!(
        resolved > chrono::Utc::now(),
        "derived expiry must be in the future"
    );
}

#[tokio::test]
async fn get_profile_reaches_the_provider_with_the_secret_and_the_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/me"))
        .and(header("authorization", "Bearer token-1"))
        .and(header("x-api-key", "k"))
        .respond_with(ResponseTemplate::new(200).set_body_string("account-7"))
        .mount(&server)
        .await;

    let state = state_with_auth().await;
    let config = serde_json::json!({"profile_url": format!("{}/me", server.uri())});
    let mut instance = state
        .plugin_executor()
        .instantiate("sdk_auth", "user:did:plc:test", secrets(), config.clone())
        .await
        .unwrap();

    let profile = instance.call_get_profile("token-1", &config).await.unwrap();

    assert_eq!(profile.account_id, "account-7");
    assert_eq!(profile.display_name.as_deref(), Some("SDK Auth fixture"));
    assert_eq!(profile.profile_url, None);
}

#[tokio::test]
async fn a_handler_error_comes_back_as_a_plugin_error_not_a_trap() {
    let state = state_with_auth().await;
    let mut instance = state
        .plugin_executor()
        .instantiate(
            "sdk_auth",
            "user:did:plc:test",
            secrets(),
            serde_json::Value::Null,
        )
        .await
        .unwrap();

    // No `code` parameter, so the fixture's handler returns `BAD_INPUT`.
    let err = instance
        .call_handle_callback(&HashMap::new(), &serde_json::json!({}))
        .await
        .expect_err("a missing code must be an error");

    assert!(
        err.to_string().contains("code is required"),
        "expected the plugin's own message, got: {err}"
    );
}

// tests/plugin_executor.rs

use happyview::db::DatabaseBackend;
use happyview::lexicon::LexiconRegistry;
use happyview::plugin::{
    ExecutionError, LoadedPlugin, PluginExecutor, PluginInfo, PluginManifest, PluginRegistry,
    PluginSource, WasmRuntime,
};
use std::collections::HashMap;
use std::sync::Arc;

type Secrets = HashMap<String, String>;

async fn create_test_executor() -> (PluginExecutor, Arc<PluginRegistry>) {
    // Create in-memory database
    sqlx::any::install_default_drivers();
    let db = sqlx::AnyPool::connect("sqlite::memory:")
        .await
        .expect("Failed to create test database");

    let runtime = Arc::new(WasmRuntime::new().expect("Failed to create runtime"));
    let registry = Arc::new(PluginRegistry::new());
    let lexicons = Arc::new(LexiconRegistry::new());
    let http_client = reqwest::Client::new();

    let executor = PluginExecutor::new(
        runtime,
        registry.clone(),
        db,
        DatabaseBackend::Sqlite,
        http_client,
        lexicons,
    );

    (executor, registry)
}

fn load_test_plugin() -> LoadedPlugin {
    let wasm_bytes = std::fs::read(
        "tests/fixtures/test_plugin/target/wasm32-unknown-unknown/release/test_plugin.wasm",
    )
    .expect(
        "Test plugin not built. Run: cd tests/fixtures/test_plugin && cargo build --target wasm32-unknown-unknown --release",
    );

    let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
        "id": "test", "name": "Test Plugin", "version": "1.0.0", "api_version": "2",
        "plugin_type": "auth", "capabilities": [],
    }))
    .unwrap();

    LoadedPlugin {
        info: PluginInfo {
            id: "test".into(),
            name: "Test Plugin".into(),
            version: "1.0.0".into(),
            api_version: "2".into(),
            icon_url: None,
            required_secrets: vec![],
            auth_type: "oauth2".into(),
            config_schema: None,
        },
        source: PluginSource::File {
            path: "tests/fixtures/test_plugin".into(),
        },
        wasm_bytes,
        manifest: Some(manifest),
    }
}

#[tokio::test]
async fn test_plugin_info() {
    let (executor, registry) = create_test_executor().await;
    let plugin = load_test_plugin();
    registry.register(plugin).await;

    let mut instance = executor
        .instantiate(
            "test",
            "user:did:plc:test",
            Secrets::new(),
            serde_json::Value::Null,
        )
        .await
        .expect("Failed to instantiate");

    let info = instance
        .call_plugin_info()
        .await
        .expect("Failed to get info");

    assert_eq!(info.id, "test");
    assert_eq!(info.name, "Test Plugin");
    assert_eq!(info.version, "1.0.0");
}

#[tokio::test]
async fn test_get_authorize_url() {
    let (executor, registry) = create_test_executor().await;
    let plugin = load_test_plugin();
    registry.register(plugin).await;

    let mut instance = executor
        .instantiate("test", "state:123", Secrets::new(), serde_json::Value::Null)
        .await
        .expect("Failed to instantiate");

    let url = instance
        .call_get_authorize_url(
            "state123",
            "https://app.example/callback",
            &serde_json::Value::Null,
        )
        .await
        .expect("Failed to get URL");

    assert!(url.starts_with("https://"));
}

#[tokio::test]
async fn test_handle_callback() {
    let (executor, registry) = create_test_executor().await;
    let plugin = load_test_plugin();
    registry.register(plugin).await;

    let mut instance = executor
        .instantiate(
            "test",
            "user:did:plc:test",
            Secrets::new(),
            serde_json::Value::Null,
        )
        .await
        .expect("Failed to instantiate");

    let mut params = HashMap::new();
    params.insert("code".to_string(), "code123".to_string());
    params.insert("state".to_string(), "state123".to_string());

    let tokens = instance
        .call_handle_callback(&params, &serde_json::Value::Null)
        .await
        .expect("Failed to handle callback");

    assert_eq!(tokens.access_token, "test-token");
    assert_eq!(tokens.token_type, "Bearer");
}

#[tokio::test]
async fn test_refresh_tokens() {
    let (executor, registry) = create_test_executor().await;
    let plugin = load_test_plugin();
    registry.register(plugin).await;

    let mut instance = executor
        .instantiate(
            "test",
            "user:did:plc:test",
            Secrets::new(),
            serde_json::Value::Null,
        )
        .await
        .expect("Failed to instantiate");

    let tokens = instance
        .call_refresh_tokens("old-refresh-token", &serde_json::Value::Null)
        .await
        .expect("Failed to refresh tokens");

    assert_eq!(tokens.access_token, "refreshed-token");
}

#[tokio::test]
async fn test_get_profile() {
    let (executor, registry) = create_test_executor().await;
    let plugin = load_test_plugin();
    registry.register(plugin).await;

    let mut instance = executor
        .instantiate(
            "test",
            "user:did:plc:test",
            Secrets::new(),
            serde_json::Value::Null,
        )
        .await
        .expect("Failed to instantiate");

    let profile = instance
        .call_get_profile("test-token", &serde_json::Value::Null)
        .await
        .expect("Failed to get profile");

    assert_eq!(profile.account_id, "12345");
    assert_eq!(profile.display_name, Some("Test User".into()));
}

#[tokio::test]
async fn test_sync_account() {
    let (executor, registry) = create_test_executor().await;
    let plugin = load_test_plugin();
    registry.register(plugin).await;

    let mut instance = executor
        .instantiate(
            "test",
            "user:did:plc:test",
            Secrets::new(),
            serde_json::Value::Null,
        )
        .await
        .expect("Failed to instantiate");

    let records = instance
        .call_sync_account("test-token", &serde_json::Value::Null)
        .await
        .expect("Failed to sync account");

    assert!(records.is_empty()); // Test plugin returns empty array
}

#[tokio::test]
async fn test_plugin_not_found() {
    let (executor, _registry) = create_test_executor().await;

    let result = executor
        .instantiate(
            "nonexistent",
            "scope",
            Secrets::new(),
            serde_json::Value::Null,
        )
        .await;

    assert!(matches!(result, Err(ExecutionError::PluginNotFound(_))));
}

/// Test that host_get_secret returns a JSON envelope that plugins can parse.
/// Uses the real Steam plugin WASM which calls get_secret("API_KEY") in get_profile.
/// This test verifies the fix for the JSON envelope mismatch where the host
/// returned raw bytes but the plugin expected {"ok": "value"}.
#[tokio::test]
async fn test_steam_get_secret_json_envelope() {
    let wasm_path = "tests/fixtures/steam.wasm";
    if !std::path::Path::new(wasm_path).exists() {
        eprintln!("Skipping test: {} not found", wasm_path);
        return;
    }

    let (executor, registry) = create_test_executor().await;
    let wasm_bytes = std::fs::read(wasm_path).expect("Failed to read steam.wasm");
    let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
        "id": "steam", "name": "Steam", "version": "1.1.0", "api_version": "2",
        "plugin_type": "auth",
        "capabilities": ["secrets:read", "network:request:unrestricted"],
    }))
    .unwrap();

    let plugin = LoadedPlugin {
        info: PluginInfo {
            id: "steam".into(),
            name: "Steam".into(),
            version: "1.1.0".into(),
            api_version: "2".into(),
            icon_url: None,
            required_secrets: vec!["API_KEY".into()],
            auth_type: "openid".into(),
            config_schema: None,
        },
        source: PluginSource::File {
            path: wasm_path.into(),
        },
        wasm_bytes,
        manifest: Some(manifest),
    };

    registry.register(plugin).await;

    let mut secrets = Secrets::new();
    secrets.insert("API_KEY".to_string(), "test-fake-key".to_string());

    let mut instance = executor
        .instantiate(
            "steam",
            "user:did:plc:test",
            secrets,
            serde_json::Value::Null,
        )
        .await
        .expect("Failed to instantiate Steam plugin");

    // get_profile calls host_get_secret("API_KEY") internally.
    // It will then try to call the Steam API with the fake key, which will fail
    // with an HTTP error — but the important thing is it does NOT fail with
    // MISSING_SECRET, which would mean host_get_secret's JSON envelope is broken.
    let result = instance
        .call_get_profile("76561198024344834", &serde_json::Value::Null)
        .await;

    match result {
        Ok(_) => {} // Unlikely with a fake key, but fine
        Err(ExecutionError::PluginError { code, .. }) => {
            // HTTP_ERROR = Steam API rejected our fake key (expected)
            // INVALID_RESPONSE = Steam returned unexpected format (also fine)
            // MISSING_SECRET = host_get_secret envelope is broken (BAD!)
            assert_ne!(
                code, "MISSING_SECRET",
                "host_get_secret JSON envelope is broken — plugin couldn't parse the secret"
            );
        }
        Err(e) => {
            panic!("Unexpected error type: {:?}", e);
        }
    }
}

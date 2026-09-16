//! Integration tests for the plugin system
//!
//! Note: These tests require a valid WASM plugin to test against.
//! For now, we test the infrastructure without actual WASM execution.

use happyview::plugin::{LoadedPlugin, PluginInfo, PluginManifest, PluginRegistry, PluginSource};

/// A manifest declaring no capabilities — fine for these registry-CRUD tests,
/// which never instantiate the plugin.
fn empty_manifest(id: &str) -> PluginManifest {
    serde_json::from_value(serde_json::json!({
        "id": id, "name": id, "version": "1.0.0", "api_version": "2", "capabilities": [],
    }))
    .unwrap()
}

#[tokio::test]
async fn test_plugin_registry_crud() {
    let registry = PluginRegistry::new();

    // Create test plugin
    let plugin = LoadedPlugin {
        info: PluginInfo {
            id: "test-plugin".into(),
            name: "Test Plugin".into(),
            version: "1.0.0".into(),
            api_version: "2".into(),
            icon_url: None,
            required_secrets: vec![],
            auth_type: "oauth2".into(),
            config_schema: None,
        },
        source: PluginSource::File {
            path: "/tmp/test".into(),
        },
        wasm_bytes: vec![],
        manifest: Some(empty_manifest("test-plugin")),
    };

    // Register
    registry.register(plugin).await;

    // Get
    let retrieved = registry.get("test-plugin").await;
    assert!(retrieved.is_some());
    assert_eq!(retrieved.unwrap().info.name, "Test Plugin");

    // List
    let all = registry.list().await;
    assert_eq!(all.len(), 1);

    // Remove
    let removed = registry.remove("test-plugin").await;
    assert!(removed.is_some());

    // Verify removed
    assert!(registry.get("test-plugin").await.is_none());
}

#[tokio::test]
async fn test_plugin_registry_multiple() {
    let registry = PluginRegistry::new();

    for i in 0..5 {
        let id = format!("plugin-{}", i);
        let plugin = LoadedPlugin {
            info: PluginInfo {
                id: id.clone(),
                name: format!("Plugin {}", i),
                version: "1.0.0".into(),
                api_version: "2".into(),
                icon_url: None,
                required_secrets: vec![],
                auth_type: "oauth2".into(),
                config_schema: None,
            },
            source: PluginSource::File {
                path: "/tmp/test".into(),
            },
            wasm_bytes: vec![],
            manifest: Some(empty_manifest(&id)),
        };
        registry.register(plugin).await;
    }

    assert_eq!(registry.list().await.len(), 5);
}

mod graph_registry {
    use happyview::plugin::graph::GraphError;
    use happyview::plugin::{
        LoadedPlugin, PluginInfo, PluginManifest, PluginRegistry, PluginSource, PluginType,
    };

    fn library(id: &str, version: &str, deps: &[(&str, &str)]) -> LoadedPlugin {
        let deps_json: Vec<serde_json::Value> = deps
            .iter()
            .map(|(id, v)| serde_json::json!({"id": id, "version": v}))
            .collect();
        let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "version": version, "api_version": "2",
            "plugin_type": "library", "dependencies": deps_json,
        }))
        .unwrap();
        LoadedPlugin {
            info: PluginInfo {
                id: id.into(),
                name: id.into(),
                version: version.into(),
                api_version: "2".into(),
                icon_url: None,
                required_secrets: vec![],
                auth_type: "oauth2".into(),
                config_schema: None,
            },
            source: PluginSource::File {
                path: format!("/tmp/{id}").into(),
            },
            wasm_bytes: vec![],
            manifest: Some(manifest),
        }
    }

    /// Like `library`, but with an explicit `namespace` in the manifest
    /// (rather than the id-derived default) so a namespace clash can be
    /// constructed independently of a clashing id.
    fn library_with_namespace(id: &str, version: &str, namespace: &str) -> LoadedPlugin {
        let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "version": version, "api_version": "2",
            "plugin_type": "library", "namespace": namespace,
        }))
        .unwrap();
        LoadedPlugin {
            info: PluginInfo {
                id: id.into(),
                name: id.into(),
                version: version.into(),
                api_version: "2".into(),
                icon_url: None,
                required_secrets: vec![],
                auth_type: "oauth2".into(),
                config_schema: None,
            },
            source: PluginSource::File {
                path: format!("/tmp/{id}").into(),
            },
            wasm_bytes: vec![],
            manifest: Some(manifest),
        }
    }

    #[tokio::test]
    async fn install_refuses_missing_dependency() {
        let registry = PluginRegistry::new();
        let err = registry
            .install(library("record", "1.0.0", &[("db", "*")]))
            .await
            .unwrap_err();
        assert!(matches!(err, GraphError::MissingDependency { .. }));
        assert!(registry.get("record").await.is_none());
    }

    #[tokio::test]
    async fn install_in_order_succeeds_and_uninstall_is_guarded() {
        let registry = PluginRegistry::new();
        registry.install(library("db", "1.0.0", &[])).await.unwrap();
        registry
            .install(library("record", "1.0.0", &[("db", "^1")]))
            .await
            .unwrap();

        let err = registry.uninstall("db", false).await.unwrap_err();
        assert_eq!(
            err,
            GraphError::HasDependents {
                plugin: "db".into(),
                dependents: vec!["record".into()]
            }
        );
        assert!(registry.get("db").await.is_some());

        let removed = registry.uninstall("db", true).await.unwrap();
        assert_eq!(removed, vec!["record".to_string(), "db".to_string()]);
        assert!(registry.get("db").await.is_none());
        assert!(registry.get("record").await.is_none());

        assert!(registry.uninstall("db", false).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn register_all_orders_by_dependency_and_reports_failures() {
        let registry = PluginRegistry::new();
        let failures = registry
            .register_all(vec![
                library("xrpc", "1.0.0", &[("record", "*")]),
                library("record", "1.0.0", &[("db", "*")]),
                library("orphan", "1.0.0", &[("nope", "*")]),
                library("db", "1.0.0", &[]),
            ])
            .await;
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, "orphan");
        assert!(registry.get("xrpc").await.is_some());
        assert!(registry.get("orphan").await.is_none());
    }

    #[tokio::test]
    async fn type_and_namespace_lookups() {
        let registry = PluginRegistry::new();
        registry
            .install(library("http", "1.0.0", &[]))
            .await
            .unwrap();
        assert_eq!(registry.list_by_type(PluginType::Library).await.len(), 1);
        assert!(registry.list_by_type(PluginType::Auth).await.is_empty());
        assert_eq!(
            registry
                .get_library_by_namespace("http")
                .await
                .unwrap()
                .info
                .id,
            "http"
        );
        assert!(registry.get_library_by_namespace("nope").await.is_none());
    }

    #[tokio::test]
    async fn register_all_replaces_an_already_installed_plugin() {
        let registry = PluginRegistry::new();
        registry.install(library("db", "1.0.0", &[])).await.unwrap();
        assert_eq!(registry.get("db").await.unwrap().info.version, "1.0.0");

        let failures = registry
            .register_all(vec![library("db", "1.1.0", &[])])
            .await;
        assert!(failures.is_empty());
        assert_eq!(registry.get("db").await.unwrap().info.version, "1.1.0");
    }

    #[tokio::test]
    async fn register_all_rejects_upgrade_that_breaks_dependents() {
        let registry = PluginRegistry::new();
        registry.install(library("db", "1.0.0", &[])).await.unwrap();
        registry
            .install(library("record", "1.0.0", &[("db", "^1")]))
            .await
            .unwrap();

        let failures = registry
            .register_all(vec![library("db", "2.0.0", &[])])
            .await;
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, "db");
        assert!(
            matches!(failures[0].1, GraphError::VersionMismatch { ref plugin, ref dependency, .. }
            if plugin == "record" && dependency == "db")
        );
        assert_eq!(registry.get("db").await.unwrap().info.version, "1.0.0");
    }

    #[tokio::test]
    async fn install_refuses_namespace_already_claimed_by_another_plugin() {
        let registry = PluginRegistry::new();
        registry
            .install(library_with_namespace("http", "1.0.0", "http"))
            .await
            .unwrap();

        let err = registry
            .install(library_with_namespace("evil", "1.0.0", "http"))
            .await
            .unwrap_err();
        assert_eq!(
            err,
            GraphError::NamespaceTaken {
                namespace: "http".into(),
                by: "http".into(),
            }
        );
        assert!(registry.get("evil").await.is_none());
        assert_eq!(
            registry
                .get_library_by_namespace("http")
                .await
                .unwrap()
                .info
                .id,
            "http"
        );

        // Reinstalling the same id under its own namespace is an upgrade,
        // not a clash, and must still succeed.
        registry
            .install(library_with_namespace("http", "1.1.0", "http"))
            .await
            .unwrap();
        assert_eq!(registry.get("http").await.unwrap().info.version, "1.1.0");
    }
}

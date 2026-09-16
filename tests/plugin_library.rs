//! Library plugin ABI: `get_api_surface`, `call`, and host-side dispatch.

use happyview::db::DatabaseBackend;
use happyview::lexicon::LexiconRegistry;
use happyview::plugin::library::LibraryCallContext;
use happyview::plugin::{
    LoadedPlugin, PluginExecutor, PluginInfo, PluginManifest, PluginRegistry, PluginSource,
    WasmRuntime,
};
use std::collections::HashMap;
use std::sync::Arc;

const FIXTURE: &str =
    "tests/fixtures/test_library/target/wasm32-unknown-unknown/release/test_library.wasm";

fn fixture_bytes() -> Vec<u8> {
    std::fs::read(FIXTURE).expect(
        "Library fixture not built. Run: cargo build --manifest-path tests/fixtures/test_library/Cargo.toml --target wasm32-unknown-unknown --release",
    )
}

/// The fixture registered as a *library* under `id`.
pub fn library_plugin(id: &str) -> LoadedPlugin {
    let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
        "id": id, "name": id, "version": "1.0.0", "api_version": "2",
        "plugin_type": "library", "namespace": id,
        "capabilities": ["library:call", "database:read", "database:write"],
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
            path: "tests/fixtures/test_library".into(),
        },
        wasm_bytes: fixture_bytes(),
        manifest: Some(manifest),
    }
}

/// The same bytes registered as an auth plugin instead of a library — an
/// api_version 2 manifest declaring only `library:call`. `plugin_type`, not
/// what it declares, is what makes the executor refuse it as a library.
fn auth_plugin(id: &str) -> LoadedPlugin {
    let mut p = library_plugin(id);
    let m = p.manifest.as_mut().unwrap();
    m.plugin_type = happyview::plugin::PluginType::Auth;
    m.capabilities = vec![happyview::plugin::capabilities::PluginCapability::LibraryCall];
    p
}

/// The fixture registered with an api_version 2 manifest that declares *no* capabilities.
/// The registry's unchecked `register` accepts it; the loader would not.
fn library_plugin_without_capabilities(id: &str) -> LoadedPlugin {
    let mut p = library_plugin(id);
    p.manifest.as_mut().unwrap().capabilities.clear();
    p
}

async fn executor() -> (PluginExecutor, Arc<PluginRegistry>) {
    sqlx::any::install_default_drivers();
    let db = sqlx::AnyPool::connect("sqlite::memory:").await.unwrap();
    let registry = Arc::new(PluginRegistry::new());
    let executor = PluginExecutor::new(
        Arc::new(WasmRuntime::new().unwrap()),
        registry.clone(),
        db,
        DatabaseBackend::Sqlite,
        reqwest::Client::new(),
        Arc::new(LexiconRegistry::new()),
    );
    (executor, registry)
}

#[tokio::test]
async fn get_api_surface_is_readable() {
    let (executor, registry) = executor().await;
    registry.register(library_plugin("liba")).await;
    let mut inst = executor
        .instantiate("liba", "test", HashMap::new(), serde_json::Value::Null)
        .await
        .unwrap();
    let surface = inst.call_get_api_surface().await.unwrap();
    assert_eq!(surface.namespace, "testlib");
    let names: Vec<&str> = surface.exports.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"echo"));
    assert!(names.contains(&"add"));
    assert_eq!(
        surface
            .exports
            .iter()
            .find(|e| e.name == "VERSION")
            .unwrap()
            .kind,
        "constant"
    );
}

#[tokio::test]
async fn call_dispatches_function_with_args_and_context() {
    let (executor, registry) = executor().await;
    registry.register(library_plugin("liba")).await;
    let mut inst = executor
        .instantiate("liba", "test", HashMap::new(), serde_json::Value::Null)
        .await
        .unwrap();
    let ctx = LibraryCallContext {
        caller_did: Some("did:plc:me".into()),
        has_pds_auth: false,
        db_backend: None,
    };

    let echoed = inst
        .call_library_function("echo", &[serde_json::json!({"a": [1, 2]})], &ctx)
        .await
        .unwrap();
    assert_eq!(echoed, serde_json::json!({"a": [1, 2]}));

    let sum = inst
        .call_library_function("add", &[serde_json::json!(2), serde_json::json!(3)], &ctx)
        .await
        .unwrap();
    assert_eq!(sum, serde_json::json!(5.0));

    let who = inst
        .call_library_function("whoami", &[], &ctx)
        .await
        .unwrap();
    assert_eq!(who, serde_json::json!("did:plc:me"));
}

#[tokio::test]
async fn call_unknown_function_is_a_plugin_error() {
    let (executor, registry) = executor().await;
    registry.register(library_plugin("liba")).await;
    let mut inst = executor
        .instantiate("liba", "test", HashMap::new(), serde_json::Value::Null)
        .await
        .unwrap();
    let err = inst
        .call_library_function("nope", &[], &LibraryCallContext::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("UNKNOWN_FUNCTION"), "{err}");
}

#[tokio::test]
async fn executor_call_library_resolves_by_id() {
    let (executor, registry) = executor().await;
    registry.register(library_plugin("liba")).await;
    let out = executor
        .call_library(
            "liba",
            "echo",
            &[serde_json::json!("hi")],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap();
    assert_eq!(out, serde_json::json!("hi"));
}

#[tokio::test]
async fn executor_call_library_refuses_non_library() {
    let (executor, registry) = executor().await;
    registry.register(auth_plugin("steamish")).await;
    let err = executor
        .call_library("steamish", "echo", &[], &LibraryCallContext::default(), 0)
        .await
        .unwrap_err();
    assert!(
        matches!(err, happyview::plugin::ExecutionError::NotALibrary(_)),
        "{err}"
    );
}

#[tokio::test]
async fn libraries_compose_through_host_call_library() {
    let (executor, registry) = executor().await;
    registry.register(library_plugin("liba")).await;
    registry.register(library_plugin("libb")).await;
    let ctx = LibraryCallContext {
        caller_did: Some("did:plc:me".into()),
        has_pds_auth: true,
        db_backend: None,
    };
    // libb.call_other("liba", "whoami", []) — context must survive the hop.
    let out = executor
        .call_library(
            "libb",
            "call_other",
            &[
                serde_json::json!("liba"),
                serde_json::json!("whoami"),
                serde_json::json!([]),
            ],
            &ctx,
            0,
        )
        .await
        .unwrap();
    assert_eq!(out, serde_json::json!("did:plc:me"));
}

#[tokio::test]
async fn host_get_api_surface_reaches_other_library() {
    let (executor, registry) = executor().await;
    registry.register(library_plugin("liba")).await;
    registry.register(library_plugin("libb")).await;
    let out = executor
        .call_library(
            "libb",
            "surface_of",
            &[serde_json::json!("liba")],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap();
    assert_eq!(out["namespace"], "testlib");
}

#[tokio::test]
async fn recursion_stops_at_depth_limit() {
    let (executor, registry) = executor().await;
    registry.register(library_plugin("liba")).await;
    let err = executor
        .call_library(
            "liba",
            "recurse",
            &[serde_json::json!("liba")],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("depth"), "{err}");
}

#[tokio::test]
async fn host_call_library_is_gated_by_capability() {
    let (executor, registry) = executor().await;
    registry.register(library_plugin("liba")).await;
    registry
        .register(library_plugin_without_capabilities("nocaps"))
        .await;
    let err = executor
        .call_library(
            "nocaps",
            "call_other",
            &[
                serde_json::json!("liba"),
                serde_json::json!("echo"),
                serde_json::json!([1]),
            ],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("FORBIDDEN"), "{err}");
    assert!(err.to_string().contains("library:call"), "{err}");
}

#[tokio::test]
async fn api_surface_is_cached_and_indexed() {
    let (executor, registry) = executor().await;
    registry.register(library_plugin("liba")).await;
    let first = executor.api_surface("liba").await.unwrap();
    let second = executor.api_surface("liba").await.unwrap();
    assert!(Arc::ptr_eq(&first, &second));

    let index = executor.library_index().await;
    assert_eq!(index.len(), 1);
    assert_eq!(index[0].id, "liba");
    assert_eq!(index[0].namespace, "liba");

    registry.remove("liba").await;
    assert!(executor.api_surface("liba").await.is_err());
}

async fn executor_with_table() -> (PluginExecutor, Arc<PluginRegistry>, sqlx::AnyPool) {
    sqlx::any::install_default_drivers();
    let db = sqlx::pool::PoolOptions::<sqlx::Any>::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query("CREATE TABLE things (id INTEGER PRIMARY KEY, name TEXT, score REAL, ok INTEGER)")
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO things (name, score, ok) VALUES ('a', 1.5, 1), ('b', 2.5, 0)")
        .execute(&db)
        .await
        .unwrap();
    let registry = Arc::new(PluginRegistry::new());
    let executor = PluginExecutor::new(
        Arc::new(WasmRuntime::new().unwrap()),
        registry.clone(),
        db.clone(),
        DatabaseBackend::Sqlite,
        reqwest::Client::new(),
        Arc::new(LexiconRegistry::new()),
    );
    (executor, registry, db)
}

fn with_capabilities(mut p: LoadedPlugin, caps: &[&str]) -> LoadedPlugin {
    let m = p.manifest.as_mut().unwrap();
    m.capabilities = caps
        .iter()
        .map(|c| happyview::plugin::capabilities::PluginCapability::parse_str(c).unwrap())
        .collect();
    p
}

#[tokio::test]
async fn db_query_returns_typed_rows_with_params() {
    let (executor, registry, _db) = executor_with_table().await;
    registry
        .register(with_capabilities(
            library_plugin("liba"),
            &["database:read"],
        ))
        .await;
    let rows = executor
        .call_library(
            "liba",
            "sql_query",
            &[
                serde_json::json!(
                    "SELECT id, name, score, ok FROM things WHERE score > ? ORDER BY id"
                ),
                serde_json::json!([1.0]),
            ],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 2);
    assert_eq!(rows[0]["name"], "a");
    assert_eq!(rows[0]["score"], 1.5);
    assert_eq!(rows[1]["id"], 2);
}

#[tokio::test]
async fn db_query_with_read_capability_refuses_writes() {
    let (executor, registry, _db) = executor_with_table().await;
    registry
        .register(with_capabilities(
            library_plugin("liba"),
            &["database:read"],
        ))
        .await;
    let err = executor
        .call_library(
            "liba",
            "sql_query",
            &[serde_json::json!("DELETE FROM things")],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("FORBIDDEN"), "{err}");
    let err = executor
        .call_library(
            "liba",
            "sql_execute",
            &[serde_json::json!("DELETE FROM things")],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("database:write"), "{err}");
}

// Regression: sqlparser 0.62 parses `WITH x AS (...) DELETE ...` as a
// `Statement::Query` whose body is a data-modifying `SetExpr`, not as
// `Statement::Delete` — so a plugin with only `database:read` must still be
// refused, and the write must not actually happen.
#[tokio::test]
async fn db_query_with_read_capability_refuses_write_hidden_in_cte() {
    let (executor, registry, db) = executor_with_table().await;
    registry
        .register(with_capabilities(
            library_plugin("liba"),
            &["database:read"],
        ))
        .await;
    let err = executor
        .call_library(
            "liba",
            "sql_query",
            &[serde_json::json!("WITH x AS (SELECT 1) DELETE FROM things")],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("FORBIDDEN"), "{err}");

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM things")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(count, 2, "the CTE-hidden DELETE must not have run");
}

#[tokio::test]
async fn db_execute_with_write_capability_reports_rows_affected() {
    let (executor, registry, db) = executor_with_table().await;
    registry
        .register(with_capabilities(
            library_plugin("liba"),
            &["database:write"],
        ))
        .await;
    let out = executor
        .call_library(
            "liba",
            "sql_execute",
            &[
                serde_json::json!("UPDATE things SET ok = ? WHERE name = ?"),
                serde_json::json!([true, "b"]),
            ],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap();
    assert_eq!(out["rows_affected"], 1);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM things WHERE ok = 1")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(n, 2);
}

#[tokio::test]
async fn db_functions_refuse_protected_tables() {
    let (executor, registry, _db) = executor_with_table().await;
    registry
        .register(with_capabilities(
            library_plugin("liba"),
            &["database:write"],
        ))
        .await;
    let err = executor
        .call_library(
            "liba",
            "sql_query",
            &[serde_json::json!("SELECT * FROM happyview_users")],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("happyview_users"), "{err}");
}

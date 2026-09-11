//! `require(name)` for native Lua: resolves an installed library plugin by
//! namespace and renders its API surface as a table whose functions dispatch
//! through `PluginExecutor::call_library`. Additive — existing globals stay.

use mlua::{Lua, LuaSerdeExt, Result as LuaResult};

use crate::AppState;
use crate::plugin::PluginExecutor;
use crate::plugin::library::{LibraryCallContext, LibraryEntry};

const LOADED_KEY: &str = "happyview.require.loaded";

/// Install `require` for a script run. Resolves the library index up front
/// (it is cached in the registry) so `require` itself can be synchronous —
/// scripts call it at top level, outside any coroutine.
pub async fn register_require(
    lua: &Lua,
    state: &AppState,
    caller_did: Option<&str>,
    has_pds_auth: bool,
) -> Result<(), String> {
    let executor = state.plugin_executor();
    let index = executor.library_index().await;
    let ctx = LibraryCallContext {
        caller_did: caller_did.map(String::from),
        has_pds_auth,
    };
    install(lua, executor, index, ctx).map_err(|e| format!("require api: {e}"))
}

fn install(
    lua: &Lua,
    executor: PluginExecutor,
    index: Vec<LibraryEntry>,
    ctx: LibraryCallContext,
) -> LuaResult<()> {
    lua.set_named_registry_value(LOADED_KEY, lua.create_table()?)?;

    let require = lua.create_function(move |lua, name: String| {
        let loaded: mlua::Table = lua.named_registry_value(LOADED_KEY)?;
        if let Ok(existing) = loaded.get::<mlua::Table>(name.as_str()) {
            return Ok(existing);
        }
        let entry = index.iter().find(|e| e.namespace == name).ok_or_else(|| {
            mlua::Error::runtime(format!(
                "module '{name}' not found -- is the '{name}' library plugin installed?"
            ))
        })?;
        let module = build_module(lua, &executor, entry, &ctx)?;
        loaded.set(name.as_str(), module.clone())?;
        Ok(module)
    })?;
    lua.globals().set("require", require)
}

/// Render one library's surface as a Lua table. Each function export becomes
/// an async function that JSON-encodes its arguments and dispatches.
fn build_module(
    lua: &Lua,
    executor: &PluginExecutor,
    entry: &LibraryEntry,
    ctx: &LibraryCallContext,
) -> LuaResult<mlua::Table> {
    let module = lua.create_table()?;
    for export in entry.surface.exports.iter().filter(|e| e.is_function()) {
        let executor = executor.clone();
        let ctx = ctx.clone();
        let lib_id = entry.id.clone();
        let fn_name = export.name.clone();
        let func = lua.create_async_function(move |lua, args: mlua::MultiValue| {
            let executor = executor.clone();
            let ctx = ctx.clone();
            let lib_id = lib_id.clone();
            let fn_name = fn_name.clone();
            async move {
                let args: Vec<serde_json::Value> = args
                    .into_iter()
                    .map(|v| lua.from_value(v))
                    .collect::<LuaResult<_>>()?;
                let result = executor
                    .call_library(&lib_id, &fn_name, &args, &ctx, 0)
                    .await
                    .map_err(|e| mlua::Error::runtime(format!("{lib_id}.{fn_name}: {e}")))?;
                lua.to_value(&result)
            }
        })?;
        module.set(export.name.as_str(), func)?;
    }
    Ok(module)
}

/// For `validate_script`: any `require("x")` yields a table whose every
/// field is a no-op function, so top-level imports and simple top-level
/// uses compile without an instance. Mirrors the `env` stub there.
pub fn register_require_stub(lua: &Lua) -> LuaResult<()> {
    let require = lua.create_function(|lua, _name: String| {
        let module = lua.create_table()?;
        let meta = lua.create_table()?;
        meta.set(
            "__index",
            lua.create_function(|lua, (_t, _k): (mlua::Value, mlua::Value)| {
                lua.create_function(|_, _: mlua::MultiValue| Ok(mlua::Value::Nil))
            })?,
        )?;
        module.set_metatable(Some(meta))?;
        Ok(module)
    })?;
    lua.globals().set("require", require)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{LoadedPlugin, PluginInfo, PluginManifest, PluginSource};
    use crate::test_support::{memory_pool, test_state_with_pool};

    const FIXTURE: &str =
        "tests/fixtures/test_library/target/wasm32-unknown-unknown/release/test_library.wasm";

    fn fixture_library(id: &str) -> LoadedPlugin {
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
            source: PluginSource::File { path: "tests/fixtures/test_library".into() },
            wasm_bytes: std::fs::read(FIXTURE).expect(
                "Library fixture not built. Run: cargo build --manifest-path tests/fixtures/test_library/Cargo.toml --target wasm32-unknown-unknown --release",
            ),
            manifest: Some(manifest),
        }
    }

    async fn state_with_library() -> crate::AppState {
        let state = test_state_with_pool(memory_pool().await);
        state
            .plugin_registry
            .register(fixture_library("liba"))
            .await;
        state
    }

    #[tokio::test]
    async fn require_returns_table_with_library_functions() {
        let state = state_with_library().await;
        let lua = crate::lua::sandbox::create_sandbox().unwrap();
        register_require(&lua, &state, Some("did:plc:me"), false)
            .await
            .unwrap();

        lua.load(
            r#"
            local lib = require("liba")
            function handle()
                return { sum = lib.add(2, 3), me = lib.whoami(), echoed = lib.echo({ a = { 1, 2 } }) }
            end
            "#,
        )
        .exec()
        .unwrap();
        let handle: mlua::Function = lua.globals().get("handle").unwrap();
        let result: mlua::Table = handle.call_async(()).await.unwrap();
        assert_eq!(result.get::<f64>("sum").unwrap(), 5.0);
        assert_eq!(result.get::<String>("me").unwrap(), "did:plc:me");
        let echoed: mlua::Table = result.get("echoed").unwrap();
        let a: mlua::Table = echoed.get("a").unwrap();
        assert_eq!(a.get::<i64>(2).unwrap(), 2);
    }

    #[tokio::test]
    async fn require_unknown_library_names_the_plugin() {
        let state = state_with_library().await;
        let lua = crate::lua::sandbox::create_sandbox().unwrap();
        register_require(&lua, &state, None, false).await.unwrap();
        let err = lua.load(r#"local x = require("nope")"#).exec().unwrap_err();
        assert!(
            err.to_string()
                .contains("module 'nope' not found -- is the 'nope' library plugin installed?"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn require_is_cached_per_vm() {
        let state = state_with_library().await;
        let lua = crate::lua::sandbox::create_sandbox().unwrap();
        register_require(&lua, &state, None, false).await.unwrap();
        let same: bool = lua
            .load(r#"return require("liba") == require("liba")"#)
            .eval()
            .unwrap();
        assert!(same);
    }

    #[tokio::test]
    async fn library_errors_surface_as_lua_errors() {
        let state = state_with_library().await;
        let lua = crate::lua::sandbox::create_sandbox().unwrap();
        register_require(&lua, &state, None, false).await.unwrap();
        lua.load(r#"local lib = require("liba"); function handle() return lib.call_other("liba", "nope", toarray({})) end"#)
            .exec()
            .unwrap();
        let handle: mlua::Function = lua.globals().get("handle").unwrap();
        let err = handle.call_async::<mlua::Value>(()).await.unwrap_err();
        assert!(err.to_string().contains("UNKNOWN_FUNCTION"), "{err}");
    }

    /// An unmarked empty Lua table `{}` stays a JSON object — the same
    /// convention `toarray`/`json.encode` use everywhere else. Only an
    /// explicit `toarray({})` should read as `[]`.
    #[tokio::test]
    async fn empty_table_argument_stays_an_object() {
        let state = state_with_library().await;
        let lua = crate::lua::sandbox::create_sandbox().unwrap();
        register_require(&lua, &state, None, false).await.unwrap();
        lua.load(
            r#"
            local lib = require("liba")
            function handle_object()
                return json.encode(lib.echo({}))
            end
            function handle_array()
                return json.encode(lib.echo(toarray({})))
            end
            "#,
        )
        .exec()
        .unwrap();
        let handle_object: mlua::Function = lua.globals().get("handle_object").unwrap();
        let object_json: String = handle_object.call_async(()).await.unwrap();
        assert_eq!(object_json, "{}");

        let handle_array: mlua::Function = lua.globals().get("handle_array").unwrap();
        let array_json: String = handle_array.call_async(()).await.unwrap();
        assert_eq!(array_json, "[]");
    }

    #[test]
    fn stub_lets_top_level_require_validate() {
        let lua = crate::lua::sandbox::create_sandbox().unwrap();
        register_require_stub(&lua).unwrap();
        lua.load(r#"local db = require("db"); local x = db.anything"#)
            .exec()
            .unwrap();
    }
}

//! `require(name)` for native Lua: resolves an installed library plugin by
//! namespace and builds its API surface as a table under the surface's
//! canonical names (the SDK's `naming` module says why those are what Lua
//! sees), whose function exports dispatch through
//! `PluginExecutor::call_library` and whose constructor exports produce
//! chainable objects. Additive — existing globals stay.
//!
//! `internal.*` built-ins resolve before any library lookup and share the
//! same cache slot, so a script uses a built-in and a plugin the same way.

use mlua::{Lua, LuaSerdeExt, Result as LuaResult};

use crate::AppState;
use crate::lua::builtins::{self, ScriptIdentity};
use crate::plugin::PluginExecutor;
use crate::plugin::library::{LibraryCallContext, LibraryEntry};

const LOADED_KEY: &str = "happyview.require.loaded";

/// Install `require` for a script run. Resolves the library index up front
/// (it is cached in the registry) so `require` itself can be synchronous —
/// scripts call it at top level, outside any coroutine.
pub async fn register_require(
    lua: &Lua,
    state: &AppState,
    identity: &ScriptIdentity,
    has_pds_auth: bool,
) -> Result<(), String> {
    let executor = state.plugin_executor();
    let index = executor.library_index().await;
    let ctx = LibraryCallContext {
        caller_did: identity.caller_did.clone(),
        has_pds_auth,
        db_backend: None,
    };
    install(lua, state.clone(), identity.clone(), executor, index, ctx)
        .map_err(|e| format!("require api: {e}"))
}

fn install(
    lua: &Lua,
    state: AppState,
    identity: ScriptIdentity,
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
        if let Some(module) = builtins::builtin_module(lua, &state, &identity, &name)? {
            loaded.set(name.as_str(), module.clone())?;
            return Ok(module);
        }
        if name.starts_with(builtins::BUILTIN_PREFIX) {
            return Err(mlua::Error::runtime(format!(
                "module '{name}' not found -- built-in modules are: {}",
                builtins::BUILTIN_MODULES.join(", ")
            )));
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

/// Build one library's surface as a Lua table. Each function export becomes
/// an async function that JSON-encodes its arguments and dispatches. Table
/// keys are the surface's canonical names, the same names every document
/// sent over the wire carries.
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

    for export in entry.surface.exports.iter().filter(|e| e.is_constructor()) {
        let ctor = lua.create_function({
            let executor = executor.clone();
            let ctx = ctx.clone();
            let lib_id = entry.id.clone();
            let ctor_name = export.name.clone();
            let methods = export.methods.clone();
            move |lua, args: mlua::MultiValue| {
                let object = lua.create_table()?;
                let args_table = lua.create_sequence_from(args)?;
                object.set("__args", args_table)?;
                object.set("__steps", lua.create_table()?)?;
                let index = lua.create_table()?;
                // Keyed by each method's own canonical name, never by
                // position: a surface can carry a method in a mode this
                // host doesn't recognise (`ApiMethod.mode` is an
                // unvalidated string from the plugin), and that method is
                // skipped here rather than shifting a later one.
                for method in &methods {
                    if method.is_lazy() {
                        index.set(method.name.as_str(), lazy_method(lua, method.name.clone())?)?;
                    } else if method.is_immediate() {
                        index.set(
                            method.name.as_str(),
                            immediate_method(
                                lua,
                                executor.clone(),
                                ctx.clone(),
                                lib_id.clone(),
                                ctor_name.clone(),
                                method.name.clone(),
                            )?,
                        )?;
                    }
                }
                let meta = lua.create_table()?;
                meta.set("__index", index)?;
                object.set_metatable(Some(meta))?;
                Ok(object)
            }
        })?;
        module.set(export.name.as_str(), ctor)?;
    }

    Ok(module)
}

/// Appends `{name = args}` to the object's step list and returns the object,
/// so a chain reads left to right and the document reads the same way.
/// `name` is always the canonical name, so the document is portable across
/// interpreters that render it differently.
fn lazy_method(lua: &Lua, name: String) -> LuaResult<mlua::Function> {
    lua.create_function(
        move |lua, (object, args): (mlua::Table, mlua::MultiValue)| {
            let steps: mlua::Table = object.get("__steps")?;
            let step = lua.create_table()?;
            step.set("name", name.as_str())?;
            step.set("args", lua.create_sequence_from(args)?)?;
            steps.push(step)?;
            Ok(object)
        },
    )
}

/// Serialises the object and the call into one document and dispatches it
/// as a library call named after the constructor. Building the JSON here
/// rather than through `to_value` keeps empty argument lists as `[]`.
/// `ctor_name` and `name` are canonical names — what the document and the
/// dispatched call carry, independent of how a script typed them.
fn immediate_method(
    lua: &Lua,
    executor: PluginExecutor,
    ctx: LibraryCallContext,
    lib_id: String,
    ctor_name: String,
    name: String,
) -> LuaResult<mlua::Function> {
    lua.create_async_function(
        move |lua, (object, args): (mlua::Table, mlua::MultiValue)| {
            let executor = executor.clone();
            let ctx = ctx.clone();
            let lib_id = lib_id.clone();
            let ctor_name = ctor_name.clone();
            let name = name.clone();
            async move {
                let ctor_args = json_array(&lua, object.get("__args")?)?;
                let steps_table: mlua::Table = object.get("__steps")?;
                let mut steps = Vec::new();
                for step in steps_table.sequence_values::<mlua::Table>() {
                    let step = step?;
                    let step_name: String = step.get("name")?;
                    let step_args = json_array(&lua, step.get("args")?)?;
                    let mut one = serde_json::Map::new();
                    one.insert(step_name, serde_json::Value::Array(step_args));
                    steps.push(serde_json::Value::Object(one));
                }
                let call_args: Vec<serde_json::Value> = args
                    .into_iter()
                    .map(|v| lua.from_value(v))
                    .collect::<LuaResult<_>>()?;
                let document = serde_json::json!({
                    "args": ctor_args,
                    "steps": steps,
                    "call": { "name": name, "args": call_args },
                });
                let result = executor
                    .call_library(&lib_id, &ctor_name, &[document], &ctx, 0)
                    .await
                    .map_err(|e| {
                        mlua::Error::runtime(format!("{lib_id}.{ctor_name}:{name}: {e}"))
                    })?;
                lua.to_value(&result)
            }
        },
    )
}

fn json_array(lua: &Lua, table: mlua::Table) -> LuaResult<Vec<serde_json::Value>> {
    table
        .sequence_values::<mlua::Value>()
        .map(|v| lua.from_value(v?))
        .collect()
}

/// For `validate_script`: any `require("x")` yields a table whose every
/// field is a function returning another such table, so a top-level chain
/// of any shape — `db.records("c"):where(...):limit(5)` — compiles without
/// an instance. Mirrors the `env` stub there.
pub fn register_require_stub(lua: &Lua) -> LuaResult<()> {
    let require = lua.create_function(|lua, _name: String| stub_table(lua))?;
    lua.globals().set("require", require)
}

/// Every field is a function returning another stub, so a top-level chain of
/// any shape validates without an instance.
fn stub_table(lua: &Lua) -> LuaResult<mlua::Table> {
    let table = lua.create_table()?;
    let meta = lua.create_table()?;
    meta.set(
        "__index",
        lua.create_function(|lua, (_t, _k): (mlua::Value, mlua::Value)| {
            lua.create_function(|lua, _: mlua::MultiValue| stub_table(lua))
        })?,
    )?;
    table.set_metatable(Some(meta))?;
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::library::{ApiExport, ApiSurface};
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

    fn identity_with(caller_did: Option<&str>) -> ScriptIdentity {
        ScriptIdentity {
            trigger_id: "test".into(),
            caller_did: caller_did.map(String::from),
            job_id: None,
        }
    }

    #[tokio::test]
    async fn require_returns_table_with_library_functions() {
        let state = state_with_library().await;
        let lua = crate::lua::sandbox::create_sandbox().unwrap();
        register_require(&lua, &state, &identity_with(Some("did:plc:me")), false)
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
        register_require(&lua, &state, &identity_with(None), false)
            .await
            .unwrap();
        let err = lua.load(r#"local x = require("nope")"#).exec().unwrap_err();
        assert!(
            err.to_string()
                .contains("module 'nope' not found -- is the 'nope' library plugin installed?"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn unknown_builtin_names_the_builtins() {
        let state = state_with_library().await;
        let lua = crate::lua::sandbox::create_sandbox().unwrap();
        register_require(&lua, &state, &identity_with(None), false)
            .await
            .unwrap();
        let err = lua
            .load(r#"local x = require("internal.loging")"#)
            .exec()
            .unwrap_err();
        assert!(
            err.to_string().contains(
                "module 'internal.loging' not found -- built-in modules are: internal.logging, internal.time, internal.tids"
            ),
            "{err}"
        );
    }

    #[tokio::test]
    async fn require_is_cached_per_vm() {
        let state = state_with_library().await;
        let lua = crate::lua::sandbox::create_sandbox().unwrap();
        register_require(&lua, &state, &identity_with(None), false)
            .await
            .unwrap();
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
        register_require(&lua, &state, &identity_with(None), false)
            .await
            .unwrap();
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
        register_require(&lua, &state, &identity_with(None), false)
            .await
            .unwrap();
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

    const OBJECTS_FIXTURE: &str =
        "tests/fixtures/sdk_objects/target/wasm32-unknown-unknown/release/sdk_objects.wasm";

    fn objects_library() -> LoadedPlugin {
        let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "id": "sdk_objects", "name": "objects", "version": "1.0.0", "api_version": "2",
            "plugin_type": "library", "namespace": "objects",
            "capabilities": ["records:read", "database:read"],
        }))
        .unwrap();
        LoadedPlugin {
            info: PluginInfo {
                id: "sdk_objects".into(), name: "objects".into(), version: "1.0.0".into(),
                api_version: "2".into(), icon_url: None, required_secrets: vec![],
                auth_type: "oauth2".into(), config_schema: None,
            },
            source: PluginSource::File { path: "tests/fixtures/sdk_objects".into() },
            wasm_bytes: std::fs::read(OBJECTS_FIXTURE).expect(
                "Objects fixture not built. Run: cargo build --manifest-path tests/fixtures/sdk_objects/Cargo.toml --target wasm32-unknown-unknown --release",
            ),
            manifest: Some(manifest),
        }
    }

    #[tokio::test]
    async fn constructor_chain_sends_one_document() {
        let state = test_state_with_pool(memory_pool().await);
        state.plugin_registry.register(objects_library()).await;
        let lua = crate::lua::sandbox::create_sandbox().unwrap();
        register_require(&lua, &state, &identity_with(None), false)
            .await
            .unwrap();
        lua.load(
            r#"
            local o = require("objects")
            function handle()
                local c = o.chain(1)
                local same = c:add(2)
                assert(same == c, "lazy methods return the object")
                return json.encode(c:add(3):doc("x"))
            end
            "#,
        )
        .exec()
        .unwrap();
        let handle: mlua::Function = lua.globals().get("handle").unwrap();
        let out: String = handle.call_async(()).await.unwrap();
        let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            doc,
            serde_json::json!({
                "args": [1],
                "steps": [{"add": [2]}, {"add": [3]}],
                "call": {"name": "doc", "args": ["x"]}
            })
        );
    }

    #[tokio::test]
    async fn object_is_reusable_after_an_immediate_call() {
        let state = test_state_with_pool(memory_pool().await);
        state.plugin_registry.register(objects_library()).await;
        let lua = crate::lua::sandbox::create_sandbox().unwrap();
        register_require(&lua, &state, &identity_with(None), false)
            .await
            .unwrap();
        lua.load(
            r#"
            local o = require("objects")
            function handle()
                local c = o.chain(1):add(2)
                local first = c:doc()
                local second = c:doc()
                return #first.steps == 1 and #second.steps == 1
            end
            "#,
        )
        .exec()
        .unwrap();
        let handle: mlua::Function = lua.globals().get("handle").unwrap();
        assert!(handle.call_async::<bool>(()).await.unwrap());
    }

    #[test]
    fn stub_lets_top_level_chains_validate() {
        let lua = crate::lua::sandbox::create_sandbox().unwrap();
        register_require_stub(&lua).unwrap();
        lua.load(r#"local db = require("happyview.db"); local q = db.records("c"):where("a", "=", 1):limit(5)"#)
            .exec()
            .unwrap();
    }

    /// A method in a mode this host doesn't recognise (the "odd"-mode
    /// `weird`, sitting between the lazy `add` and the immediate `run_it`)
    /// must not shift any later method's binding onto the wrong name. Uses
    /// the real `sdk_objects` plugin (registered under `objects_library()`)
    /// as the dispatch target but a synthetic surface, since `build_module`
    /// never validates that the surface it's given matches what the plugin
    /// itself declares.
    #[tokio::test]
    async fn unknown_mode_method_does_not_shift_a_later_methods_binding() {
        use happyview_plugin_sdk::wire::ApiMethod;

        let state = test_state_with_pool(memory_pool().await);
        state.plugin_registry.register(objects_library()).await;
        let lua = crate::lua::sandbox::create_sandbox().unwrap();

        let mut export = ApiExport::constructor("chain").lazy("add");
        export.methods.push(ApiMethod {
            name: "weird".into(),
            mode: "odd".into(),
        });
        export.methods.push(ApiMethod::immediate("run_it"));
        let surface = ApiSurface::new("objects").export(export);
        let entry = LibraryEntry {
            id: "sdk_objects".into(),
            namespace: "objects".into(),
            surface: std::sync::Arc::new(surface),
        };

        let executor = state.plugin_executor();
        let module = build_module(&lua, &executor, &entry, &LibraryCallContext::default())
            .expect("build module");
        lua.globals().set("o", module).unwrap();
        lua.load(
            r#"
            function handle()
                local c = o.chain(1):add(2)
                return c:run_it()
            end
            "#,
        )
        .exec()
        .unwrap();
        let handle: mlua::Function = lua.globals().get("handle").unwrap();
        let err = handle.call_async::<mlua::Value>(()).await.unwrap_err();
        // The wasm fixture's `chain` only recognises a call named `doc`, so
        // this errors; what matters is that the error names `run_it`, which
        // proves `run_it` was bound and `weird` was skipped rather than
        // shifting `run_it` onto the wrong key.
        assert!(err.to_string().contains("chain:run_it"), "{err}");
    }

    #[tokio::test]
    async fn builtins_resolve_before_libraries_and_are_cached() {
        let state = test_state_with_pool(memory_pool().await);
        let lua = crate::lua::sandbox::create_sandbox().unwrap();
        register_require(&lua, &state, &identity_with(None), false)
            .await
            .unwrap();
        let same: bool = lua
            .load(r#"return require("internal.time") == require("internal.time")"#)
            .eval()
            .unwrap();
        assert!(same);
        let ms: i64 = lua
            .load(r#"return require("internal.time").now()"#)
            .eval()
            .unwrap();
        assert!(ms > 0);
    }
}

use mlua::{Lua, Result as LuaResult};

const INSTRUCTION_LIMIT: u32 = 1_000_000;

/// The names a v2 script reached the host through. Reading one raises rather
/// than yielding `nil`: a stored script that predates the codemod would
/// otherwise fail as `attempt to index a nil value` somewhere inside its own
/// body, naming neither the global nor the fix. Assignment is untouched, so a
/// script can still define helpers at file scope.
///
/// The label event's fields sit alongside the record event's: both arrive as
/// `input`, and a label script reading a bare `val` is as unmigrated as a
/// record script reading a bare `record`.
pub const REMOVED_GLOBALS: [&str; 32] = [
    "now",
    "log",
    "TID",
    "toarray",
    "json",
    "method",
    "input",
    "params",
    "caller_did",
    "collection",
    "delegate_did",
    "env",
    "space",
    "action",
    "uri",
    "did",
    "rkey",
    "record",
    "event",
    "job",
    "src",
    "val",
    "neg",
    "cts",
    "exp",
    "db",
    "http",
    "xrpc",
    "atproto",
    "linked_repos",
    "jobs",
    "Record",
];

/// The error a read of a removed global raises.
pub fn removed_global_message(name: &str) -> String {
    format!(
        "the '{name}' global was removed in v3; run the script codemod \
         (Settings → Scripts → Migrate, or happyview-codemod) -- see the \
         Migrating scripts guide"
    )
}

/// Create a fresh sandboxed Lua VM.
///
/// - Dangerous globals (`io`, `debug`, `package`, `require`, `dofile`, `loadfile`, `load`) are removed.
/// - `os` is replaced with a safe subset exposing only `time`, `date`, `difftime`, and `clock`.
/// - An instruction-count hook prevents infinite loops.
/// - A read of any name in [`REMOVED_GLOBALS`] raises; any other unknown name
///   is `nil`, as in plain Lua.
///
/// Everything else a script needs arrives through `require`, which the runner
/// installs, and through the arguments of `handle(input, ctx)`.
pub fn create_sandbox() -> LuaResult<Lua> {
    let lua = Lua::new();

    // Preserve safe os functions before removing the full os table
    let globals = lua.globals();
    let safe_os = lua.create_table()?;
    if let Ok(os_table) = globals.get::<mlua::Table>("os") {
        for name in &["time", "date", "difftime", "clock"] {
            if let Ok(func) = os_table.get::<mlua::Function>(*name) {
                safe_os.set(*name, func)?;
            }
        }
    }

    // Remove dangerous globals
    for name in &[
        "os",
        "io",
        "debug",
        "package",
        "require",
        "dofile",
        "loadfile",
        "load",
        "collectgarbage",
    ] {
        globals.raw_set(*name, mlua::Value::Nil)?;
    }

    // Re-add os with only safe functions (time, date, difftime, clock)
    globals.set("os", safe_os)?;

    // Instruction limit to prevent infinite loops
    lua.set_hook(
        mlua::HookTriggers::new().every_nth_instruction(INSTRUCTION_LIMIT),
        |_lua, _debug| Err(mlua::Error::runtime("script exceeded execution limit")),
    )?;

    let guard = lua.create_table()?;
    guard.set(
        "__index",
        lua.create_function(|_, (_globals, key): (mlua::Table, mlua::Value)| {
            if let mlua::Value::String(name) = &key {
                let name = name.to_str()?;
                if REMOVED_GLOBALS.contains(&&*name) {
                    return Err(mlua::Error::runtime(removed_global_message(&name)));
                }
            }
            Ok(mlua::Value::Nil)
        })?,
    )?;
    globals.set_metatable(Some(guard))?;

    Ok(lua)
}

/// Validate that a script compiles and defines a `handle` function.
pub fn validate_script(source: &str) -> Result<(), String> {
    let lua = create_sandbox().map_err(|e| format!("failed to create Lua VM: {e}"))?;
    super::require_api::register_require_stub(&lua)
        .map_err(|e| format!("failed to set require stub: {e}"))?;
    lua.load(source)
        .exec()
        .map_err(|e| format!("script compilation failed: {e}"))?;

    let globals = lua.globals();
    match globals.get::<mlua::Function>("handle") {
        Ok(_) => Ok(()),
        Err(_) => Err("script must define a handle() function".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_removes_dangerous_globals() {
        let lua = create_sandbox().unwrap();
        let globals = lua.globals();
        assert!(globals.get::<mlua::Value>("io").unwrap().is_nil());
        assert!(globals.get::<mlua::Value>("debug").unwrap().is_nil());
        assert!(globals.get::<mlua::Value>("package").unwrap().is_nil());
        assert!(globals.get::<mlua::Value>("require").unwrap().is_nil());
    }

    #[test]
    fn sandbox_provides_safe_os_subset() {
        let lua = create_sandbox().unwrap();
        let os_table: mlua::Table = lua.globals().get("os").unwrap();
        assert!(os_table.get::<mlua::Function>("time").is_ok());
        assert!(os_table.get::<mlua::Function>("date").is_ok());
        assert!(os_table.get::<mlua::Function>("difftime").is_ok());
        assert!(os_table.get::<mlua::Function>("clock").is_ok());
        // Dangerous os functions should not be present
        assert!(os_table.get::<mlua::Value>("execute").unwrap().is_nil());
        assert!(os_table.get::<mlua::Value>("remove").unwrap().is_nil());
        assert!(os_table.get::<mlua::Value>("rename").unwrap().is_nil());
        assert!(os_table.get::<mlua::Value>("exit").unwrap().is_nil());
    }

    #[test]
    fn sandbox_keeps_print_and_the_standard_libraries() {
        let lua = create_sandbox().unwrap();
        let ok: bool = lua
            .load(
                "return type(print) == 'function' and type(string.upper) == 'function' \
                 and type(table.concat) == 'function' and type(math.floor) == 'function'",
            )
            .eval()
            .unwrap();
        assert!(ok);
    }

    /// A name the guard raises on and the codemod has never heard of is a
    /// script the codemod calls finished and the runtime refuses.
    #[test]
    fn the_guard_and_the_codemod_name_the_same_globals() {
        let mut guarded = REMOVED_GLOBALS.to_vec();
        let mut rewritten = crate::codemod::REMOVED_GLOBALS.to_vec();
        guarded.sort_unstable();
        rewritten.sort_unstable();
        assert_eq!(guarded, rewritten);
    }

    #[test]
    fn sandbox_defines_none_of_the_utility_globals() {
        let lua = create_sandbox().unwrap();
        for name in ["now", "log", "TID", "toarray", "json"] {
            let raw: mlua::Value = lua.globals().raw_get(name).unwrap();
            assert!(raw.is_nil(), "{name} is still defined on the globals table");
        }
    }

    #[test]
    fn every_removed_global_raises_the_migration_sentence() {
        let lua = create_sandbox().unwrap();
        for name in REMOVED_GLOBALS {
            let err = lua
                .load(format!("return {name}"))
                .eval::<mlua::Value>()
                .unwrap_err()
                .to_string();
            assert!(err.contains(&removed_global_message(name)), "{name}: {err}");
        }
    }

    #[test]
    fn the_migration_sentence_names_the_global_and_the_codemod() {
        assert_eq!(
            removed_global_message("db"),
            "the 'db' global was removed in v3; run the script codemod \
             (Settings → Scripts → Migrate, or happyview-codemod) -- see the \
             Migrating scripts guide"
        );
    }

    #[test]
    fn a_removed_global_raises_inside_handle_too() {
        let lua = create_sandbox().unwrap();
        lua.load("function handle() return params.q end")
            .exec()
            .unwrap();
        let handle: mlua::Function = lua.globals().get("handle").unwrap();
        let err = handle.call::<mlua::Value>(()).unwrap_err().to_string();
        assert!(
            err.contains("the 'params' global was removed in v3"),
            "{err}"
        );
    }

    #[test]
    fn an_unrelated_unknown_global_is_nil() {
        let lua = create_sandbox().unwrap();
        let value: mlua::Value = lua.load("return no_such_thing").eval().unwrap();
        assert!(value.is_nil());
        let value: mlua::Value = lua.load("return _G[42]").eval().unwrap();
        assert!(value.is_nil());
    }

    #[test]
    fn assigning_a_global_stays_allowed() {
        let lua = create_sandbox().unwrap();
        let n: i64 = lua
            .load("helper_count = 3; function helper() return helper_count end; return helper()")
            .eval()
            .unwrap();
        assert_eq!(n, 3);
    }

    #[test]
    fn a_script_may_shadow_a_removed_name_by_assigning_it() {
        let lua = create_sandbox().unwrap();
        let s: String = lua
            .load(r#"log = function(m) return "mine:" .. m end; return log("x")"#)
            .eval()
            .unwrap();
        assert_eq!(s, "mine:x");
    }

    #[test]
    fn sandbox_kills_infinite_loop() {
        let lua = create_sandbox().unwrap();
        let result = lua.load("while true do end").exec();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("execution limit"),
            "expected execution limit error, got: {err}"
        );
    }

    #[test]
    fn validate_script_accepts_valid() {
        let result = validate_script("function handle() return {} end");
        assert!(result.is_ok());
    }

    #[test]
    fn validate_script_accepts_the_v3_contract() {
        let result = validate_script(
            r#"
            local db = require("happyview.db")
            local log = require("internal.logging")
            function handle(input, ctx)
                log.info("hi", { who = ctx.caller_did })
                return db.records("app.test.rec"):limit(input.limit):all()
            end
            "#,
        );
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn validate_script_rejects_missing_handle() {
        let result = validate_script("function other() return {} end");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("handle"));
    }

    #[test]
    fn validate_script_rejects_syntax_error() {
        let result = validate_script("function handle(");
        assert!(result.is_err());
    }

    #[test]
    fn validate_script_reports_a_top_level_removed_global_by_name() {
        let err =
            validate_script(r#"local base = env.URL .. "/x"; function handle() end"#).unwrap_err();
        assert!(err.contains("the 'env' global was removed in v3"), "{err}");
    }

    /// `validate_script` loads the chunk in a guarded sandbox, so passing it
    /// proves no removed global is read at load; `needs_migration` covers the
    /// reads inside `handle`, under the kind that reports the most names. A
    /// template failing either would be refused by the very form it prefills.
    #[test]
    fn every_editor_template_loads_under_the_guard_and_would_save() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web/src/lib/lua-templates");
        let mut templates: Vec<_> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "lua"))
            .collect();
        templates.sort();
        assert!(
            !templates.is_empty(),
            "no templates under {}",
            dir.display()
        );

        for path in templates {
            let source = std::fs::read_to_string(&path).unwrap();
            if let Err(e) = validate_script(&source) {
                panic!("{}: {e}", path.display());
            }
            let remaining =
                crate::codemod::needs_migration(&source, crate::codemod::ScriptKind::Unknown);
            assert!(
                remaining.is_empty(),
                "{}: still references {remaining:?}",
                path.display()
            );
        }
    }
}

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::task::{Context, Poll};

use mlua::{Lua, Result as LuaResult};

use super::limits::DEFAULT_INSTRUCTION_LIMIT;

/// The name every script chunk is loaded under. Lua renders an error in a
/// named chunk as `[string "script"]:12: message`, which is the form
/// `error::parse_lua_line` reads the line out of; a chunk left unnamed is
/// named after the Rust source location that loaded it and yields no line.
const CHUNK_NAME: &str = "script";

/// What the count hook and the runner share about a run: whether its budget
/// is spent, and how many times the hook has fired since.
#[derive(Default)]
struct ExecutionLimit {
    tripped: AtomicBool,
    post_trip_triggers: AtomicU32,
}

fn execution_limit_error() -> mlua::Error {
    mlua::Error::runtime("script exceeded execution limit")
}

/// Wraps every Lua function that can hand a script control it should no
/// longer have. `pcall` and `xpcall` catch the hook's raise; `coroutine.resume`
/// catches it too when it fires inside a script's own coroutine, and
/// delivers the hook's yield to the script instead of the runner; a
/// `coroutine.wrap` function does the same for the yield. Each wrapper
/// re-raises the execution-limit error after the call it protects returns,
/// whatever it returned, so the error walks out to the top of `handle`
/// through every layer of catching. They are Lua so that yields, the host
/// calls' and the hook's own, still cross them.
const CATCH_GUARDS: &str = r#"
local tripped, raise = ...
local pack, unpack = table.pack, table.unpack
local function guarded(f)
    return function(...)
        local results = pack(f(...))
        if tripped() then raise() end
        return unpack(results, 1, results.n)
    end
end
pcall = guarded(pcall)
xpcall = guarded(xpcall)
coroutine.resume = guarded(coroutine.resume)
local wrap = coroutine.wrap
coroutine.wrap = function(f) return guarded(wrap(f)) end
"#;

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

/// Create a fresh sandboxed Lua VM under the default instruction budget, for
/// validation and tests. The runners pass the operator's budget to
/// [`create_sandbox_with_limit`].
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
    create_sandbox_with_limit(DEFAULT_INSTRUCTION_LIMIT)
}

/// [`create_sandbox`] with an explicit instruction budget.
pub fn create_sandbox_with_limit(instruction_limit: u32) -> LuaResult<Lua> {
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

    // The budget is spent the first time the hook fires, and that raise is all
    // an honest script sees. After it, the hook alternates between yielding
    // the coroutine to the runner, which ends the run at its next poll, and
    // raising again: a yield takes effect only at a yieldable point, and the
    // raise covers the stretches (a `gsub` callback, a sort comparator) where
    // it cannot. The hook is global rather than per-thread because `handle`
    // runs on a coroutine of its own, and a global hook is the only kind it
    // inherits.
    let limit = Arc::new(ExecutionLimit::default());
    lua.set_app_data(Arc::clone(&limit));
    let hook_limit = Arc::clone(&limit);
    lua.set_global_hook(
        mlua::HookTriggers::new().every_nth_instruction(instruction_limit),
        move |_lua, _debug| {
            if !hook_limit.tripped.swap(true, Ordering::Relaxed) {
                return Err(execution_limit_error());
            }
            if hook_limit
                .post_trip_triggers
                .fetch_add(1, Ordering::Relaxed)
                % 2
                == 0
            {
                Ok(mlua::VmState::Yield)
            } else {
                Err(execution_limit_error())
            }
        },
    )?;

    let tripped = lua.create_function(move |_, ()| Ok(limit.tripped.load(Ordering::Relaxed)))?;
    let raise = lua.create_function(|_, ()| -> LuaResult<()> { Err(execution_limit_error()) })?;
    lua.load(CATCH_GUARDS).call::<()>((tripped, raise))?;

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

/// Load a script body as the chunk every runner loads it as.
pub fn load_script<'a>(lua: &'a Lua, source: &'a str) -> mlua::chunk::Chunk<'a> {
    lua.load(source).set_name(CHUNK_NAME)
}

/// Take the instruction budget off a VM, for the runner whose scripts are
/// meant to run long.
pub fn lift_execution_limit(lua: &Lua) {
    lua.remove_global_hook();
    lua.remove_hook();
}

/// Whether this VM's budget is spent, by instructions or by the clock.
pub fn limit_tripped(lua: &Lua) -> bool {
    shared_limit(lua).is_some_and(|limit| limit.tripped.load(Ordering::Relaxed))
}

fn shared_limit(lua: &Lua) -> Option<Arc<ExecutionLimit>> {
    lua.app_data_ref::<Arc<ExecutionLimit>>()
        .map(|limit| Arc::clone(&limit))
}

/// Call `handle` under the instruction budget. Once the budget is spent the
/// run ends with the execution-limit error at the first point the runner
/// regains control: an error propagating out, a yield from the hook, or a
/// normal return the script reached by catching the raise.
pub async fn call_handle(
    lua: &Lua,
    handle: &mlua::Function,
    args: impl mlua::IntoLuaMulti,
) -> LuaResult<mlua::Value> {
    Budgeted {
        limit: shared_limit(lua),
        inner: Box::pin(handle.call_async::<mlua::Value>(args)),
    }
    .await
}

struct Budgeted<F> {
    limit: Option<Arc<ExecutionLimit>>,
    inner: Pin<Box<F>>,
}

impl<F> Budgeted<F> {
    fn tripped(&self) -> bool {
        self.limit
            .as_ref()
            .is_some_and(|limit| limit.tripped.load(Ordering::Relaxed))
    }
}

impl<F: Future<Output = LuaResult<mlua::Value>>> Future for Budgeted<F> {
    type Output = LuaResult<mlua::Value>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if this.tripped() {
            return Poll::Ready(Err(execution_limit_error()));
        }
        match this.inner.as_mut().poll(cx) {
            Poll::Ready(Ok(_)) if this.tripped() => Poll::Ready(Err(execution_limit_error())),
            other => other,
        }
    }
}

/// Validate that a script compiles and defines a `handle` function.
pub fn validate_script(source: &str) -> Result<(), String> {
    let lua = create_sandbox().map_err(|e| format!("failed to create Lua VM: {e}"))?;
    super::require_api::register_require_stub(&lua)
        .map_err(|e| format!("failed to set require stub: {e}"))?;
    load_script(&lua, source)
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

    /// Runs `source` and calls its `handle` on a thread of its own, so a run
    /// that never yields cannot hang the test process. `None` means the
    /// deadline passed first.
    fn run_handle_with_deadline(source: &'static str) -> Option<Result<serde_json::Value, String>> {
        run_handle_on(source, create_sandbox)
    }

    fn run_handle_on(
        source: &'static str,
        vm: fn() -> LuaResult<Lua>,
    ) -> Option<Result<serde_json::Value, String>> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let outcome = rt.block_on(async {
                let lua = vm().unwrap();
                load_script(&lua, source)
                    .exec()
                    .map_err(|e| e.to_string())?;
                let handle: mlua::Function = lua.globals().get("handle").unwrap();
                let value = call_handle(&lua, &handle, ())
                    .await
                    .map_err(|e| e.to_string())?;
                mlua::LuaSerdeExt::from_value(&lua, value).map_err(|e| e.to_string())
            });
            let _ = tx.send(outcome);
        });
        rx.recv_timeout(std::time::Duration::from_secs(5)).ok()
    }

    #[test]
    fn the_limit_reaches_handle_called_through_call_async() {
        let outcome = run_handle_with_deadline("function handle() while true do end end")
            .expect("handle must stop within the deadline");
        let err = outcome.unwrap_err();
        assert!(err.contains("execution limit"), "{err}");
    }

    #[test]
    fn a_pcall_loop_cannot_swallow_the_limit() {
        let outcome = run_handle_with_deadline(
            "function handle() while true do pcall(function() while true do end end) end end",
        )
        .expect("handle must stop within the deadline");
        let err = outcome.unwrap_err();
        assert!(err.contains("execution limit"), "{err}");
    }

    #[test]
    fn a_script_that_catches_the_limit_and_returns_still_fails() {
        let outcome = run_handle_with_deadline(
            "function handle() pcall(function() while true do end end) return { ok = true } end",
        )
        .expect("handle must stop within the deadline");
        let err = outcome.unwrap_err();
        assert!(err.contains("execution limit"), "{err}");
    }

    #[test]
    fn a_caught_limit_followed_by_a_plain_loop_still_stops() {
        let outcome = run_handle_with_deadline(
            "function handle() pcall(function() while true do end end) while true do end end",
        )
        .expect("handle must stop within the deadline");
        let err = outcome.unwrap_err();
        assert!(err.contains("execution limit"), "{err}");
    }

    #[test]
    fn a_script_under_the_limit_is_unaffected() {
        let outcome = run_handle_with_deadline(
            "function handle() local n = 0; for i = 1, 10000 do n = n + i end; return n end",
        )
        .expect("handle must stop within the deadline");
        assert_eq!(outcome.unwrap(), serde_json::json!(50_005_000));
    }

    #[test]
    fn a_script_that_uses_coroutines_still_works() {
        let outcome = run_handle_with_deadline(
            r#"
            function handle()
                local gen = coroutine.wrap(function()
                    for i = 1, 3 do coroutine.yield(i) end
                end)
                local sum = 0
                for _ = 1, 3 do sum = sum + gen() end
                local co = coroutine.create(function(a) local b = coroutine.yield(a + 1); return b * 2 end)
                local _, first = coroutine.resume(co, 1)
                local _, second = coroutine.resume(co, 10)
                return sum * 100 + first * 10 + second
            end
            "#,
        )
        .expect("handle must stop within the deadline");
        assert_eq!(outcome.unwrap(), serde_json::json!(640));
    }

    /// Pins the job worker's exemption at the call that matters: `handle`
    /// runs on a coroutine, and only the global hook reaches one.
    #[test]
    fn a_lifted_vm_runs_handle_past_the_limit() {
        fn lifted() -> LuaResult<Lua> {
            let lua = create_sandbox()?;
            lift_execution_limit(&lua);
            Ok(lua)
        }
        let outcome = run_handle_on(
            "function handle() local n = 0; for i = 1, 3000000 do n = n + 1 end; return n end",
            lifted,
        )
        .expect("handle must finish within the deadline");
        assert_eq!(outcome.unwrap(), serde_json::json!(3_000_000));
    }

    #[test]
    fn a_coroutine_loop_cannot_absorb_the_limit() {
        let outcome = run_handle_with_deadline(
            "function handle() while true do coroutine.resume(coroutine.create(function() while true do end end)) end end",
        )
        .expect("handle must stop within the deadline");
        let err = outcome.unwrap_err();
        assert!(err.contains("execution limit"), "{err}");
    }

    #[test]
    fn a_wrapped_coroutine_loop_cannot_absorb_the_limit() {
        let outcome = run_handle_with_deadline(
            "function handle() while true do pcall(coroutine.wrap(function() while true do end end)) end end",
        )
        .expect("handle must stop within the deadline");
        let err = outcome.unwrap_err();
        assert!(err.contains("execution limit"), "{err}");
    }

    #[test]
    fn a_nested_coroutine_loop_cannot_absorb_the_limit() {
        let outcome = run_handle_with_deadline(
            r#"
            function handle()
                while true do
                    coroutine.resume(coroutine.create(function()
                        while true do
                            coroutine.resume(coroutine.create(function() while true do end end))
                        end
                    end))
                end
            end
            "#,
        )
        .expect("handle must stop within the deadline");
        let err = outcome.unwrap_err();
        assert!(err.contains("execution limit"), "{err}");
    }

    #[test]
    fn a_pcall_around_a_non_yieldable_loop_cannot_absorb_the_limit() {
        let outcome = run_handle_with_deadline(
            r#"
            function handle()
                while true do
                    pcall(function()
                        string.gsub("x", "x", function() while true do end end)
                    end)
                end
            end
            "#,
        )
        .expect("handle must stop within the deadline");
        let err = outcome.unwrap_err();
        assert!(err.contains("execution limit"), "{err}");
    }

    #[test]
    fn a_script_under_the_limit_keeps_ordinary_pcall_and_xpcall() {
        let outcome = run_handle_with_deadline(
            r#"
            function handle()
                local ok, err = pcall(error, "boom")
                local ok2, seen = xpcall(function() error("bang") end, function(m) return "handled " .. tostring(m) end)
                local fine, value = pcall(function() return 7 end)
                return { ok = ok, err = err, ok2 = ok2, seen = seen, fine = fine, value = value }
            end
            "#,
        )
        .expect("handle must stop within the deadline");
        let value = outcome.unwrap();
        assert_eq!(value["ok"], false);
        assert!(value["err"].as_str().unwrap().ends_with("boom"), "{value}");
        assert_eq!(value["ok2"], false);
        assert!(
            value["seen"].as_str().unwrap().starts_with("handled "),
            "{value}"
        );
        assert_eq!(value["fine"], true);
        assert_eq!(value["value"], 7);
    }

    #[test]
    fn the_flag_reports_a_spent_budget() {
        let lua = create_sandbox().unwrap();
        assert!(!limit_tripped(&lua));
        let _ = lua.load("while true do end").exec();
        assert!(limit_tripped(&lua));
    }

    #[test]
    fn an_explicit_limit_is_the_one_applied() {
        fn small() -> LuaResult<Lua> {
            create_sandbox_with_limit(1_000)
        }
        let outcome = run_handle_on(
            "function handle() for i = 1, 10000 do end return 1 end",
            small,
        )
        .expect("handle must stop within the deadline");
        let err = outcome.unwrap_err();
        assert!(err.contains("execution limit"), "{err}");
    }
}

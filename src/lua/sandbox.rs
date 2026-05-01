use mlua::{Lua, LuaSerdeExt, Result as LuaResult};

use super::tid::generate_tid;

const INSTRUCTION_LIMIT: u32 = 1_000_000;

/// Create a fresh sandboxed Lua VM.
///
/// - Dangerous globals (`io`, `debug`, `package`, `require`, `dofile`, `loadfile`, `load`) are removed.
/// - `os` is replaced with a safe subset exposing only `time`, `date`, `difftime`, and `clock`.
/// - An instruction-count hook prevents infinite loops.
/// - Utility globals `now()` and `log()` are injected.
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

    // Utility: now() returns UTC ISO 8601 string
    let now_fn = lua.create_function(|_, ()| Ok(chrono::Utc::now().to_rfc3339()))?;
    globals.set("now", now_fn)?;

    // `log(message)` no-op stub. The real implementation lives in
    // `super::scripts::register_log_event_api` and is registered by
    // every runner so the trigger context can be threaded into each
    // `event_logs` row. The stub here exists only so paths that exec
    // a script body OUTSIDE a runner — namely `validate_script`
    // (admin write-time linting) and the in-process xrpc_api tests —
    // don't break on top-level `log("...")` calls in user scripts.
    // The runner-level registration always overrides this stub.
    let log_fn = lua.create_function(|_, _msg: String| Ok(()))?;
    globals.set("log", log_fn)?;

    // Utility: TID() returns a fresh AT Protocol TID string
    let tid_fn = lua.create_function(|_, ()| Ok(generate_tid()))?;
    globals.set("TID", tid_fn)?;

    // Utility: toarray(table) marks a table as a JSON array for serialization.
    // Ensures empty tables serialize as [] instead of {}.
    let toarray_fn = lua.create_function(|lua, table: mlua::Table| {
        let values: Vec<mlua::Value> = table.sequence_values().collect::<LuaResult<_>>()?;
        let seq = lua.create_sequence_from(values)?;
        seq.set_metatable(Some(lua.array_metatable()))?;
        Ok(seq)
    })?;
    globals.set("toarray", toarray_fn)?;

    // JSON utilities: json.encode(table) -> string, json.decode(string) -> table
    let json_table = lua.create_table()?;

    let encode_fn = lua.create_function(|lua, value: mlua::Value| {
        let json_value: serde_json::Value = lua
            .from_value(value)
            .map_err(|e| mlua::Error::runtime(format!("json.encode: {e}")))?;
        serde_json::to_string(&json_value)
            .map_err(|e| mlua::Error::runtime(format!("json.encode: {e}")))
    })?;
    json_table.set("encode", encode_fn)?;

    let decode_fn = lua.create_function(|lua, s: String| {
        let json_value: serde_json::Value = serde_json::from_str(&s)
            .map_err(|e| mlua::Error::runtime(format!("json.decode: {e}")))?;
        lua.to_value(&json_value)
            .map_err(|e| mlua::Error::runtime(format!("json.decode: {e}")))
    })?;
    json_table.set("decode", decode_fn)?;

    globals.set("json", json_table)?;

    Ok(lua)
}

/// Validate that a script compiles and defines a `handle` function.
pub fn validate_script(source: &str) -> Result<(), String> {
    let lua = create_sandbox().map_err(|e| format!("failed to create Lua VM: {e}"))?;
    // Set a stub env table that returns "" for any missing key so scripts
    // that do top-level concatenation (e.g. `env.URL .. "/path"`) don't fail.
    let env_stub = lua.create_table().unwrap();
    let meta = lua.create_table().unwrap();
    meta.set(
        "__index",
        lua.create_function(|_, (_t, _k): (mlua::Value, mlua::Value)| Ok("".to_string()))
            .unwrap(),
    )
    .unwrap();
    let _ = env_stub.set_metatable(Some(meta));
    lua.globals()
        .set("env", env_stub)
        .map_err(|e| format!("failed to set env stub: {e}"))?;
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
    fn sandbox_provides_now() {
        let lua = create_sandbox().unwrap();
        let result: String = lua.load("return now()").eval().unwrap();
        assert!(result.contains("T")); // ISO 8601 format
    }

    #[test]
    fn sandbox_provides_log() {
        let lua = create_sandbox().unwrap();
        lua.load(r#"log("test message")"#).exec().unwrap();
    }

    #[test]
    fn sandbox_provides_tid() {
        let lua = create_sandbox().unwrap();
        let result: String = lua.load("return TID()").eval().unwrap();
        assert_eq!(result.len(), 13);
        let valid = "234567abcdefghijklmnopqrstuvwxyz";
        for ch in result.chars() {
            assert!(valid.contains(ch), "invalid char '{ch}' in TID");
        }
    }

    #[test]
    fn sandbox_tid_returns_unique_values() {
        let lua = create_sandbox().unwrap();
        let a: String = lua.load("return TID()").eval().unwrap();
        let b: String = lua.load("return TID()").eval().unwrap();
        assert_ne!(a, b);
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
    fn sandbox_provides_toarray() {
        let lua = create_sandbox().unwrap();
        lua.load(r#"result = toarray({})"#).exec().unwrap();
    }

    #[test]
    fn sandbox_toarray_preserves_values() {
        let lua = create_sandbox().unwrap();
        let result: Vec<i64> = lua
            .load(r#"return toarray({10, 20, 30})"#)
            .eval::<mlua::Table>()
            .unwrap()
            .sequence_values()
            .collect::<LuaResult<_>>()
            .unwrap();
        assert_eq!(result, vec![10, 20, 30]);
    }

    #[test]
    fn sandbox_toarray_empty_serializes_as_array() {
        use mlua::LuaSerdeExt;
        let lua = create_sandbox().unwrap();
        let table: mlua::Table = lua.load(r#"return toarray({})"#).eval().unwrap();
        let json: serde_json::Value = lua.from_value(mlua::Value::Table(table)).unwrap();
        assert!(json.is_array(), "expected JSON array, got: {json}");
    }

    #[test]
    fn sandbox_provides_json_encode() {
        let lua = create_sandbox().unwrap();
        let result: String = lua
            .load(r#"return json.encode({name = "test", count = 42})"#)
            .eval()
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["name"], "test");
        assert_eq!(parsed["count"], 42);
    }

    #[test]
    fn sandbox_provides_json_decode() {
        let lua = create_sandbox().unwrap();
        let result: mlua::Table = lua
            .load(r#"return json.decode('{"name":"test","count":42}')"#)
            .eval()
            .unwrap();
        assert_eq!(result.get::<String>("name").unwrap(), "test");
        assert_eq!(result.get::<i64>("count").unwrap(), 42);
    }

    #[test]
    fn sandbox_json_encode_array() {
        let lua = create_sandbox().unwrap();
        let result: String = lua
            .load(r#"return json.encode(toarray({1, 2, 3}))"#)
            .eval()
            .unwrap();
        assert_eq!(result, "[1,2,3]");
    }

    #[test]
    fn sandbox_json_decode_invalid_returns_error() {
        let lua = create_sandbox().unwrap();
        let result: Result<mlua::Value, _> =
            lua.load(r#"return json.decode("not valid json")"#).eval();
        assert!(result.is_err());
    }
}

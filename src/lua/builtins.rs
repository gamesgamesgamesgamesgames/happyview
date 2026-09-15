//! Modules `require` serves without a plugin: the ones that *are* the host.
//! A log line has to carry the trigger and caller it came from, a clock has
//! to be the host's, and a wasm round trip on a script's hottest path buys
//! nothing. They live under `internal.` so a published plugin can never
//! shadow them.

use mlua::{Lua, LuaSerdeExt, Result as LuaResult};

use crate::AppState;
use crate::event_log::{EventLog, Severity, log_event};
use crate::lua::tid::{generate_tid, tid_from_unix_microseconds, tid_to_unix_microseconds};

pub const BUILTIN_PREFIX: &str = "internal.";

/// Every built-in module name, for `require` lookup and for naming them in
/// an unknown-module error. The one list, so a new built-in can't be added
/// to the match below and forgotten here.
pub const BUILTIN_MODULES: [&str; 3] = ["internal.logging", "internal.time", "internal.tids"];

/// Who a script is running as, for log attribution.
#[derive(Debug, Clone, Default)]
pub struct ScriptIdentity {
    pub trigger_id: String,
    pub caller_did: Option<String>,
    /// Set for job scripts, whose log lines also land in the job's own log.
    pub job_id: Option<String>,
}

pub fn builtin_module(
    lua: &Lua,
    state: &AppState,
    identity: &ScriptIdentity,
    name: &str,
) -> LuaResult<Option<mlua::Table>> {
    if !BUILTIN_MODULES.contains(&name) {
        return Ok(None);
    }
    let suffix = name
        .strip_prefix(BUILTIN_PREFIX)
        .expect("every entry in BUILTIN_MODULES carries BUILTIN_PREFIX");
    Ok(Some(match suffix {
        "logging" => logging(lua, state, identity)?,
        "time" => time(lua)?,
        "tids" => tids(lua)?,
        _ => unreachable!("BUILTIN_MODULES and this match must name the same suffixes"),
    }))
}

fn time(lua: &Lua) -> LuaResult<mlua::Table> {
    let t = lua.create_table()?;
    t.set(
        "now",
        lua.create_function(|_, ()| Ok(chrono::Utc::now().timestamp_millis()))?,
    )?;
    t.set(
        "to_iso8601",
        lua.create_function(|_, ms: i64| {
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
                .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
                .ok_or_else(|| mlua::Error::runtime(format!("timestamp out of range: {ms}")))
        })?,
    )?;
    t.set(
        "from_iso8601",
        lua.create_function(|_, s: String| {
            Ok(chrono::DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|dt| dt.timestamp_millis()))
        })?,
    )?;
    Ok(t)
}

fn tids(lua: &Lua) -> LuaResult<mlua::Table> {
    let t = lua.create_table()?;
    t.set("create", lua.create_function(|_, ()| Ok(generate_tid()))?)?;
    t.set(
        "to_tid",
        lua.create_function(|_, ms: i64| Ok(tid_from_unix_microseconds(ms * 1000)))?,
    )?;
    t.set(
        "from_tid",
        lua.create_function(|_, tid: String| {
            tid_to_unix_microseconds(&tid)
                .map(|us| us / 1000)
                .ok_or_else(|| mlua::Error::runtime(format!("invalid TID: {tid}")))
        })?,
    )?;
    Ok(t)
}

fn logging(lua: &Lua, state: &AppState, identity: &ScriptIdentity) -> LuaResult<mlua::Table> {
    let t = lua.create_table()?;
    for (level, severity) in [
        ("debug", None),
        ("info", Some(Severity::Info)),
        ("warn", Some(Severity::Warn)),
        ("error", Some(Severity::Error)),
    ] {
        let state = state.clone();
        let identity = identity.clone();
        let f = lua.create_async_function(
            move |lua, (msg, fields): (String, Option<mlua::Table>)| {
                let state = state.clone();
                let identity = identity.clone();
                async move {
                    let fields: serde_json::Value = match fields {
                        Some(t) => lua.from_value(mlua::Value::Table(t))?,
                        None => serde_json::Value::Null,
                    };
                    tracing::debug!(lua_log = %msg, level, trigger = %identity.trigger_id, "script log");
                    // `debug` is for a developer watching the process, not an
                    // audit trail, so it never becomes a row.
                    let Some(severity) = severity else {
                        return Ok(());
                    };
                    log_event(
                        &state.db,
                        EventLog {
                            event_type: "script.log".to_string(),
                            severity,
                            actor_did: identity.caller_did.clone(),
                            subject: Some(identity.trigger_id.clone()),
                            detail: serde_json::json!({
                                "trigger": identity.trigger_id,
                                "level": level,
                                "message": msg,
                                "fields": fields,
                            }),
                        },
                        state.db_backend,
                    )
                    .await;
                    if let Some(job_id) = &identity.job_id
                        && let Err(e) = crate::jobs::logs::insert_log(
                            &state.db,
                            state.db_backend,
                            job_id,
                            level,
                            &msg,
                        )
                        .await
                    {
                        tracing::warn!(job_id = %job_id, error = %e, "job log insert failed");
                    }
                    Ok(())
                }
            },
        )?;
        t.set(level, f)?;
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lua::sandbox::create_sandbox;
    use crate::test_support::{memory_pool, test_state_with_pool};

    fn identity() -> ScriptIdentity {
        ScriptIdentity {
            trigger_id: "xrpc.query:app.test.q".into(),
            caller_did: Some("did:plc:me".into()),
            job_id: None,
        }
    }

    #[tokio::test]
    async fn unknown_names_are_not_builtins() {
        let lua = create_sandbox().unwrap();
        let state = test_state_with_pool(memory_pool().await);
        assert!(
            builtin_module(&lua, &state, &identity(), "happyview.db")
                .unwrap()
                .is_none()
        );
        assert!(
            builtin_module(&lua, &state, &identity(), "internal.nope")
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn time_module_round_trips_iso8601() {
        let lua = create_sandbox().unwrap();
        let state = test_state_with_pool(memory_pool().await);
        let time = builtin_module(&lua, &state, &identity(), "internal.time")
            .unwrap()
            .unwrap();
        lua.globals().set("time", time).unwrap();
        let ms: i64 = lua.load("return time.now()").eval().unwrap();
        assert!(ms > 1_700_000_000_000);
        let iso: String = lua
            .load("return time.to_iso8601(1757775845000)")
            .eval()
            .unwrap();
        assert_eq!(iso, "2025-09-13T15:04:05.000Z");
        let back: i64 = lua
            .load(r#"return time.from_iso8601("2025-09-13T15:04:05.000Z")"#)
            .eval()
            .unwrap();
        assert_eq!(back, 1757775845000);
        let bad: mlua::Value = lua
            .load(r#"return time.from_iso8601("soon")"#)
            .eval()
            .unwrap();
        assert!(bad.is_nil());
    }

    #[tokio::test]
    async fn tids_module_creates_and_converts() {
        let lua = create_sandbox().unwrap();
        let state = test_state_with_pool(memory_pool().await);
        let tids = builtin_module(&lua, &state, &identity(), "internal.tids")
            .unwrap()
            .unwrap();
        lua.globals().set("tids", tids).unwrap();
        let tid: String = lua.load("return tids.create()").eval().unwrap();
        assert_eq!(tid.len(), 13);
        let from: String = lua
            .load("return tids.to_tid(1757775845000)")
            .eval()
            .unwrap();
        let back: i64 = lua
            .load(format!(r#"return tids.from_tid("{from}")"#))
            .eval()
            .unwrap();
        assert_eq!(back, 1757775845000);
        let err = lua
            .load(r#"return tids.from_tid("nope")"#)
            .eval::<mlua::Value>()
            .unwrap_err();
        assert!(err.to_string().contains("invalid TID"), "{err}");
    }

    #[tokio::test]
    async fn logging_module_writes_an_event_with_fields() {
        let lua = create_sandbox().unwrap();
        let state = test_state_with_pool(memory_pool().await);
        crate::db::query(
            "CREATE TABLE happyview_event_logs (id TEXT PRIMARY KEY, event_type TEXT, severity TEXT, actor_did TEXT, subject TEXT, detail TEXT, created_at TEXT)",
        )
        .execute(&state.db)
        .await
        .unwrap();
        let log = builtin_module(&lua, &state, &identity(), "internal.logging")
            .unwrap()
            .unwrap();
        lua.globals().set("log", log).unwrap();
        lua.load(r#"function handle() log.warn("careful", { uri = "at://x", n = 2 }) end"#)
            .exec()
            .unwrap();
        let handle: mlua::Function = lua.globals().get("handle").unwrap();
        handle.call_async::<()>(()).await.unwrap();
        let (severity, subject, detail): (String, String, String) = crate::db::query_as(
            "SELECT severity, subject, detail FROM happyview_event_logs WHERE event_type = 'script.log'",
        )
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(severity, "warn");
        assert_eq!(subject, "xrpc.query:app.test.q");
        let detail: serde_json::Value = serde_json::from_str(&detail).unwrap();
        assert_eq!(detail["message"], "careful");
        assert_eq!(detail["fields"]["uri"], "at://x");
        assert_eq!(detail["fields"]["n"], 2);
    }
}

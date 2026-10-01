pub(crate) mod builtins;
pub(crate) mod context;
mod execute;
pub mod limits;
pub(crate) mod require_api;
pub(crate) mod sandbox;
pub mod scripts;

pub(crate) use execute::{execute_procedure_script, execute_query_script};
pub(crate) use sandbox::validate_script;
pub use scripts::{
    LabelAppliedEvent, LabelHookOutcome, NATIVE_LANGUAGE, ParsedTrigger, RecordEventPayload,
    RecordHookOutcome, ResolvedScript, ScriptRow, TriggerKind, resolve, resolve_native,
    resolve_record_event, run_label_applied_script, run_record_event_once, run_record_event_script,
    trigger_for_label_uri,
};

/// Test-only seams for integration tests that need a runner-shaped VM
/// without going through an XRPC handler.
#[doc(hidden)]
pub fn sandbox_for_tests() -> mlua::Lua {
    sandbox::create_sandbox().expect("sandbox")
}

#[doc(hidden)]
pub async fn require_api_for_tests(lua: &mlua::Lua, state: &crate::AppState) {
    let identity = builtins::ScriptIdentity {
        trigger_id: "test".into(),
        caller_did: Some("did:plc:test".into()),
        job_id: None,
    };
    require_api::register_require(lua, state, &identity, None)
        .await
        .expect("require api");
}

/// Run `source` the way the runners run it and report what it produced in the
/// shape the interpreter plugin reports it, so `tests/lua_differential.rs` can
/// compare the two case for case. The pieces are the runners' own —
/// [`sandbox`], [`require_api`], [`context::build_ctx`] and
/// [`crate::error::parse_lua_line`] — because a second copy of any of them
/// here would be comparing this file against itself.
///
/// Leaves with `src/lua/`, and with the harness that is its only caller.
#[doc(hidden)]
pub async fn run_for_differential(
    state: &crate::AppState,
    input: &crate::plugin::ScriptExecuteInput,
) -> crate::plugin::ScriptExecuteOutput {
    use crate::plugin::{ScriptErrorKind, ScriptExecuteOutput, ScriptValueKind};
    use mlua::LuaSerdeExt;

    let context = &input.context;
    let lua = match input.limits.instructions {
        Some(limit) => sandbox::create_sandbox_with_limit(limit),
        None => {
            let lua = sandbox::create_sandbox();
            if let Ok(lua) = &lua {
                sandbox::lift_execution_limit(lua);
            }
            lua
        }
    };
    let lua = match lua {
        Ok(lua) => lua,
        Err(e) => return failed(ScriptErrorKind::Runtime, &e.to_string(), false),
    };

    let identity = builtins::ScriptIdentity {
        trigger_id: context.trigger.clone(),
        caller_did: context.caller_did.clone(),
        job_id: context.job.as_ref().map(|job| job.id.clone()),
    };
    if let Err(e) = require_api::register_require(&lua, state, &identity, None).await {
        return failed(ScriptErrorKind::Runtime, &e, false);
    }

    if let Err(e) = sandbox::load_script(&lua, &input.source).exec() {
        let tripped = sandbox::limit_tripped(&lua);
        return failed(ScriptErrorKind::Syntax, &e.to_string(), tripped);
    }
    let Ok(handle) = lua.globals().get::<mlua::Function>("handle") else {
        return ScriptExecuteOutput::Error {
            kind: ScriptErrorKind::MissingHandle,
            message: "script must define a handle() function".into(),
            line: None,
            raw: "script must define a handle() function".into(),
        };
    };

    let job = match &context.job {
        Some(job) => {
            match context::job_ctx(&lua, std::sync::Arc::new(state.clone()), job.id.clone()) {
                Ok(table) => Some(table),
                Err(e) => return failed(ScriptErrorKind::Runtime, &e.to_string(), false),
            }
        }
        None => None,
    };
    let space = context.space.as_ref().map(|space| context::SpaceContext {
        space: space.uri.clone(),
        space_id: space.id.clone(),
        did: space.did.clone(),
        authority_did: space.authority_did.clone(),
        type_nsid: space.type_nsid.clone(),
        skey: space.skey.clone(),
    });
    let env: std::collections::HashMap<String, String> = context.env.clone().into_iter().collect();
    let params: Option<std::collections::HashMap<String, serde_json::Value>> = context
        .params
        .as_ref()
        .map(|params| params.clone().into_iter().collect());
    let ctx = context::build_ctx(
        &lua,
        &context::Invocation {
            trigger_id: &context.trigger,
            caller_did: context.caller_did.as_deref(),
            has_pds_auth: context.has_pds_auth,
            env: &env,
            method: context.method.as_deref(),
            collection: context.collection.as_deref(),
            params: params.as_ref(),
            delegate_did: context.delegate_did.as_deref(),
            space: space.as_ref(),
            job,
        },
    );
    let ctx = match ctx {
        Ok(ctx) => ctx,
        Err(e) => return failed(ScriptErrorKind::Runtime, &e.to_string(), false),
    };
    let argument = match lua.to_value(&input.input) {
        Ok(argument) => argument,
        Err(e) => return failed(ScriptErrorKind::Runtime, &e.to_string(), false),
    };

    // The wall clock is the host's on the plugin path, so this one carries
    // only the instruction budget, which is the guest's on both.
    let returned = match sandbox::call_handle(&lua, &handle, (argument, ctx)).await {
        Ok(value) => value,
        Err(e) => {
            let tripped = sandbox::limit_tripped(&lua);
            return failed(ScriptErrorKind::Runtime, &e.to_string(), tripped);
        }
    };

    let value_kind = match &returned {
        mlua::Value::Nil => ScriptValueKind::None,
        mlua::Value::Table(_) => ScriptValueKind::Object,
        _ => ScriptValueKind::Other,
    };
    match lua.from_value(returned) {
        Ok(value) => ScriptExecuteOutput::Returned { value, value_kind },
        Err(e) => failed(ScriptErrorKind::Runtime, &e.to_string(), false),
    }
}

/// The failure shape both paths report: `kind` from the run rather than from
/// the text, `message` and `line` split by the host's own parser.
#[doc(hidden)]
fn failed(
    default: crate::plugin::ScriptErrorKind,
    raw: &str,
    budget_spent: bool,
) -> crate::plugin::ScriptExecuteOutput {
    use crate::plugin::{ScriptErrorKind, ScriptExecuteOutput};

    let (line, message) = crate::error::parse_lua_line(raw);
    let kind = if budget_spent {
        ScriptErrorKind::Timeout
    } else if raw.contains("not enough memory") {
        ScriptErrorKind::Memory
    } else {
        default
    };
    ScriptExecuteOutput::Error {
        kind,
        message,
        line,
        raw: raw.to_string(),
    }
}

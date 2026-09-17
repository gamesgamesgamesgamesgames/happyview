pub(crate) mod builtins;
pub(crate) mod context;
mod execute;
pub(crate) mod require_api;
pub(crate) mod sandbox;
pub mod scripts;

pub(crate) use execute::{execute_procedure_script, execute_query_script};
pub(crate) use sandbox::validate_script;
pub use scripts::{
    LabelAppliedEvent, LabelHookOutcome, ParsedTrigger, RecordEventPayload, RecordHookOutcome,
    ResolvedScript, ScriptLanguage, ScriptRow, TriggerKind, resolve, resolve_record_event,
    run_label_applied_script, run_record_event_once, run_record_event_script,
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

//! Both forms of a script, run against the same runtime, compared by what
//! they return.
//!
//! A rewrite that produces valid Lua proves nothing about behaviour, and the
//! built-in and `ctx` rows are the ones where a wrong name fails silently —
//! a missing global reads as `nil` rather than as an error. Library rows are
//! not here: those libraries are plugins, and this crate ships none.

use std::collections::HashMap;
use std::sync::Arc;

use mlua::LuaSerdeExt;
use serde_json::{Value, json};

use crate::AppState;
use crate::codemod::ScriptKind;
use crate::lua::builtins::ScriptIdentity;
use crate::lua::context::{self, Invocation};
use crate::lua::sandbox::create_sandbox;
use crate::test_support::{memory_pool, test_state_with_pool};

const TRIGGER: &str = "xrpc.query:app.test.q";
const CALLER: &str = "did:plc:codemod";

fn params() -> HashMap<String, Value> {
    HashMap::from([
        ("q".to_string(), json!("happy")),
        ("limit".to_string(), json!("10")),
    ])
}

fn env() -> HashMap<String, String> {
    HashMap::from([("API_KEY".to_string(), "secret".to_string())])
}

fn identity() -> ScriptIdentity {
    ScriptIdentity {
        trigger_id: TRIGGER.into(),
        caller_did: Some(CALLER.into()),
        job_id: None,
    }
}

/// The v2 form: context arrives as globals and `handle` takes nothing.
async fn run_old(state: &AppState, source: &str) -> Value {
    let lua = create_sandbox().expect("sandbox");
    crate::lua::require_api::register_require(&lua, state, &identity(), None)
        .await
        .expect("require");
    let shared = Arc::new(state.clone());
    crate::lua::scripts::register_log_event_api(&lua, &shared, TRIGGER, Some(CALLER))
        .expect("log api");
    context::set_query_context(
        &lua,
        "app.test.q",
        &params(),
        "app.test.rec",
        Some(CALLER),
        None,
    )
    .expect("query context");
    context::set_env_context(&lua, &env()).expect("env context");
    lua.load(source).exec().expect("load v2 script");
    let handle: mlua::Function = lua.globals().get("handle").expect("handle");
    let out: mlua::Value = handle.call_async(()).await.expect("call v2 handle");
    lua.from_value(out).expect("v2 result")
}

/// The v3 form: nothing but `require`, so a name the rewrite got wrong is a
/// missing local rather than a global that happens to still be there.
async fn run_new(state: &AppState, source: &str) -> Value {
    let lua = create_sandbox().expect("sandbox");
    crate::lua::require_api::register_require(&lua, state, &identity(), None)
        .await
        .expect("require");
    lua.load(source).exec().expect("load v3 script");
    let handle: mlua::Function = lua.globals().get("handle").expect("handle");
    let params = params();
    let env = env();
    let ctx = context::build_ctx(
        &lua,
        &Invocation {
            trigger_id: TRIGGER,
            caller_did: Some(CALLER),
            has_pds_auth: false,
            env: &env,
            method: Some("app.test.q"),
            collection: Some("app.test.rec"),
            params: Some(&params),
            delegate_did: None,
            space: None,
            job: None,
        },
    )
    .expect("ctx");
    let input = lua.to_value(&json!(params)).expect("input");
    let out: mlua::Value = handle
        .call_async((input, ctx))
        .await
        .expect("call v3 handle");
    lua.from_value(out).expect("v3 result")
}

async fn both(source: &str) -> (Value, Value) {
    let state = test_state_with_pool(memory_pool().await);
    let migrated = super::rewrite(source, ScriptKind::Query).expect("rewrite");
    assert!(migrated.notes.is_empty(), "{:?}", migrated.notes);
    let old = run_old(&state, source).await;
    let new = run_new(&state, &migrated.source).await;
    (old, new)
}

#[tokio::test]
async fn context_and_logging_carry_the_same_values() {
    let (old, new) = both(
        r#"
        function handle()
          log("listing " .. params.q)
          return { q = params.q, did = caller_did, key = env.API_KEY }
        end
        "#,
    )
    .await;
    assert_eq!(old, new);
    assert_eq!(old["did"], json!(CALLER));
    assert_eq!(old["key"], json!("secret"));
}

#[tokio::test]
async fn json_encoding_and_array_marking_are_unchanged() {
    let (old, new) = both(
        r#"
        function handle()
          return {
            encoded = json.encode({ a = 1 }),
            decoded = json.decode('{"b":2}').b,
            empty = toarray({}),
            listed = toarray({ "x", "y" }),
          }
        end
        "#,
    )
    .await;
    assert_eq!(old, new);
    assert_eq!(old["empty"], json!([]));
    assert_eq!(old["listed"], json!(["x", "y"]));
}

/// Both forms return an RFC 3339 string. They are not the same string: the
/// global spells the offset `+00:00` with nanosecond precision, the built-in
/// spells it `Z` with millisecond precision. Equality is therefore of the
/// instant, which is the part a script's stored timestamp depends on.
#[tokio::test]
async fn the_clock_names_the_same_instant_in_both_forms() {
    let state = test_state_with_pool(memory_pool().await);
    let source = "function handle() return { at = now() } end";
    let migrated = super::rewrite(source, ScriptKind::Query).expect("rewrite");
    let old = run_old(&state, source).await;
    let new = run_new(&state, &migrated.source).await;

    let instant = |value: &Value| {
        chrono::DateTime::parse_from_rfc3339(value["at"].as_str().expect("rfc 3339"))
            .expect("parse")
            .timestamp_millis()
    };
    assert!((instant(&old) - instant(&new)).abs() < 2_000);
}

/// Two calls to a TID generator never return the same value, so the claim is
/// that both mint a TID for the same moment.
#[tokio::test]
async fn both_tid_generators_mint_a_tid_for_now() {
    let state = test_state_with_pool(memory_pool().await);
    let source = "function handle() return { rkey = TID() } end";
    let migrated = super::rewrite(source, ScriptKind::Query).expect("rewrite");
    let old = run_old(&state, source).await;
    let new = run_new(&state, &migrated.source).await;

    let micros = |value: &Value| {
        let tid = value["rkey"].as_str().expect("tid");
        assert_eq!(tid.len(), 13, "{tid}");
        crate::lua::tid::tid_to_unix_microseconds(tid).expect("valid tid")
    };
    assert!((micros(&old) - micros(&new)).abs() < 2_000_000);
}

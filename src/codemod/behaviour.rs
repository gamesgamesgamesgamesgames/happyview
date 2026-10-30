//! A rewritten script, run against the runtime and checked by what it
//! returns.
//!
//! A rewrite that produces valid Lua proves nothing about behaviour, and the
//! built-in and `ctx` rows are the ones where a wrong name fails silently —
//! a missing local reads as `nil` rather than as an error. Library rows are
//! not here: those libraries are plugins, and this crate ships none.

use std::collections::HashMap;

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

/// Nothing but `require`, so a name the rewrite got wrong is a missing local
/// rather than a global that happens to still be there.
async fn run(state: &AppState, source: &str) -> Value {
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

async fn migrated(source: &str) -> Value {
    let state = test_state_with_pool(memory_pool().await);
    let migrated = super::rewrite(source, ScriptKind::Query).expect("rewrite");
    assert!(migrated.notes.is_empty(), "{:?}", migrated.notes);
    run(&state, &migrated.source).await
}

#[tokio::test]
async fn context_and_logging_carry_the_values_the_globals_did() {
    let out = migrated(
        r#"
        function handle()
          log("listing " .. params.q)
          return { q = params.q, did = caller_did, key = env.API_KEY }
        end
        "#,
    )
    .await;
    assert_eq!(out, json!({ "q": "happy", "did": CALLER, "key": "secret" }));
}

#[tokio::test]
async fn json_encoding_and_array_marking_survive_the_rewrite() {
    let out = migrated(
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
    assert_eq!(
        out,
        json!({ "encoded": "{\"a\":1}", "decoded": 2, "empty": [], "listed": ["x", "y"] })
    );
}

/// The built-in spells the offset `Z` with millisecond precision; what a
/// script's stored timestamp depends on is the instant.
#[tokio::test]
async fn the_rewritten_clock_names_now() {
    let out = migrated("function handle() return { at = now() } end").await;
    let at = chrono::DateTime::parse_from_rfc3339(out["at"].as_str().expect("rfc 3339"))
        .expect("parse")
        .timestamp_millis();
    assert!((chrono::Utc::now().timestamp_millis() - at).abs() < 2_000);
}

#[tokio::test]
async fn the_rewritten_tid_generator_mints_a_tid_for_now() {
    let out = migrated("function handle() return { rkey = TID() } end").await;
    let tid = out["rkey"].as_str().expect("tid");
    assert_eq!(tid.len(), 13, "{tid}");
    let micros = crate::tid::tid_to_unix_microseconds(tid).expect("valid tid");
    assert!((chrono::Utc::now().timestamp_micros() - micros).abs() < 2_000_000);
}

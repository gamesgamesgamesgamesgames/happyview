//! Trigger-keyed scripts dispatcher.
//!
//! Each row in the `scripts` table is identified by a TRIGGER STRING — the
//! `id` column IS the binding. There's no separate "name" or "host column."
//!
//! Trigger grammar:
//!
//! - `record.index:<nsid>` — fires for any record event on `<nsid>`
//!   (wildcard fallback).
//! - `record.create:<nsid>` / `record.update:<nsid>` /
//!   `record.delete:<nsid>` — fires only for that specific action.
//! - `xrpc.query:<nsid>` / `xrpc.procedure:<nsid>` — fires when the
//!   matching XRPC method is invoked.
//! - `labeler.apply:<nsid>` — fires when a label arrives whose `uri`
//!   is `at://<did>/<nsid>/<rkey>`.
//! - `labeler.apply:_actor` — fires when a label arrives whose `uri`
//!   is a bare DID (actor-level label).
//!
//! **Cascade for record events ONLY**: the dispatcher tries
//! `record.<action>:<nsid>` first, falls back to `record.index:<nsid>`
//! if no specific row exists. No cascade for XRPC or labeler triggers.
//!
//! Fail mode varies by host:
//!
//! - **Record / label events**: fail-OPEN — a buggy script eats its
//!   retry budget then dead-letters; the upstream operation proceeds
//!   with whatever the dispatcher returns (original record / original
//!   label). The firehose has no caller to surface errors to.
//! - **XRPC procedures / queries**: fail-CLOSED, single-shot — a script
//!   error becomes a 5xx response. The XRPC dispatchers in
//!   [`crate::xrpc`] resolve the script via [`resolve`] and call
//!   [`super::execute::execute_procedure_script`] /
//!   [`super::execute::execute_query_script`] directly.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use crate::AppState;
use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::event_log::{EventLog, Severity, log_event};

use super::{atproto_api, context, db_api, http_api, record, sandbox, xrpc_api};

/// Number of attempts (1 initial + 3 retries) before dead-lettering.
const MAX_ATTEMPTS: u32 = 4;

// ---------------------------------------------------------------------------
// Trigger grammar
// ---------------------------------------------------------------------------

/// Which family a trigger belongs to. Determines auth context, fail mode,
/// and which event payload shape the script expects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerKind {
    RecordIndex,
    RecordCreate,
    RecordUpdate,
    RecordDelete,
    XrpcQuery,
    XrpcProcedure,
    LabelerApply,
}

/// A trigger id parsed into `(kind, suffix)`. The suffix is either an NSID
/// (`record.*`, `xrpc.*`, `labeler.apply:<nsid>`) or the literal `"_actor"`
/// for `labeler.apply:_actor`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedTrigger {
    pub kind: TriggerKind,
    pub suffix: String,
}

impl ParsedTrigger {
    /// Reconstruct the canonical trigger id from `(kind, suffix)`.
    pub fn id(&self) -> String {
        match self.kind {
            TriggerKind::RecordIndex => format!("record.index:{}", self.suffix),
            TriggerKind::RecordCreate => format!("record.create:{}", self.suffix),
            TriggerKind::RecordUpdate => format!("record.update:{}", self.suffix),
            TriggerKind::RecordDelete => format!("record.delete:{}", self.suffix),
            TriggerKind::XrpcQuery => format!("xrpc.query:{}", self.suffix),
            TriggerKind::XrpcProcedure => format!("xrpc.procedure:{}", self.suffix),
            TriggerKind::LabelerApply => format!("labeler.apply:{}", self.suffix),
        }
    }

    /// Parse a trigger id. Returns a structured error message naming the
    /// valid prefixes when the input doesn't match the grammar.
    pub fn parse(id: &str) -> Result<Self, String> {
        let (prefix, suffix) = id.split_once(':').ok_or_else(|| {
            format!(
                "trigger id '{id}' must contain a ':' separator; \
                 valid prefixes: record.{{index,create,update,delete}}:<nsid>, \
                 xrpc.{{query,procedure}}:<nsid>, labeler.apply:<nsid|_actor>"
            )
        })?;

        if suffix.is_empty() {
            return Err(format!("trigger id '{id}' has empty suffix"));
        }

        let kind = match prefix {
            "record.index" => TriggerKind::RecordIndex,
            "record.create" => TriggerKind::RecordCreate,
            "record.update" => TriggerKind::RecordUpdate,
            "record.delete" => TriggerKind::RecordDelete,
            "xrpc.query" => TriggerKind::XrpcQuery,
            "xrpc.procedure" => TriggerKind::XrpcProcedure,
            "labeler.apply" => TriggerKind::LabelerApply,
            other => {
                return Err(format!(
                    "unknown trigger prefix '{other}'; valid prefixes: \
                     record.{{index,create,update,delete}}, xrpc.{{query,procedure}}, \
                     labeler.apply"
                ));
            }
        };

        // Suffix validation: NSID for everything except `labeler.apply:_actor`.
        match (kind, suffix) {
            (TriggerKind::LabelerApply, "_actor") => {}
            _ => validate_nsid(suffix)?,
        }

        Ok(Self {
            kind,
            suffix: suffix.to_string(),
        })
    }
}

/// Minimal NSID validation: at least two dot-separated segments, each
/// non-empty and matching `[a-zA-Z][a-zA-Z0-9-]*`. Mirrors the AT Protocol
/// spec's character class for everyday use; full Unicode strictness lives
/// in atrium downstream.
fn validate_nsid(nsid: &str) -> Result<(), String> {
    let segments: Vec<&str> = nsid.split('.').collect();
    if segments.len() < 2 {
        return Err(format!(
            "invalid NSID '{nsid}': need at least 2 dot-separated segments"
        ));
    }
    for (idx, seg) in segments.iter().enumerate() {
        if seg.is_empty() {
            return Err(format!("invalid NSID '{nsid}': empty segment"));
        }
        let mut chars = seg.chars();
        let first = chars.next().unwrap();
        if !first.is_ascii_alphabetic() {
            return Err(format!(
                "invalid NSID '{nsid}': segment {idx} must start with a letter"
            ));
        }
        for c in chars {
            if !c.is_ascii_alphanumeric() && c != '-' {
                return Err(format!(
                    "invalid NSID '{nsid}': segment {idx} contains invalid character '{c}'"
                ));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// ScriptLanguage
// ---------------------------------------------------------------------------

/// Runtime a script is written for. Today only [`ScriptLanguage::Lua`] ships;
/// the column is stamped per row so a future runtime (e.g. TypeScript) can
/// land without a schema migration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScriptLanguage {
    #[default]
    Lua,
}

impl ScriptLanguage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Lua => "lua",
        }
    }

    pub fn parse_str(s: &str) -> Option<Self> {
        match s {
            "lua" => Some(Self::Lua),
            _ => None,
        }
    }

    pub fn supported() -> &'static [&'static str] {
        &["lua"]
    }
}

// ---------------------------------------------------------------------------
// Script row + resolution
// ---------------------------------------------------------------------------

/// A row from the `scripts` table — the wire shape the admin API returns.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScriptRow {
    pub id: String,
    pub body: String,
    pub description: Option<String>,
    pub script_type: String,
    pub created_at: String,
    pub updated_at: String,
}

/// A script ready to execute.
#[derive(Clone, Debug)]
pub struct ResolvedScript {
    pub id: String,
    pub language: ScriptLanguage,
    pub body: String,
}

/// Look up a single trigger id. Returns `None` when no row matches OR when
/// the row's `script_type` is unknown to this binary (logged at warn).
pub async fn resolve(state: &AppState, trigger_id: &str) -> Option<ResolvedScript> {
    let sql = adapt_sql(
        "SELECT id, body, script_type FROM scripts WHERE id = ?",
        state.db_backend,
    );
    let row: Option<(String, String, String)> = match sqlx::query_as(&sql)
        .bind(trigger_id)
        .fetch_optional(&state.db)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(trigger_id, "scripts lookup failed: {e}");
            return None;
        }
    };
    let (id, body, script_type) = row?;
    let language = match ScriptLanguage::parse_str(&script_type) {
        Some(l) => l,
        None => {
            tracing::warn!(
                id,
                script_type,
                "unknown script_type; this binary supports: {}",
                ScriptLanguage::supported().join(", ")
            );
            return None;
        }
    };
    Some(ResolvedScript { id, language, body })
}

/// Resolve a record-event trigger with the cascade rule:
/// `record.<action>:<nsid>` first, then `record.index:<nsid>`.
pub async fn resolve_record_event(
    state: &AppState,
    nsid: &str,
    action: &str,
) -> Option<ResolvedScript> {
    let action_trigger = match action {
        "create" => Some(format!("record.create:{nsid}")),
        "update" => Some(format!("record.update:{nsid}")),
        "delete" => Some(format!("record.delete:{nsid}")),
        _ => None,
    };
    if let Some(t) = action_trigger
        && let Some(s) = resolve(state, &t).await
    {
        return Some(s);
    }
    resolve(state, &format!("record.index:{nsid}")).await
}

// ---------------------------------------------------------------------------
// Record-event runner (fail-open, retry + dead-letter)
// ---------------------------------------------------------------------------

/// All the contextual fields a record-event script needs at execution
/// time. Bundled into a struct so the runner doesn't take 6+ `&str`
/// positional arguments — easy to swap `did` and `uri` and have the
/// type checker shrug.
#[derive(Clone, Copy, Debug)]
pub struct RecordEventPayload<'a> {
    pub nsid: &'a str,
    pub action: &'a str,
    pub uri: &'a str,
    pub did: &'a str,
    pub rkey: &'a str,
    pub record: Option<&'a Value>,
}

/// Run the record-event script (if any) for a given event. Returns the
/// record body the indexer should store: `Some(record)` to proceed,
/// `None` to skip indexing.
///
/// Failure mode is fail-open: a script that exhausts its retry budget is
/// dead-lettered and the indexer proceeds with the original record.
pub async fn run_record_event_script(
    state: &AppState,
    payload: RecordEventPayload<'_>,
) -> Option<Value> {
    let resolved = match resolve_record_event(state, payload.nsid, payload.action).await {
        Some(s) => s,
        // No script for this trigger → indexer keeps the original record.
        None => return payload.record.cloned(),
    };

    let host_id = format!("{}:{}", payload.nsid, payload.action);
    let event_payload = serde_json::json!({
        "trigger": resolved.id,
        "action": payload.action,
        "uri": payload.uri,
        "did": payload.did,
        "collection": payload.nsid,
        "rkey": payload.rkey,
        "record": payload.record,
    });

    let mut last_error = String::new();
    for attempt in 0..MAX_ATTEMPTS {
        if attempt > 0 {
            let delay = std::time::Duration::from_secs(1 << (attempt - 1));
            tokio::time::sleep(delay).await;
        }
        match run_record_event_once(state, &resolved, payload).await {
            Ok(outcome) => {
                log_event(
                    &state.db,
                    EventLog {
                        event_type: "script.executed".to_string(),
                        severity: Severity::Info,
                        actor_did: None,
                        subject: Some(payload.uri.to_string()),
                        detail: serde_json::json!({
                            "host_kind": "record",
                            "host_id": host_id,
                            "trigger": resolved.id,
                            "attempts": attempt + 1,
                        }),
                    },
                    state.db_backend,
                )
                .await;
                return outcome;
            }
            Err(e) => {
                last_error = e;
                tracing::warn!(
                    uri = %payload.uri,
                    trigger = %resolved.id,
                    attempt = attempt + 1,
                    "record script attempt failed: {last_error}"
                );
            }
        }
    }

    write_dead_letter(
        state,
        &resolved,
        "record",
        &host_id,
        &event_payload,
        &last_error,
        MAX_ATTEMPTS,
    )
    .await;
    log_event(
        &state.db,
        EventLog {
            event_type: "script.dead_lettered".to_string(),
            severity: Severity::Error,
            actor_did: None,
            subject: Some(payload.uri.to_string()),
            detail: serde_json::json!({
                "host_kind": "record",
                "host_id": host_id,
                "trigger": resolved.id,
                "error": last_error,
            }),
        },
        state.db_backend,
    )
    .await;

    // Fail-open: indexer proceeds with the original record.
    payload.record.cloned()
}

/// Single attempt at the record-event Lua script. Used internally by the
/// retry loop and externally by admin retry endpoints.
///
/// Returns `Ok(Some(value))` to continue indexing with `value`,
/// `Ok(None)` when the script returned `nil` (skip), or `Err(msg)` on
/// any execution failure.
pub async fn run_record_event_once(
    state: &AppState,
    script: &ResolvedScript,
    payload: RecordEventPayload<'_>,
) -> Result<Option<Value>, String> {
    if script.language != ScriptLanguage::Lua {
        return Err(format!(
            "this binary cannot run {} scripts",
            script.language.as_str()
        ));
    }
    let lua = sandbox::create_sandbox().map_err(|e| format!("create sandbox: {e}"))?;
    let state_arc = Arc::new(state.clone());
    register_default_apis(&lua, &state_arc, &script.id, Some(payload.did))?;

    // Legacy globals (action, uri, did, collection, rkey, record) for
    // back-compat with scripts written against the old `index_hook`
    // surface.
    context::set_hook_context(
        &lua,
        payload.action,
        payload.uri,
        payload.did,
        payload.nsid,
        payload.rkey,
        payload.record,
    )
    .map_err(|e| format!("set hook context: {e}"))?;

    // Also expose an `event` table — same fields, different idiom. New
    // scripts can read `event.action` / `event.record.title` instead of
    // the bare globals; both styles work.
    use mlua::LuaSerdeExt;
    let event_value = serde_json::json!({
        "action": payload.action,
        "uri": payload.uri,
        "did": payload.did,
        "collection": payload.nsid,
        "rkey": payload.rkey,
        "record": payload.record,
    });
    lua.globals()
        .set(
            "event",
            lua.to_value(&event_value)
                .map_err(|e| format!("event lua-conv: {e}"))?,
        )
        .map_err(|e| format!("set event global: {e}"))?;

    context::set_env_context(&lua, &load_env_vars(&state.db, state.db_backend).await)
        .map_err(|e| format!("set env: {e}"))?;

    lua.load(script.body.as_str())
        .exec()
        .map_err(|e| format!("script load: {e}"))?;
    let handle: mlua::Function = lua
        .globals()
        .get("handle")
        .map_err(|e| format!("missing handle(): {e}"))?;
    let result: mlua::Value = handle
        .call_async::<mlua::Value>(())
        .await
        .map_err(|e| e.to_string())?;

    match result {
        mlua::Value::Nil => Ok(None),
        mlua::Value::Table(_) => {
            let v: Value = lua
                .from_value(result)
                .map_err(|e| format!("convert lua return to JSON: {e}"))?;
            Ok(Some(v))
        }
        // Non-nil, non-table return — pass-through: keep the original record.
        _ => Ok(payload.record.cloned()),
    }
}

// ---------------------------------------------------------------------------
// Label-applied dispatcher (fail-open, retry + dead-letter)
// ---------------------------------------------------------------------------

/// Payload passed to `labeler.apply:*` scripts. Mirrors the AT Proto label
/// shape from `com.atproto.label.subscribeLabels`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LabelAppliedEvent {
    pub src: String,
    pub uri: String,
    pub val: String,
    #[serde(default)]
    pub neg: bool,
    pub cts: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exp: Option<String>,
}

/// What an `on_label_applied` script chain decided.
#[derive(Debug)]
pub enum LabelHookOutcome {
    /// Persist the (possibly rewritten) label.
    Continue(LabelAppliedEvent),
    /// Skip persistence — the script returned `nil`.
    Skip,
}

/// Compute the trigger string for a given label. `at://` URIs route to
/// `labeler.apply:<nsid>` (using the second path segment); everything else
/// (bare DIDs, malformed) routes to `labeler.apply:_actor`.
pub fn trigger_for_label_uri(uri: &str) -> String {
    if let Some(rest) = uri.strip_prefix("at://") {
        match rest.split('/').nth(1) {
            Some(nsid) if !nsid.is_empty() => format!("labeler.apply:{nsid}"),
            _ => "labeler.apply:_actor".to_string(),
        }
    } else {
        "labeler.apply:_actor".to_string()
    }
}

/// Run the label-applied script (if any) for an inbound label. Fail-open:
/// dead-lettered failures fall through with the original label.
pub async fn run_label_applied_script(
    state: &AppState,
    event: LabelAppliedEvent,
) -> LabelHookOutcome {
    let trigger = trigger_for_label_uri(&event.uri);
    let resolved = match resolve(state, &trigger).await {
        Some(s) => s,
        None => return LabelHookOutcome::Continue(event),
    };

    let payload = serde_json::to_value(&event).unwrap_or(Value::Null);
    let host_id = event.src.clone();
    let original = event.clone();

    let mut last_error = String::new();
    for attempt in 0..MAX_ATTEMPTS {
        if attempt > 0 {
            let delay = std::time::Duration::from_secs(1 << (attempt - 1));
            tokio::time::sleep(delay).await;
        }
        match run_label_lua_once(state, &resolved, &event).await {
            Ok(outcome) => return outcome,
            Err(e) => {
                last_error = e;
                tracing::warn!(
                    src = %event.src, uri = %event.uri,
                    trigger = %resolved.id,
                    attempt = attempt + 1,
                    "label script attempt failed: {last_error}"
                );
            }
        }
    }
    write_dead_letter(
        state,
        &resolved,
        "label",
        &host_id,
        &payload,
        &last_error,
        MAX_ATTEMPTS,
    )
    .await;
    LabelHookOutcome::Continue(original)
}

async fn run_label_lua_once(
    state: &AppState,
    script: &ResolvedScript,
    event: &LabelAppliedEvent,
) -> Result<LabelHookOutcome, String> {
    if script.language != ScriptLanguage::Lua {
        return Err(format!(
            "this binary cannot run {} scripts",
            script.language.as_str()
        ));
    }
    let lua = sandbox::create_sandbox().map_err(|e| format!("create sandbox: {e}"))?;
    let state_arc = Arc::new(state.clone());
    register_default_apis(&lua, &state_arc, &script.id, None)?;

    use mlua::LuaSerdeExt;
    let globals = lua.globals();
    globals
        .set("src", event.src.clone())
        .map_err(|e| format!("set src: {e}"))?;
    globals
        .set("uri", event.uri.clone())
        .map_err(|e| format!("set uri: {e}"))?;
    globals
        .set("val", event.val.clone())
        .map_err(|e| format!("set val: {e}"))?;
    globals
        .set("neg", event.neg)
        .map_err(|e| format!("set neg: {e}"))?;
    globals
        .set("cts", event.cts.clone())
        .map_err(|e| format!("set cts: {e}"))?;
    match &event.exp {
        Some(exp) => globals.set("exp", exp.clone()),
        None => globals.set("exp", mlua::Value::Nil),
    }
    .map_err(|e| format!("set exp: {e}"))?;
    let event_value = serde_json::to_value(event).map_err(|e| format!("encode event: {e}"))?;
    globals
        .set(
            "event",
            lua.to_value(&event_value)
                .map_err(|e| format!("event lua-conv: {e}"))?,
        )
        .map_err(|e| format!("set event: {e}"))?;
    context::set_env_context(&lua, &load_env_vars(&state.db, state.db_backend).await)
        .map_err(|e| format!("set env: {e}"))?;

    lua.load(script.body.as_str())
        .exec()
        .map_err(|e| format!("script load: {e}"))?;
    let handle: mlua::Function = lua
        .globals()
        .get("handle")
        .map_err(|e| format!("missing handle(): {e}"))?;
    let result: mlua::Value = handle
        .call_async::<mlua::Value>(())
        .await
        .map_err(|e| e.to_string())?;

    match result {
        mlua::Value::Nil => Ok(LabelHookOutcome::Skip),
        mlua::Value::Table(_) => {
            let v: Value = lua
                .from_value(result)
                .map_err(|e| format!("convert lua return: {e}"))?;
            // Merge: any field the script omitted falls back to the
            // original. This makes "filter only" scripts (return `event`)
            // and "rewrite val" scripts (return `{ val = "..." }`) both
            // ergonomic.
            let next = LabelAppliedEvent {
                src: extract_string(&v, "src").unwrap_or_else(|| event.src.clone()),
                uri: extract_string(&v, "uri").unwrap_or_else(|| event.uri.clone()),
                val: extract_string(&v, "val").unwrap_or_else(|| event.val.clone()),
                neg: extract_bool(&v, "neg").unwrap_or(event.neg),
                cts: extract_string(&v, "cts").unwrap_or_else(|| event.cts.clone()),
                exp: extract_string(&v, "exp").or_else(|| event.exp.clone()),
            };
            Ok(LabelHookOutcome::Continue(next))
        }
        _ => Ok(LabelHookOutcome::Continue(event.clone())),
    }
}

fn extract_string(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(String::from)
}

fn extract_bool(v: &Value, key: &str) -> Option<bool> {
    v.get(key).and_then(|x| x.as_bool())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Register the default API surface on a fresh sandbox: db / http / xrpc /
/// atproto / Record. `caller_did` flows into xrpc so authenticated calls
/// work; pass `None` for unauthenticated contexts.
///
/// The Record API is registered in **no-auth mode** here — fine for
/// record-event and label scripts which have no caller credentials.
/// Calling `:save()` / `:delete()` (the PDS-touching variants) errors
/// clearly with the no-PDS-auth message; the local-only variants
/// (`:save_local`, `:delete_local`, `Record.delete_local`) work.
fn register_default_apis(
    lua: &mlua::Lua,
    state: &Arc<AppState>,
    trigger_id: &str,
    caller_did: Option<&str>,
) -> Result<(), String> {
    db_api::register_db_api(lua, state.clone()).map_err(|e| format!("db api: {e}"))?;
    http_api::register_http_api(lua, state.clone()).map_err(|e| format!("http api: {e}"))?;
    xrpc_api::register_xrpc_api(lua, state.clone(), caller_did.map(String::from))
        .map_err(|e| format!("xrpc api: {e}"))?;
    atproto_api::register_atproto_api(lua, state.clone(), None)
        .map_err(|e| format!("atproto api: {e}"))?;
    record::register_record_api_no_auth(lua, state.clone())
        .map_err(|e| format!("record api: {e}"))?;
    register_log_event_api(lua, state, trigger_id, caller_did)?;
    Ok(())
}

/// Register `log(msg)` as a Lua global that writes a `script.log` row to
/// `event_logs` (so operators can inspect script output from
/// `/dashboard/events` without tailing stderr) AND emits a
/// `tracing::debug!` for ops who do tail.
///
/// `trigger_id` is recorded as the row's `subject` so events for a
/// specific script can be filtered by trigger. `caller_did` is recorded
/// as `actor_did` when the runner has one (XRPC handlers); record /
/// label runners pass `None`.
///
/// This intentionally **overrides** the basic `log()` helper that
/// `sandbox::create_sandbox()` registers (which only writes to
/// `tracing::debug!`). All script runners call this helper so every
/// trigger family lands its `log()` calls in the event log.
pub(crate) fn register_log_event_api(
    lua: &mlua::Lua,
    state: &Arc<AppState>,
    trigger_id: &str,
    caller_did: Option<&str>,
) -> Result<(), String> {
    let state = state.clone();
    let trigger_id = trigger_id.to_string();
    let caller_did = caller_did.map(String::from);
    let log_fn = lua
        .create_async_function(move |_, msg: String| {
            let state = state.clone();
            let trigger_id = trigger_id.clone();
            let caller_did = caller_did.clone();
            async move {
                tracing::debug!(lua_log = %msg, trigger = %trigger_id, "lua script log");
                log_event(
                    &state.db,
                    EventLog {
                        event_type: "script.log".to_string(),
                        severity: Severity::Info,
                        actor_did: caller_did,
                        subject: Some(trigger_id.clone()),
                        detail: serde_json::json!({
                            "trigger": trigger_id,
                            "message": msg,
                        }),
                    },
                    state.db_backend,
                )
                .await;
                Ok(())
            }
        })
        .map_err(|e| format!("log api: {e}"))?;
    lua.globals()
        .set("log", log_fn)
        .map_err(|e| format!("set log global: {e}"))?;
    Ok(())
}

/// Load `script_variables` as a flat key→value map for the `env` global.
async fn load_env_vars(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
) -> std::collections::HashMap<String, String> {
    let sql = adapt_sql("SELECT key, value FROM script_variables", backend);
    sqlx::query_as::<_, (String, String)>(&sql)
        .fetch_all(db)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect()
}

/// Persist a permanently-failed run for later admin triage.
async fn write_dead_letter(
    state: &AppState,
    script: &ResolvedScript,
    host_kind: &str,
    host_id: &str,
    payload: &Value,
    error: &str,
    attempts: u32,
) {
    let payload_str = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string());
    let sql = adapt_sql(
        "INSERT INTO dead_letter_scripts
            (script_ref, host_kind, host_id, payload, error, attempts, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        state.db_backend,
    );
    if let Err(e) = sqlx::query(&sql)
        .bind(script.id.as_str())
        .bind(host_kind)
        .bind(host_id)
        .bind(&payload_str)
        .bind(error)
        .bind(attempts as i64)
        .bind(now_rfc3339())
        .execute(&state.db)
        .await
    {
        tracing::error!(
            host_kind,
            host_id,
            trigger = %script.id,
            "failed to write dead_letter_scripts: {e}"
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_record_index_trigger() {
        let t = ParsedTrigger::parse("record.index:com.example.thing").unwrap();
        assert_eq!(t.kind, TriggerKind::RecordIndex);
        assert_eq!(t.suffix, "com.example.thing");
        assert_eq!(t.id(), "record.index:com.example.thing");
    }

    #[test]
    fn parse_record_action_triggers() {
        for (prefix, kind) in [
            ("record.create", TriggerKind::RecordCreate),
            ("record.update", TriggerKind::RecordUpdate),
            ("record.delete", TriggerKind::RecordDelete),
        ] {
            let id = format!("{prefix}:com.example.thing");
            let t = ParsedTrigger::parse(&id).unwrap();
            assert_eq!(t.kind, kind);
            assert_eq!(t.id(), id);
        }
    }

    #[test]
    fn parse_xrpc_triggers() {
        let q = ParsedTrigger::parse("xrpc.query:com.example.list").unwrap();
        assert_eq!(q.kind, TriggerKind::XrpcQuery);
        let p = ParsedTrigger::parse("xrpc.procedure:com.example.create").unwrap();
        assert_eq!(p.kind, TriggerKind::XrpcProcedure);
    }

    #[test]
    fn parse_labeler_apply_with_nsid() {
        let t = ParsedTrigger::parse("labeler.apply:app.bsky.feed.post").unwrap();
        assert_eq!(t.kind, TriggerKind::LabelerApply);
        assert_eq!(t.suffix, "app.bsky.feed.post");
    }

    #[test]
    fn parse_labeler_apply_actor_special_case() {
        let t = ParsedTrigger::parse("labeler.apply:_actor").unwrap();
        assert_eq!(t.kind, TriggerKind::LabelerApply);
        assert_eq!(t.suffix, "_actor");
    }

    #[test]
    fn rejects_no_colon() {
        let err = ParsedTrigger::parse("record.index").unwrap_err();
        assert!(err.contains("must contain a ':' separator"));
        assert!(err.contains("valid prefixes"));
    }

    #[test]
    fn rejects_unknown_prefix() {
        let err = ParsedTrigger::parse("garbage:com.example.thing").unwrap_err();
        assert!(err.contains("unknown trigger prefix 'garbage'"));
    }

    #[test]
    fn rejects_bad_nsid() {
        // single segment
        assert!(ParsedTrigger::parse("record.index:foo").is_err());
        // empty suffix
        assert!(ParsedTrigger::parse("record.index:").is_err());
        // non-letter start
        assert!(ParsedTrigger::parse("record.index:1.foo").is_err());
        // invalid char
        assert!(ParsedTrigger::parse("record.index:com.foo!bar").is_err());
    }

    #[test]
    fn allows_only_actor_special_case_for_labeler() {
        // _actor is not a valid NSID, but it's the literal special case.
        assert!(ParsedTrigger::parse("labeler.apply:_actor").is_ok());
        // Other prefixes don't get the _actor escape hatch.
        assert!(ParsedTrigger::parse("record.index:_actor").is_err());
    }

    #[test]
    fn label_uri_routes_at_uri_to_nsid() {
        assert_eq!(
            trigger_for_label_uri("at://did:plc:abc/app.bsky.feed.post/rkey1"),
            "labeler.apply:app.bsky.feed.post"
        );
    }

    #[test]
    fn label_uri_routes_bare_did_to_actor() {
        assert_eq!(trigger_for_label_uri("did:plc:abc"), "labeler.apply:_actor");
    }

    #[test]
    fn label_uri_routes_malformed_at_uri_to_actor() {
        // `at://` with no path → no second segment → actor.
        assert_eq!(
            trigger_for_label_uri("at://did:plc:abc"),
            "labeler.apply:_actor"
        );
        // `at://<did>/` → second segment exists but is empty → actor.
        assert_eq!(
            trigger_for_label_uri("at://did:plc:abc/"),
            "labeler.apply:_actor"
        );
    }

    #[test]
    fn script_language_round_trip() {
        assert_eq!(ScriptLanguage::Lua.as_str(), "lua");
        assert_eq!(ScriptLanguage::parse_str("lua"), Some(ScriptLanguage::Lua));
        assert_eq!(ScriptLanguage::parse_str("typescript"), None);
        assert_eq!(ScriptLanguage::default(), ScriptLanguage::Lua);
    }

    #[test]
    fn extract_helpers() {
        let v = serde_json::json!({"a": "x", "b": true, "c": null});
        assert_eq!(extract_string(&v, "a"), Some("x".into()));
        assert_eq!(extract_string(&v, "missing"), None);
        assert_eq!(extract_bool(&v, "b"), Some(true));
        assert_eq!(extract_bool(&v, "a"), None);
    }
}

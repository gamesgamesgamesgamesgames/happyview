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

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::event_log::{EventLog, Severity, log_event};
use crate::plugin::{ScriptExecuteOutput, ScriptValueKind};
use crate::script::{Invocation, Trigger, dispatch};

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
    JobRun,
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
            TriggerKind::JobRun => format!("job.run:{}", self.suffix),
        }
    }

    /// Parse a trigger id. Returns a structured error message naming the
    /// valid prefixes when the input doesn't match the grammar.
    pub fn parse(id: &str) -> Result<Self, String> {
        let (prefix, suffix) = id.split_once(':').ok_or_else(|| {
            format!(
                "trigger id '{id}' must contain a ':' separator; \
                 valid prefixes: record.{{index,create,update,delete}}:<nsid>, \
                 xrpc.{{query,procedure}}:<nsid>, labeler.apply:<nsid|_actor>, \
                 job.run:<type>"
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
            "job.run" => TriggerKind::JobRun,
            other => {
                return Err(format!(
                    "unknown trigger prefix '{other}'; valid prefixes: \
                     record.{{index,create,update,delete}}, xrpc.{{query,procedure}}, \
                     labeler.apply, job.run"
                ));
            }
        };

        // Suffix validation: NSID for most triggers, but `labeler.apply:_actor`
        // and `job.run:<type>` have their own formats.
        match kind {
            TriggerKind::JobRun => crate::jobs::validate_job_type(suffix)?,
            TriggerKind::LabelerApply if suffix == "_actor" => {}
            _ => happyview_nsid::validate_nsid(suffix).map_err(|e| e.to_string())?,
        }

        Ok(Self {
            kind,
            suffix: suffix.to_string(),
        })
    }
}

// ---------------------------------------------------------------------------
// Script row + resolution
// ---------------------------------------------------------------------------

/// The language the reference implementation in this crate reads, which is
/// what the codemod and the save-time compile check are written against.
/// Which other languages a script may be written in is a question about
/// installed interpreters, so only this one is a constant.
pub const NATIVE_LANGUAGE: &str = "lua";

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
    /// Which interpreter runs `body`, as the row stamped it.
    pub script_type: String,
    pub body: String,
}

/// Look up a single trigger id. Returns `None` when no row matches.
///
/// Which language the row names is not a filter here. A trigger an operator
/// bound to a script must resolve to that script whether or not its
/// interpreter is installed, so that the answer is the dispatcher's refusal
/// rather than a different script's output or silence.
pub async fn resolve(state: &AppState, trigger_id: &str) -> Option<ResolvedScript> {
    let sql = adapt_sql(
        "SELECT id, body, script_type FROM happyview_scripts WHERE id = ?",
        state.db_backend,
    );
    let row: Option<(String, String, String)> = match crate::db::query_as(&sql)
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
    Some(ResolvedScript {
        id,
        script_type,
        body,
    })
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

/// What a record-event script chain decided for an event.
///
/// Deliberately not `Option<Value>`. A delete carries no record body, so
/// "no script ran" and "the script returned `nil`" both used to spell
/// themselves `None`, and the delete path read the first as the second —
/// an instance with no scripts at all silently skipped every delete (#80).
#[derive(Debug, Clone, PartialEq)]
pub enum RecordHookOutcome {
    /// Index the event with the body it arrived with. Either no script ran,
    /// or the script waved the event through without rewriting it.
    Proceed,
    /// Index this body in place of the one that arrived. Only meaningful for
    /// create/update — a delete has no body to replace.
    Replace(Value),
    /// Skip the event entirely — the script returned nothing. Only ever
    /// produced by a script that actually ran.
    Skip,
}

/// Run the record-event script (if any) for a given event.
///
/// Failure mode is fail-open: a script that exhausts its retry budget is
/// dead-lettered and the event proceeds as if no script had run.
pub async fn run_record_event_script(
    state: &AppState,
    payload: RecordEventPayload<'_>,
) -> RecordHookOutcome {
    let resolved = match resolve_record_event(state, payload.nsid, payload.action).await {
        Some(s) => s,
        // No script for this trigger → index the event unchanged.
        None => return RecordHookOutcome::Proceed,
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
        &DeadLetterEntry {
            script: &resolved,
            host_kind: "record",
            host_id: &host_id,
            payload: &event_payload,
            error: &last_error,
            attempts: MAX_ATTEMPTS,
            collection: Some(payload.nsid),
        },
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

    // Fail-open: index the event as if no script had run. For a delete this
    // means the delete still happens — the record body it lacks is not a
    // reason to keep a record its PDS no longer has.
    RecordHookOutcome::Proceed
}

/// Single attempt at the record-event script. Used internally by the retry
/// loop and externally by admin retry endpoints.
///
/// Returns `Ok(Replace(value))` to continue indexing with `value`,
/// `Ok(Skip)` when the script returned nothing, `Ok(Proceed)` when it waved
/// the event through, or `Err(msg)` on any execution failure.
pub async fn run_record_event_once(
    state: &AppState,
    script: &ResolvedScript,
    payload: RecordEventPayload<'_>,
) -> Result<RecordHookOutcome, String> {
    let event = serde_json::json!({
        "action": payload.action,
        "uri": payload.uri,
        "did": payload.did,
        "collection": payload.nsid,
        "rkey": payload.rkey,
        "record": payload.record,
    });
    let outcome = dispatch(
        state,
        &Invocation {
            trigger_id: &script.id,
            trigger: Trigger::RecordEvent,
            language: &script.script_type,
            source: &script.body,
            input: &event,
            // The record's author is who a library reads as the caller, and a
            // firehose event carries none of their credentials.
            caller_did: Some(payload.did),
            has_pds_auth: false,
            method: None,
            collection: Some(payload.nsid),
            params: None,
            delegate_did: None,
            space: None,
        },
        None,
    )
    .await;

    match outcome.map_err(|e| e.to_string())? {
        ScriptExecuteOutput::Returned { value, value_kind } => Ok(match value_kind {
            ScriptValueKind::None => RecordHookOutcome::Skip,
            ScriptValueKind::Object => RecordHookOutcome::Replace(value),
            ScriptValueKind::Other => RecordHookOutcome::Proceed,
        }),
        ScriptExecuteOutput::Error { kind, raw, .. } => Err(failure_text(kind, &raw)),
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
    /// Skip persistence — the script returned nothing.
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
        match run_label_once(state, &resolved, &event).await {
            Ok(outcome) => {
                log_event(
                    &state.db,
                    EventLog {
                        event_type: "script.executed".to_string(),
                        severity: Severity::Info,
                        actor_did: None,
                        subject: Some(event.uri.clone()),
                        detail: serde_json::json!({
                            "host_kind": "label",
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
                    src = %event.src, uri = %event.uri,
                    trigger = %resolved.id,
                    attempt = attempt + 1,
                    "label script attempt failed: {last_error}"
                );
            }
        }
    }
    let collection = resolved
        .id
        .split_once(':')
        .map(|(_, suf)| suf)
        .filter(|s| *s != "_actor");
    write_dead_letter(
        state,
        &DeadLetterEntry {
            script: &resolved,
            host_kind: "label",
            host_id: &host_id,
            payload: &payload,
            error: &last_error,
            attempts: MAX_ATTEMPTS,
            collection,
        },
    )
    .await;
    log_event(
        &state.db,
        EventLog {
            event_type: "script.dead_lettered".to_string(),
            severity: Severity::Error,
            actor_did: None,
            subject: Some(event.uri.clone()),
            detail: serde_json::json!({
                "host_kind": "label",
                "host_id": host_id,
                "trigger": resolved.id,
                "error": last_error,
            }),
        },
        state.db_backend,
    )
    .await;
    LabelHookOutcome::Continue(original)
}

async fn run_label_once(
    state: &AppState,
    script: &ResolvedScript,
    event: &LabelAppliedEvent,
) -> Result<LabelHookOutcome, String> {
    let input = serde_json::to_value(event).map_err(|e| format!("encode event: {e}"))?;
    let outcome = dispatch(
        state,
        &Invocation {
            trigger_id: &script.id,
            trigger: Trigger::Label,
            language: &script.script_type,
            source: &script.body,
            input: &input,
            // A label arrives from a subscription, so there is nobody for the
            // run to act as and no collection of its own to read.
            caller_did: None,
            has_pds_auth: false,
            method: None,
            collection: None,
            params: None,
            delegate_did: None,
            space: None,
        },
        None,
    )
    .await;

    match outcome.map_err(|e| e.to_string())? {
        ScriptExecuteOutput::Returned { value, value_kind } => Ok(match value_kind {
            ScriptValueKind::None => LabelHookOutcome::Skip,
            // Any field the script omitted falls back to the original. This
            // makes "filter only" scripts (return `event`) and "rewrite val"
            // scripts (return `{ val = "..." }`) both ergonomic.
            ScriptValueKind::Object => LabelHookOutcome::Continue(LabelAppliedEvent {
                src: extract_string(&value, "src").unwrap_or_else(|| event.src.clone()),
                uri: extract_string(&value, "uri").unwrap_or_else(|| event.uri.clone()),
                val: extract_string(&value, "val").unwrap_or_else(|| event.val.clone()),
                neg: extract_bool(&value, "neg").unwrap_or(event.neg),
                cts: extract_string(&value, "cts").unwrap_or_else(|| event.cts.clone()),
                exp: extract_string(&value, "exp").or_else(|| event.exp.clone()),
            }),
            ScriptValueKind::Other => LabelHookOutcome::Continue(event.clone()),
        }),
        ScriptExecuteOutput::Error { kind, raw, .. } => Err(failure_text(kind, &raw)),
    }
}

/// How a failed run reads to an operator: the category it arrived with, then
/// the interpreter's unparsed text.
///
/// Every reader of this is an operator — the retry's warn, the dead-letter
/// row's only error column, a job row's `error` column, the
/// `script.dead_lettered` row — and neither half is recoverable from the
/// other. The text carries the line that points at the script and no category;
/// a spent budget and an exhausted heap describe themselves in no text at all,
/// so without the category they read as any other runtime failure.
pub(crate) fn failure_text(kind: crate::plugin::ScriptErrorKind, raw: &str) -> String {
    format!("{}: {raw}", kind.as_str())
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

struct DeadLetterEntry<'a> {
    script: &'a ResolvedScript,
    host_kind: &'a str,
    host_id: &'a str,
    payload: &'a Value,
    error: &'a str,
    attempts: u32,
    collection: Option<&'a str>,
}

async fn write_dead_letter(state: &AppState, entry: &DeadLetterEntry<'_>) {
    let payload_str = serde_json::to_string(entry.payload).unwrap_or_else(|_| "{}".to_string());
    let sql = adapt_sql(
        "INSERT INTO happyview_dead_letter_scripts
            (script_ref, host_kind, host_id, payload, error, attempts, created_at, collection)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        state.db_backend,
    );
    if let Err(e) = crate::db::query(&sql)
        .bind(entry.script.id.as_str())
        .bind(entry.host_kind)
        .bind(entry.host_id)
        .bind(&payload_str)
        .bind(entry.error)
        .bind(entry.attempts as i64)
        .bind(now_rfc3339())
        .bind(entry.collection)
        .execute(&state.db)
        .await
    {
        tracing::error!(
            host_kind = entry.host_kind,
            host_id = entry.host_id,
            trigger = %entry.script.id,
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

    /// The cascade reads trigger ids and nothing else. Skipping a row whose
    /// language has no interpreter would answer an event with a different
    /// script than the one its operator bound to that action, which is a
    /// quieter wrong answer than the failure the runner records instead.
    #[tokio::test]
    async fn the_cascade_prefers_the_action_trigger_whatever_language_it_names() {
        let state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
        let seed = |id: &'static str, script_type: &'static str| {
            let state = state.clone();
            async move {
                let now = now_rfc3339();
                let sql = adapt_sql(
                    "INSERT INTO happyview_scripts (id, script_type, body, created_at, updated_at)
                     VALUES (?, ?, ?, ?, ?)",
                    state.db_backend,
                );
                crate::db::query(&sql)
                    .bind(id)
                    .bind(script_type)
                    .bind("function handle() end")
                    .bind(&now)
                    .bind(&now)
                    .execute(&state.db)
                    .await
                    .expect("seed a script row");
            }
        };
        seed("record.create:com.example.thing", "typescript").await;
        seed("record.index:com.example.thing", NATIVE_LANGUAGE).await;

        let cascaded = resolve_record_event(&state, "com.example.thing", "create")
            .await
            .expect("the cascade resolves the action trigger");
        assert_eq!(cascaded.id, "record.create:com.example.thing");
        assert_eq!(cascaded.script_type, "typescript");

        let cascaded = resolve_record_event(&state, "com.example.thing", "update")
            .await
            .expect("no update trigger, so the wildcard answers");
        assert_eq!(cascaded.id, "record.index:com.example.thing");
    }

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
    fn parse_job_run_trigger() {
        let t = ParsedTrigger::parse("job.run:test.export").unwrap();
        assert_eq!(t.kind, TriggerKind::JobRun);
        assert_eq!(t.suffix, "test.export");
        assert_eq!(t.id(), "job.run:test.export");
    }

    #[test]
    fn rejects_bad_job_type() {
        assert!(ParsedTrigger::parse("job.run:UPPER").is_err());
        assert!(ParsedTrigger::parse("job.run:has space").is_err());
        assert!(ParsedTrigger::parse("job.run:").is_err());
    }

    #[test]
    fn rejects_unknown_prefix() {
        let err = ParsedTrigger::parse("garbage:com.example.thing").unwrap_err();
        assert!(err.contains("unknown trigger prefix 'garbage'"));
    }

    #[test]
    fn delegates_nsid_validation_to_the_shared_crate() {
        // Shape rules are the crate's job and are pinned by the interop corpus
        // there. This only proves the wiring.
        assert!(ParsedTrigger::parse("xrpc.query:pics.2bit.feed.getPhotos").is_ok());
        assert!(ParsedTrigger::parse("record.index:1.foo").is_err());
        assert!(ParsedTrigger::parse("record.index:").is_err());
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
    fn extract_helpers() {
        let v = serde_json::json!({"a": "x", "b": true, "c": null});
        assert_eq!(extract_string(&v, "a"), Some("x".into()));
        assert_eq!(extract_string(&v, "missing"), None);
        assert_eq!(extract_bool(&v, "b"), Some(true));
        assert_eq!(extract_bool(&v, "a"), None);
    }
}

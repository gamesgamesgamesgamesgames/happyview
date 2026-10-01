//! What a runner knows about one invocation, and the input it becomes.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use crate::AppState;
use crate::db::adapt_sql;
use crate::lua::TriggerKind;
use crate::plugin::{
    DEFAULT_SCRIPT_MEMORY_BYTES, ScriptExecuteContext, ScriptExecuteInput, ScriptExecuteLimits,
    ScriptJob, ScriptKind, ScriptLibraryRef, ScriptSpace,
};

/// Which runner is dispatching. A job run carries the job it reports to,
/// because [`ScriptKind::Job`] and `context.job` are independent fields on the
/// wire and holding the id here is what stops them disagreeing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger<'a> {
    RecordEvent,
    Label,
    XrpcQuery,
    XrpcProcedure,
    Job { id: &'a str },
}

impl Trigger<'_> {
    /// The contract the script's result is read under.
    pub fn kind(self) -> ScriptKind {
        match self {
            Self::RecordEvent => ScriptKind::RecordEvent,
            Self::Label => ScriptKind::Label,
            Self::XrpcQuery => ScriptKind::XrpcQuery,
            Self::XrpcProcedure => ScriptKind::XrpcProcedure,
            Self::Job { .. } => ScriptKind::Job,
        }
    }

    /// The trigger a parsed trigger id names. The four record actions share
    /// one, since the contract a record-event script is read under does not
    /// distinguish them. `job.run` answers `None`: the job id a job run needs
    /// is the worker's, not the id's.
    pub fn for_trigger_kind(kind: TriggerKind) -> Option<Self> {
        Some(match kind {
            TriggerKind::RecordIndex
            | TriggerKind::RecordCreate
            | TriggerKind::RecordUpdate
            | TriggerKind::RecordDelete => Self::RecordEvent,
            TriggerKind::LabelerApply => Self::Label,
            TriggerKind::XrpcQuery => Self::XrpcQuery,
            TriggerKind::XrpcProcedure => Self::XrpcProcedure,
            TriggerKind::JobRun => return None,
        })
    }
}

/// One invocation, as the runner that fired it sees it. The three fields a
/// script row supplies are its id (the trigger string), its `script_type` and
/// its body; the rest is the event.
#[derive(Clone, Copy, Debug)]
pub struct Invocation<'a> {
    pub trigger_id: &'a str,
    pub trigger: Trigger<'a>,
    /// Which interpreter runs `source`.
    pub language: &'a str,
    pub source: &'a str,
    /// The first argument of `handle`.
    pub input: &'a Value,
    pub caller_did: Option<&'a str>,
    pub has_pds_auth: bool,
    pub method: Option<&'a str>,
    pub collection: Option<&'a str>,
    pub params: Option<&'a HashMap<String, Value>>,
    pub delegate_did: Option<&'a str>,
    pub space: Option<&'a ScriptSpace>,
}

/// Build the input for one invocation: the kind from the trigger, the context
/// from the fields the runner supplied, the libraries from what is installed,
/// and the budgets from the instance's own settings rather than the call site.
///
/// Execution time is not here. The host owns it as an epoch deadline, armed
/// per run by `execute_script_as`.
pub async fn build_input(state: &AppState, inv: &Invocation<'_>) -> ScriptExecuteInput {
    let kind = inv.trigger.kind();
    let libraries = state
        .plugin_executor()
        .library_index()
        .await
        .into_iter()
        .map(|entry| ScriptLibraryRef {
            namespace: entry.namespace,
            id: entry.id,
        })
        .collect();
    ScriptExecuteInput {
        source: inv.source.to_string(),
        kind,
        input: inv.input.clone(),
        context: ScriptExecuteContext {
            trigger: inv.trigger_id.to_string(),
            caller_did: inv.caller_did.map(str::to_string),
            has_pds_auth: inv.has_pds_auth,
            env: load_env(state).await,
            method: inv.method.map(str::to_string),
            collection: inv.collection.map(str::to_string),
            params: inv
                .params
                .map(|params| params.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
            delegate_did: inv.delegate_did.map(str::to_string),
            space: inv.space.cloned(),
            // Read through the trigger rather than beside it, so a job id
            // cannot reach another kind and a job run cannot lose one.
            job: match inv.trigger {
                Trigger::Job { id } => Some(ScriptJob { id: id.to_string() }),
                _ => None,
            },
        },
        libraries,
        limits: ScriptExecuteLimits {
            // A job has no instruction budget: running long is what a job is
            // for, and `should_stop` is its stop.
            instructions: match kind {
                ScriptKind::Job => None,
                _ => Some(state.script_limits.instruction_limit()),
            },
            memory_bytes: DEFAULT_SCRIPT_MEMORY_BYTES as u64,
        },
        // `execute_script_as` overrides the guard list with the host's own,
        // so sending one here would only mislead a reader.
        removed_globals: Vec::new(),
    }
}

/// Script variables, which every kind of run reads as `ctx.env`.
async fn load_env(state: &AppState) -> BTreeMap<String, String> {
    let sql = adapt_sql(
        "SELECT key, value FROM happyview_script_variables",
        state.db_backend,
    );
    crate::db::query_as::<(String, String)>(&sql)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{migrated_memory_pool, test_state_with_pool};
    use serde_json::json;

    async fn state_with_one_variable() -> AppState {
        let state = test_state_with_pool(migrated_memory_pool().await);
        crate::db::query(
            "INSERT INTO happyview_script_variables (key, value) VALUES ('API_KEY', 'k')",
        )
        .execute(&state.db)
        .await
        .expect("seed a script variable");
        state
    }

    fn space() -> ScriptSpace {
        ScriptSpace {
            uri: "at://did:plc:a/space/com.example.type/sk".into(),
            id: "sid".into(),
            did: "did:plc:a".into(),
            authority_did: "did:plc:auth".into(),
            type_nsid: "com.example.type".into(),
            skey: "sk".into(),
        }
    }

    /// The bare invocation every test varies: a trigger, a body and an input.
    fn invocation<'a>(
        trigger_id: &'a str,
        trigger: Trigger<'a>,
        input: &'a Value,
    ) -> Invocation<'a> {
        Invocation {
            trigger_id,
            trigger,
            language: "lua",
            source: "function handle() end",
            input,
            caller_did: None,
            has_pds_auth: false,
            method: None,
            collection: None,
            params: None,
            delegate_did: None,
            space: None,
        }
    }

    /// Every trigger the grammar admits reaches exactly one `ScriptKind`, and
    /// the four record actions share theirs — the script contract does not
    /// distinguish them, only the event payload does.
    #[test]
    fn every_trigger_kind_reaches_its_script_kind() {
        for (kind, expected) in [
            (TriggerKind::RecordIndex, ScriptKind::RecordEvent),
            (TriggerKind::RecordCreate, ScriptKind::RecordEvent),
            (TriggerKind::RecordUpdate, ScriptKind::RecordEvent),
            (TriggerKind::RecordDelete, ScriptKind::RecordEvent),
            (TriggerKind::LabelerApply, ScriptKind::Label),
            (TriggerKind::XrpcQuery, ScriptKind::XrpcQuery),
            (TriggerKind::XrpcProcedure, ScriptKind::XrpcProcedure),
        ] {
            assert_eq!(
                Trigger::for_trigger_kind(kind).map(Trigger::kind),
                Some(expected),
                "{kind:?}"
            );
        }
        assert_eq!(
            Trigger::for_trigger_kind(TriggerKind::JobRun),
            None,
            "a job run is only buildable with the job id it reports to"
        );
        assert_eq!(Trigger::Job { id: "j" }.kind(), ScriptKind::Job);
    }

    /// A procedure's context carries every request field, and the budgets come
    /// from the instance rather than the call site.
    #[tokio::test]
    async fn a_procedure_input_carries_the_request_and_the_budgets() {
        let state = state_with_one_variable().await;
        let input = json!({ "text": "hi" });
        let params = HashMap::from([("limit".to_string(), json!(10))]);
        let space = space();

        let built = build_input(
            &state,
            &Invocation {
                caller_did: Some("did:plc:me"),
                has_pds_auth: true,
                method: Some("com.example.post"),
                collection: Some("com.example.record"),
                params: Some(&params),
                delegate_did: Some("did:plc:other"),
                space: Some(&space),
                ..invocation(
                    "xrpc.procedure:com.example.post",
                    Trigger::XrpcProcedure,
                    &input,
                )
            },
        )
        .await;

        assert_eq!(built.kind, ScriptKind::XrpcProcedure);
        assert_eq!(built.source, "function handle() end");
        assert_eq!(built.input, input);
        assert_eq!(built.context.trigger, "xrpc.procedure:com.example.post");
        assert_eq!(built.context.caller_did.as_deref(), Some("did:plc:me"));
        assert!(built.context.has_pds_auth);
        assert_eq!(
            built.context.env,
            BTreeMap::from([("API_KEY".to_string(), "k".to_string())]),
            "env is the instance's script variables"
        );
        assert_eq!(built.context.method.as_deref(), Some("com.example.post"));
        assert_eq!(
            built.context.collection.as_deref(),
            Some("com.example.record")
        );
        assert_eq!(built.context.params.as_ref().unwrap()["limit"], json!(10));
        assert_eq!(built.context.delegate_did.as_deref(), Some("did:plc:other"));
        assert_eq!(built.context.space.as_ref(), Some(&space));
        assert_eq!(built.context.job, None);
        assert_eq!(
            built.limits.instructions,
            Some(state.script_limits.instruction_limit())
        );
        assert_eq!(
            built.limits.memory_bytes,
            DEFAULT_SCRIPT_MEMORY_BYTES as u64
        );
        assert!(
            built.libraries.is_empty(),
            "no library is installed in this instance"
        );
        assert!(
            built.removed_globals.is_empty(),
            "the guard list is the host's to fill"
        );
    }

    /// A field a runner has no value for is absent from the JSON rather than
    /// null, so an interpreter for a language that tells the two apart renders
    /// each correctly.
    #[tokio::test]
    async fn a_field_a_kind_has_no_value_for_is_absent() {
        let state = state_with_one_variable().await;
        let input = json!({ "action": "create" });

        let record = build_input(
            &state,
            &Invocation {
                caller_did: Some("did:plc:author"),
                collection: Some("com.example.record"),
                ..invocation(
                    "record.create:com.example.record",
                    Trigger::RecordEvent,
                    &input,
                )
            },
        )
        .await;
        assert_eq!(record.kind, ScriptKind::RecordEvent);
        let json = serde_json::to_value(&record).unwrap();
        for absent in ["method", "params", "delegate_did", "space", "job"] {
            assert!(
                json["context"].get(absent).is_none(),
                "a record event has no {absent}"
            );
        }
        assert!(
            json.get("removed_globals").is_none(),
            "an empty guard list is not sent"
        );

        let label = build_input(
            &state,
            &invocation("labeler.apply:_actor", Trigger::Label, &input),
        )
        .await;
        assert_eq!(label.kind, ScriptKind::Label);
        let json = serde_json::to_value(&label).unwrap();
        for absent in [
            "caller_did",
            "method",
            "collection",
            "params",
            "delegate_did",
            "space",
            "job",
        ] {
            assert!(
                json["context"].get(absent).is_none(),
                "a label run has no {absent}"
            );
        }
        assert_eq!(
            json["context"]["has_pds_auth"], false,
            "a label run acts as nobody"
        );
    }

    /// A job is the only kind carrying `context.job` and the only one with no
    /// instruction budget: running long is what a job is for. Both follow from
    /// the trigger, so neither can be set without the other.
    #[tokio::test]
    async fn only_a_job_names_a_job_and_runs_unbudgeted() {
        let state = state_with_one_variable().await;
        let input = json!({});

        let job = build_input(
            &state,
            &Invocation {
                caller_did: Some("did:plc:creator"),
                ..invocation("job.run:rebuild", Trigger::Job { id: "job-0001" }, &input)
            },
        )
        .await;
        assert_eq!(job.kind, ScriptKind::Job);
        assert_eq!(
            job.context.job,
            Some(ScriptJob {
                id: "job-0001".into()
            })
        );
        assert_eq!(job.limits.instructions, None);
        let json = serde_json::to_value(&job).unwrap();
        assert!(
            json["limits"]["instructions"].is_null(),
            "the key is sent as null, so the interpreter can tell it from a host that did not decide"
        );

        for trigger in [
            Trigger::RecordEvent,
            Trigger::Label,
            Trigger::XrpcQuery,
            Trigger::XrpcProcedure,
        ] {
            let other = build_input(
                &state,
                &Invocation {
                    trigger,
                    ..invocation("xrpc.query:com.example.list", trigger, &input)
                },
            )
            .await;
            assert_ne!(other.kind, ScriptKind::Job, "{trigger:?}");
            assert_eq!(other.context.job, None, "{trigger:?}");
            assert!(other.limits.instructions.is_some(), "{trigger:?}");
        }
    }
}

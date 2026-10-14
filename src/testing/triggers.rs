//! `host_test_trigger`: each kind reaches its trigger by the entry point the
//! real event uses, so lexicon validation, auth, indexing and the script
//! contract are all exercised, not only the script.

use axum::body::Body;
use axum::http::{Method, Request, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::AppState;
use crate::lexicon::LexiconType;
use crate::plugin::{TestTrigger, TestTriggerKind, TestTriggerResult};
use crate::record_handler::{RecordEvent, RecordOutcome, handle_record_event};

use super::harness::{ForgedCaller, TestHarness, TestHostError};
use super::{DEFAULT_DID, TEST_CLIENT_KEY, TEST_HOST};

const DEFAULT_LABELER: &str = "did:plc:labeler";

pub async fn fire(
    state: &AppState,
    harness: &TestHarness,
    spec: TestTrigger,
) -> Result<TestTriggerResult, TestHostError> {
    match spec.kind {
        TestTriggerKind::Query => xrpc(state, Method::GET, &spec).await,
        TestTriggerKind::Procedure => xrpc(state, Method::POST, &spec).await,
        TestTriggerKind::RecordCreate
        | TestTriggerKind::RecordUpdate
        | TestTriggerKind::RecordDelete => record(state, harness, &spec).await,
        TestTriggerKind::Job => job(state, &spec).await,
        TestTriggerKind::Label => label(state, harness, &spec).await,
    }
}

fn scalar(key: &str, value: &Value) -> Result<String, TestHostError> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => Err(TestHostError::BadInput(format!(
            "query parameter '{key}' must be a string, number, boolean or a list of them"
        ))),
    }
}

/// Parameters as a query string, a list becoming a repeated key the way the
/// XRPC parameter parser reads one. Keys are sorted, so a request is the same
/// whatever order the guest's table iterated in.
pub(crate) fn query_string(input: &Value) -> Result<String, TestHostError> {
    let map = match input {
        Value::Null => return Ok(String::new()),
        Value::Object(map) => map,
        _ => {
            return Err(TestHostError::BadInput(
                "a query's input is its parameters, an object".into(),
            ));
        }
    };
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort();
    let mut pairs = Vec::new();
    for key in keys {
        match &map[key] {
            Value::Array(items) => {
                for item in items {
                    pairs.push((key, scalar(key, item)?));
                }
            }
            value => pairs.push((key, scalar(key, value)?)),
        }
    }
    Ok(pairs
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&"))
}

pub(crate) fn body_value(bytes: &[u8]) -> Value {
    if bytes.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(bytes).into_owned()))
}

/// The target spliced into `/xrpc/{target}`. Anything but an NSID could walk
/// the path out of `/xrpc/`, and the routes beyond it, `/auth/*` among them,
/// write through clients a test file's state shares with the template.
fn xrpc_method(target: &str) -> Result<&str, TestHostError> {
    happyview_nsid::validate_nsid(target).map_err(|e| {
        TestHostError::BadInput(format!(
            "a query or procedure trigger names an XRPC method, which is an NSID: {e}"
        ))
    })?;
    Ok(target)
}

async fn xrpc(
    state: &AppState,
    method: Method,
    spec: &TestTrigger,
) -> Result<TestTriggerResult, TestHostError> {
    let nsid = xrpc_method(&spec.target)?;
    let query = if method == Method::GET {
        query_string(&spec.input)?
    } else {
        String::new()
    };
    let uri = if query.is_empty() {
        format!("/xrpc/{nsid}")
    } else {
        format!("/xrpc/{nsid}?{query}")
    };
    let body = if method == Method::POST {
        Body::from(serde_json::to_vec(&spec.input).unwrap_or_default())
    } else {
        Body::empty()
    };
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, TEST_HOST)
        .header("x-client-key", TEST_CLIENT_KEY)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .map_err(|e| TestHostError::BadInput(format!("not a request: {e}")))?;
    if let Some(did) = &spec.as_did {
        request
            .extensions_mut()
            .insert(ForgedCaller::new(did.clone()));
    }
    // Built per request rather than kept on the harness, which the state it
    // would capture already holds.
    let response = crate::server::router(state.clone())
        .oneshot(request)
        .await
        .map_err(|e| TestHostError::Host(e.to_string()))?;
    let status = response.status().as_u16();
    let bytes = response
        .into_body()
        .collect()
        .await
        .map_err(|e| TestHostError::Host(format!("reading the response: {e}")))?
        .to_bytes();
    Ok(TestTriggerResult {
        status,
        body: body_value(&bytes),
    })
}

fn outcome_name(outcome: RecordOutcome) -> &'static str {
    match outcome {
        RecordOutcome::Matched => "matched",
        RecordOutcome::Skipped => "skipped",
        RecordOutcome::SchemaEvent => "schema_event",
        RecordOutcome::Errored => "errored",
        RecordOutcome::Unavailable => "unavailable",
        RecordOutcome::Invalid => "invalid",
    }
}

async fn record(
    state: &AppState,
    harness: &TestHarness,
    spec: &TestTrigger,
) -> Result<TestTriggerResult, TestHostError> {
    let action = match spec.kind {
        TestTriggerKind::RecordCreate => "create",
        TestTriggerKind::RecordUpdate => "update",
        _ => "delete",
    };
    // The record handler skips an untracked collection silently, which in a
    // test reads as a trigger that ran and did nothing.
    let tracked = state
        .lexicons
        .get(&spec.target)
        .await
        .is_some_and(|lex| lex.lexicon_type == LexiconType::Record);
    if !tracked {
        return Err(TestHostError::BadInput(format!(
            "{} has no record lexicon in this project, so no record trigger can fire for it",
            spec.target
        )));
    }
    let did = spec
        .did
        .clone()
        .or_else(|| spec.as_did.clone())
        .unwrap_or_else(|| DEFAULT_DID.to_string());
    let rkey = match (&spec.rkey, action) {
        (Some(rkey), _) => rkey.clone(),
        (None, "create") => crate::tid::generate_tid(),
        (None, _) => {
            return Err(TestHostError::BadInput(
                "record_update and record_delete need the rkey of the record they change".into(),
            ));
        }
    };
    let (record, cid) = if action == "delete" {
        (None, None)
    } else {
        let mut record = match &spec.input {
            Value::Object(_) => spec.input.clone(),
            Value::Null => json!({}),
            _ => return Err(TestHostError::BadInput("a record is an object".into())),
        };
        if let Some(object) = record.as_object_mut() {
            object
                .entry("$type")
                .or_insert_with(|| Value::String(spec.target.clone()));
        }
        let cid = crate::cid_verify::compute_record_cid(&record)
            .ok_or_else(|| {
                TestHostError::BadInput("the record cannot be encoded as DAG-CBOR".into())
            })?
            .to_string();
        (Some(record), Some(cid))
    };
    let uri = format!("at://{did}/{}/{rkey}", spec.target);
    harness.take_script_failures();
    let outcome = handle_record_event(
        state,
        &RecordEvent {
            did,
            collection: spec.target.clone(),
            rkey,
            action: action.to_string(),
            record,
            cid: cid.clone(),
            // No rev, as a dead-letter retry carries none: a revless delete
            // of a revless row leaves no tombstone, so a test may delete a
            // record and create it again under the same rkey.
            rev: None,
        },
    )
    .await;
    let errors = harness.take_script_failures();
    let failed = matches!(
        outcome,
        RecordOutcome::Errored | RecordOutcome::Unavailable | RecordOutcome::Invalid
    );
    let status = if errors.is_empty() && !failed {
        200
    } else {
        500
    };
    Ok(TestTriggerResult {
        status,
        body: json!({"outcome": outcome_name(outcome), "uri": uri, "cid": cid, "errors": errors}),
    })
}

async fn job(state: &AppState, spec: &TestTrigger) -> Result<TestTriggerResult, TestHostError> {
    crate::jobs::validate_job_type(&spec.target).map_err(TestHostError::BadInput)?;
    let created_by = spec
        .as_did
        .clone()
        .unwrap_or_else(|| DEFAULT_DID.to_string());
    let host = |e: crate::error::AppError| TestHostError::Host(e.to_string());
    let id = crate::jobs::db::create_job(
        state,
        &spec.target,
        &spec.input,
        &created_by,
        spec.as_did.is_some(),
        None,
        None,
    )
    .await
    .map_err(host)?;
    // No worker runs in a test, so a job runs exactly when the test fires it.
    crate::jobs::db::set_status(state, &id, "running")
        .await
        .map_err(host)?;
    let job = crate::jobs::db::get_job(state, &id)
        .await
        .map_err(host)?
        .ok_or_else(|| TestHostError::Host("the job row vanished".into()))?;
    crate::jobs::worker::execute_job(state, &job).await;
    let job = crate::jobs::db::get_job(state, &id)
        .await
        .map_err(host)?
        .ok_or_else(|| TestHostError::Host("the job row vanished".into()))?;
    Ok(TestTriggerResult {
        status: if job.status == "completed" { 200 } else { 500 },
        body: json!({
            "id": job.id, "status": job.status, "result": job.result,
            "error": job.error, "progress": job.progress,
        }),
    })
}

#[derive(serde::Deserialize)]
struct LabelInput {
    val: String,
    #[serde(default)]
    src: Option<String>,
    #[serde(default)]
    neg: bool,
    #[serde(default)]
    cts: Option<String>,
    #[serde(default)]
    exp: Option<String>,
}

async fn label(
    state: &AppState,
    harness: &TestHarness,
    spec: &TestTrigger,
) -> Result<TestTriggerResult, TestHostError> {
    let input: LabelInput = serde_json::from_value(spec.input.clone()).map_err(|e| {
        TestHostError::BadInput(format!(
            "a label trigger's input is {{val, src?, neg?, cts?, exp?}}: {e}"
        ))
    })?;
    harness.take_script_failures();
    let applied = crate::labeler::apply_label_event(
        state,
        crate::lua::LabelAppliedEvent {
            src: input.src.unwrap_or_else(|| DEFAULT_LABELER.to_string()),
            uri: spec.target.clone(),
            val: input.val,
            neg: input.neg,
            cts: input.cts.unwrap_or_else(crate::db::now_rfc3339),
            exp: input.exp,
        },
    )
    .await;
    let errors = harness.take_script_failures();
    Ok(TestTriggerResult {
        status: if errors.is_empty() { 200 } else { 500 },
        body: json!({"applied": applied, "errors": errors}),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn query_parameters_repeat_array_keys_and_encode_values() {
        assert_eq!(
            query_string(&json!({"tag": ["a", "b c"], "limit": 5, "on": true})).unwrap(),
            "limit=5&on=true&tag=a&tag=b%20c"
        );
        assert_eq!(query_string(&json!(null)).unwrap(), "");
        assert!(query_string(&json!([1])).is_err());
        assert!(query_string(&json!({"nested": {"a": 1}})).is_err());
    }

    #[test]
    fn a_response_body_is_json_text_or_null() {
        assert_eq!(body_value(br#"{"a":1}"#), json!({"a": 1}));
        assert_eq!(body_value(b"plain"), json!("plain"));
        assert_eq!(body_value(b""), json!(null));
    }
}

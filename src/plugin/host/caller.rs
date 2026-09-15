//! Host imports that act as the calling script's user: repo writes, blob
//! uploads and XRPC calls issued with the caller's own credentials. Each one
//! takes the same route the Lua globals take, so DPoP nonce handling, token
//! refresh and the local XRPC handlers are shared rather than reimplemented.

use std::collections::HashMap;

use axum::body::Bytes;
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;
use serde_json::{Value, json};

use crate::AppState;
use crate::auth::Claims;
use crate::error::AppError;

use happyview_plugin_sdk::wire::{
    CallerBlobUpload, CallerRecordCreate, CallerRecordDelete, CallerRecordPut, CallerXrpcProcedure,
    CallerXrpcQuery, RecordRef,
};

use crate::plugin::caller::{CallerSession, check_writable_repo};

#[derive(Debug, thiserror::Error)]
pub enum CallerError {
    /// The user's session is gone or was refused. It has to stay distinct all
    /// the way out: a dead session is a 401 the client can act on, and folding
    /// it in with everything else reports it as a 500 the client cannot.
    #[error("{0}")]
    Auth(String),
    #[error("{0}")]
    WritableRepo(String),
    #[error("PDS returned {status}: {body}")]
    Pds { status: u16, body: String },
    #[error("XRPC returned {status}: {body}")]
    Xrpc { status: u16, body: String },
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    Other(String),
}

impl CallerError {
    /// The envelope code a guest sees. A write refused for pointing at the
    /// wrong repo and one the PDS rejected are different problems with
    /// different fixes, so they never share a code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Auth(_) => "AUTH_REQUIRED",
            Self::WritableRepo(_) => "WRITABLE_REPO",
            Self::Pds { .. } => "PDS_ERROR",
            Self::Xrpc { .. } => "XRPC_ERROR",
            Self::Validation(_) => "BAD_INPUT",
            Self::Other(_) => "HOST_ERROR",
        }
    }
}

/// `{repo, collection, rkey?, record, validate}`, the body
/// `com.atproto.repo.createRecord` takes.
pub fn create_body(
    repo: &str,
    collection: &str,
    rkey: Option<&str>,
    record: &Value,
    validate: bool,
) -> Value {
    let mut body = json!({
        "repo": repo,
        "collection": collection,
        "record": record,
        "validate": validate,
    });
    if let Some(rkey) = rkey {
        body["rkey"] = json!(rkey);
    }
    body
}

/// `{repo, collection, rkey, record, validate, swapRecord?}`, the body
/// `com.atproto.repo.putRecord` takes. `swapRecord` is the caller's
/// no-create guarantee and is sent only when they supplied one.
fn put_body(
    repo: &str,
    collection: &str,
    rkey: &str,
    record: &Value,
    swap_cid: Option<&str>,
    validate: bool,
) -> Value {
    let mut body = json!({
        "repo": repo,
        "collection": collection,
        "rkey": rkey,
        "record": record,
        "validate": validate,
    });
    if let Some(cid) = swap_cid {
        body["swapRecord"] = json!(cid);
    }
    body
}

/// `{repo, collection, rkey}`, the body `com.atproto.repo.deleteRecord` takes.
fn delete_body(repo: &str, collection: &str, rkey: &str) -> Value {
    json!({
        "repo": repo,
        "collection": collection,
        "rkey": rkey,
    })
}

/// Required fields the lexicon names must be present and non-null. The PDS
/// checks this too; doing it here means the error names the field rather than
/// arriving as an opaque `InvalidRequest` after a round trip.
pub fn validate_required_fields(record: &Value, schema: &Value) -> Result<(), String> {
    let Some(required) = schema.get("required").and_then(|v| v.as_array()) else {
        return Ok(());
    };
    for field in required {
        let Some(name) = field.as_str() else {
            continue;
        };
        if record.get(name).is_none_or(Value::is_null) {
            return Err(format!("missing required field '{name}'"));
        }
    }
    Ok(())
}

/// A PDS write that never produced a response. `AppError` carries a status
/// only when the failure was itself a PDS status, so anything else reports 0
/// rather than a guessed code.
fn pds_failure(e: AppError) -> CallerError {
    match e {
        AppError::Auth(msg) => CallerError::Auth(msg),
        AppError::PdsError(status, body) => CallerError::Pds {
            status: status.as_u16(),
            body: String::from_utf8_lossy(&body).into_owned(),
        },
        other => CallerError::Pds {
            status: 0,
            body: other.to_string(),
        },
    }
}

/// A local XRPC handler that refused before producing a response. The status
/// is the one the same failure would have reached an HTTP client with, taken
/// from `AppError`'s own rendering so the two cannot drift.
fn xrpc_failure(e: AppError) -> CallerError {
    let body = e.to_string();
    CallerError::Xrpc {
        status: e.into_response().status().as_u16(),
        body,
    }
}

/// Split `at://{did}/{collection}/{rkey}` into its three parts.
fn parse_at_uri(uri: &str) -> Result<(String, String, String), CallerError> {
    let invalid = || CallerError::Validation(format!("invalid AT URI: {uri}"));
    let rest = uri.strip_prefix("at://").ok_or_else(invalid)?;
    let mut parts = rest.splitn(3, '/');
    let (Some(did), Some(collection), Some(rkey)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(invalid());
    };
    if did.is_empty() || collection.is_empty() || rkey.is_empty() {
        return Err(invalid());
    }
    Ok((did.to_string(), collection.to_string(), rkey.to_string()))
}

pub async fn create_record(
    caller: &CallerSession,
    spec: CallerRecordCreate,
) -> Result<RecordRef, CallerError> {
    let repo = spec
        .repo
        .unwrap_or_else(|| caller.default_repo().to_string());
    check_writable_repo(&repo, &caller.did, caller.delegate_did.as_deref())
        .map_err(CallerError::WritableRepo)?;
    if spec.validate {
        check_against_lexicon(caller, &spec.collection, &spec.record).await?;
    }
    let body = create_body(
        &repo,
        &spec.collection,
        spec.rkey.as_deref(),
        &spec.record,
        spec.validate,
    );
    let result = post(caller, &repo, "com.atproto.repo.createRecord", &body).await?;
    record_ref(&result)
}

pub async fn put_record(
    caller: &CallerSession,
    spec: CallerRecordPut,
) -> Result<RecordRef, CallerError> {
    let (repo, collection, rkey) = parse_at_uri(&spec.uri)?;
    check_writable_repo(&repo, &caller.did, caller.delegate_did.as_deref())
        .map_err(CallerError::WritableRepo)?;
    if spec.validate {
        check_against_lexicon(caller, &collection, &spec.record).await?;
    }
    let body = put_body(
        &repo,
        &collection,
        &rkey,
        &spec.record,
        spec.swap_cid.as_deref(),
        spec.validate,
    );
    let result = post(caller, &repo, "com.atproto.repo.putRecord", &body).await?;
    record_ref(&result)
}

pub async fn delete_record(
    caller: &CallerSession,
    spec: CallerRecordDelete,
) -> Result<(), CallerError> {
    let (repo, collection, rkey) = parse_at_uri(&spec.uri)?;
    check_writable_repo(&repo, &caller.did, caller.delegate_did.as_deref())
        .map_err(CallerError::WritableRepo)?;
    let body = delete_body(&repo, &collection, &rkey);
    post(caller, &repo, "com.atproto.repo.deleteRecord", &body).await?;
    Ok(())
}

/// Returns the PDS's blob reference verbatim, so the guest can drop it into a
/// record field unchanged.
pub async fn upload_blob(
    caller: &CallerSession,
    spec: CallerBlobUpload,
) -> Result<Value, CallerError> {
    crate::lua::atproto_api::upload_blob_to_pds(
        &caller.app_state,
        &caller.did,
        &caller.pds_auth,
        &spec.mime_type,
        Bytes::from(spec.bytes),
    )
    .await
    .map_err(pds_failure)
}

/// Unlike every other `host_caller_*` import, this one runs with no
/// [`CallerSession`] at all — exactly the way the Lua `xrpc.query` global
/// needs no PDS auth for a query, record-event or label script. `caller_did`
/// stands in for a session's claims when there is no session; a fully
/// anonymous call still reaches a registered local handler or the proxy, the
/// same as it always did.
pub async fn xrpc_query(
    app_state: &AppState,
    caller: Option<&CallerSession>,
    caller_did: Option<&str>,
    spec: CallerXrpcQuery,
) -> Result<Value, CallerError> {
    let mut params: HashMap<String, Value> = spec.params.into_iter().collect();
    let internal_claims;
    let claims: Option<&Claims> = match caller {
        Some(session) => Some(session.claims.as_ref()),
        None => {
            internal_claims = caller_did.map(|did| Claims::internal(did.to_string()));
            internal_claims.as_ref()
        }
    };
    let response =
        crate::lua::xrpc_api::execute_local_query(app_state, &spec.method, &mut params, claims)
            .await
            .map_err(xrpc_failure)?;
    response_json(response).await
}

pub async fn xrpc_procedure(
    caller: &CallerSession,
    spec: CallerXrpcProcedure,
) -> Result<Value, CallerError> {
    let mut params: HashMap<String, Value> = spec.params.into_iter().collect();
    let response = crate::lua::xrpc_api::execute_local_procedure(
        &caller.app_state,
        &spec.method,
        &caller.claims,
        &spec.input,
        &mut params,
    )
    .await
    .map_err(xrpc_failure)?;
    response_json(response).await
}

/// A collection with no uploaded lexicon is left to the PDS to judge: this
/// instance's registry is not the authority on somebody else's schema.
async fn check_against_lexicon(
    caller: &CallerSession,
    collection: &str,
    record: &Value,
) -> Result<(), CallerError> {
    let Some(schema) = caller
        .app_state
        .lexicons
        .get(collection)
        .await
        .and_then(|l| l.record_schema)
    else {
        return Ok(());
    };
    validate_required_fields(record, &schema).map_err(CallerError::Validation)
}

async fn post(
    caller: &CallerSession,
    repo: &str,
    method: &str,
    body: &Value,
) -> Result<Value, CallerError> {
    let response = caller
        .pds_auth
        .post_json(&caller.app_state, repo, method, body)
        .await
        .map_err(pds_failure)?;
    let status = response.status().as_u16();
    let bytes = response
        .bytes()
        .await
        .map_err(|e| CallerError::Other(format!("failed to read PDS response: {e}")))?;
    if !(200..300).contains(&status) {
        return Err(CallerError::Pds {
            status,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        });
    }
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes).map_err(|e| CallerError::Other(format!("invalid PDS JSON: {e}")))
}

async fn response_json(response: Response) -> Result<Value, CallerError> {
    let status = response.status().as_u16();
    let bytes = response
        .into_body()
        .collect()
        .await
        .map_err(|e| CallerError::Other(format!("failed to read response body: {e}")))?
        .to_bytes();
    if !(200..300).contains(&status) {
        return Err(CallerError::Xrpc {
            status,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        });
    }
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| CallerError::Other(format!("invalid XRPC JSON: {e}")))
}

/// A write whose response carries no URI leaves the guest with nothing to
/// reference, so it is an error rather than an empty ref.
fn record_ref(result: &Value) -> Result<RecordRef, CallerError> {
    let uri = result
        .get("uri")
        .and_then(|v| v.as_str())
        .ok_or_else(|| CallerError::Other("PDS response has no uri".into()))?;
    Ok(RecordRef {
        uri: uri.to_string(),
        cid: result
            .get("cid")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_body_omits_an_absent_rkey() {
        let body = create_body(
            "did:plc:me",
            "app.bsky.feed.post",
            None,
            &json!({"a": 1}),
            true,
        );
        assert_eq!(
            body,
            json!({
                "repo": "did:plc:me",
                "collection": "app.bsky.feed.post",
                "record": {"a": 1},
                "validate": true,
            })
        );
    }

    #[test]
    fn create_body_carries_rkey_and_validate_false() {
        let body = create_body(
            "did:plc:me",
            "app.bsky.feed.post",
            Some("abc"),
            &json!({"a": 1}),
            false,
        );
        assert_eq!(body["rkey"], "abc");
        assert_eq!(body["validate"], false);
    }

    #[test]
    fn put_body_omits_swap_record_unless_asked() {
        let body = put_body(
            "did:plc:me",
            "app.bsky.feed.post",
            "abc",
            &json!({"a": 1}),
            None,
            true,
        );
        assert!(body.get("swapRecord").is_none());
        assert_eq!(body["rkey"], "abc");

        let guarded = put_body(
            "did:plc:me",
            "app.bsky.feed.post",
            "abc",
            &json!({"a": 1}),
            Some("bafy"),
            true,
        );
        assert_eq!(guarded["swapRecord"], "bafy");
    }

    #[test]
    fn delete_body_names_repo_collection_and_rkey() {
        assert_eq!(
            delete_body("did:plc:me", "app.bsky.feed.post", "abc"),
            json!({"repo": "did:plc:me", "collection": "app.bsky.feed.post", "rkey": "abc"})
        );
    }

    #[test]
    fn at_uris_split_into_repo_collection_and_rkey() {
        let (did, collection, rkey) =
            parse_at_uri("at://did:plc:me/app.bsky.feed.post/abc").unwrap();
        assert_eq!(did, "did:plc:me");
        assert_eq!(collection, "app.bsky.feed.post");
        assert_eq!(rkey, "abc");
    }

    #[test]
    fn malformed_at_uris_are_refused() {
        for uri in [
            "https://example.com/a/b",
            "at://did:plc:me/app.bsky.feed.post",
            "at://did:plc:me//abc",
            "at:///app.bsky.feed.post/abc",
        ] {
            let err = parse_at_uri(uri).expect_err(uri);
            assert_eq!(err.code(), "BAD_INPUT", "{uri}");
            assert!(err.to_string().contains(uri), "{uri}");
        }
    }

    #[test]
    fn required_fields_must_be_present_and_non_null() {
        let schema = json!({"required": ["text", "createdAt"]});
        assert!(
            validate_required_fields(&json!({"text": "hi", "createdAt": "now"}), &schema).is_ok()
        );

        let missing = validate_required_fields(&json!({"text": "hi"}), &schema).unwrap_err();
        assert!(missing.contains("createdAt"), "{missing}");

        let null =
            validate_required_fields(&json!({"text": "hi", "createdAt": Value::Null}), &schema)
                .unwrap_err();
        assert!(null.contains("createdAt"), "{null}");
    }

    #[test]
    fn a_schema_without_required_accepts_anything() {
        assert!(validate_required_fields(&json!({}), &json!({"type": "object"})).is_ok());
    }

    #[test]
    fn a_dead_session_stays_an_auth_failure_through_the_pds_path() {
        let err = pds_failure(AppError::Auth("DPoP session not found".into()));
        assert_eq!(err.code(), "AUTH_REQUIRED");
        assert_eq!(err.to_string(), "DPoP session not found");
    }

    #[test]
    fn a_pds_status_survives_the_blob_path() {
        let err = pds_failure(AppError::PdsError(
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            axum::body::Bytes::from_static(b"BlobTooLarge"),
        ));
        assert_eq!(err.code(), "PDS_ERROR");
        assert!(matches!(err, CallerError::Pds { status: 413, .. }));
        assert!(err.to_string().contains("BlobTooLarge"), "{err}");
    }

    /// A transport failure never reached a PDS, so there is no status to
    /// report and none is invented.
    #[test]
    fn a_blob_failure_without_a_status_reports_zero() {
        let err = pds_failure(AppError::Internal("connection refused".into()));
        assert!(matches!(err, CallerError::Pds { status: 0, .. }));
        assert!(err.to_string().contains("connection refused"), "{err}");
    }

    #[test]
    fn xrpc_failures_carry_the_status_the_handler_would_have_returned() {
        for (error, expected) in [
            (AppError::BadRequest("no such param".into()), 400),
            (AppError::NotFound("no such method".into()), 404),
            (AppError::Auth("expired".into()), 401),
        ] {
            let err = xrpc_failure(error);
            assert_eq!(err.code(), "XRPC_ERROR");
            assert!(
                matches!(err, CallerError::Xrpc { status, .. } if status == expected),
                "expected {expected}, got {err:?}"
            );
        }
    }

    #[test]
    fn error_codes_separate_the_four_failure_kinds() {
        assert_eq!(
            CallerError::WritableRepo("nope".into()).code(),
            "WRITABLE_REPO"
        );
        let pds = CallerError::Pds {
            status: 400,
            body: "InvalidSwap".into(),
        };
        assert_eq!(pds.code(), "PDS_ERROR");
        assert!(pds.to_string().contains("400"), "{pds}");
        assert!(pds.to_string().contains("InvalidSwap"), "{pds}");

        let xrpc = CallerError::Xrpc {
            status: 502,
            body: "upstream".into(),
        };
        assert_eq!(xrpc.code(), "XRPC_ERROR");
        assert!(xrpc.to_string().contains("502"), "{xrpc}");

        assert_eq!(CallerError::Validation("bad".into()).code(), "BAD_INPUT");
        assert_eq!(CallerError::Other("boom".into()).code(), "HOST_ERROR");
    }

    /// The refusal message reaches the guest verbatim, so it has to survive
    /// the trip through `CallerError`.
    #[test]
    fn a_foreign_repo_is_refused_with_the_shared_message() {
        let err = CallerError::WritableRepo(
            check_writable_repo("did:plc:other", "did:plc:me", None).unwrap_err(),
        );
        assert!(err.to_string().contains("did:plc:other"), "{err}");
        assert!(err.to_string().contains("linked_repos"), "{err}");
    }
}

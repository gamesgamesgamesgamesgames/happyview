//! Host imports that act as the calling script's user: repo writes, blob
//! uploads and XRPC calls issued with the caller's own credentials. Each one
//! goes through the instance's own PDS and XRPC paths, so DPoP nonce
//! handling, token refresh and the local XRPC handlers are shared rather than
//! reimplemented.

use std::collections::HashMap;

use axum::body::Bytes;
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;
use serde_json::{Value, json};

use crate::AppState;
use crate::auth::Claims;
use crate::error::AppError;

use happyview_plugin_sdk::wire::{
    CallerAccount, CallerBlobUpload, CallerRecordCreate, CallerRecordDelete, CallerRecordPut,
    CallerXrpcProcedure, CallerXrpcQuery, RecordRef,
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

/// `{repo, collection, rkey?, record, validate?}`, the body
/// `com.atproto.repo.createRecord` takes. `validate` is sent only when the
/// caller chose one: without it the PDS validates a record whose lexicon it
/// knows or can resolve and accepts one whose lexicon it cannot, where
/// `true` would refuse that second record.
pub fn create_body(
    repo: &str,
    collection: &str,
    rkey: Option<&str>,
    record: &Value,
    validate: Option<bool>,
) -> Value {
    let mut body = json!({
        "repo": repo,
        "collection": collection,
        "record": record,
    });
    if let Some(rkey) = rkey {
        body["rkey"] = json!(rkey);
    }
    if let Some(validate) = validate {
        body["validate"] = json!(validate);
    }
    body
}

/// `{repo, collection, rkey, record, validate?, swapRecord?}`, the body
/// `com.atproto.repo.putRecord` takes. `swapRecord` is the caller's
/// no-create guarantee and is sent only when they supplied one; `validate`
/// as for [`create_body`].
fn put_body(
    repo: &str,
    collection: &str,
    rkey: &str,
    record: &Value,
    swap_cid: Option<&str>,
    validate: Option<bool>,
) -> Value {
    let mut body = json!({
        "repo": repo,
        "collection": collection,
        "rkey": rkey,
        "record": record,
    });
    if let Some(cid) = swap_cid {
        body["swapRecord"] = json!(cid);
    }
    if let Some(validate) = validate {
        body["validate"] = json!(validate);
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
    // Checked here unless the caller asked for no validation at all.
    if spec.validate != Some(false) {
        check_against_lexicon(caller, &spec.collection, &spec.record).await?;
    }
    let record = with_type(spec.record, &spec.collection);
    let body = create_body(
        &repo,
        &spec.collection,
        spec.rkey.as_deref(),
        &record,
        spec.validate,
    );
    let result = post(caller, &repo, "com.atproto.repo.createRecord", &body).await?;
    let reference = record_ref(&result)?;
    mirror_write(caller, &reference, &record).await;
    Ok(reference)
}

pub async fn put_record(
    caller: &CallerSession,
    spec: CallerRecordPut,
) -> Result<RecordRef, CallerError> {
    let (repo, collection, rkey) = parse_at_uri(&spec.uri)?;
    check_writable_repo(&repo, &caller.did, caller.delegate_did.as_deref())
        .map_err(CallerError::WritableRepo)?;
    // Checked here unless the caller asked for no validation at all.
    if spec.validate != Some(false) {
        check_against_lexicon(caller, &collection, &spec.record).await?;
    }
    let record = with_type(spec.record, &collection);
    let body = put_body(
        &repo,
        &collection,
        &rkey,
        &record,
        spec.swap_cid.as_deref(),
        spec.validate,
    );
    let result = post(caller, &repo, "com.atproto.repo.putRecord", &body).await?;
    let reference = record_ref(&result)?;
    mirror_write(caller, &reference, &record).await;
    Ok(reference)
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
    let state = &caller.app_state;
    if let Err(e) = super::mirror_network_delete(&state.records, &spec.uri).await {
        tracing::warn!(uri = %spec.uri, "record deleted on the PDS but still in the index: {e}");
    }
    Ok(())
}

/// The PDS has accepted the write, so the index is told what it holds.
/// A mirror that fails leaves the index stale until Jetstream echoes the
/// record; it never turns an accepted write into an error the script would
/// have to undo.
async fn mirror_write(caller: &CallerSession, reference: &RecordRef, record: &Value) {
    let state = &caller.app_state;
    let (did, collection, rkey) = match parse_at_uri(&reference.uri) {
        Ok(parts) => parts,
        Err(e) => {
            tracing::warn!(
                uri = %reference.uri,
                "record written to the PDS but its index row not written: {e}"
            );
            return;
        }
    };
    let mirrored = super::mirror_network_write(
        &state.records,
        super::NetworkWrite {
            uri: &reference.uri,
            did: &did,
            collection: &collection,
            rkey: &rkey,
            record,
            cid: &reference.cid,
        },
    )
    .await;
    if let Err(e) = mirrored {
        tracing::warn!(uri = %reference.uri, "record written to the PDS but its index mirror failed: {e}");
    }
}

/// A record carries `$type = collection` on the wire, and a PDS fills it in
/// when absent, so the body the index mirrors has to carry it too or the row's
/// body would not match the version its `cid` describes.
fn with_type(mut record: Value, collection: &str) -> Value {
    if let Some(obj) = record.as_object_mut()
        && !obj.contains_key("$type")
    {
        obj.insert("$type".to_string(), json!(collection));
    }
    record
}

/// Returns the PDS's blob reference verbatim, so the guest can drop it into a
/// record field unchanged.
pub async fn upload_blob(
    caller: &CallerSession,
    spec: CallerBlobUpload,
) -> Result<Value, CallerError> {
    crate::repo::upload_blob_to_pds(
        &caller.app_state,
        &caller.did,
        &caller.pds_auth,
        &spec.mime_type,
        Bytes::from(spec.bytes),
    )
    .await
    .map_err(pds_failure)
}

/// The caller's account as their PDS reports it. Only the DID, handle and
/// email fields are passed on, so whatever else a PDS adds to `getSession`
/// never reaches a plugin. A PDS that names a different account is refused
/// rather than believed.
pub async fn get_session(
    caller: &CallerSession,
    _spec: Value,
) -> Result<CallerAccount, CallerError> {
    let response = caller
        .pds_auth
        .get_with_headers(
            &caller.app_state,
            &caller.did,
            "com.atproto.server.getSession",
            "",
            &[],
        )
        .await
        .map_err(pds_failure)?;
    let status = response.status();
    let bytes = response.bytes().await.map_err(|e| CallerError::Pds {
        status: 0,
        body: e.to_string(),
    })?;
    if !status.is_success() {
        return Err(CallerError::Pds {
            status: status.as_u16(),
            body: String::from_utf8_lossy(&bytes).into_owned(),
        });
    }
    let body: Value = serde_json::from_slice(&bytes).map_err(|e| CallerError::Pds {
        status: status.as_u16(),
        body: format!("getSession answered with something that isn't JSON: {e}"),
    })?;
    account_from_session(&caller.did, &body)
}

/// The fields of a `getSession` body a plugin may see, checked against the
/// account the session belongs to.
fn account_from_session(did: &str, body: &Value) -> Result<CallerAccount, CallerError> {
    let text = |key: &str| {
        body.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    match text("did") {
        Some(reported) if reported == did => {}
        other => {
            return Err(CallerError::Pds {
                status: 200,
                body: format!(
                    "getSession named {} rather than the caller, {did}",
                    other.as_deref().unwrap_or("no account")
                ),
            });
        }
    }
    let email = text("email");
    Ok(CallerAccount {
        did: did.to_string(),
        handle: text("handle"),
        email_confirmed: email
            .as_ref()
            .and_then(|_| body.get("emailConfirmed").and_then(Value::as_bool)),
        email,
    })
}

/// Unlike every other `host_caller_*` import, this one runs with no
/// [`CallerSession`] at all: a query needs no PDS auth, and a query,
/// record-event or label script has no session to lend. `caller_did` stands
/// in for a session's claims when there is no session; a fully anonymous call
/// still reaches a registered local handler or the proxy.
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
        crate::xrpc::local::execute_local_query(app_state, &spec.method, &mut params, claims)
            .await
            .map_err(xrpc_failure)?;
    response_json(response).await
}

pub async fn xrpc_procedure(
    caller: &CallerSession,
    spec: CallerXrpcProcedure,
) -> Result<Value, CallerError> {
    let mut params: HashMap<String, Value> = spec.params.into_iter().collect();
    let response = crate::xrpc::local::execute_local_procedure(
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
            Some(true),
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
            Some(false),
        );
        assert_eq!(body["rkey"], "abc");
        assert_eq!(body["validate"], false);
    }

    /// No flag is the PDS's own default: it validates a record whose lexicon
    /// it knows or can resolve, and accepts one whose lexicon it cannot. An
    /// explicit `true` refuses that second record outright.
    #[test]
    fn a_write_that_leaves_validation_to_the_pds_sends_no_flag() {
        let create = create_body("did:plc:me", "app.test.rec", None, &json!({"a": 1}), None);
        assert!(create.get("validate").is_none(), "{create}");
        let put = put_body(
            "did:plc:me",
            "app.test.rec",
            "abc",
            &json!({"a": 1}),
            None,
            None,
        );
        assert!(put.get("validate").is_none(), "{put}");
    }

    #[test]
    fn put_body_omits_swap_record_unless_asked() {
        let body = put_body(
            "did:plc:me",
            "app.bsky.feed.post",
            "abc",
            &json!({"a": 1}),
            None,
            Some(true),
        );
        assert!(body.get("swapRecord").is_none());
        assert_eq!(body["rkey"], "abc");

        let guarded = put_body(
            "did:plc:me",
            "app.bsky.feed.post",
            "abc",
            &json!({"a": 1}),
            Some("bafy"),
            Some(true),
        );
        assert_eq!(guarded["swapRecord"], "bafy");
    }

    #[test]
    fn a_record_without_a_type_gets_its_collection_as_one() {
        let typed = with_type(json!({"a": 1}), "app.bsky.feed.post");
        assert_eq!(typed, json!({"$type": "app.bsky.feed.post", "a": 1}));
        let body = create_body("did:plc:me", "app.bsky.feed.post", None, &typed, Some(true));
        assert_eq!(body["record"]["$type"], "app.bsky.feed.post");
    }

    #[test]
    fn an_existing_type_is_left_alone() {
        let record = json!({"$type": "app.bsky.feed.like", "a": 1});
        assert_eq!(with_type(record.clone(), "app.bsky.feed.post"), record);
        assert_eq!(
            with_type(json!("scalar"), "app.bsky.feed.post"),
            json!("scalar")
        );
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

    /// Only the account fields pass, the email only with its address, and a
    /// session naming another account is refused.
    #[test]
    fn a_session_is_trimmed_to_the_account_fields() {
        let full = json!({
            "did": "did:plc:me", "handle": "me.test", "email": "me@example.com",
            "emailConfirmed": true, "emailAuthFactor": false, "active": true,
            "didDoc": {"id": "did:plc:me"},
        });
        let account = account_from_session("did:plc:me", &full).unwrap();
        assert_eq!(
            serde_json::to_value(&account).unwrap(),
            json!({"did": "did:plc:me", "handle": "me.test", "email": "me@example.com", "email_confirmed": true})
        );

        // No transition:email: the PDS leaves the address out.
        let bare = account_from_session(
            "did:plc:me",
            &json!({"did": "did:plc:me", "handle": "me.test"}),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            json!({"did": "did:plc:me", "handle": "me.test"})
        );
        // A confirmation flag with no address means nothing.
        let flag_only = account_from_session(
            "did:plc:me",
            &json!({"did": "did:plc:me", "email": "", "emailConfirmed": true}),
        )
        .unwrap();
        assert_eq!(flag_only.email, None);
        assert_eq!(flag_only.email_confirmed, None);

        for body in [
            json!({"did": "did:plc:other"}),
            json!({"handle": "me.test"}),
        ] {
            let err = account_from_session("did:plc:me", &body).unwrap_err();
            assert_eq!(err.code(), "PDS_ERROR", "{body}");
        }
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

    /// The path `happyview.record` takes: a write answered by the in-process PDS
    /// is mirrored into the index with the CID the fake computed.
    #[tokio::test]
    async fn a_test_runs_write_reaches_the_fake_and_is_mirrored() {
        let harness = std::sync::Arc::new(crate::testing::TestHarness::new("a.test.lua"));
        let mut state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
        state.test = Some(harness.clone());
        let caller = CallerSession {
            did: "did:plc:alice".into(),
            delegate_did: None,
            claims: std::sync::Arc::new(Claims::internal("did:plc:alice".into())),
            pds_auth: std::sync::Arc::new(crate::repo::test_pds_auth(&state).unwrap()),
            app_state: state.clone(),
        };
        let reference = create_record(
            &caller,
            serde_json::from_value(json!({
                "collection": "app.test.rec", "rkey": "1", "record": {"text": "hi"}, "validate": false,
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            harness.pds().writes()[0].cid.as_deref(),
            Some(reference.cid.as_str())
        );
        let (cid,): (String,) =
            crate::db::query_as("SELECT cid FROM happyview_records WHERE uri = ?")
                .bind(&reference.uri)
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(cid, reference.cid);
    }

    /// An unindexed collection never sees a Jetstream echo, so the mirrored
    /// row is the only copy a script's `db.get` can find. It must be stored,
    /// and readable, even though `indexed_at` stays unset.
    #[tokio::test]
    async fn a_write_into_an_unindexed_collection_stays_readable() {
        let harness = std::sync::Arc::new(crate::testing::TestHarness::new("a.test.lua"));
        let mut state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
        state.test = Some(harness.clone());
        let parsed = crate::lexicon::ParsedLexicon::parse(
            json!({
                "lexicon": 1,
                "id": "app.test.quiet",
                "defs": {"main": {"type": "record", "key": "tid"}},
            }),
            1,
            None,
            crate::lexicon::ProcedureAction::Upsert,
            None,
        )
        .unwrap();
        state.lexicons.upsert_unindexed(parsed).await;
        assert!(!state.lexicons.is_indexed("app.test.quiet"));

        let caller = CallerSession {
            did: "did:plc:alice".into(),
            delegate_did: None,
            claims: std::sync::Arc::new(Claims::internal("did:plc:alice".into())),
            pds_auth: std::sync::Arc::new(crate::repo::test_pds_auth(&state).unwrap()),
            app_state: state.clone(),
        };
        let reference = create_record(
            &caller,
            serde_json::from_value(json!({
                "collection": "app.test.quiet", "rkey": "1", "record": {"text": "hi"}, "validate": false,
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let got = crate::plugin::host::records_get(&state.db, state.db_backend, &reference.uri)
            .await
            .unwrap()
            .expect("the mirrored row is readable");
        assert_eq!(got["cid"], reference.cid.as_str());
        assert_eq!(got["record"]["text"], "hi");

        let (indexed_at,): (Option<String>,) =
            crate::db::query_as("SELECT indexed_at FROM happyview_records WHERE uri = ?")
                .bind(&reference.uri)
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(indexed_at, None);
    }
}

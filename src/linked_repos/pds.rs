use atrium_api::types::string::Did;
use atrium_xrpc::http::Method;
use atrium_xrpc::{InputDataOrBytes, OutputDataOrBytes, XrpcClient, XrpcRequest};
use serde_json::{Value, json};

use crate::AppState;
use crate::HappyViewOAuthSession;
use crate::error::AppError;

use super::flow;
use super::scope::{self, RepoAction};
use super::types::{LinkedRepo, STATUS_NEEDS_REAUTH};

fn require_did(grant: &LinkedRepo) -> Result<&str, AppError> {
    grant.did.as_deref().ok_or_else(|| {
        AppError::BadRequest(format!(
            "linked repo {} has not been authorized yet",
            grant.id
        ))
    })
}

fn require_repo_scope(
    grant: &LinkedRepo,
    collection: &str,
    action: RepoAction,
) -> Result<(), AppError> {
    let scopes = scope::parse(&grant.scopes);
    if scope::allows_repo(&scopes, collection, action) {
        return Ok(());
    }
    Err(AppError::Forbidden(format!(
        "linked repo {} lacks the scope needed to {} {} — grant it repo:{}?action={}",
        grant.did.as_deref().unwrap_or(&grant.id),
        action.as_str(),
        collection,
        collection,
        action.as_str(),
    )))
}

fn require_put_scope(grant: &LinkedRepo, collection: &str, has_swap: bool) -> Result<(), AppError> {
    let scopes = scope::parse(&grant.scopes);

    if has_swap {
        if scope::allows_repo(&scopes, collection, RepoAction::Update) {
            return Ok(());
        }
        return Err(AppError::Forbidden(format!(
            "linked repo {} lacks the scope needed to put {} — putRecord with swap_cid can only \
             update an existing record (the PDS enforces the compare-and-swap), so it requires \
             repo:{}?action=update",
            grant.did.as_deref().unwrap_or(&grant.id),
            collection,
            collection,
        )));
    }

    let can_create = scope::allows_repo(&scopes, collection, RepoAction::Create);
    let can_update = scope::allows_repo(&scopes, collection, RepoAction::Update);
    if can_create && can_update {
        return Ok(());
    }
    Err(AppError::Forbidden(format!(
        "linked repo {} lacks the scope needed to put {} — putRecord can create a new record \
         as well as update an existing one, so it requires both repo:{}?action=create and \
         repo:{}?action=update, not just one of them; passing swap_cid would narrow this to \
         just repo:{}?action=update",
        grant.did.as_deref().unwrap_or(&grant.id),
        collection,
        collection,
        collection,
        collection,
    )))
}

fn require_blob_scope(grant: &LinkedRepo, mime: &str) -> Result<(), AppError> {
    let scopes = scope::parse(&grant.scopes);
    if scope::allows_blob(&scopes, mime) {
        return Ok(());
    }
    Err(AppError::Forbidden(format!(
        "linked repo {} lacks a blob scope covering {mime}",
        grant.did.as_deref().unwrap_or(&grant.id),
    )))
}

pub async fn session_for(
    state: &AppState,
    grant: &LinkedRepo,
) -> Result<HappyViewOAuthSession, AppError> {
    let did_str = require_did(grant)?;

    if grant.status == STATUS_NEEDS_REAUTH {
        return Err(AppError::Auth(format!(
            "linked repo {did_str} needs reauthorization"
        )));
    }

    let did = Did::new(did_str.to_string())
        .map_err(|_| AppError::Internal(format!("invalid DID on grant: {did_str}")))?;

    let client = flow::client_for_grant(state, grant)?;

    match client.restore(&did).await {
        Ok(session) => Ok(session),
        Err(e) => {
            let message = format!("{e}");
            super::db::mark_needs_reauth(state, &grant.id, &message).await?;
            Err(AppError::Auth(format!(
                "linked repo {did_str} needs reauthorization: {message}"
            )))
        }
    }
}

async fn post_json(
    session: &HappyViewOAuthSession,
    nsid: &str,
    body: &Value,
) -> Result<Value, AppError> {
    let request = XrpcRequest {
        method: Method::POST,
        nsid: nsid.to_string(),
        parameters: None::<()>,
        input: Some(InputDataOrBytes::Data(body.clone())),
        encoding: Some("application/json".to_string()),
    };

    let result: Result<OutputDataOrBytes<Value>, atrium_xrpc::Error<Value>> =
        session.send_xrpc(&request).await;

    match result {
        Ok(OutputDataOrBytes::Data(data)) => Ok(data),
        Ok(OutputDataOrBytes::Bytes(bytes)) => serde_json::from_slice(&bytes)
            .map_err(|e| AppError::Internal(format!("PDS returned non-JSON: {e}"))),
        Err(e) => Err(AppError::Internal(format!("{nsid} failed: {e}"))),
    }
}

fn extract_uri_cid(value: &Value, nsid: &str) -> Result<(String, String), AppError> {
    let uri = value
        .get("uri")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::Internal(format!("{nsid} response had no uri")))?;
    let cid = value
        .get("cid")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::Internal(format!("{nsid} response had no cid")))?;
    Ok((uri.to_string(), cid.to_string()))
}

pub async fn create_record(
    state: &AppState,
    grant: &LinkedRepo,
    collection: &str,
    rkey: Option<&str>,
    record: Value,
) -> Result<(String, String), AppError> {
    require_repo_scope(grant, collection, RepoAction::Create)?;
    let session = session_for(state, grant).await?;
    let did = grant.did.as_deref().expect("session_for requires a did");

    let mut body = json!({
        "repo": did,
        "collection": collection,
        "record": record,
    });
    if let Some(rkey) = rkey {
        body["rkey"] = Value::String(rkey.to_string());
    }

    let out = post_json(&session, "com.atproto.repo.createRecord", &body).await?;
    extract_uri_cid(&out, "createRecord")
}

pub async fn put_record(
    state: &AppState,
    grant: &LinkedRepo,
    collection: &str,
    rkey: &str,
    record: Value,
    swap_cid: Option<&str>,
) -> Result<(String, String), AppError> {
    require_put_scope(grant, collection, swap_cid.is_some())?;
    let session = session_for(state, grant).await?;
    let did = grant.did.as_deref().expect("session_for requires a did");

    let mut body = json!({
        "repo": did,
        "collection": collection,
        "rkey": rkey,
        "record": record,
    });
    if let Some(swap) = swap_cid {
        body["swapRecord"] = Value::String(swap.to_string());
    }

    let out = post_json(&session, "com.atproto.repo.putRecord", &body).await?;
    extract_uri_cid(&out, "putRecord")
}

pub async fn delete_record(
    state: &AppState,
    grant: &LinkedRepo,
    collection: &str,
    rkey: &str,
) -> Result<(), AppError> {
    require_repo_scope(grant, collection, RepoAction::Delete)?;
    let session = session_for(state, grant).await?;
    let did = grant.did.as_deref().expect("session_for requires a did");

    let body = json!({
        "repo": did,
        "collection": collection,
        "rkey": rkey,
    });

    post_json(&session, "com.atproto.repo.deleteRecord", &body).await?;
    Ok(())
}

pub async fn upload_blob(
    state: &AppState,
    grant: &LinkedRepo,
    mime: &str,
    bytes: Vec<u8>,
) -> Result<Value, AppError> {
    require_blob_scope(grant, mime)?;
    let session = session_for(state, grant).await?;

    let request = XrpcRequest {
        method: Method::POST,
        nsid: "com.atproto.repo.uploadBlob".to_string(),
        parameters: None::<()>,
        input: Some(InputDataOrBytes::<()>::Bytes(bytes)),
        encoding: Some(mime.to_string()),
    };

    let result: Result<OutputDataOrBytes<Value>, atrium_xrpc::Error<Value>> =
        session.send_xrpc(&request).await;

    match result {
        Ok(OutputDataOrBytes::Data(data)) => Ok(data),
        Ok(OutputDataOrBytes::Bytes(b)) => serde_json::from_slice(&b)
            .map_err(|e| AppError::Internal(format!("uploadBlob returned non-JSON: {e}"))),
        Err(e) => Err(AppError::Internal(format!("uploadBlob failed: {e}"))),
    }
}

pub async fn call(
    state: &AppState,
    grant: &LinkedRepo,
    nsid: &str,
    params: Option<Value>,
    input: Option<Value>,
) -> Result<Value, AppError> {
    let session = session_for(state, grant).await?;

    if let Some(input) = input {
        return post_json(&session, nsid, &input).await;
    }

    let request = XrpcRequest {
        method: Method::GET,
        nsid: nsid.to_string(),
        parameters: params,
        input: None::<InputDataOrBytes<Value>>,
        encoding: None,
    };

    let result: Result<OutputDataOrBytes<Value>, atrium_xrpc::Error<Value>> =
        session.send_xrpc(&request).await;

    match result {
        Ok(OutputDataOrBytes::Data(data)) => Ok(data),
        Ok(OutputDataOrBytes::Bytes(b)) => serde_json::from_slice(&b)
            .map_err(|e| AppError::Internal(format!("{nsid} returned non-JSON: {e}"))),
        Err(e) => Err(AppError::Internal(format!("{nsid} failed: {e}"))),
    }
}

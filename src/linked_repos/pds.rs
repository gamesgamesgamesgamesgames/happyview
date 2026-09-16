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

    let client = flow::client_for_grant(state, grant).await?;

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

/// Mirror a successful linked-repo write into the local record index.
///
/// This matches what `Record:save()` does after its own PDS write
/// (`lua/record.rs`): a best-effort upsert plus a `sync_refs` pass, with no
/// filter on whether the collection is a tracked record lexicon. Failures are
/// swallowed deliberately — the PDS write has already landed, so the caller's
/// operation succeeded whether or not we managed to index it, and the record
/// will be indexed anyway when Jetstream echoes it back.
///
/// `indexed_at` is left alone: it is network-arrival provenance, and this
/// record has not come back over the firehose yet. See [`crate::db::NO_INDEXED_AT`].
///
/// Public so tests can exercise the statement directly. They need to: the
/// errors are swallowed here, so a column/bind mismatch would be invisible in
/// production rather than loud.
pub async fn index_write(
    state: &AppState,
    did: &str,
    collection: &str,
    uri: &str,
    cid: &str,
    record: &Value,
) {
    let backend = state.db_backend;
    let rkey = uri.split('/').next_back().unwrap_or_default();
    let record_str = serde_json::to_string(record).unwrap_or_default();
    let now = crate::db::now_rfc3339();

    let sql = crate::db::adapt_sql(
        r#"INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?)
           ON CONFLICT (uri) DO UPDATE
               SET record = EXCLUDED.record,
                   cid = EXCLUDED.cid"#,
        backend,
    );
    let _ = crate::db::query(&sql)
        .bind(uri)
        .bind(did)
        .bind(collection)
        .bind(rkey)
        .bind(&record_str)
        .bind(cid)
        .bind(crate::db::NO_INDEXED_AT)
        .bind(&now)
        .execute(&state.db)
        .await;

    let _ = crate::record_refs::sync_refs(&state.db, uri, collection, record, backend).await;
}

/// Remove a record from the local index after a successful linked-repo delete.
///
/// Unlike `Record:delete()`, which drops the local row even when the PDS call
/// fails, this only runs once the PDS has actually accepted the delete —
/// `delete_record` propagates PDS errors to its caller rather than logging and
/// continuing.
pub async fn unindex_write(state: &AppState, uri: &str) {
    let sql = crate::db::adapt_sql(
        "DELETE FROM happyview_records WHERE uri = ?",
        state.db_backend,
    );
    let _ = crate::db::query(&sql).bind(uri).execute(&state.db).await;
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
    let (uri, cid) = extract_uri_cid(&out, "createRecord")?;
    index_write(state, did, collection, &uri, &cid, &body["record"]).await;
    Ok((uri, cid))
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
    let (uri, cid) = extract_uri_cid(&out, "putRecord")?;
    index_write(state, did, collection, &uri, &cid, &body["record"]).await;
    Ok((uri, cid))
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
    unindex_write(state, &format!("at://{did}/{collection}/{rkey}")).await;
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

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use k256;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::auth::XrpcClaims;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::lua::tid::generate_tid;
use crate::spaces::scope::{SpaceReadAccess, check_delegation_token_access, check_read_access};
use crate::spaces::service;
use crate::spaces::types::*;
use crate::spaces::{db, members, notifications, oplog};

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LatestCommitQuery {
    space: String,
    did: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetRepoQuery {
    space: String,
    did: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListRepoOpsQuery {
    space: String,
    did: String,
    limit: Option<i64>,
    cursor: Option<String>,
    exclude_values: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegisterNotifyInput {
    space: String,
    service_did: String,
    endpoint: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotifyWriteInput {
    space: String,
    did: String,
    collection: String,
    rkey: String,
    cid: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotifySpaceDeletedInput {
    space: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetDelegationTokenQuery {
    space: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpaceUriQuery {
    space: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListSpacesQuery {
    did: Option<String>,
    limit: Option<i64>,
    cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PutRecordInput {
    space: String,
    collection: String,
    rkey: String,
    record: serde_json::Value,
    swap_record: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteRecordInput {
    space: String,
    collection: String,
    rkey: String,
    swap_record: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetRecordQuery {
    space: String,
    collection: String,
    rkey: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListRecordsQuery {
    space: String,
    repo: Option<String>,
    collection: Option<String>,
    limit: Option<i64>,
    cursor: Option<String>,
    reverse: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateInviteInput {
    space: String,
    access: Option<SpaceAccess>,
    max_uses: Option<i64>,
    expires_at: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RedeemInviteInput {
    token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RevokeInviteInput {
    space: String,
    invite_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetSpaceCredentialInput {
    grant: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateRecordInput {
    space: String,
    collection: String,
    record: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetSpaceBlobQuery {
    space: String,
    cid: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApplyWritesInput {
    space: String,
    swap_commit: Option<String>,
    writes: Vec<WriteOp>,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "camelCase")]
enum WriteOp {
    Create {
        collection: String,
        rkey: Option<String>,
        value: serde_json::Value,
    },
    Update {
        collection: String,
        rkey: String,
        value: serde_json::Value,
        #[serde(rename = "swapRecord")]
        swap_record: Option<String>,
    },
    Delete {
        collection: String,
        rkey: String,
        #[serde(rename = "swapRecord")]
        swap_record: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Route registration
// ---------------------------------------------------------------------------

const PROTO_NS: &str = "com.atproto";
const LEGACY_NS: &str = "dev.happyview";

pub fn space_routes() -> Router<AppState> {
    Router::new()
        // Protocol-level routes (com.atproto.space.*)
        .route(&format!("/xrpc/{PROTO_NS}.space.getSpace"), get(get_space))
        .route(
            &format!("/xrpc/{PROTO_NS}.space.listSpaces"),
            get(list_spaces),
        )
        .route(
            &format!("/xrpc/{PROTO_NS}.space.getRecord"),
            get(get_record),
        )
        .route(
            &format!("/xrpc/{PROTO_NS}.space.listRecords"),
            get(list_records),
        )
        .route(
            &format!("/xrpc/{PROTO_NS}.space.getLatestCommit"),
            get(get_latest_commit),
        )
        .route(
            &format!("/xrpc/{PROTO_NS}.space.getRepoState"),
            get(get_latest_commit),
        )
        .route(&format!("/xrpc/{PROTO_NS}.space.getRepo"), get(get_repo))
        .route(
            &format!("/xrpc/{PROTO_NS}.space.listRepoOps"),
            get(list_repo_ops),
        )
        .route(
            &format!("/xrpc/{PROTO_NS}.space.listRepos"),
            get(list_repos),
        )
        .route(
            &format!("/xrpc/{PROTO_NS}.space.getDelegationToken"),
            get(get_delegation_token),
        )
        .route(
            &format!("/xrpc/{PROTO_NS}.space.getSpaceCredential"),
            post(get_space_credential),
        )
        .route(
            &format!("/xrpc/{PROTO_NS}.space.createRecord"),
            post(create_record),
        )
        .route(
            &format!("/xrpc/{PROTO_NS}.space.putRecord"),
            post(put_record),
        )
        .route(
            &format!("/xrpc/{PROTO_NS}.space.deleteRecord"),
            post(delete_record),
        )
        .route(
            &format!("/xrpc/{PROTO_NS}.space.applyWrites"),
            post(apply_writes),
        )
        .route(
            &format!("/xrpc/{PROTO_NS}.space.registerNotify"),
            post(register_notify),
        )
        .route(
            &format!("/xrpc/{PROTO_NS}.space.notifyWrite"),
            post(notify_write),
        )
        .route(
            &format!("/xrpc/{PROTO_NS}.space.notifySpaceDeleted"),
            post(notify_space_deleted),
        )
        .route(
            &format!("/xrpc/{PROTO_NS}.space.getBlob"),
            get(get_space_blob),
        )
        // Invites (HappyView extension, no com.atproto equivalent)
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.createInvite"),
            post(create_invite),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.acceptInvite"),
            post(accept_invite),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.revokeInvite"),
            post(revoke_invite),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.listInvites"),
            get(list_invites),
        )
        // Backward-compatible aliases (dev.happyview.space.*) — kept until v3
        .route(&format!("/xrpc/{LEGACY_NS}.space.getSpace"), get(get_space))
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.listSpaces"),
            get(list_spaces),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.getRecord"),
            get(get_record),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.listRecords"),
            get(list_records),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.getMemberGrant"),
            get(get_delegation_token),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.getSpaceCredential"),
            post(get_space_credential),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.createRecord"),
            post(create_record),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.putRecord"),
            post(put_record),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.deleteRecord"),
            post(delete_record),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.applyWrites"),
            post(apply_writes),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.getBlob"),
            get(get_space_blob),
        )
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn require_auth(claims: &XrpcClaims) -> Result<&crate::auth::Claims, AppError> {
    claims
        .identity
        .as_ref()
        .ok_or_else(|| AppError::Auth("This endpoint requires authentication".into()))
}

/// Like `require_auth`, but also accepts a verified space credential as an
/// identity source. Use this in space endpoints that support `Bearer
/// <space_credential>` in addition to DPoP auth.
/// Whether a verified space credential has been revoked (e.g. its holder was
/// removed from the space). Consulted after signature/exp verification so a
/// leaked or stale credential can be invalidated before its TTL expires (M3).
pub(crate) async fn space_credential_revoked(
    state: &AppState,
    token: &str,
) -> Result<bool, AppError> {
    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
    db::is_space_credential_revoked(&state.db, state.db_backend, &token_hash).await
}

async fn require_auth_or_credential(
    state: &AppState,
    claims: &XrpcClaims,
) -> Result<String, AppError> {
    if let Some(identity) = &claims.identity {
        return Ok(identity.did().to_string());
    }

    if let Some(token) = &claims.space_credential {
        let verified = crate::spaces::credential::verify_external_credential(
            token,
            &state.http,
            &state.config.plc_url,
        )
        .await?;
        if space_credential_revoked(state, token).await? {
            return Err(AppError::Auth("space credential has been revoked".into()));
        }
        return Ok(verified.sub);
    }

    Err(AppError::Auth(
        "This endpoint requires authentication".into(),
    ))
}

/// Resolve the caller's DID from an authenticated identity for the inter-service
/// notify routes: a DPoP/cookie identity or a verified service-auth JWT. (Space
/// credentials are not accepted here — their subject is a space, not the
/// authority DID these routes gate on.)
fn require_notify_caller(claims: &XrpcClaims) -> Result<String, AppError> {
    if let Some(identity) = &claims.identity {
        return Ok(identity.did().to_string());
    }
    if let Some(service_auth) = &claims.service_auth {
        return Ok(service_auth.did.clone());
    }
    Err(AppError::Auth(
        "This endpoint requires authentication".into(),
    ))
}

async fn resolve_client_id_url(
    state: &AppState,
    client_key: &str,
) -> Result<Option<String>, AppError> {
    let sql = adapt_sql(
        "SELECT client_id_url FROM happyview_api_clients WHERE client_key = ?",
        state.db_backend,
    );
    let row: Option<(String,)> = crate::db::query_as(&sql)
        .bind(client_key)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to look up API client: {e}")))?;
    Ok(row.map(|(url,)| url))
}

// ---------------------------------------------------------------------------
// Space read handlers
// ---------------------------------------------------------------------------

async fn get_space(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Query(query): Query<SpaceUriQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let space = service::resolve_space(&state, &query.space).await?;

    // If the space's membership is not public, require auth + membership
    if !space.config.membership_public {
        let claims = require_auth(&xrpc_claims)?;
        let did = claims.did();
        if space.authority_did != did {
            members::is_member(&state.db, state.db_backend, &space.id, did)
                .await?
                .ok_or_else(|| AppError::NotFound("Space not found".into()))?;
        }
    }

    let space_uri = format!(
        "at://{}/space/{}/{}",
        space.did, space.type_nsid, space.skey
    );
    let simplespace_config = serde_json::json!({
        "$type": "com.atproto.simplespace.defs#spaceConfig",
        "mintPolicy": space.mint_policy,
        "appAccess": space.app_access,
        "managingApp": space.managing_app_did,
    });
    Ok(Json(serde_json::json!({
        "uri": space_uri,
        "space": space,
        "config": simplespace_config,
    })))
}

async fn list_spaces(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Query(query): Query<ListSpacesQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let did = query.did.unwrap_or_else(|| claims.did().to_string());
    let limit = query.limit.unwrap_or(50).min(100);

    let (views, cursor) = db::list_spaces_for_user(
        &state.db,
        state.db_backend,
        &did,
        limit,
        query.cursor.as_deref(),
    )
    .await?;

    let spaces_json: Vec<serde_json::Value> = views
        .into_iter()
        .map(|v| {
            serde_json::json!({
                "uri": v.uri,
                "isOwner": v.is_owner,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "spaces": spaces_json,
        "cursor": cursor,
    })))
}

// ---------------------------------------------------------------------------
// Record handlers
// ---------------------------------------------------------------------------

async fn create_record(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<CreateRecordInput>,
) -> Result<Response, AppError> {
    let did = require_auth_or_credential(&state, &xrpc_claims).await?;
    let (uri, cid) = service::create_record(
        &state,
        &did,
        xrpc_claims.space_credential.as_deref(),
        &input.space,
        &input.collection,
        input.record,
    )
    .await?;
    let mut response = Json(serde_json::json!({ "uri": uri, "cid": cid })).into_response();
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

async fn put_record(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<PutRecordInput>,
) -> Result<Response, AppError> {
    let did = require_auth_or_credential(&state, &xrpc_claims).await?;
    let (uri, cid) = service::put_record(
        &state,
        &did,
        xrpc_claims.space_credential.as_deref(),
        &input.space,
        &input.collection,
        &input.rkey,
        input.record,
        input.swap_record,
    )
    .await?;
    let mut response = Json(serde_json::json!({ "uri": uri, "cid": cid })).into_response();
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

async fn delete_record(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<DeleteRecordInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let did = claims.did().to_string();
    service::delete_record(
        &state,
        &did,
        &input.space,
        &input.collection,
        &input.rkey,
        input.swap_record,
    )
    .await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

async fn apply_writes(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<ApplyWritesInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let did = require_auth_or_credential(&state, &xrpc_claims).await?;
    let space = service::resolve_space(&state, &input.space).await?;
    service::require_membership(
        &state,
        &space,
        &did,
        true,
        xrpc_claims.space_credential.as_deref(),
    )
    .await?;

    if let Some(ref expected_rev) = input.swap_commit {
        match &space.revision {
            Some(current_rev) if current_rev != expected_rev => {
                return Err(AppError::Conflict("swapCommit mismatch".into()));
            }
            None if !expected_rev.is_empty() => {
                return Err(AppError::Conflict("swapCommit mismatch".into()));
            }
            _ => {}
        }
    }

    let mut results = Vec::with_capacity(input.writes.len());
    let rev = generate_tid();
    let mut ops: Vec<service::AppliedOp> = Vec::with_capacity(input.writes.len());

    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| AppError::Internal(format!("failed to begin transaction: {e}")))?;

    for op in input.writes {
        match op {
            WriteOp::Create {
                collection,
                rkey,
                value,
            } => {
                service::check_collection_allowed(&space, &collection)?;
                let rkey = rkey.unwrap_or_else(generate_tid);
                let cid = service::content_cid(&value)?;
                let record_uri = format!(
                    "at://{}/space/{}/{}/{}/{}/{}",
                    space.did, space.type_nsid, space.skey, did, collection, rkey
                );
                let record = SpaceRecord {
                    uri: record_uri.clone(),
                    space_id: space.id.clone(),
                    author_did: did.clone(),
                    collection: collection.clone(),
                    rkey: rkey.clone(),
                    record: value,
                    cid: cid.clone(),
                    indexed_at: now_rfc3339(),
                };
                db::insert_space_record(&mut *tx, state.db_backend, &record).await?;
                ops.push(service::AppliedOp {
                    action: OplogAction::Create,
                    collection,
                    rkey,
                    new_cid: Some(cid.clone()),
                    old_cid: None,
                });
                results.push(serde_json::json!({
                    "uri": record_uri,
                    "cid": cid,
                }));
            }
            WriteOp::Update {
                collection,
                rkey,
                value,
                swap_record,
            } => {
                service::check_collection_allowed(&space, &collection)?;
                let cid = service::content_cid(&value)?;
                let record_uri = format!(
                    "at://{}/space/{}/{}/{}/{}/{}",
                    space.did, space.type_nsid, space.skey, did, collection, rkey
                );
                let record = SpaceRecord {
                    uri: record_uri.clone(),
                    space_id: space.id.clone(),
                    author_did: did.clone(),
                    collection: collection.clone(),
                    rkey: rkey.clone(),
                    record: value,
                    cid: cid.clone(),
                    indexed_at: now_rfc3339(),
                };
                let old_cid = match swap_record.as_deref() {
                    Some(swap) => Some(swap.to_string()),
                    None => db::get_space_record(&mut *tx, state.db_backend, &record_uri)
                        .await?
                        .map(|r| r.cid),
                };
                if let Some(swap_cid) = swap_record {
                    db::upsert_space_record_with_swap(
                        &mut tx,
                        state.db_backend,
                        &record,
                        &swap_cid,
                    )
                    .await?;
                } else {
                    db::upsert_space_record(&mut *tx, state.db_backend, &record).await?;
                }
                ops.push(service::AppliedOp {
                    action: if old_cid.is_some() {
                        OplogAction::Update
                    } else {
                        OplogAction::Create
                    },
                    collection,
                    rkey,
                    new_cid: Some(cid.clone()),
                    old_cid,
                });
                results.push(serde_json::json!({
                    "uri": record_uri,
                    "cid": cid,
                }));
            }
            WriteOp::Delete {
                collection,
                rkey,
                swap_record,
            } => {
                let record_uri = format!(
                    "at://{}/space/{}/{}/{}/{}/{}",
                    space.did, space.type_nsid, space.skey, did, collection, rkey
                );
                let old_cid = if let Some(swap_cid) = swap_record {
                    db::delete_space_record_with_swap(
                        &mut tx,
                        state.db_backend,
                        &record_uri,
                        &swap_cid,
                    )
                    .await?;
                    swap_cid
                } else {
                    // Mirrors `service::delete_record`'s non-swap ownership
                    // check: only the record's own author may delete it.
                    let existing =
                        db::get_space_record(&mut *tx, state.db_backend, &record_uri).await?;
                    let cid = match existing {
                        Some(r) if r.author_did != did => {
                            return Err(AppError::Forbidden(
                                "You can only delete your own records".into(),
                            ));
                        }
                        None => return Err(AppError::NotFound("Record not found".into())),
                        Some(r) => r.cid,
                    };
                    db::delete_space_record(&mut *tx, state.db_backend, &record_uri).await?;
                    cid
                };
                ops.push(service::AppliedOp {
                    action: OplogAction::Delete,
                    collection,
                    rkey,
                    new_cid: None,
                    old_cid: Some(old_cid),
                });
                results.push(serde_json::json!({}));
            }
        }
    }

    service::commit_write(&mut tx, state.db_backend, &space, &did, &rev, &ops).await?;
    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("failed to commit transaction: {e}")))?;

    service::notify_ops(&state, &space, &did, &ops).await;

    Ok(Json(serde_json::json!({
        "results": results,
    })))
}

async fn get_record(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Query(query): Query<GetRecordQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let did = require_auth_or_credential(&state, &xrpc_claims).await?;
    let space = service::resolve_space(&state, &query.space).await?;
    let has_credential = xrpc_claims.space_credential.is_some();
    let membership = service::require_membership(
        &state,
        &space,
        &did,
        false,
        xrpc_claims.space_credential.as_deref(),
    )
    .await?;

    let record = db::get_space_record_by_parts(
        &state.db,
        state.db_backend,
        &space.id,
        &query.collection,
        &query.rkey,
    )
    .await?
    .ok_or_else(|| AppError::NotFound("Record not found".into()))?;

    let read_access = SpaceReadAccess::from_space_access(membership);
    check_read_access(&did, &record.author_did, read_access, has_credential)?;

    Ok(Json(serde_json::json!({
        "uri": record.uri,
        "cid": record.cid,
        "value": record.record,
    })))
}

async fn list_records(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Query(query): Query<ListRecordsQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let did = require_auth_or_credential(&state, &xrpc_claims).await?;
    let space = service::resolve_space(&state, &query.space).await?;
    let has_credential = xrpc_claims.space_credential.is_some();
    let membership = service::require_membership(
        &state,
        &space,
        &did,
        false,
        xrpc_claims.space_credential.as_deref(),
    )
    .await?;

    let read_access = SpaceReadAccess::from_space_access(membership);

    // read_self members may only list their own records regardless of what the caller requests
    let repo = if !has_credential && read_access == SpaceReadAccess::ReadSelf {
        Some(did.as_str())
    } else {
        query.repo.as_deref().or(if has_credential {
            None
        } else {
            Some(did.as_str())
        })
    };

    let limit = query.limit.unwrap_or(50).min(100);
    let reverse = query.reverse.unwrap_or(false);
    let (records, cursor) = db::list_space_records(
        &state.db,
        state.db_backend,
        &space.id,
        repo,
        query.collection.as_deref(),
        limit,
        query.cursor.as_deref(),
        reverse,
    )
    .await?;

    let records_json: Vec<serde_json::Value> = records
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "collection": r.collection,
                "rkey": r.rkey,
                "cid": r.cid,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "records": records_json,
        "cursor": cursor,
    })))
}

// ---------------------------------------------------------------------------
// Invite handlers
// ---------------------------------------------------------------------------

async fn create_invite(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<CreateInviteInput>,
) -> Result<Response, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let (invite, token) = service::create_invite(
        &state,
        claims.did(),
        &input.space,
        input.access,
        input.max_uses,
        input.expires_at,
    )
    .await?;

    let mut response = Json(serde_json::json!({
        "inviteId": invite.id,
        "token": token,
        "access": invite.access,
        "maxUses": invite.max_uses,
        "expiresAt": invite.expires_at,
    }))
    .into_response();
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

async fn accept_invite(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<RedeemInviteInput>,
) -> Result<Response, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let (space_uri, access) = service::accept_invite(&state, claims.did(), &input.token).await?;

    let mut response = Json(serde_json::json!({
        "uri": space_uri,
        "access": access,
    }))
    .into_response();
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

async fn revoke_invite(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<RevokeInviteInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let space = service::resolve_space(&state, &input.space).await?;
    service::require_space_admin(&state, &space, claims.did()).await?;

    let revoked = db::revoke_invite(&state.db, state.db_backend, &input.invite_id).await?;
    if !revoked {
        return Err(AppError::NotFound("Invite not found".into()));
    }

    Ok(Json(serde_json::json!({ "success": true })))
}

async fn list_invites(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Query(query): Query<SpaceUriQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let space = service::resolve_space(&state, &query.space).await?;
    service::require_space_admin(&state, &space, claims.did()).await?;

    let invites = db::list_invites(&state.db, state.db_backend, &space.id).await?;

    let invites_json: Vec<serde_json::Value> = invites
        .into_iter()
        .map(|i| {
            serde_json::json!({
                "id": i.id,
                "access": i.access,
                "maxUses": i.max_uses,
                "uses": i.uses,
                "expiresAt": i.expires_at,
                "revoked": i.revoked,
                "createdBy": i.created_by,
                "createdAt": i.created_at,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({ "invites": invites_json })))
}

// ---------------------------------------------------------------------------
// Credential handlers
// ---------------------------------------------------------------------------

async fn get_delegation_token(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Query(params): Query<GetDelegationTokenQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let did = claims.did().to_string();
    let space = service::resolve_space(&state, &params.space).await?;

    let membership = service::require_membership(&state, &space, &did, false, None).await?;
    let read_access = SpaceReadAccess::from_space_access(membership);
    check_delegation_token_access(read_access, false)?;

    let encryption_key = state.config.token_encryption_key.as_ref().ok_or_else(|| {
        AppError::Internal("TOKEN_ENCRYPTION_KEY is required for space credentials".into())
    })?;

    let signing_key = k256::ecdsa::SigningKey::from_slice(encryption_key)
        .map_err(|e| AppError::Internal(format!("failed to derive delegation signing key: {e}")))?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let exp = now + crate::spaces::credential::DELEGATION_TOKEN_TTL_SECS;

    let space_uri = format!(
        "at://{}/space/{}/{}",
        space.did, space.type_nsid, space.skey
    );
    let space_host = format!("{}#atproto_space_host", space.did);
    let delegation_claims = crate::spaces::credential::DelegationTokenClaims {
        iss: did,
        sub: space_uri,
        aud: space_host,
        iat: now,
        exp,
        jti: crate::spaces::credential::make_jti(),
    };

    let grant = crate::spaces::credential::sign_delegation_token(&delegation_claims, &signing_key)?;

    let expires_at = chrono::DateTime::from_timestamp(exp as i64, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();

    Ok(Json(serde_json::json!({
        "delegationToken": grant,
        "expiresAt": expires_at,
    })))
}

// ---------------------------------------------------------------------------
// Protocol endpoint implementations
// ---------------------------------------------------------------------------

fn repo_state_commit_json(repo_state: &RepoState) -> Result<Option<serde_json::Value>, AppError> {
    let Some(h) = repo_state.hash.as_deref() else {
        return Ok(None);
    };
    let ikm = repo_state.ikm.as_deref().ok_or_else(|| {
        AppError::Internal("corrupt repo state: hash present but ikm missing".into())
    })?;
    let mac = repo_state.mac.as_deref().ok_or_else(|| {
        AppError::Internal("corrupt repo state: hash present but mac missing".into())
    })?;
    Ok(Some(serde_json::json!({
        "ver": 1,
        "hash": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(h),
        "ikm": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(ikm),
        "mac": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac),
        "rev": repo_state.rev,
    })))
}

async fn get_latest_commit(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Query(params): Query<LatestCommitQuery>,
) -> Result<impl IntoResponse, AppError> {
    let did = require_auth_or_credential(&state, &claims).await?;
    let space = service::resolve_space(&state, &params.space).await?;
    let has_credential = claims.space_credential.is_some();
    let membership = service::require_membership(
        &state,
        &space,
        &did,
        false,
        claims.space_credential.as_deref(),
    )
    .await?;

    let read_access = SpaceReadAccess::from_space_access(membership);
    check_read_access(&did, &params.did, read_access, has_credential)?;

    let mut conn = state
        .db
        .acquire()
        .await
        .map_err(|e| AppError::Internal(format!("failed to acquire connection: {e}")))?;
    let repo_state =
        db::get_or_create_repo_state(&mut conn, state.db_backend, &space.id, &params.did).await?;

    let commit = repo_state_commit_json(&repo_state)?;

    Ok(Json(serde_json::json!({
        "rev": repo_state.rev,
        "commit": commit,
    })))
}

async fn get_repo(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Query(params): Query<GetRepoQuery>,
) -> Result<Response, AppError> {
    let did = require_auth_or_credential(&state, &claims).await?;
    let space = service::resolve_space(&state, &params.space).await?;
    let has_credential = claims.space_credential.is_some();
    let membership = service::require_membership(
        &state,
        &space,
        &did,
        false,
        claims.space_credential.as_deref(),
    )
    .await?;

    let read_access = SpaceReadAccess::from_space_access(membership);
    check_read_access(&did, &params.did, read_access, has_credential)?;

    let mut conn = state
        .db
        .acquire()
        .await
        .map_err(|e| AppError::Internal(format!("failed to acquire connection: {e}")))?;
    let repo_state =
        db::get_or_create_repo_state(&mut conn, state.db_backend, &space.id, &params.did).await?;
    drop(conn);
    let records =
        db::list_all_space_records(&state.db, state.db_backend, &space.id, &params.did).await?;

    let hash = repo_state
        .hash
        .as_deref()
        .ok_or_else(|| AppError::NotFound("no commit exists for this repo".into()))?;
    let hash: [u8; 32] = hash
        .try_into()
        .map_err(|_| AppError::Internal("corrupt repo state: hash is not 32 bytes".into()))?;
    let ikm: [u8; 32] = repo_state
        .ikm
        .as_deref()
        .ok_or_else(|| AppError::Internal("corrupt repo state: missing ikm".into()))?
        .try_into()
        .map_err(|_| AppError::Internal("corrupt repo state: ikm is not 32 bytes".into()))?;
    let mac: [u8; 32] = repo_state
        .mac
        .as_deref()
        .ok_or_else(|| AppError::Internal("corrupt repo state: missing mac".into()))?
        .try_into()
        .map_err(|_| AppError::Internal("corrupt repo state: mac is not 32 bytes".into()))?;
    let rev = repo_state
        .rev
        .ok_or_else(|| AppError::Internal("corrupt repo state: missing rev".into()))?;
    let commit = crate::spaces::commit::SignedCommit {
        ver: 1,
        hash,
        ikm,
        mac,
        rev,
    };

    let car_bytes = crate::spaces::car::serialize_repo(&commit, &records)?;

    Ok((
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/vnd.ipld.car")],
        car_bytes,
    )
        .into_response())
}

async fn list_repo_ops(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Query(params): Query<ListRepoOpsQuery>,
) -> Result<impl IntoResponse, AppError> {
    let did = require_auth_or_credential(&state, &claims).await?;
    let space = service::resolve_space(&state, &params.space).await?;
    let has_credential = claims.space_credential.is_some();
    let membership = service::require_membership(
        &state,
        &space,
        &did,
        false,
        claims.space_credential.as_deref(),
    )
    .await?;

    let read_access = SpaceReadAccess::from_space_access(membership);
    check_read_access(&did, &params.did, read_access, has_credential)?;

    let limit = params.limit.unwrap_or(100).clamp(1, 1000);
    let exclude_values = params.exclude_values.unwrap_or(false);

    let (ops, cursor) = if exclude_values {
        oplog::list_ops(
            &state.db,
            state.db_backend,
            &space.id,
            &params.did,
            params.cursor.as_deref(),
            limit,
        )
        .await?
    } else {
        oplog::list_ops_with_values(
            &state.db,
            state.db_backend,
            &space.id,
            &params.did,
            params.cursor.as_deref(),
            limit,
        )
        .await?
    };

    let mut conn = state
        .db
        .acquire()
        .await
        .map_err(|e| AppError::Internal(format!("failed to acquire connection: {e}")))?;
    let repo_state =
        db::get_or_create_repo_state(&mut conn, state.db_backend, &space.id, &params.did).await?;
    let commit = repo_state_commit_json(&repo_state)?;

    Ok(Json(serde_json::json!({
        "ops": ops,
        "cursor": cursor,
        "commit": commit,
    })))
}

async fn list_repos(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Query(params): Query<SpaceUriQuery>,
) -> Result<impl IntoResponse, AppError> {
    let space = service::resolve_space(&state, &params.space).await?;

    if !space.config.membership_public {
        let did = require_auth_or_credential(&state, &claims).await?;
        service::require_membership(
            &state,
            &space,
            &did,
            false,
            claims.space_credential.as_deref(),
        )
        .await?;
    }

    let repos = db::list_space_repos(&state.db, state.db_backend, &space.id).await?;
    Ok(Json(serde_json::json!({ "repos": repos })))
}

async fn get_space_blob(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Query(params): Query<GetSpaceBlobQuery>,
) -> Result<impl IntoResponse, AppError> {
    let did = require_auth_or_credential(&state, &claims).await?;
    let space = service::resolve_space(&state, &params.space).await?;
    let has_credential = claims.space_credential.is_some();
    let membership = service::require_membership(
        &state,
        &space,
        &did,
        false,
        claims.space_credential.as_deref(),
    )
    .await?;

    let author_did = db::find_blob_author_did(&state.db, state.db_backend, &space.id, &params.cid)
        .await?
        .ok_or_else(|| AppError::NotFound("Blob not found in this space".into()))?;

    let read_access = SpaceReadAccess::from_space_access(membership);
    check_read_access(&did, &author_did, read_access, has_credential)?;

    let pds_endpoint =
        crate::profile::resolve_pds_endpoint(&state.http, &state.config.plc_url, &author_did)
            .await?;

    let url = format!(
        "{}/xrpc/com.atproto.sync.getBlob?did={}&cid={}",
        pds_endpoint,
        urlencoding::encode(&author_did),
        urlencoding::encode(&params.cid),
    );

    let resp = state
        .http
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::BadGateway(format!("blob fetch failed: {e}")))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(AppError::BadGateway(format!(
            "PDS returned {status} for blob cid={}",
            params.cid
        )));
    }

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| AppError::BadGateway(format!("failed to read blob body: {e}")))?;

    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        content_type
            .parse()
            .unwrap_or_else(|_| "application/octet-stream".parse().unwrap()),
    );

    Ok((status, headers, bytes))
}

async fn register_notify(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Json(input): Json<RegisterNotifyInput>,
) -> Result<impl IntoResponse, AppError> {
    let did = require_auth_or_credential(&state, &claims).await?;
    let space = service::resolve_space(&state, &input.space).await?;

    let id = notifications::register(
        &state.db,
        state.db_backend,
        &space.id,
        &input.service_did,
        &input.endpoint,
        &did,
    )
    .await?;

    Ok(Json(serde_json::json!({ "id": id })))
}

async fn notify_write(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Json(input): Json<NotifyWriteInput>,
) -> Result<impl IntoResponse, AppError> {
    // Inter-service route: only the space authority (or a super admin) may fire
    // write notifications. Previously this ignored the caller entirely, letting
    // anyone spam/forge notifications to registered endpoints (M1).
    let did = require_notify_caller(&claims)?;
    let space = service::resolve_space(&state, &input.space).await?;
    service::require_space_admin(&state, &space, &did).await?;

    notifications::dispatch_write_notification(
        &state.db,
        state.db_backend,
        &state.http,
        &space.id,
        &input.did,
        &input.collection,
        &input.rkey,
        input.cid.as_deref(),
    )
    .await?;

    Ok(Json(serde_json::json!({ "success": true })))
}

async fn notify_space_deleted(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Json(input): Json<NotifySpaceDeletedInput>,
) -> Result<impl IntoResponse, AppError> {
    // Inter-service route: only the space authority (or a super admin) may fire
    // "space deleted" events (which may cause consumers to purge cached data).
    let did = require_notify_caller(&claims)?;
    let space = service::resolve_space(&state, &input.space).await?;
    service::require_space_admin(&state, &space, &did).await?;

    notifications::dispatch_space_deleted(&state.db, state.db_backend, &state.http, &space.id)
        .await?;

    Ok(Json(serde_json::json!({ "success": true })))
}

async fn get_space_credential(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<GetSpaceCredentialInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = require_auth(&xrpc_claims)?;

    let encryption_key = state.config.token_encryption_key.as_ref().ok_or_else(|| {
        AppError::Internal("TOKEN_ENCRYPTION_KEY is required for space credentials".into())
    })?;

    let verifying_key = {
        let signing_key = k256::ecdsa::SigningKey::from_slice(encryption_key).map_err(|e| {
            AppError::Internal(format!("failed to derive delegation signing key: {e}"))
        })?;
        k256::ecdsa::VerifyingKey::from(&signing_key)
    };

    let delegation_claims = {
        let unverified_sub = crate::spaces::credential::peek_delegation_sub(&input.grant)
            .ok_or_else(|| AppError::Auth("invalid delegation token".into()))?;
        let space_did = crate::spaces::SpaceUri::parse(&unverified_sub)
            .map(|u| u.did.clone())
            .unwrap_or_default();
        let expected_aud = format!("{space_did}#atproto_space_host");
        crate::spaces::credential::verify_delegation_token(
            &input.grant,
            &verifying_key,
            &expected_aud,
        )?
    };

    // The credential is minted for the delegation token's subject (`iss`).
    // Require the authenticated caller to *be* that subject — otherwise anyone
    // who captures a member's short-lived (60s) delegation token could mint a 2h
    // credential in that member's name (M4). The documented flow has the same
    // member's app perform both steps; only the final credential is handed to an
    // external service.
    if claims.did() != delegation_claims.iss.as_str() {
        return Err(AppError::Forbidden(
            "delegation token was issued to a different account".into(),
        ));
    }

    let space = service::resolve_space(&state, &delegation_claims.sub).await?;

    // Re-verify current membership before minting. The `MemberList` mint policy
    // is a no-op that trusts the delegation token, so a member removed within the
    // token's 60s window would otherwise still be able to mint.
    service::require_membership(&state, &space, &delegation_claims.iss, false, None).await?;

    let client_id = if let Some(key) = claims.client_key() {
        resolve_client_id_url(&state, key).await?
    } else {
        None
    };
    let issued = crate::spaces::auth::issue_credential(
        &state.db,
        state.db_backend,
        &state.http,
        encryption_key,
        &space,
        &delegation_claims.iss,
        client_id.as_deref(),
        &space.authority_did,
    )
    .await?;

    Ok(Json(serde_json::json!({
        "credential": issued.token,
        "expiresAt": issued.expires_at,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn content_cid_deterministic() {
        let record = json!({"text": "hello"});
        let cid1 = service::content_cid(&record).expect("encodable");
        let cid2 = service::content_cid(&record).expect("encodable");
        assert_eq!(cid1, cid2);
        assert!(cid1.starts_with("bafyrei"));
    }

    #[test]
    fn content_cid_changes_for_different_records() {
        let a = service::content_cid(&json!({"text": "hello"})).expect("encodable");
        let b = service::content_cid(&json!({"text": "world"})).expect("encodable");
        assert_ne!(a, b);
    }

    #[test]
    fn content_cid_matches_canonical_recomputation() {
        let record = json!({ "$type": "com.example.item", "text": "hello" });
        let minted = service::content_cid(&record).expect("encodable");
        assert_eq!(
            crate::cid_verify::verify_record_cid(&minted, &record),
            crate::cid_verify::CidCheck::Match,
        );
    }

    #[test]
    fn content_cid_is_independent_of_key_order() {
        let a = service::content_cid(&json!({ "aa": 2, "b": 1 })).expect("encodable");
        let b = service::content_cid(&json!({ "b": 1, "aa": 2 })).expect("encodable");
        assert_eq!(a, b);
    }

    #[test]
    fn content_cid_rejects_unencodable_record() {
        // A malformed `$link` cannot become an IPLD link.
        let bad = json!({ "ref": { "$link": "not-a-cid" } });
        assert!(matches!(
            service::content_cid(&bad),
            Err(AppError::BadRequest(_))
        ));
    }

    #[test]
    fn deserialize_create_record_input() {
        let input: CreateRecordInput = serde_json::from_value(json!({
            "space": "at://did:plc:abc/space/com.example.forum/main",
            "collection": "com.example.forum.post",
            "record": { "text": "hello" }
        }))
        .unwrap();
        assert_eq!(input.space, "at://did:plc:abc/space/com.example.forum/main");
        assert_eq!(input.collection, "com.example.forum.post");
        assert_eq!(input.record["text"], "hello");
    }

    #[test]
    fn deserialize_put_record_with_swap() {
        let input: PutRecordInput = serde_json::from_value(json!({
            "space": "at://did:plc:abc/space/com.example.forum/main",
            "collection": "com.example.forum.post",
            "rkey": "3k2abc",
            "record": { "text": "updated" },
            "swapRecord": "bafyrei123"
        }))
        .unwrap();
        assert_eq!(input.swap_record.as_deref(), Some("bafyrei123"));
    }

    #[test]
    fn deserialize_put_record_without_swap() {
        let input: PutRecordInput = serde_json::from_value(json!({
            "space": "at://did:plc:abc/space/com.example.forum/main",
            "collection": "com.example.forum.post",
            "rkey": "3k2abc",
            "record": { "text": "hello" }
        }))
        .unwrap();
        assert_eq!(input.swap_record, None);
    }

    #[test]
    fn deserialize_delete_record_with_swap() {
        let input: DeleteRecordInput = serde_json::from_value(json!({
            "space": "at://did:plc:abc/space/com.example.forum/main",
            "collection": "com.example.forum.post",
            "rkey": "3k2abc",
            "swapRecord": "bafyrei456"
        }))
        .unwrap();
        assert_eq!(input.swap_record.as_deref(), Some("bafyrei456"));
    }

    #[test]
    fn deserialize_write_op_create() {
        let op: WriteOp = serde_json::from_value(json!({
            "action": "create",
            "collection": "com.example.forum.post",
            "value": { "text": "new post" }
        }))
        .unwrap();
        match op {
            WriteOp::Create {
                collection,
                rkey,
                value,
            } => {
                assert_eq!(collection, "com.example.forum.post");
                assert_eq!(rkey, None);
                assert_eq!(value["text"], "new post");
            }
            _ => panic!("expected Create"),
        }
    }

    #[test]
    fn deserialize_write_op_create_with_rkey() {
        let op: WriteOp = serde_json::from_value(json!({
            "action": "create",
            "collection": "com.example.forum.post",
            "rkey": "custom-key",
            "value": { "text": "new post" }
        }))
        .unwrap();
        match op {
            WriteOp::Create { rkey, .. } => {
                assert_eq!(rkey.as_deref(), Some("custom-key"));
            }
            _ => panic!("expected Create"),
        }
    }

    #[test]
    fn deserialize_write_op_update() {
        let op: WriteOp = serde_json::from_value(json!({
            "action": "update",
            "collection": "com.example.forum.post",
            "rkey": "3k2abc",
            "value": { "text": "updated" },
            "swapRecord": "bafyrei789"
        }))
        .unwrap();
        match op {
            WriteOp::Update {
                collection,
                rkey,
                swap_record,
                ..
            } => {
                assert_eq!(collection, "com.example.forum.post");
                assert_eq!(rkey, "3k2abc");
                assert_eq!(swap_record.as_deref(), Some("bafyrei789"));
            }
            _ => panic!("expected Update"),
        }
    }

    #[test]
    fn deserialize_write_op_delete() {
        let op: WriteOp = serde_json::from_value(json!({
            "action": "delete",
            "collection": "com.example.forum.post",
            "rkey": "3k2abc"
        }))
        .unwrap();
        match op {
            WriteOp::Delete {
                collection,
                rkey,
                swap_record,
            } => {
                assert_eq!(collection, "com.example.forum.post");
                assert_eq!(rkey, "3k2abc");
                assert_eq!(swap_record, None);
            }
            _ => panic!("expected Delete"),
        }
    }

    #[test]
    fn deserialize_apply_writes_input() {
        let input: ApplyWritesInput = serde_json::from_value(json!({
            "space": "at://did:plc:abc/space/com.example.forum/main",
            "swapCommit": "tid123",
            "writes": [
                {
                    "action": "create",
                    "collection": "com.example.forum.post",
                    "value": { "text": "post 1" }
                },
                {
                    "action": "delete",
                    "collection": "com.example.forum.post",
                    "rkey": "old-key"
                }
            ]
        }))
        .unwrap();
        assert_eq!(input.space, "at://did:plc:abc/space/com.example.forum/main");
        assert_eq!(input.swap_commit.as_deref(), Some("tid123"));
        assert_eq!(input.writes.len(), 2);
    }

    #[test]
    fn deserialize_apply_writes_without_swap_commit() {
        let input: ApplyWritesInput = serde_json::from_value(json!({
            "space": "at://did:plc:abc/space/com.example.forum/main",
            "writes": [
                {
                    "action": "create",
                    "collection": "com.example.forum.post",
                    "value": { "text": "post" }
                }
            ]
        }))
        .unwrap();
        assert_eq!(input.swap_commit, None);
    }

    #[test]
    fn deserialize_write_op_rejects_unknown_action() {
        let result = serde_json::from_value::<WriteOp>(json!({
            "action": "unknown",
            "collection": "test",
            "rkey": "key"
        }));
        assert!(result.is_err());
    }
}

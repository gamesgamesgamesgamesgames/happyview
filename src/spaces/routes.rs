use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::AppState;
use crate::auth::XrpcClaims;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::lua::tid::generate_tid;
use crate::spaces::types::*;
use crate::spaces::{SpaceUri, db, members};

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSpaceInput {
    #[serde(rename = "type")]
    type_nsid: String,
    skey: String,
    display_name: Option<String>,
    description: Option<String>,
    access_mode: Option<AccessMode>,
    managing_app_did: Option<String>,
    config: Option<SpaceConfig>,
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
struct DeleteSpaceInput {
    space: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateSpaceInput {
    space: String,
    display_name: Option<Option<String>>,
    description: Option<Option<String>>,
    access_mode: Option<AccessMode>,
    app_allowlist: Option<Option<Vec<String>>>,
    app_denylist: Option<Option<Vec<String>>>,
    managing_app_did: Option<Option<String>>,
    config: Option<SpaceConfig>,
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
struct AddMemberInput {
    space: String,
    did: String,
    access: Option<SpaceAccess>,
    is_delegation: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoveMemberInput {
    space: String,
    did: String,
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
struct GetMemberGrantInput {
    space: String,
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

const NS: &str = "dev.happyview";

pub fn space_routes() -> Router<AppState> {
    Router::new()
        // Space CRUD
        .route(&format!("/xrpc/{NS}.space.createSpace"), post(create_space))
        .route(&format!("/xrpc/{NS}.space.getSpace"), get(get_space))
        .route(&format!("/xrpc/{NS}.space.listSpaces"), get(list_spaces))
        .route(&format!("/xrpc/{NS}.space.deleteSpace"), post(delete_space))
        .route(&format!("/xrpc/{NS}.space.updateSpace"), post(update_space))
        // Records
        .route(
            &format!("/xrpc/{NS}.space.createRecord"),
            post(create_record),
        )
        .route(&format!("/xrpc/{NS}.space.putRecord"), post(put_record))
        .route(
            &format!("/xrpc/{NS}.space.deleteRecord"),
            post(delete_record),
        )
        .route(&format!("/xrpc/{NS}.space.applyWrites"), post(apply_writes))
        .route(&format!("/xrpc/{NS}.space.getRecord"), get(get_record))
        .route(&format!("/xrpc/{NS}.space.listRecords"), get(list_records))
        // Members
        .route(&format!("/xrpc/{NS}.space.listMembers"), get(list_members))
        .route(&format!("/xrpc/{NS}.space.addMember"), post(add_member))
        .route(
            &format!("/xrpc/{NS}.space.removeMember"),
            post(remove_member),
        )
        // Invites
        .route(
            &format!("/xrpc/{NS}.space.createInvite"),
            post(create_invite),
        )
        .route(
            &format!("/xrpc/{NS}.space.redeemInvite"),
            post(redeem_invite),
        )
        .route(
            &format!("/xrpc/{NS}.space.revokeInvite"),
            post(revoke_invite),
        )
        .route(&format!("/xrpc/{NS}.space.listInvites"), get(list_invites))
        // Credentials
        .route(
            &format!("/xrpc/{NS}.space.getMemberGrant"),
            post(get_member_grant),
        )
        .route(
            &format!("/xrpc/{NS}.space.getSpaceCredential"),
            post(get_space_credential),
        )
        // Legacy aliases (will be removed in a future release)
        .route(&format!("/xrpc/{NS}.space.create"), post(create_space))
        .route(&format!("/xrpc/{NS}.space.get"), get(get_space))
        .route(&format!("/xrpc/{NS}.space.list"), get(list_spaces))
        .route(&format!("/xrpc/{NS}.space.delete"), post(delete_space))
        .route(&format!("/xrpc/{NS}.space.update"), post(update_space))
        .route(
            &format!("/xrpc/{NS}.space.invite.create"),
            post(create_invite),
        )
        .route(
            &format!("/xrpc/{NS}.space.invite.redeem"),
            post(redeem_invite),
        )
        .route(
            &format!("/xrpc/{NS}.space.invite.revoke"),
            post(revoke_invite),
        )
        .route(&format!("/xrpc/{NS}.space.invite.list"), get(list_invites))
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
        return Ok(verified.sub);
    }

    Err(AppError::Auth(
        "This endpoint requires authentication".into(),
    ))
}

async fn resolve_space(state: &AppState, space_uri: &str) -> Result<Space, AppError> {
    let uri = SpaceUri::parse(space_uri)?;
    db::get_space_by_address(
        &state.db,
        state.db_backend,
        &uri.did,
        &uri.type_nsid,
        &uri.skey,
    )
    .await?
    .ok_or_else(|| AppError::NotFound("Space not found".into()))
}

async fn require_space_admin(state: &AppState, space: &Space, did: &str) -> Result<(), AppError> {
    if space.owner_did == did {
        return Ok(());
    }
    let sql = adapt_sql(
        "SELECT is_super FROM happyview_users WHERE did = ?",
        state.db_backend,
    );
    let row: Option<(i32,)> = sqlx::query_as(&sql)
        .bind(did)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to check admin status: {e}")))?;
    if row.is_some_and(|(is_super,)| is_super != 0) {
        return Ok(());
    }
    Err(AppError::Forbidden(
        "Only the space owner can perform this action".into(),
    ))
}

async fn require_membership(
    state: &AppState,
    space: &Space,
    did: &str,
    require_write: bool,
    space_credential: Option<&str>,
) -> Result<SpaceAccess, AppError> {
    if let Some(token) = space_credential {
        let space_uri = format!("ats://{}/{}/{}", space.did, space.type_nsid, space.skey);
        match crate::spaces::credential::verify_external_credential(
            token,
            &state.http,
            &state.config.plc_url,
        )
        .await
        {
            Ok(claims) if claims.space == space_uri => {
                let access = match claims.scope.as_str() {
                    "write" => SpaceAccess::Write,
                    _ => SpaceAccess::Read,
                };
                if require_write && !access.can_write() {
                    return Err(AppError::Forbidden(
                        "Write access is required for this action".into(),
                    ));
                }
                return Ok(access);
            }
            Ok(_) => {
                // Credential is valid but for a different space — fall through
            }
            Err(_) => {
                // External verification failed — fall through to local check
            }
        }
    }

    let access = members::is_member(&state.db, state.db_backend, &space.id, did)
        .await?
        .ok_or_else(|| AppError::Forbidden("You are not a member of this space".into()))?;
    if require_write && !access.can_write() {
        return Err(AppError::Forbidden(
            "Write access is required for this action".into(),
        ));
    }
    Ok(access)
}

fn content_cid(record: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(record).unwrap_or_default();
    let hash = Sha256::digest(&bytes);
    format!("bafyrei{}", hex::encode(&hash[..20]))
}

// ---------------------------------------------------------------------------
// Space CRUD handlers
// ---------------------------------------------------------------------------

async fn create_space(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<CreateSpaceInput>,
) -> Result<Response, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let did = claims.did().to_string();

    if input.type_nsid.is_empty() || input.skey.is_empty() {
        return Err(AppError::BadRequest("type and skey are required".into()));
    }

    let existing = db::get_space_by_address(
        &state.db,
        state.db_backend,
        &did,
        &input.type_nsid,
        &input.skey,
    )
    .await?;
    if existing.is_some() {
        return Err(AppError::Conflict(
            "A space with this address already exists".into(),
        ));
    }

    let space = Space {
        id: Uuid::new_v4().to_string(),
        did: did.clone(),
        owner_did: did.clone(),
        type_nsid: input.type_nsid,
        skey: input.skey,
        display_name: input.display_name,
        description: input.description,
        access_mode: input.access_mode.unwrap_or(AccessMode::DefaultAllow),
        app_allowlist: None,
        app_denylist: None,
        managing_app_did: input.managing_app_did,
        config: input.config.unwrap_or_default(),
        revision: None,
        created_at: now_rfc3339(),
        updated_at: now_rfc3339(),
    };

    db::create_space(&state.db, state.db_backend, &space).await?;

    // Auto-add the creator as a write member
    let member = SpaceMember {
        id: Uuid::new_v4().to_string(),
        space_id: space.id.clone(),
        did: did.clone(),
        access: SpaceAccess::Write,
        is_delegation: false,
        granted_by: Some(did),
        created_at: now_rfc3339(),
    };
    db::add_member(&state.db, state.db_backend, &member).await?;

    let space_uri = format!("ats://{}/{}/{}", space.did, space.type_nsid, space.skey);
    let body = serde_json::json!({
        "uri": space_uri,
    });

    let mut response = Json(body).into_response();
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

async fn get_space(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Query(query): Query<SpaceUriQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let space = resolve_space(&state, &query.space).await?;

    // If the space's membership is not public, require auth + membership
    if !space.config.membership_public {
        let claims = require_auth(&xrpc_claims)?;
        let did = claims.did();
        if space.owner_did != did {
            members::is_member(&state.db, state.db_backend, &space.id, did)
                .await?
                .ok_or_else(|| AppError::NotFound("Space not found".into()))?;
        }
    }

    let space_uri = format!("ats://{}/{}/{}", space.did, space.type_nsid, space.skey);
    Ok(Json(serde_json::json!({
        "uri": space_uri,
        "space": space,
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

async fn delete_space(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<DeleteSpaceInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let space = resolve_space(&state, &input.space).await?;
    require_space_admin(&state, &space, claims.did()).await?;

    db::delete_space(&state.db, state.db_backend, &space.id).await?;

    Ok(Json(serde_json::json!({ "success": true })))
}

async fn update_space(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<UpdateSpaceInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let mut space = resolve_space(&state, &input.space).await?;
    require_space_admin(&state, &space, claims.did()).await?;

    if let Some(name) = input.display_name {
        space.display_name = name;
    }
    if let Some(desc) = input.description {
        space.description = desc;
    }
    if let Some(mode) = input.access_mode {
        space.access_mode = mode;
    }
    if let Some(list) = input.app_allowlist {
        space.app_allowlist = list;
    }
    if let Some(list) = input.app_denylist {
        space.app_denylist = list;
    }
    if let Some(did) = input.managing_app_did {
        space.managing_app_did = did;
    }
    if let Some(config) = input.config {
        space.config = config;
    }

    db::update_space(&state.db, state.db_backend, &space).await?;

    let space_uri = format!("ats://{}/{}/{}", space.did, space.type_nsid, space.skey);
    Ok(Json(serde_json::json!({
        "uri": space_uri,
        "space": space,
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
    let space = resolve_space(&state, &input.space).await?;
    require_membership(
        &state,
        &space,
        &did,
        true,
        xrpc_claims.space_credential.as_deref(),
    )
    .await?;

    let rkey = generate_tid();
    let cid = content_cid(&input.record);
    let record_uri = format!(
        "ats://{}/{}/{}/{}/{}/{}",
        space.did, space.type_nsid, space.skey, did, input.collection, rkey
    );

    let record = SpaceRecord {
        uri: record_uri.clone(),
        space_id: space.id.clone(),
        author_did: did,
        collection: input.collection,
        rkey,
        record: input.record,
        cid: cid.clone(),
        indexed_at: now_rfc3339(),
    };

    db::insert_space_record(&state.db, state.db_backend, &record).await?;

    let rev = generate_tid();
    db::update_space_revision(&state.db, state.db_backend, &space.id, &rev).await?;

    let body = serde_json::json!({
        "uri": record_uri,
        "cid": cid,
    });

    let mut response = Json(body).into_response();
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

async fn put_record(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<PutRecordInput>,
) -> Result<Response, AppError> {
    let did = require_auth_or_credential(&state, &xrpc_claims).await?;
    let space = resolve_space(&state, &input.space).await?;
    require_membership(
        &state,
        &space,
        &did,
        true,
        xrpc_claims.space_credential.as_deref(),
    )
    .await?;

    let cid = content_cid(&input.record);
    let record_uri = format!(
        "ats://{}/{}/{}/{}/{}/{}",
        space.did, space.type_nsid, space.skey, did, input.collection, input.rkey
    );

    let record = SpaceRecord {
        uri: record_uri.clone(),
        space_id: space.id.clone(),
        author_did: did,
        collection: input.collection,
        rkey: input.rkey,
        record: input.record,
        cid: cid.clone(),
        indexed_at: now_rfc3339(),
    };

    if let Some(swap_cid) = input.swap_record {
        db::upsert_space_record_with_swap(&state.db, state.db_backend, &record, &swap_cid).await?;
    } else {
        db::upsert_space_record(&state.db, state.db_backend, &record).await?;
    }

    let rev = generate_tid();
    db::update_space_revision(&state.db, state.db_backend, &space.id, &rev).await?;

    let body = serde_json::json!({
        "uri": record_uri,
        "cid": cid,
    });

    let mut response = Json(body).into_response();
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
    let space = resolve_space(&state, &input.space).await?;

    let record_uri = format!(
        "ats://{}/{}/{}/{}/{}/{}",
        space.did, space.type_nsid, space.skey, did, input.collection, input.rkey
    );

    if let Some(swap_cid) = input.swap_record {
        db::delete_space_record_with_swap(&state.db, state.db_backend, &record_uri, &swap_cid)
            .await?;
    } else {
        let record = db::get_space_record(&state.db, state.db_backend, &record_uri).await?;
        match record {
            Some(r) if r.author_did != did => {
                return Err(AppError::Forbidden(
                    "You can only delete your own records".into(),
                ));
            }
            None => {
                return Err(AppError::NotFound("Record not found".into()));
            }
            _ => {}
        }
        db::delete_space_record(&state.db, state.db_backend, &record_uri).await?;
    }

    let rev = generate_tid();
    db::update_space_revision(&state.db, state.db_backend, &space.id, &rev).await?;

    Ok(Json(serde_json::json!({ "success": true })))
}

async fn apply_writes(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<ApplyWritesInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let did = require_auth_or_credential(&state, &xrpc_claims).await?;
    let space = resolve_space(&state, &input.space).await?;
    require_membership(
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

    for op in input.writes {
        match op {
            WriteOp::Create {
                collection,
                rkey,
                value,
            } => {
                let rkey = rkey.unwrap_or_else(generate_tid);
                let cid = content_cid(&value);
                let record_uri = format!(
                    "ats://{}/{}/{}/{}/{}/{}",
                    space.did, space.type_nsid, space.skey, did, collection, rkey
                );
                let record = SpaceRecord {
                    uri: record_uri.clone(),
                    space_id: space.id.clone(),
                    author_did: did.clone(),
                    collection,
                    rkey,
                    record: value,
                    cid: cid.clone(),
                    indexed_at: now_rfc3339(),
                };
                db::insert_space_record(&state.db, state.db_backend, &record).await?;
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
                let cid = content_cid(&value);
                let record_uri = format!(
                    "ats://{}/{}/{}/{}/{}/{}",
                    space.did, space.type_nsid, space.skey, did, collection, rkey
                );
                let record = SpaceRecord {
                    uri: record_uri.clone(),
                    space_id: space.id.clone(),
                    author_did: did.clone(),
                    collection,
                    rkey,
                    record: value,
                    cid: cid.clone(),
                    indexed_at: now_rfc3339(),
                };
                if let Some(swap_cid) = swap_record {
                    db::upsert_space_record_with_swap(
                        &state.db,
                        state.db_backend,
                        &record,
                        &swap_cid,
                    )
                    .await?;
                } else {
                    db::upsert_space_record(&state.db, state.db_backend, &record).await?;
                }
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
                    "ats://{}/{}/{}/{}/{}/{}",
                    space.did, space.type_nsid, space.skey, did, collection, rkey
                );
                if let Some(swap_cid) = swap_record {
                    db::delete_space_record_with_swap(
                        &state.db,
                        state.db_backend,
                        &record_uri,
                        &swap_cid,
                    )
                    .await?;
                } else {
                    db::delete_space_record(&state.db, state.db_backend, &record_uri).await?;
                }
                results.push(serde_json::json!({}));
            }
        }
    }

    let rev = generate_tid();
    db::update_space_revision(&state.db, state.db_backend, &space.id, &rev).await?;

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
    let space = resolve_space(&state, &query.space).await?;
    require_membership(
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
    let space = resolve_space(&state, &query.space).await?;
    require_membership(
        &state,
        &space,
        &did,
        false,
        xrpc_claims.space_credential.as_deref(),
    )
    .await?;

    let repo = query.repo.as_deref().or_else(|| {
        if xrpc_claims.space_credential.is_some() {
            None
        } else {
            Some(did.as_str())
        }
    });

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
// Member handlers
// ---------------------------------------------------------------------------

async fn list_members(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Query(query): Query<SpaceUriQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let space = resolve_space(&state, &query.space).await?;

    if !space.config.membership_public {
        let did = require_auth_or_credential(&state, &xrpc_claims).await?;
        require_membership(
            &state,
            &space,
            &did,
            false,
            xrpc_claims.space_credential.as_deref(),
        )
        .await?;
    }

    let resolved = members::resolve_members(&state.db, state.db_backend, &space.id).await?;

    Ok(Json(serde_json::json!({ "members": resolved })))
}

async fn add_member(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<AddMemberInput>,
) -> Result<Response, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let space = resolve_space(&state, &input.space).await?;
    require_space_admin(&state, &space, claims.did()).await?;

    let existing = db::get_member(&state.db, state.db_backend, &space.id, &input.did).await?;
    if existing.is_some() {
        return Err(AppError::Conflict(
            "Member already exists in this space".into(),
        ));
    }

    let member = SpaceMember {
        id: Uuid::new_v4().to_string(),
        space_id: space.id,
        did: input.did,
        access: input.access.unwrap_or(SpaceAccess::Read),
        is_delegation: input.is_delegation.unwrap_or(false),
        granted_by: Some(claims.did().to_string()),
        created_at: now_rfc3339(),
    };

    db::add_member(&state.db, state.db_backend, &member).await?;

    let mut response = Json(serde_json::json!({ "member": member })).into_response();
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

async fn remove_member(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<RemoveMemberInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let space = resolve_space(&state, &input.space).await?;
    require_space_admin(&state, &space, claims.did()).await?;

    let removed = db::remove_member(&state.db, state.db_backend, &space.id, &input.did).await?;

    if !removed {
        return Err(AppError::NotFound("Member not found in this space".into()));
    }

    Ok(Json(serde_json::json!({ "success": true })))
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
    let space = resolve_space(&state, &input.space).await?;
    require_space_admin(&state, &space, claims.did()).await?;

    let mut token_bytes = [0u8; 24];
    rand::Fill::fill(&mut token_bytes, &mut rand::rng());
    let token = hex::encode(token_bytes);
    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));

    let invite = SpaceInvite {
        id: Uuid::new_v4().to_string(),
        space_id: space.id,
        token_hash,
        created_by: claims.did().to_string(),
        access: input.access.unwrap_or(SpaceAccess::Read),
        max_uses: input.max_uses,
        uses: 0,
        expires_at: input.expires_at,
        revoked: false,
        created_at: now_rfc3339(),
    };

    db::create_invite(&state.db, state.db_backend, &invite).await?;

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

async fn redeem_invite(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<RedeemInviteInput>,
) -> Result<Response, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let did = claims.did().to_string();

    let token_hash = hex::encode(Sha256::digest(input.token.as_bytes()));
    let invite = db::get_invite_by_token_hash(&state.db, state.db_backend, &token_hash)
        .await?
        .ok_or_else(|| AppError::NotFound("Invalid invite token".into()))?;

    if invite.revoked {
        return Err(AppError::BadRequest("This invite has been revoked".into()));
    }

    if let Some(max) = invite.max_uses
        && invite.uses >= max
    {
        return Err(AppError::BadRequest(
            "This invite has reached its maximum uses".into(),
        ));
    }

    if let Some(ref expires) = invite.expires_at {
        let now = now_rfc3339();
        if now > *expires {
            return Err(AppError::BadRequest("This invite has expired".into()));
        }
    }

    let existing = db::get_member(&state.db, state.db_backend, &invite.space_id, &did).await?;
    if existing.is_some() {
        return Err(AppError::Conflict(
            "You are already a member of this space".into(),
        ));
    }

    let member = SpaceMember {
        id: Uuid::new_v4().to_string(),
        space_id: invite.space_id.clone(),
        did,
        access: invite.access,
        is_delegation: false,
        granted_by: Some(invite.created_by.clone()),
        created_at: now_rfc3339(),
    };

    db::add_member(&state.db, state.db_backend, &member).await?;
    db::increment_invite_uses(&state.db, state.db_backend, &invite.id).await?;

    let space = db::get_space(&state.db, state.db_backend, &invite.space_id).await?;
    let space_uri = space.map(|s| format!("ats://{}/{}/{}", s.did, s.type_nsid, s.skey));

    let mut response = Json(serde_json::json!({
        "uri": space_uri,
        "access": member.access,
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
    let space = resolve_space(&state, &input.space).await?;
    require_space_admin(&state, &space, claims.did()).await?;

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
    let space = resolve_space(&state, &query.space).await?;
    require_space_admin(&state, &space, claims.did()).await?;

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

async fn get_member_grant(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<GetMemberGrantInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let did = claims.did().to_string();
    let space = resolve_space(&state, &input.space).await?;

    require_membership(&state, &space, &did, false, None).await?;

    let encryption_key = state.config.token_encryption_key.as_ref().ok_or_else(|| {
        AppError::Internal("TOKEN_ENCRYPTION_KEY is required for space credentials".into())
    })?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let exp = now + crate::spaces::credential::GRANT_TTL_SECS;

    let space_uri = format!("ats://{}/{}/{}", space.did, space.type_nsid, space.skey);
    let grant_claims = crate::spaces::credential::MemberGrantClaims {
        sub: did,
        space: space_uri,
        scope: "read".into(),
        iat: now,
        exp,
    };

    let grant = crate::spaces::credential::sign_grant(&grant_claims, encryption_key)?;

    let expires_at = chrono::DateTime::from_timestamp(exp as i64, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();

    Ok(Json(serde_json::json!({
        "grant": grant,
        "expiresAt": expires_at,
    })))
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

    let grant_claims = crate::spaces::credential::verify_grant(&input.grant, encryption_key)?;

    let space = resolve_space(&state, &grant_claims.space).await?;

    let client_id = claims.client_key().map(|k| k.to_string());
    let issued = crate::spaces::auth::issue_credential(
        &state.db,
        state.db_backend,
        encryption_key,
        &space,
        &grant_claims.sub,
        client_id.as_deref(),
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
        let cid1 = content_cid(&record);
        let cid2 = content_cid(&record);
        assert_eq!(cid1, cid2);
        assert!(cid1.starts_with("bafyrei"));
    }

    #[test]
    fn content_cid_changes_for_different_records() {
        let a = content_cid(&json!({"text": "hello"}));
        let b = content_cid(&json!({"text": "world"}));
        assert_ne!(a, b);
    }

    #[test]
    fn deserialize_create_record_input() {
        let input: CreateRecordInput = serde_json::from_value(json!({
            "space": "ats://did:plc:abc/com.example.forum/main",
            "collection": "com.example.forum.post",
            "record": { "text": "hello" }
        }))
        .unwrap();
        assert_eq!(input.space, "ats://did:plc:abc/com.example.forum/main");
        assert_eq!(input.collection, "com.example.forum.post");
        assert_eq!(input.record["text"], "hello");
    }

    #[test]
    fn deserialize_put_record_with_swap() {
        let input: PutRecordInput = serde_json::from_value(json!({
            "space": "ats://did:plc:abc/com.example.forum/main",
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
            "space": "ats://did:plc:abc/com.example.forum/main",
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
            "space": "ats://did:plc:abc/com.example.forum/main",
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
            "space": "ats://did:plc:abc/com.example.forum/main",
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
        assert_eq!(input.space, "ats://did:plc:abc/com.example.forum/main");
        assert_eq!(input.swap_commit.as_deref(), Some("tid123"));
        assert_eq!(input.writes.len(), 2);
    }

    #[test]
    fn deserialize_apply_writes_without_swap_commit() {
        let input: ApplyWritesInput = serde_json::from_value(json!({
            "space": "ats://did:plc:abc/com.example.forum/main",
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

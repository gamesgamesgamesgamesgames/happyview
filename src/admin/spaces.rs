//! Operator access to spaces for moderation.
//!
//! Spaces are private to their members, so these routes sit outside the XRPC
//! space API: a moderator is not a member and is not an app acting for one.
//! Reading a space's contents is logged so the instance keeps a record of who
//! looked inside which space.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use serde::Deserialize;

use crate::AppState;
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};
use crate::spaces::types::Space;
use crate::spaces::{db, members, service};

use super::auth::UserAuth;
use super::permissions::Permission;

#[derive(Deserialize)]
pub(super) struct ListSpacesParams {
    pub limit: Option<i64>,
    pub cursor: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct ListSpaceRecordsParams {
    pub repo: Option<String>,
    pub collection: Option<String>,
    pub limit: Option<i64>,
    pub cursor: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct GetSpaceBlobParams {
    pub cid: String,
}

fn space_uri(space: &Space) -> String {
    format!(
        "at://{}/space/{}/{}",
        space.did, space.type_nsid, space.skey
    )
}

fn space_json(space: &Space) -> serde_json::Value {
    let mut value = serde_json::to_value(space).unwrap_or_default();
    value["uri"] = space_uri(space).into();
    value
}

async fn load_space(state: &AppState, id: &str) -> Result<Space, AppError> {
    db::get_space(&state.db, state.db_backend, id)
        .await?
        .ok_or_else(|| AppError::NotFound("Space not found".into()))
}

async fn log_content_read(
    state: &AppState,
    auth: &UserAuth,
    space: &Space,
    detail: serde_json::Value,
) {
    let mut detail = detail;
    detail["space_id"] = space.id.clone().into();
    detail["user_id"] = auth.user_id.clone().into();
    log_event(
        &state.db,
        EventLog {
            event_type: "space.moderator_read".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(space_uri(space)),
            detail,
        },
        state.db_backend,
    )
    .await;
}

/// GET /admin/spaces — every space on the instance, newest first.
pub(super) async fn list_spaces(
    State(state): State<AppState>,
    auth: UserAuth,
    Query(params): Query<ListSpacesParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::SpacesRead).await?;
    let limit = params.limit.unwrap_or(50).clamp(1, 100);
    let offset: i64 = params
        .cursor
        .as_deref()
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);

    let spaces = db::list_all_spaces(&state.db, state.db_backend, limit, offset).await?;
    let cursor = (spaces.len() as i64 == limit).then(|| (offset + limit).to_string());

    let mut body = serde_json::json!({
        "spaces": spaces.iter().map(space_json).collect::<Vec<_>>(),
    });
    if let Some(cursor) = cursor {
        body["cursor"] = cursor.into();
    }
    Ok(Json(body))
}

/// GET /admin/spaces/{id} — a space's metadata, members, and collections.
pub(super) async fn get_space(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::SpacesRead).await?;
    let space = load_space(&state, &id).await?;

    let members = members::resolve_members(&state.db, state.db_backend, &space.id).await?;
    let collections =
        db::count_space_records_by_collection(&state.db, state.db_backend, &space.id).await?;

    Ok(Json(serde_json::json!({
        "space": space_json(&space),
        "members": members,
        "collections": collections
            .into_iter()
            .map(|(collection, count)| serde_json::json!({
                "collection": collection,
                "count": count,
            }))
            .collect::<Vec<_>>(),
    })))
}

/// GET /admin/spaces/{id}/records — records in a space, newest first.
pub(super) async fn list_space_records(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
    Query(params): Query<ListSpaceRecordsParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::SpacesManageRecords).await?;
    let space = load_space(&state, &id).await?;
    let limit = params.limit.unwrap_or(20).clamp(1, 100);

    let (records, cursor) = db::list_space_records(
        &state.db,
        state.db_backend,
        &space.id,
        params.repo.as_deref(),
        params.collection.as_deref(),
        limit,
        params.cursor.as_deref(),
        true,
    )
    .await?;

    log_content_read(
        &state,
        &auth,
        &space,
        serde_json::json!({
            "action": "list_records",
            "repo": params.repo,
            "collection": params.collection,
            "uris": records.iter().map(|r| r.uri.as_str()).collect::<Vec<_>>(),
        }),
    )
    .await;

    let mut body = serde_json::json!({
        "records": records
            .iter()
            .map(|r| serde_json::json!({
                "uri": r.uri,
                "did": r.author_did,
                "collection": r.collection,
                "rkey": r.rkey,
                "cid": r.cid,
                "indexed_at": r.indexed_at,
                "record": r.record,
            }))
            .collect::<Vec<_>>(),
    });
    if let Some(cursor) = cursor {
        body["cursor"] = cursor.into();
    }
    Ok(Json(body))
}

/// GET /admin/spaces/{id}/blob?cid=X — a blob referenced by a record in a space.
pub(super) async fn get_space_blob(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
    Query(params): Query<GetSpaceBlobParams>,
) -> Result<impl IntoResponse, AppError> {
    auth.require(Permission::SpacesManageRecords).await?;
    let space = load_space(&state, &id).await?;

    let author_did = db::find_blob_author_did(&state.db, state.db_backend, &space.id, &params.cid)
        .await?
        .ok_or_else(|| AppError::NotFound("Blob not found in this space".into()))?;

    log_content_read(
        &state,
        &auth,
        &space,
        serde_json::json!({
            "action": "get_blob",
            "cid": params.cid,
            "repo": author_did,
        }),
    )
    .await;

    service::fetch_space_blob(&state, &author_did, &params.cid).await
}

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
use crate::db::{adapt_sql, parse_dt};
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, write_event};
use crate::spaces::types::Space;
use crate::spaces::{db, members, service};

use super::auth::UserAuth;
use super::permissions::Permission;
use super::space_access::{self, Grant, GrantScope};

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

#[derive(Deserialize)]
pub(super) struct AccountRecordsParams {
    pub space: Option<String>,
    pub collection: Option<String>,
    pub limit: Option<i64>,
    pub cursor: Option<String>,
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

/// Record a read of space contents under the grant that allowed it.
async fn log_content_read(
    state: &AppState,
    auth: &UserAuth,
    grant: &Grant,
    subject: String,
    detail: serde_json::Value,
) -> Result<(), AppError> {
    let mut detail = detail;
    detail["user_id"] = auth.user_id.clone().into();
    detail["grant_id"] = grant.id.clone().into();
    detail["scope"] = grant.scope.as_str().into();
    let event = EventLog {
        event_type: "space.moderator_read".to_string(),
        severity: Severity::Info,
        actor_did: Some(auth.did.clone()),
        subject: Some(subject),
        detail,
    };
    // Fails the request rather than return content whose read went unrecorded.
    // The event and its link to the grant commit together.
    let failed = |e: sqlx::Error| AppError::Internal(format!("failed to record the read: {e}"));
    let mut tx = state.db.begin().await.map_err(failed)?;
    let event_id = write_event(&mut *tx, &event, state.db_backend)
        .await
        .map_err(failed)?;
    crate::db::query(&adapt_sql(
        "INSERT INTO happyview_space_access_reads (event_id, grant_id) VALUES (?, ?)",
        state.db_backend,
    ))
    .bind(&event_id)
    .bind(&grant.id)
    .execute(&mut *tx)
    .await
    .map_err(failed)?;
    tx.commit().await.map_err(failed)
}

/// GET /admin/spaces — every space on the instance, newest first.
pub(super) async fn list_spaces(
    State(state): State<AppState>,
    auth: UserAuth,
    Query(params): Query<ListSpacesParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::SpacesRead).await?;
    space_access::require_enabled(&state).await?;
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
    space_access::require_enabled(&state).await?;
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

/// GET /admin/spaces/{id}/access — the caller's active grant that covers this
/// space's page: a space grant for it if one exists, otherwise the account
/// grant expiring last whose account is a member or has records here.
/// `{ "grant": null }` when none does.
pub(super) async fn covering_grant(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let grants = space_access::active_grants(&state, &auth).await?;
    let space = load_space(&state, &id).await?;
    let now = chrono::Utc::now();

    let space_grant = space_access::select_covering(
        &grants,
        &space_access::Covers::Space {
            space_id: &space.id,
            repo: None,
        },
        now,
    );
    let grant = match space_grant {
        Some(g) => Some(g.clone()),
        None => {
            let targets: Vec<String> = grants
                .iter()
                .filter(|g| g.scope == GrantScope::Account && g.is_active(now))
                .map(|g| g.target.clone())
                .collect();
            let present =
                db::space_accounts_among(&state.db, state.db_backend, &space.id, &targets).await?;
            grants
                .iter()
                .filter(|g| {
                    g.scope == GrantScope::Account
                        && g.is_active(now)
                        && present.contains(&g.target)
                })
                .max_by_key(|g| parse_dt(&g.expires_at))
                .cloned()
        }
    };
    Ok(Json(serde_json::json!({ "grant": grant })))
}

/// GET /admin/spaces/{id}/records — records in a space, newest first.
pub(super) async fn list_space_records(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(id): Path<String>,
    Query(params): Query<ListSpaceRecordsParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Permission and switch first, so a caller who can't read contents
    // learns nothing about which spaces exist.
    let grants = space_access::active_grants(&state, &auth).await?;
    let space = load_space(&state, &id).await?;
    let grant = space_access::select_covering(
        &grants,
        &space_access::Covers::Space {
            space_id: &space.id,
            repo: params.repo.as_deref(),
        },
        chrono::Utc::now(),
    )
    .cloned()
    .ok_or_else(space_access::grant_required)?;
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
        &grant,
        space_uri(&space),
        serde_json::json!({
            "action": "list_records",
            "space_id": space.id,
            "repo": params.repo,
            "collection": params.collection,
            "uris": records.iter().map(|r| r.uri.as_str()).collect::<Vec<_>>(),
        }),
    )
    .await?;

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
    let grants = space_access::active_grants(&state, &auth).await?;
    let space = load_space(&state, &id).await?;
    let now = chrono::Utc::now();

    // A space grant opens any blob in the space. An account grant opens a
    // blob only when that account's own record references it, even if
    // another author's record shares the CID.
    let mut found: Option<(Grant, String)> = None;
    if let Some(g) = space_access::select_covering(
        &grants,
        &space_access::Covers::Space {
            space_id: &space.id,
            repo: None,
        },
        now,
    ) {
        if let Some(author) =
            db::find_blob_author_did(&state.db, state.db_backend, &space.id, &params.cid, None)
                .await?
        {
            found = Some((g.clone(), author));
        }
    } else {
        let mut account_grants: Vec<&Grant> = grants
            .iter()
            .filter(|g| g.scope == GrantScope::Account)
            .collect();
        if account_grants.is_empty() {
            return Err(space_access::grant_required());
        }
        account_grants.sort_by_key(|g| std::cmp::Reverse(parse_dt(&g.expires_at)));
        // One lookup for every author whose record references the blob, then
        // match grants in memory, however many account grants the caller holds.
        let authors =
            db::find_blob_authors(&state.db, state.db_backend, &space.id, &params.cid).await?;
        found = account_grants
            .into_iter()
            .find(|g| authors.contains(&g.target))
            .map(|g| (g.clone(), g.target.clone()));
    }
    let (grant, author_did) =
        found.ok_or_else(|| AppError::NotFound("Blob not found in this space".into()))?;

    log_content_read(
        &state,
        &auth,
        &grant,
        space_uri(&space),
        serde_json::json!({
            "action": "get_blob",
            "space_id": space.id,
            "cid": params.cid,
            "repo": author_did,
        }),
    )
    .await?;

    service::fetch_space_blob(&state, &author_did, &params.cid).await
}

/// GET /admin/accounts/{did}/spaces — where an account has membership or
/// records. Metadata only.
pub(super) async fn list_account_spaces(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(did): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::SpacesRead).await?;
    space_access::require_enabled(&state).await?;
    let spaces = db::list_author_spaces(&state.db, state.db_backend, &did).await?;
    Ok(Json(serde_json::json!({
        "spaces": spaces
            .iter()
            .map(|(space, count)| serde_json::json!({ "space": space_json(space), "record_count": count }))
            .collect::<Vec<_>>(),
    })))
}

/// GET /admin/accounts/{did}/space-records — one account's records across
/// spaces, newest first.
pub(super) async fn list_account_space_records(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(did): Path<String>,
    Query(params): Query<AccountRecordsParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    let grant =
        space_access::require_grant(&state, &auth, space_access::Covers::Account { did: &did })
            .await?;
    let limit = params.limit.unwrap_or(20).clamp(1, 100);
    let (records, cursor) = db::list_author_space_records(
        &state.db,
        state.db_backend,
        &did,
        params.space.as_deref(),
        params.collection.as_deref(),
        limit,
        params.cursor.as_deref(),
    )
    .await?;

    log_content_read(
        &state,
        &auth,
        &grant,
        did.clone(),
        serde_json::json!({
            "action": "list_account_records",
            "repo": did,
            "space_id": params.space,
            "collection": params.collection,
            "uris": records.iter().map(|r| r.uri.as_str()).collect::<Vec<_>>(),
        }),
    )
    .await?;

    let mut body = serde_json::json!({
        "records": records
            .iter()
            .map(|r| {
                let suffix = format!("/{}/{}/{}", r.author_did, r.collection, r.rkey);
                serde_json::json!({
                    "uri": r.uri,
                    "did": r.author_did,
                    "collection": r.collection,
                    "rkey": r.rkey,
                    "cid": r.cid,
                    "indexed_at": r.indexed_at,
                    "record": r.record,
                    "space_id": r.space_id,
                    "space_uri": r.uri.strip_suffix(&suffix).unwrap_or(&r.uri),
                })
            })
            .collect::<Vec<_>>(),
    });
    if let Some(cursor) = cursor {
        body["cursor"] = cursor.into();
    }
    Ok(Json(body))
}

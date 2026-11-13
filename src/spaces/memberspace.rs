//! `com.atproto.memberspace.*`: a `memberspace` space's member list.
//!
//! The Nov 12 revision of Proposal 0016 puts and removes members in batches
//! (`putMembers`, `removeMembers`, up to 100 per call, one commit each); the
//! Oct 29 revision did it one at a time (`putMember`, `removeMember`). Both
//! are served, over the same batch path in [`service`]. `listMembers` has one
//! name in both. The `simplespace` member methods are in
//! [`crate::spaces::simplespace`].
//!
//! Each needs the caller to own the space. Putting and removing members needs
//! `manage=update` on the session's `space:` grant; listing them, a read
//! grant.

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{MethodRouter, get, post};
use happyview_scopes::{ManageOp, SpaceTarget};
use serde::Deserialize;

use crate::AppState;
use crate::auth::XrpcClaims;
use crate::error::AppError;
use crate::spaces::service::{self, MemberEntry};
use crate::spaces::types::Space;
use crate::spaces::{lifecycle, scope};

/// The most entries one `putMembers` or `removeMembers` takes.
const MAX_BATCH: usize = 100;
/// `listMembers`'s page size: its default and its ceiling.
const DEFAULT_PAGE: i64 = 100;
const MAX_PAGE: i64 = 1000;

const NS: &str = "com.atproto.memberspace";

pub(crate) fn memberspace_method_table() -> Vec<(String, MethodRouter<AppState>)> {
    vec![
        (format!("{NS}.putMembers"), post(put_members)),
        (format!("{NS}.removeMembers"), post(remove_members)),
        (format!("{NS}.listMembers"), get(list_members)),
        (format!("{NS}.putMember"), post(put_member)),
        (format!("{NS}.removeMember"), post(remove_member)),
    ]
}

#[derive(Deserialize)]
struct PutMembersInput {
    space: String,
    members: Vec<MemberEntry>,
}

#[derive(Deserialize)]
struct RemoveMembersInput {
    space: String,
    dids: Vec<String>,
}

#[derive(Deserialize)]
struct PutMemberInput {
    space: String,
    did: String,
    read: bool,
    write: bool,
}

#[derive(Deserialize)]
struct RemoveMemberInput {
    space: String,
    did: String,
}

#[derive(Deserialize)]
struct ListMembersQuery {
    space: String,
    limit: Option<i64>,
    cursor: Option<String>,
}

fn invalid(message: String) -> AppError {
    AppError::XrpcError {
        status: StatusCode::BAD_REQUEST,
        code: "InvalidRequest",
        message,
    }
}

fn space_not_found() -> AppError {
    AppError::XrpcError {
        status: StatusCode::BAD_REQUEST,
        code: "SpaceNotFound",
        message: "space not found".into(),
    }
}

/// The space, which the caller owns and may manage with `op`.
async fn managed_space(
    state: &AppState,
    claims: &XrpcClaims,
    space_ref: &str,
    op: ManageOp,
) -> Result<Space, AppError> {
    let identity = lifecycle::require_identity(claims)?;
    let space = service::resolve_space(state, space_ref)
        .await
        .map_err(|e| match e {
            AppError::NotFound(_) => space_not_found(),
            e => e,
        })?;
    lifecycle::require_owner(state, identity, &space, op).await?;
    Ok(space)
}

async fn put_members(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Json(input): Json<PutMembersInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    if input.members.len() > MAX_BATCH {
        return Err(invalid(format!(
            "members takes at most {MAX_BATCH} entries"
        )));
    }
    let space = managed_space(&state, &claims, &input.space, ManageOp::Update).await?;
    let actor = lifecycle::require_identity(&claims)?.did().to_string();
    service::put_members(&state, &actor, &space, input.members).await?;
    Ok(Json(serde_json::json!({})))
}

async fn remove_members(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Json(input): Json<RemoveMembersInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    if input.dids.len() > MAX_BATCH {
        return Err(invalid(format!("dids takes at most {MAX_BATCH} entries")));
    }
    let space = managed_space(&state, &claims, &input.space, ManageOp::Update).await?;
    service::remove_members(&state, &space, input.dids).await?;
    Ok(Json(serde_json::json!({})))
}

/// Oct 29's single `putMember`: a batch of one.
async fn put_member(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Json(input): Json<PutMemberInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let space = managed_space(&state, &claims, &input.space, ManageOp::Update).await?;
    let actor = lifecycle::require_identity(&claims)?.did().to_string();
    let entry = MemberEntry {
        did: input.did,
        read: input.read,
        write: input.write,
    };
    service::put_members(&state, &actor, &space, vec![entry]).await?;
    Ok(Json(serde_json::json!({})))
}

/// Oct 29's single `removeMember`: a batch of one.
async fn remove_member(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Json(input): Json<RemoveMemberInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let space = managed_space(&state, &claims, &input.space, ManageOp::Update).await?;
    service::remove_members(&state, &space, vec![input.did]).await?;
    Ok(Json(serde_json::json!({})))
}

/// The member list, a page at a time, ordered by DID. The cursor is the last
/// DID of the page before.
async fn list_members(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Query(query): Query<ListMembersQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let limit = query.limit.unwrap_or(DEFAULT_PAGE);
    if !(1..=MAX_PAGE).contains(&limit) {
        return Err(invalid(format!("limit must be between 1 and {MAX_PAGE}")));
    }
    if let Some(cursor) = &query.cursor
        && !cursor.starts_with("did:")
    {
        return Err(AppError::XrpcError {
            status: StatusCode::BAD_REQUEST,
            code: "InvalidCursor",
            message: "cursor is not one this method gave".into(),
        });
    }
    let identity = lifecycle::require_identity(&claims)?;
    let space = service::resolve_space(&state, &query.space)
        .await
        .map_err(|e| match e {
            AppError::NotFound(_) => space_not_found(),
            e => e,
        })?;
    service::require_space_admin(&state, &space, identity.did())
        .await
        .map_err(|e| match e {
            AppError::Forbidden(message) => AppError::XrpcError {
                status: StatusCode::FORBIDDEN,
                code: "NotAuthorized",
                message,
            },
            e => e,
        })?;
    scope::require_space_scope(&state, identity, &space, SpaceTarget::Read).await?;
    if !matches!(
        crate::spaces::policy::SpacePolicy::of(&space),
        Ok(crate::spaces::policy::SpacePolicy::Member(_))
    ) {
        return Err(service::policy_mismatch());
    }

    // The effective list: nested spaces' members included, with the access
    // they actually have.
    let effective =
        crate::spaces::members::resolve_members(&state.db, state.db_backend, &space.id).await?;
    let members: Vec<MemberEntry> = effective
        .into_iter()
        .filter(|m| {
            query
                .cursor
                .as_deref()
                .is_none_or(|after| m.did.as_str() > after)
        })
        .take(limit as usize)
        .map(|m| MemberEntry {
            did: m.did,
            read: m.access.read,
            write: m.access.write,
        })
        .collect();
    let cursor = (members.len() as i64 == limit)
        .then(|| members.last().map(|m| m.did.clone()))
        .flatten();
    let mut body = serde_json::json!({ "members": members });
    if let Some(cursor) = cursor {
        body["cursor"] = cursor.into();
    }
    Ok(Json(body))
}

//! Spaces nested in a memberspace (`dev.happyview.space.*NestedSpace`).
//!
//! Nesting a space makes its members members of the nesting space, with at
//! most the access the reference grants. Membership is resolved when it is
//! checked ([`crate::spaces::members`]); nothing is copied. A space's
//! `listMembers` and published member records hold its own members only.
//! Nesting a space requires administering it.
//!
//! Only spaces on this host can be nested for now.

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{MethodRouter, get, post};
use happyview_scopes::{ManageOp, SpaceTarget};
use serde::Deserialize;

use crate::AppState;
use crate::auth::XrpcClaims;
use crate::db::now_rfc3339;
use crate::error::AppError;
use crate::spaces::members;
use crate::spaces::policy::SpacePolicy;
use crate::spaces::types::{MemberAccess, Space, SpaceMember};
use crate::spaces::{SpaceUri, db, lifecycle, scope, service};

const NS: &str = "dev.happyview.space";

pub(crate) fn nested_method_table() -> Vec<(String, MethodRouter<AppState>)> {
    vec![
        (format!("{NS}.putNestedSpace"), post(put_nested_space)),
        (format!("{NS}.removeNestedSpace"), post(remove_nested_space)),
        (format!("{NS}.listNestedSpaces"), get(list_nested_spaces)),
    ]
}

#[derive(Deserialize)]
struct PutNestedInput {
    space: String,
    nested: String,
    read: bool,
    write: bool,
}

#[derive(Deserialize)]
struct RemoveNestedInput {
    space: String,
    nested: String,
}

#[derive(Deserialize)]
struct ListNestedQuery {
    space: String,
}

fn invalid(message: String) -> AppError {
    AppError::XrpcError {
        status: StatusCode::BAD_REQUEST,
        code: "InvalidRequest",
        message,
    }
}

async fn owned_space(
    state: &AppState,
    claims: &XrpcClaims,
    space_ref: &str,
    target: SpaceTarget<'_>,
) -> Result<(Space, String), AppError> {
    let identity = lifecycle::require_identity(claims)?;
    let space = service::resolve_space(state, space_ref)
        .await
        .map_err(|e| match e {
            AppError::NotFound(_) => AppError::XrpcError {
                status: StatusCode::BAD_REQUEST,
                code: "SpaceNotFound",
                message: "space not found".into(),
            },
            e => e,
        })?;
    service::require_space_admin(state, &space, identity.did())
        .await
        .map_err(|e| match e {
            AppError::Forbidden(message) => AppError::XrpcError {
                status: StatusCode::FORBIDDEN,
                code: "NotAuthorized",
                message,
            },
            e => e,
        })?;
    scope::require_space_scope(state, identity, &space, target).await?;
    Ok((space, identity.did().to_string()))
}

async fn put_nested_space(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Json(input): Json<PutNestedInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (space, actor) = owned_space(
        &state,
        &claims,
        &input.space,
        SpaceTarget::Manage(ManageOp::Update),
    )
    .await?;
    put_nested(
        &state,
        &actor,
        &space,
        &input.nested,
        input.read,
        input.write,
    )
    .await?;
    Ok(Json(serde_json::json!({})))
}

async fn remove_nested_space(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Json(input): Json<RemoveNestedInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (space, _) = owned_space(
        &state,
        &claims,
        &input.space,
        SpaceTarget::Manage(ManageOp::Update),
    )
    .await?;
    remove_nested(&state, &space, &input.nested).await?;
    Ok(Json(serde_json::json!({})))
}

async fn list_nested_spaces(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Query(query): Query<ListNestedQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (space, _) = owned_space(&state, &claims, &query.space, SpaceTarget::Read).await?;
    let nested = db::list_nested(&state.db, state.db_backend, &space.id).await?;
    Ok(Json(serde_json::json!({ "nested": nested })))
}

fn require_memberspace(space: &Space) -> Result<(), AppError> {
    match SpacePolicy::of(space) {
        Ok(SpacePolicy::Member(_)) => Ok(()),
        _ => Err(service::policy_mismatch()),
    }
}

fn space_not_found() -> AppError {
    AppError::XrpcError {
        status: StatusCode::BAD_REQUEST,
        code: "SpaceNotFound",
        message: "space not found".into(),
    }
}

fn address_of(space: &Space) -> String {
    format!(
        "at://{}/space/{}/{}",
        space.did, space.type_nsid, space.skey
    )
}

/// The space on this host that `nested` names: an `at://` or `ats://`
/// address, or a space id, as `simplespace` named spaces before addresses.
async fn named_space(state: &AppState, nested: &str) -> Result<Option<Space>, AppError> {
    if let Ok(uri) = SpaceUri::parse(nested) {
        return db::get_space_by_address(
            &state.db,
            state.db_backend,
            &uri.did,
            &uri.type_nsid,
            &uri.skey,
        )
        .await;
    }
    if nested.starts_with("did:") || nested.contains("://") {
        return Ok(None);
    }
    db::get_space(&state.db, state.db_backend, nested).await
}

/// Nesting a space gives the nesting space's members-to-be the nested
/// space's members, so it takes the nested space's administrator to allow it.
async fn require_target_admin(
    state: &AppState,
    target: &Space,
    actor: &str,
) -> Result<(), AppError> {
    service::require_space_admin(state, target, actor)
        .await
        .map_err(|e| match e {
            AppError::Forbidden(_) => AppError::XrpcError {
                status: StatusCode::FORBIDDEN,
                code: "NotAuthorized",
                message: "nesting a space requires administering it".into(),
            },
            e => e,
        })
}

/// Nest a space, or replace the access its reference grants
/// (`putNestedSpace`). The caller has checked that `actor` may manage
/// `space`; `actor` must administer the nested space too, which must be a
/// memberspace on this host other than `space`.
pub(crate) async fn put_nested(
    state: &AppState,
    actor: &str,
    space: &Space,
    nested: &str,
    read: bool,
    write: bool,
) -> Result<(), AppError> {
    require_memberspace(space)?;
    SpaceUri::parse(nested).map_err(|_| invalid(format!("{nested:?} is not a space address")))?;
    let target = named_space(state, nested).await?.ok_or_else(|| {
        invalid(format!(
            "{nested} is not a space on this host; only local spaces can be nested for now"
        ))
    })?;
    require_target_admin(state, &target, actor).await?;
    require_memberspace(&target).map_err(|_| AppError::XrpcError {
        status: StatusCode::BAD_REQUEST,
        code: "PolicyMismatch",
        message: "only a memberspace can be nested: it is the only policy with a member list"
            .into(),
    })?;
    if target.id == space.id {
        return Err(invalid("a space can't nest itself".into()));
    }
    let reference = reference_to(actor, &target, read, write);
    change_reference(state, space, Change::Put(&reference, &target.id)).await?;
    Ok(())
}

fn reference_to(actor: &str, target: &Space, read: bool, write: bool) -> db::NestedRef {
    db::NestedRef {
        target_space_uri: address_of(target),
        read,
        write,
        added_by: Some(actor.to_string()),
        created_at: now_rfc3339(),
    }
}

/// `simplespace` `putMember` (or, with `add`, `addMember`) naming a space as
/// a member. A memberspace on this host is nested in a memberspace, which
/// takes administering both; `addMember` on one already nested is a
/// `Conflict`. Any other entry is stored as a member row naming the space, as
/// `simplespace` always stored it; one naming a space elsewhere brings in no
/// members. The answer is the member as `simplespace` shaped it.
pub(crate) async fn put_space_member(
    state: &AppState,
    actor: &str,
    space_ref: &str,
    member: &str,
    access: Option<MemberAccess>,
    is_delegation: Option<bool>,
    add: bool,
) -> Result<SpaceMember, AppError> {
    let space = service::resolve_space(state, space_ref).await?;
    service::require_space_admin(state, &space, actor).await?;
    if let Some(target) = named_space(state, member).await? {
        require_target_admin(state, &target, actor).await?;
        if target.id != space.id
            && require_memberspace(&space).is_ok()
            && require_memberspace(&target).is_ok()
        {
            let access = access.unwrap_or(MemberAccess::READ);
            let reference = reference_to(actor, &target, access.read, access.write);
            let change = if add {
                Change::Add(&reference, &target.id)
            } else {
                Change::Put(&reference, &target.id)
            };
            change_reference(state, &space, change).await?;
            let (id, created_at) = db::nested_row(
                &state.db,
                state.db_backend,
                &space.id,
                &reference.target_space_uri,
            )
            .await?
            .ok_or_else(|| AppError::Internal("nested space vanished after nesting".into()))?;
            return Ok(SpaceMember {
                id,
                space_id: space.id,
                did: member.to_string(),
                access: MemberAccess {
                    read: access.read,
                    write: access.write,
                    read_self: false,
                },
                is_delegation: true,
                granted_by: Some(actor.to_string()),
                created_at,
            });
        }
    }
    if add {
        service::add_member(state, actor, space_ref, member, access, is_delegation).await
    } else {
        service::put_member(state, actor, space_ref, member, access, is_delegation).await
    }
}

/// `simplespace` `removeMember`, and the script API's: a space nested under
/// the name given (an `at://` or `ats://` address, or a space id) is removed,
/// and so is a member row of that name.
pub(crate) async fn remove_space_member(
    state: &AppState,
    actor: &str,
    space_ref: &str,
    member: &str,
) -> Result<(), AppError> {
    let address = match SpaceUri::parse(member) {
        Ok(uri) => Some(format!(
            "at://{}/space/{}/{}",
            uri.did, uri.type_nsid, uri.skey
        )),
        Err(_) => named_space(state, member).await?.map(|t| address_of(&t)),
    };
    if let Some(address) = address {
        let space = service::resolve_space(state, space_ref).await?;
        service::require_space_admin(state, &space, actor).await?;
        if change_reference(state, &space, Change::Remove(&address)).await? {
            return match service::remove_member(state, actor, space_ref, member).await {
                Err(AppError::NotFound(_)) => Ok(()),
                result => result,
            };
        }
    }
    service::remove_member(state, actor, space_ref, member).await
}

/// Remove a nested space. Removing one that isn't nested is `InvalidRequest`.
pub(crate) async fn remove_nested(
    state: &AppState,
    space: &Space,
    nested: &str,
) -> Result<(), AppError> {
    let uri = SpaceUri::parse(nested)
        .map_err(|_| invalid(format!("{nested:?} is not a space address")))?;
    let address = format!("at://{}/space/{}/{}", uri.did, uri.type_nsid, uri.skey);
    if !change_reference(state, space, Change::Remove(&address)).await? {
        return Err(invalid(format!("{address} is not nested in this space")));
    }
    Ok(())
}

/// A change to one of a space's nested references. A put or an add names
/// the nested space's id as well as its address.
#[derive(Clone, Copy)]
enum Change<'a> {
    Put(&'a db::NestedRef, &'a str),
    /// A put that refuses a space already nested.
    Add(&'a db::NestedRef, &'a str),
    Remove(&'a str),
}

/// Make `change` to `space`'s references in one transaction, under the
/// space's row lock as member changes take it, and return whether anything
/// changed. A put or an add needs the space, as locked, to be a memberspace.
///
/// In the same transaction: a put or an add that would take the space, or a
/// space that takes its members, past what resolution reads is refused,
/// unless that space was past it already; and the credentials of accounts
/// that no longer read the space, or a space that takes its members, are
/// revoked (`service::revoke_lost_readers`). The revocations are announced
/// once the transaction commits.
async fn change_reference(
    state: &AppState,
    space: &Space,
    change: Change<'_>,
) -> Result<bool, AppError> {
    let backend = state.db_backend;
    let (changed, revoked) = crate::db::retry_on_busy(|| async {
        let mut tx = crate::db::begin_write(&state.db, backend).await?;
        let Some(locked) = db::lock_space(&mut *tx, backend, &space.id).await? else {
            return Ok(Err(space_not_found()));
        };
        let grows = !matches!(change, Change::Remove(_));
        if grows && let Err(e) = require_memberspace(&locked) {
            return Ok(Err(e));
        }
        let before = match service::readers_before(&mut tx, backend, &space.id).await {
            Ok(before) => before,
            Err(e) => return Ok(Err(e)),
        };
        // The spaces whose effective lists a put can grow, that resolve
        // within the bound now. A removal only shrinks them.
        let mut bounded = Vec::new();
        if grows {
            for id in std::iter::once(&space.id).chain(before.delegating()) {
                match members::resolve_on(&mut tx, backend, id).await {
                    Ok(_) => bounded.push(id.clone()),
                    Err(e) if members::is_too_large(&e) => {}
                    Err(e) => return Ok(Err(e)),
                }
            }
        }
        if let Change::Add(reference, _) = change {
            match db::nested_row(&mut *tx, backend, &space.id, &reference.target_space_uri).await {
                Ok(None) => {}
                Ok(Some(_)) => {
                    return Ok(Err(AppError::Conflict(
                        "Member already exists in this space".into(),
                    )));
                }
                Err(e) => return Ok(Err(e)),
            }
        }
        let changed = match change {
            Change::Put(reference, target_id) | Change::Add(reference, target_id) => {
                db::upsert_nested(&mut *tx, backend, &space.id, target_id, reference)
                    .await
                    .map(|()| true)
            }
            Change::Remove(address) => {
                db::delete_nested(&mut *tx, backend, &space.id, address).await
            }
        };
        match changed {
            Ok(true) => {}
            Ok(false) => return Ok(Ok((false, Vec::new()))),
            Err(e) => return Ok(Err(e)),
        }
        // Dropping the transaction on a refusal puts the reference back.
        for id in &bounded {
            if let Err(e) = members::resolve_on(&mut tx, backend, id).await {
                return Ok(Err(e));
            }
        }
        let revoked = match service::revoke_lost_readers(&mut tx, backend, before).await? {
            Ok(revoked) => revoked,
            Err(e) => return Ok(Err(e)),
        };
        tx.commit().await?;
        Ok(Ok((true, revoked)))
    })
    .await
    .map_err(|e: sqlx::Error| {
        AppError::Internal(format!("failed to change nested spaces: {e}"))
    })??;
    service::announce_all_revoked(state, revoked);
    Ok(changed)
}

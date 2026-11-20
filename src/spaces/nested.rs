//! Spaces nested in a memberspace (`dev.happyview.space.*NestedSpace`).
//!
//! Nesting a space makes its members members of the nesting space, with at
//! most the access the reference grants. Membership is resolved when it is
//! checked ([`crate::spaces::members`]); nothing is copied. A space's
//! `listMembers` and published member records hold its own members only.
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
use crate::spaces::types::Space;
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

/// The canonical address of a space to nest, which must be a memberspace on
/// this host.
async fn local_target(state: &AppState, nested: &str) -> Result<String, AppError> {
    let uri = SpaceUri::parse(nested)
        .map_err(|_| invalid(format!("{nested:?} is not a space address")))?;
    let target = db::get_space_by_address(
        &state.db,
        state.db_backend,
        &uri.did,
        &uri.type_nsid,
        &uri.skey,
    )
    .await?
    .ok_or_else(|| {
        invalid(format!(
            "{nested} is not a space on this host; only local spaces can be nested for now"
        ))
    })?;
    require_memberspace(&target).map_err(|_| AppError::XrpcError {
        status: StatusCode::BAD_REQUEST,
        code: "PolicyMismatch",
        message: "only a memberspace can be nested: it is the only policy with a member list"
            .into(),
    })?;
    Ok(format!(
        "at://{}/space/{}/{}",
        target.did, target.type_nsid, target.skey
    ))
}

/// Nest a space, or replace the access its reference grants. The caller has
/// checked that `actor` may manage `space`.
pub(crate) async fn put_nested(
    state: &AppState,
    actor: &str,
    space: &Space,
    nested: &str,
    read: bool,
    write: bool,
) -> Result<(), AppError> {
    require_memberspace(space)?;
    let target = local_target(state, nested).await?;
    let reference = db::NestedRef {
        target_space_uri: target,
        read,
        write,
        added_by: Some(actor.to_string()),
        created_at: now_rfc3339(),
    };
    change_reference(state, space, Change::Put(&reference)).await?;
    Ok(())
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

/// A change to one of a space's nested references.
enum Change<'a> {
    Put(&'a db::NestedRef),
    Remove(&'a str),
}

/// Make `change` to `space`'s references in one transaction, under the
/// space's row lock as member changes take it, and return whether anything
/// changed.
///
/// In the same transaction: a change that would take the space, or a space
/// that takes its members, past what resolution reads is refused, unless
/// that space was past it already; and the credentials of accounts that no
/// longer read the space, or a space that takes its members, are revoked
/// (`service::revoke_lost_readers`). The revocations are announced once the
/// transaction commits.
async fn change_reference(
    state: &AppState,
    space: &Space,
    change: Change<'_>,
) -> Result<bool, AppError> {
    let backend = state.db_backend;
    let (changed, revoked) = crate::db::retry_on_busy(|| async {
        let mut tx = crate::db::begin_write(&state.db, backend).await?;
        if db::lock_space(&mut *tx, backend, &space.id)
            .await?
            .is_none()
        {
            return Ok(Err(AppError::NotFound("Space not found".into())));
        }
        let before = match service::readers_before(&mut tx, backend, &space.id).await {
            Ok(before) => before,
            Err(e) => return Ok(Err(e)),
        };
        // The spaces whose effective lists this change can grow, that
        // resolve within the bound now.
        let mut bounded = Vec::new();
        for id in std::iter::once(&space.id).chain(before.delegating()) {
            match members::resolve_on(&mut tx, backend, id).await {
                Ok(_) => bounded.push(id.clone()),
                Err(e) if members::is_too_large(&e) => {}
                Err(e) => return Ok(Err(e)),
            }
        }
        let changed = match change {
            Change::Put(reference) => db::upsert_nested(&mut *tx, backend, &space.id, reference)
                .await
                .map(|()| true),
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

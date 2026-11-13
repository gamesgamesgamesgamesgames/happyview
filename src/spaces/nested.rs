//! Spaces nested in a memberspace (`dev.happyview.space.*NestedSpace`).
//!
//! Nesting a space makes its members members of the nesting space, with at
//! most the access the reference grants. Membership is resolved when it is
//! checked ([`crate::spaces::members`]); nothing is copied. When anything
//! changes what a space's members are, [`Cascade`] brings every space that
//! reaches it into step: credentials for accounts that lost read access are
//! revoked, and published member lists are updated.
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
use crate::spaces::members::{self, Resolution};
use crate::spaces::policy::SpacePolicy;
use crate::spaces::types::Space;
use crate::spaces::{SpaceUri, db, lifecycle, publish, scope, service};

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
    let previous = db::list_nested(&state.db, state.db_backend, &space.id)
        .await?
        .into_iter()
        .find(|r| r.target_space_uri == target);
    let cascade = Cascade::before(state, space, true).await?;
    db::upsert_nested(
        &state.db,
        state.db_backend,
        &space.id,
        &db::NestedRef {
            target_space_uri: target.clone(),
            read,
            write,
            added_by: Some(actor.to_string()),
            created_at: now_rfc3339(),
        },
    )
    .await?;
    // A reference that would take this space, or one that reaches it, past
    // what resolution reads is refused, and the previous state put back.
    if let Err(e) = cascade.check(state).await {
        match previous {
            Some(previous) => {
                db::upsert_nested(&state.db, state.db_backend, &space.id, &previous).await?
            }
            None => {
                db::delete_nested(&state.db, state.db_backend, &space.id, &target).await?;
            }
        }
        return Err(e);
    }
    cascade.after(state).await
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
    let cascade = Cascade::before(state, space, true).await?;
    let removed = db::delete_nested(&state.db, state.db_backend, &space.id, &address).await?;
    // Run even when nothing was removed: a retry of a removal whose cascade
    // failed finishes it here.
    cascade.after(state).await?;
    if !removed {
        return Err(invalid(format!("{address} is not nested in this space")));
    }
    Ok(())
}

/// The effective lists of a changed space's dependents, taken before the
/// change, to be compared with them after it.
pub(crate) struct Cascade {
    before: Vec<(String, Resolution)>,
}

impl Cascade {
    /// Snapshot every space that reaches `space` (and `space` itself, with
    /// `include_self`), before a change to what `space`'s members are.
    pub(crate) async fn before(
        state: &AppState,
        space: &Space,
        include_self: bool,
    ) -> Result<Self, AppError> {
        let mut conn = state
            .db
            .acquire()
            .await
            .map_err(|e| AppError::Internal(format!("failed to acquire connection: {e}")))?;
        let mut ids = members::reaching_on(&mut conn, state.db_backend, &space.id).await?;
        if include_self {
            ids.insert(0, space.id.clone());
        }
        let mut before = Vec::with_capacity(ids.len());
        for id in ids {
            // A space already past the bound answers every access check with
            // an error, so there is nothing to compare it against.
            match members::resolve_on(&mut conn, state.db_backend, &id).await {
                Ok(resolution) => before.push((id, resolution)),
                Err(e) if is_too_large(&e) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(Self { before })
    }

    /// Whether every snapshotted space still resolves within the bound.
    pub(crate) async fn check(&self, state: &AppState) -> Result<(), AppError> {
        let mut conn = state
            .db
            .acquire()
            .await
            .map_err(|e| AppError::Internal(format!("failed to acquire connection: {e}")))?;
        for (id, _) in &self.before {
            members::resolve_on(&mut conn, state.db_backend, id).await?;
        }
        Ok(())
    }

    /// After the change: revoke the credentials of accounts that lost read
    /// access to each snapshotted space, and bring its published member list
    /// into step.
    pub(crate) async fn after(self, state: &AppState) -> Result<(), AppError> {
        for (id, before) in self.before {
            let Some(space) = db::get_space(&state.db, state.db_backend, &id).await? else {
                continue;
            };
            let after = {
                let mut conn = state.db.acquire().await.map_err(|e| {
                    AppError::Internal(format!("failed to acquire connection: {e}"))
                })?;
                members::resolve_on(&mut conn, state.db_backend, &id).await
            };
            // The change took this space past the bound. Its access checks
            // now fail closed; the change itself stands.
            let after = match after {
                Ok(after) => after,
                Err(e) if is_too_large(&e) => {
                    tracing::warn!(
                        space_id = %id,
                        "a membership change took a nesting space past the resolution bound"
                    );
                    continue;
                }
                Err(e) => return Err(e),
            };
            let mut changed: Vec<String> = Vec::new();
            for (did, was) in &before {
                let now = after.get(did);
                if now.map(|e| e.access) != Some(was.access) {
                    changed.push(did.clone());
                }
            }
            // Every outstanding credential whose account no longer reads the
            // space is revoked, not only those of accounts that changed just
            // now, so a retry finishes what a failed request left undone.
            if space.read_policy == crate::spaces::types::Policy::MemberList {
                for did in
                    db::outstanding_credential_subjects(&state.db, state.db_backend, &space.id)
                        .await?
                {
                    if did.is_empty() || did == space.creator_did {
                        continue;
                    }
                    if !after.get(&did).is_some_and(|e| e.access.read) {
                        service::revoke_member_credentials(state, &space, &did).await?;
                    }
                }
            }
            for did in after.keys() {
                if !before.contains_key(did) {
                    changed.push(did.clone());
                }
            }
            if !changed.is_empty() {
                publish::sync(state, &space.id, publish::Scope::Members(&changed)).await?;
            }
        }
        Ok(())
    }
}

fn is_too_large(e: &AppError) -> bool {
    matches!(
        e,
        AppError::XrpcError {
            code: "MemberListTooLarge",
            ..
        }
    )
}

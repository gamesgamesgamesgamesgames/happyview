use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::AppState;
use crate::auth::XrpcClaims;
use crate::error::AppError;
use crate::spaces::service;
use crate::spaces::types::*;
use crate::spaces::{SpaceUri, db, members};

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SpaceUriQuery {
    pub space: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PutMemberInput {
    pub space: String,
    pub did: String,
    /// Both required by the lexicon, and not defaulted: putMember replaces the
    /// pair wholesale, so a defaulted value would grant or revoke access the
    /// caller never specified.
    pub read: bool,
    pub write: bool,
    /// HappyView extension: a member added transitively via a delegated space.
    pub is_delegation: Option<bool>,
}

/// `addMember`, which predates `putMember`, names access with one word.
/// Accepted until v3.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AddMemberInput {
    pub space: String,
    pub did: String,
    pub access: Option<String>,
    pub is_delegation: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemoveMemberInput {
    pub space: String,
    pub did: String,
}

// ---------------------------------------------------------------------------
// Route registration
// ---------------------------------------------------------------------------

const NS: &str = "com.atproto";
const LEGACY_NS: &str = "dev.happyview";

/// Every `simplespace` method this build serves. See
/// [`crate::spaces::routes::protocol_method_table`] for why this is a table.
pub(crate) fn management_method_table() -> Vec<(String, MethodRouter<AppState>)> {
    vec![
        (format!("{NS}.simplespace.putMember"), post(put_member)),
        (
            format!("{NS}.simplespace.removeMember"),
            post(remove_member),
        ),
        (format!("{NS}.simplespace.listMembers"), get(list_members)),
    ]
}

pub fn simplespace_routes() -> Router<AppState> {
    let mut router = Router::new();
    for (nsid, handler) in management_method_table() {
        router = router.route(&format!("/xrpc/{nsid}"), handler);
    }

    router
        // Backward-compatible aliases (dev.happyview.space.*) — kept until v3
        .route("/xrpc/com.atproto.simplespace.addMember", post(add_member))
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.addMember"),
            post(add_member),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.putMember"),
            post(put_member),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.removeMember"),
            post(remove_member),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.listMembers"),
            get(list_members),
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

// ---------------------------------------------------------------------------
// Space management handlers
// ---------------------------------------------------------------------------

async fn list_members(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Query(query): Query<SpaceUriQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let space = resolve_space(&state, &query.space).await?;
    let claims = require_auth(&xrpc_claims)?;
    service::require_space_admin(&state, &space, claims.did()).await?;

    let resolved = members::resolve_members(&state.db, state.db_backend, &space.id).await?;

    Ok(Json(serde_json::json!({ "members": resolved })))
}

async fn put_member(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<PutMemberInput>,
) -> Result<Response, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    // read_self is never settable over the wire: the spec's member list has no
    // such concept. See `MemberAccess`.
    //
    // Always `Some`: `read` and `write` are both required here, so this
    // surface can never mean "leave access as it is".
    let access = MemberAccess {
        read: input.read,
        write: input.write,
        read_self: false,
    };
    let member = service::put_member(
        &state,
        claims.did(),
        &input.space,
        &input.did,
        Some(access),
        input.is_delegation,
    )
    .await?;
    let mut response = Json(serde_json::json!({ "member": member })).into_response();
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

async fn add_member(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<AddMemberInput>,
) -> Result<Response, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let access = input
        .access
        .as_deref()
        .map(|word| {
            MemberAccess::parse_wire(word)
                .ok_or_else(|| AppError::BadRequest(format!("unknown access '{word}'")))
        })
        .transpose()?;
    let member = service::add_member(
        &state,
        claims.did(),
        &input.space,
        &input.did,
        access,
        input.is_delegation,
    )
    .await?;
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
    service::remove_member(&state, claims.did(), &input.space, &input.did).await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

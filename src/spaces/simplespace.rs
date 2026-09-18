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
pub(crate) struct CreateSpaceInput {
    #[serde(rename = "type")]
    pub type_nsid: String,
    pub skey: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    // Raw, so `parse_policy` can report an unimplemented variant with its own
    // error code rather than as a generic deserialize failure.
    pub read_policy: Option<serde_json::Value>,
    pub write_policy: Option<serde_json::Value>,
    pub app_access: Option<serde_json::Value>,
    pub config: Option<SpaceConfig>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SpaceUriQuery {
    pub space: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeleteSpaceInput {
    pub space: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateSpaceInput {
    pub space: String,
    pub display_name: Option<Option<String>>,
    pub description: Option<Option<String>>,
    pub read_policy: Option<serde_json::Value>,
    pub write_policy: Option<serde_json::Value>,
    pub app_access: Option<serde_json::Value>,
    pub config: Option<SpaceConfig>,
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
        (format!("{NS}.simplespace.createSpace"), post(create_space)),
        (format!("{NS}.simplespace.updateSpace"), post(update_space)),
        (format!("{NS}.simplespace.deleteSpace"), post(delete_space)),
        (
            format!("{NS}.simplespace.getSpace"),
            get(crate::spaces::routes::get_space),
        ),
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
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.createSpace"),
            post(create_space),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.updateSpace"),
            post(update_space),
        )
        .route(
            &format!("/xrpc/{LEGACY_NS}.space.deleteSpace"),
            post(delete_space),
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

/// Convert a supplied policy value, naming the failure.
///
/// A host MUST reject a policy it does not implement rather than store one it
/// cannot enforce, and the rejection carries `UnsupportedPolicy` so a client can
/// distinguish it from a malformed request.
fn parse_policy(raw: Option<serde_json::Value>, field: &str) -> Result<Option<Policy>, AppError> {
    raw.map(|v| {
        serde_json::from_value(v).map_err(|_| AppError::XrpcError {
            status: StatusCode::BAD_REQUEST,
            code: "UnsupportedPolicy",
            message: format!("{field} names a policy this host does not implement"),
        })
    })
    .transpose()
}

fn parse_app_access(raw: Option<serde_json::Value>) -> Result<Option<AppAccess>, AppError> {
    raw.map(|v| {
        serde_json::from_value(v).map_err(|_| AppError::XrpcError {
            status: StatusCode::BAD_REQUEST,
            code: "UnsupportedAppAccess",
            message: "appAccess names a variant this host does not implement".into(),
        })
    })
    .transpose()
}

async fn create_space(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<CreateSpaceInput>,
) -> Result<Response, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let space = service::create_space(
        &state,
        claims.did(),
        &input.type_nsid,
        &input.skey,
        input.display_name,
        input.description,
        parse_policy(input.read_policy, "readPolicy")?,
        parse_policy(input.write_policy, "writePolicy")?,
        parse_app_access(input.app_access)?,
        input.config,
    )
    .await?;
    let space_uri = format!(
        "at://{}/space/{}/{}",
        space.did, space.type_nsid, space.skey
    );
    let mut response = Json(serde_json::json!({ "uri": space_uri })).into_response();
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

async fn delete_space(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<DeleteSpaceInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    service::delete_space(&state, claims.did(), &input.space).await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

async fn update_space(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<UpdateSpaceInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let space = service::update_space(
        &state,
        claims.did(),
        &input.space,
        input.display_name,
        input.description,
        parse_policy(input.read_policy, "readPolicy")?,
        parse_policy(input.write_policy, "writePolicy")?,
        parse_app_access(input.app_access)?,
        input.config,
    )
    .await?;
    let space_uri = format!(
        "at://{}/space/{}/{}",
        space.did, space.type_nsid, space.skey
    );
    Ok(Json(serde_json::json!({
        "uri": space_uri,
        "space": space,
    })))
}

async fn list_members(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Query(query): Query<SpaceUriQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let space = resolve_space(&state, &query.space).await?;
    let claims = require_auth(&xrpc_claims)?;
    require_space_admin(&state, &space, claims.did()).await?;

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

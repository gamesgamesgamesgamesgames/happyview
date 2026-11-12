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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateSpaceInput {
    /// `type` is the earlier name.
    #[serde(rename = "spaceType", alias = "type")]
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
    /// One policy for both reads and writes, as clients sent it before the
    /// split: `policy` from earlier drafts, `mintPolicy` from HappyView.
    pub policy: Option<serde_json::Value>,
    pub mint_policy: Option<serde_json::Value>,
    /// The legacy policy's managing app, sent beside it rather than inside.
    pub managing_app: Option<String>,
    pub managing_app_did: Option<String>,
    /// A did:key the creator holds, listed as a rotation key of the space's
    /// DID ahead of this instance's, so the creator can move the space.
    pub rotation_key: Option<String>,
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
    pub policy: Option<serde_json::Value>,
    pub mint_policy: Option<serde_json::Value>,
    pub managing_app: Option<String>,
    pub managing_app_did: Option<String>,
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
        (format!("{NS}.simplespace.getSpace"), get(get_space)),
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
        // Backward-compatible aliases (dev.happyview.space.*)
        .route("/xrpc/com.atproto.simplespace.addMember", post(add_member))
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
        .route(&format!("/xrpc/{LEGACY_NS}.space.getSpace"), get(get_space))
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

/// A space named on a simplespace route.
///
/// A simplespace client may address a space under its creator's DID, as it was
/// addressed before every space had a DID of its own. On these routes only, an
/// address that names no space resolves to the space its DID created with that
/// type and key (HAP-388 H9).
async fn resolve_space(state: &AppState, space_uri: &str) -> Result<Space, AppError> {
    let uri = SpaceUri::parse(space_uri)?;
    if let Some(space) = db::get_space_by_address(
        &state.db,
        state.db_backend,
        &uri.did,
        &uri.type_nsid,
        &uri.skey,
    )
    .await?
    {
        return Ok(space);
    }
    db::list_spaces_by_owner(&state.db, state.db_backend, &uri.did)
        .await?
        .into_iter()
        .find(|s| s.type_nsid == uri.type_nsid && s.skey == uri.skey)
        .ok_or_else(|| AppError::NotFound("Space not found".into()))
}

/// The space's own address, for a reference a simplespace client sent, so the
/// shared service functions find it.
async fn canonical_ref(state: &AppState, space_uri: &str) -> Result<String, AppError> {
    resolve_space(state, space_uri)
        .await
        .map(|space| crate::spaces::lifecycle::space_uri(&space))
}

// ---------------------------------------------------------------------------
// Policies, as simplespace configured them
// ---------------------------------------------------------------------------

fn unsupported_policy(message: String) -> AppError {
    AppError::XrpcError {
        status: StatusCode::BAD_REQUEST,
        code: "UnsupportedPolicy",
        message,
    }
}

/// Convert a supplied policy value, naming the failure.
///
/// A host MUST reject a policy it does not implement rather than store one it
/// cannot enforce, and the rejection carries `UnsupportedPolicy` so a client can
/// distinguish it from a malformed request.
fn parse_policy(raw: Option<serde_json::Value>, field: &str) -> Result<Option<Policy>, AppError> {
    raw.map(|v| {
        serde_json::from_value(v).map_err(|_| {
            unsupported_policy(format!(
                "{field} names a policy this host does not implement"
            ))
        })
    })
    .transpose()
}

/// The single policy clients sent before reads and writes were split, which
/// governed both.
fn legacy_policy(
    policy: Option<serde_json::Value>,
    mint_policy: Option<serde_json::Value>,
    managing_app: Option<String>,
    managing_app_did: Option<String>,
) -> Result<Option<Policy>, AppError> {
    let Some(raw) = policy.or(mint_policy) else {
        return Ok(None);
    };
    let unsupported =
        || unsupported_policy("policy names a policy this host does not implement".into());
    let Some(name) = raw.as_str() else {
        return parse_policy(Some(raw), "policy");
    };
    match name {
        "public" => Ok(Some(Policy::Public)),
        "member-list" => Ok(Some(Policy::MemberList)),
        "managing-app" => managing_app
            .or(managing_app_did)
            .map(|managing_app| Some(Policy::ManagingApp { managing_app }))
            .ok_or_else(unsupported),
        _ => Err(unsupported()),
    }
}

fn parse_app_access(raw: Option<serde_json::Value>) -> Result<Option<AppAccess>, AppError> {
    // Earlier HappyView tagged variants with `type` and a short name.
    let raw = raw.map(|mut v| {
        if let Some(obj) = v.as_object_mut()
            && !obj.contains_key("$type")
            && let Some(kind) = obj
                .remove("type")
                .and_then(|t| t.as_str().map(str::to_string))
        {
            obj.insert(
                "$type".into(),
                format!("com.atproto.simplespace.defs#{kind}").into(),
            );
        }
        v
    });
    raw.map(|v| {
        serde_json::from_value(v).map_err(|_| AppError::XrpcError {
            status: StatusCode::BAD_REQUEST,
            code: "UnsupportedAppAccess",
            message: "appAccess names a variant this host does not implement".into(),
        })
    })
    .transpose()
}

/// Read, write and app rules that amount to no policy are refused: since
/// spaces have policies, a mixed managing-app rule cannot be stored
/// (HAP-388 H6).
fn require_a_policy(
    read: &Policy,
    write: &Policy,
    app: &AppAccess,
    config: &SpaceConfig,
) -> Result<(), AppError> {
    crate::spaces::policy::SpacePolicy::from_rules(read, write, app, config).map(|_| ())
}

/// `allowedCollections`, which simplespace copied from the type declaration,
/// is no longer enforced: any collection may appear in a space. A config
/// naming it has the field dropped, and the answer says so (HAP-388 H5).
fn drop_allowed_collections(config: &mut Option<SpaceConfig>) -> Vec<String> {
    let dropped = config
        .as_mut()
        .is_some_and(|config| config.extra.remove("allowedCollections").is_some());
    if dropped {
        vec!["allowedCollections is not supported and was ignored".into()]
    } else {
        Vec::new()
    }
}

fn with_warnings(mut body: serde_json::Value, warnings: Vec<String>) -> serde_json::Value {
    if !warnings.is_empty() {
        body["warnings"] = warnings.into();
    }
    body
}

// ---------------------------------------------------------------------------
// Space lifecycle
// ---------------------------------------------------------------------------

/// `simplespace.createSpace`: a space configured by read and write policies
/// and app access, stored as the policy they amount to.
async fn create_space(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(mut input): Json<CreateSpaceInput>,
) -> Result<Response, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let legacy = legacy_policy(
        input.policy,
        input.mint_policy,
        input.managing_app,
        input.managing_app_did,
    )?;
    let read = parse_policy(input.read_policy, "readPolicy")?.or_else(|| legacy.clone());
    let write = parse_policy(input.write_policy, "writePolicy")?.or(legacy);
    let app = parse_app_access(input.app_access)?;
    let warnings = drop_allowed_collections(&mut input.config);
    require_a_policy(
        &read.clone().unwrap_or_default(),
        &write.clone().unwrap_or_default(),
        &app.clone().unwrap_or_default(),
        &input.config.clone().unwrap_or_default(),
    )?;
    // The same grant `space.createSpace` needs: the space's DID does not exist
    // yet, so only an `authority=*` grant can cover creating it.
    crate::spaces::scope::require_space_scope_at(
        &state,
        claims,
        &input.type_nsid,
        "*",
        &input.skey,
        happyview_scopes::SpaceTarget::Manage(happyview_scopes::ManageOp::Create),
    )
    .await?;
    let space = service::create_space(
        &state,
        claims.did(),
        &input.type_nsid,
        &input.skey,
        input.display_name,
        input.description,
        read,
        write,
        app,
        input.config,
        input.rotation_key.as_deref(),
    )
    .await?;
    let body = serde_json::json!({ "uri": crate::spaces::lifecycle::space_uri(&space) });
    let mut response = Json(with_warnings(body, warnings)).into_response();
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

async fn update_space(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(mut input): Json<UpdateSpaceInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let current = resolve_space(&state, &input.space).await?;
    crate::spaces::lifecycle::require_owner(
        &state,
        claims,
        &current,
        happyview_scopes::ManageOp::Update,
    )
    .await?;
    let legacy = legacy_policy(
        input.policy,
        input.mint_policy,
        input.managing_app,
        input.managing_app_did,
    )?;
    let read = parse_policy(input.read_policy, "readPolicy")?.or_else(|| legacy.clone());
    let write = parse_policy(input.write_policy, "writePolicy")?.or(legacy);
    let app = parse_app_access(input.app_access)?;
    let warnings = drop_allowed_collections(&mut input.config);
    require_a_policy(
        read.as_ref().unwrap_or(&current.read_policy),
        write.as_ref().unwrap_or(&current.write_policy),
        app.as_ref().unwrap_or(&current.app_access),
        input.config.as_ref().unwrap_or(&current.config),
    )?;
    let space = service::update_space(
        &state,
        claims.did(),
        &crate::spaces::lifecycle::space_uri(&current),
        input.display_name,
        input.description,
        read,
        write,
        app,
        input.config,
    )
    .await?;
    let body = serde_json::json!({
        "uri": crate::spaces::lifecycle::space_uri(&space),
        "space": space,
    });
    Ok(Json(with_warnings(body, warnings)))
}

async fn delete_space(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<SpaceUriQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let space = resolve_space(&state, &input.space).await?;
    crate::spaces::lifecycle::require_owner(
        &state,
        claims,
        &space,
        happyview_scopes::ManageOp::Delete,
    )
    .await?;
    service::delete_space(
        &state,
        claims.did(),
        &crate::spaces::lifecycle::space_uri(&space),
    )
    .await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

/// `simplespace.getSpace`: the space and its rules as simplespace described
/// them, to its owner or to a syncer holding a credential for it.
async fn get_space(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Query(query): Query<SpaceUriQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let space = resolve_space(&state, &query.space).await?;
    crate::spaces::lifecycle::authorize_reader(&state, &xrpc_claims, &space).await?;
    let config = serde_json::json!({
        "$type": "com.atproto.simplespace.defs#spaceConfig",
        "readPolicy": space.read_policy,
        "writePolicy": space.write_policy,
        "appAccess": space.app_access,
    });
    Ok(Json(serde_json::json!({
        "uri": crate::spaces::lifecycle::space_uri(&space),
        "space": space,
        "config": config,
    })))
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
    let space = canonical_ref(&state, &input.space).await?;
    let member = service::put_member(
        &state,
        claims.did(),
        &space,
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
    // Own-records-only access has no form in a member list, which has only
    // `read` and `write`. Refused, not narrowed to no read access, which would
    // quietly take access away (HAP-388 H5).
    if access.is_some_and(|a| a.restricted_to_own_records()) {
        return Err(AppError::XrpcError {
            status: StatusCode::BAD_REQUEST,
            code: "InvalidRequest",
            message: "access 'read_self' is no longer supported: a member list grants read \
                      and write only"
                .into(),
        });
    }
    let space = canonical_ref(&state, &input.space).await?;
    let member = service::add_member(
        &state,
        claims.did(),
        &space,
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
    let space = canonical_ref(&state, &input.space).await?;
    service::remove_member(&state, claims.did(), &space, &input.did).await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

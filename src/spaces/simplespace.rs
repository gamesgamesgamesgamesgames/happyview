use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use crate::auth::XrpcClaims;
use crate::db::now_rfc3339;
use crate::error::AppError;
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
    pub mint_policy: Option<MintPolicy>,
    pub app_access: Option<AppAccess>,
    pub managing_app_did: Option<String>,
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
    pub mint_policy: Option<MintPolicy>,
    pub app_access: Option<AppAccess>,
    pub managing_app_did: Option<Option<String>>,
    pub config: Option<SpaceConfig>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AddMemberInput {
    pub space: String,
    pub did: String,
    pub access: Option<SpaceAccess>,
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
pub(crate) struct UpdateConfigInput {
    pub space: String,
    pub mint_policy: Option<MintPolicy>,
    pub app_access: Option<AppAccess>,
    pub managing_app: Option<Option<String>>,
}

// ---------------------------------------------------------------------------
// Route registration
// ---------------------------------------------------------------------------

const NS: &str = "com.atproto";
const LEGACY_NS: &str = "dev.happyview";

pub fn simplespace_routes() -> Router<AppState> {
    Router::new()
        // Management routes (com.atproto.simplespace.*)
        .route(
            &format!("/xrpc/{NS}.simplespace.createSpace"),
            post(create_space),
        )
        .route(
            &format!("/xrpc/{NS}.simplespace.updateSpace"),
            post(update_space),
        )
        .route(
            &format!("/xrpc/{NS}.simplespace.deleteSpace"),
            post(delete_space),
        )
        .route(
            &format!("/xrpc/{NS}.simplespace.addMember"),
            post(add_member),
        )
        .route(
            &format!("/xrpc/{NS}.simplespace.removeMember"),
            post(remove_member),
        )
        .route(
            &format!("/xrpc/{NS}.simplespace.listMembers"),
            get(list_members),
        )
        .route(
            &format!("/xrpc/{NS}.simplespace.getConfig"),
            get(get_config),
        )
        .route(
            &format!("/xrpc/{NS}.simplespace.updateConfig"),
            post(update_config),
        )
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
            &format!("/xrpc/{LEGACY_NS}.space.addMember"),
            post(add_member),
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
    use crate::db::adapt_sql;
    if space.authority_did == did {
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

// ---------------------------------------------------------------------------
// Space management handlers
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

    // Optionally resolve the space type declaration from the lexicon registry.
    // If the type NSID maps to a stored space declaration, use its collections
    // as the default `allowed_collections` in the space config.
    let mut config = input.config.unwrap_or_default();
    if let Some(decl) = state.lexicons.get_space_declaration(&input.type_nsid).await
        && let Some(collections) = decl.space_collections
        && !collections.is_empty()
        && !config.extra.contains_key("allowedCollections")
    {
        config.extra.insert(
            "allowedCollections".to_string(),
            serde_json::Value::Array(
                collections
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
    }

    let space = Space {
        id: Uuid::new_v4().to_string(),
        did: did.clone(),
        authority_did: did.clone(),
        creator_did: did.clone(),
        type_nsid: input.type_nsid,
        skey: input.skey,
        display_name: input.display_name,
        description: input.description,
        mint_policy: input.mint_policy.unwrap_or(MintPolicy::MemberList),
        app_access: input.app_access.unwrap_or_default(),
        managing_app_did: input.managing_app_did,
        config,
        revision: None,
        created_at: now_rfc3339(),
        updated_at: now_rfc3339(),
    };

    db::create_space(&state.db, state.db_backend, &space).await?;

    // Auto-provision #atproto_space verification method if TOKEN_ENCRYPTION_KEY is available
    if let Some(encryption_key) = &state.config.token_encryption_key
        && let Err(e) = crate::verification_methods::ensure_atproto_space_method(
            &state.db,
            state.db_backend,
            encryption_key,
        )
        .await
    {
        tracing::warn!("failed to auto-provision #atproto_space verification method: {e}");
    }

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
    if let Some(policy) = input.mint_policy {
        space.mint_policy = policy;
    }
    if let Some(access) = input.app_access {
        space.app_access = access;
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

async fn list_members(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Query(query): Query<SpaceUriQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let space = resolve_space(&state, &query.space).await?;

    if !space.config.membership_public {
        let did = require_auth_or_credential(&state, &xrpc_claims).await?;
        let member = members::is_member(&state.db, state.db_backend, &space.id, &did).await?;
        member.ok_or_else(|| AppError::Forbidden("You are not a member of this space".into()))?;
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

async fn get_config(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Query(query): Query<SpaceUriQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let space = resolve_space(&state, &query.space).await?;
    let claims = require_auth(&xrpc_claims)?;
    require_space_admin(&state, &space, claims.did()).await?;

    Ok(Json(serde_json::json!({
        "$type": "com.atproto.simplespace.defs#spaceConfig",
        "mintPolicy": space.mint_policy,
        "appAccess": space.app_access,
        "managingApp": space.managing_app_did,
    })))
}

async fn update_config(
    State(state): State<AppState>,
    xrpc_claims: XrpcClaims,
    Json(input): Json<UpdateConfigInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = require_auth(&xrpc_claims)?;
    let mut space = resolve_space(&state, &input.space).await?;
    require_space_admin(&state, &space, claims.did()).await?;

    if let Some(policy) = input.mint_policy {
        space.mint_policy = policy;
    }
    if let Some(access) = input.app_access {
        space.app_access = access;
    }
    if let Some(managing_app) = input.managing_app {
        space.managing_app_did = managing_app;
    }

    db::update_space(&state.db, state.db_backend, &space).await?;

    Ok(Json(serde_json::json!({
        "$type": "com.atproto.simplespace.defs#spaceConfig",
        "mintPolicy": space.mint_policy,
        "appAccess": space.app_access,
        "managingApp": space.managing_app_did,
    })))
}

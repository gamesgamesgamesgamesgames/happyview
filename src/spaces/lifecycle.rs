//! `com.atproto.space.{createSpace, updateSpace, getSpace, deleteSpace}`: a
//! space's lifecycle, configured by one `policy` (see [`crate::spaces::policy`]).
//!
//! The protocol names the owner of a space as its authority's account. Here a
//! space lives on a DID of its own that no one signs in as, so its creator
//! stands in as owner: the creator manages the space, under the `manage=` part
//! of their session's `space:` grant.

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{MethodRouter, get, post};
use happyview_scopes::{ManageOp, SpaceTarget};
use serde::Deserialize;

use crate::AppState;
use crate::auth::XrpcClaims;
use crate::error::AppError;
use crate::spaces::policy::{MANAGEDSPACE, SpacePolicy};
use crate::spaces::types::{Space, SpaceConfig};
use crate::spaces::{db, scope, service};

pub(crate) fn lifecycle_method_table() -> Vec<(String, MethodRouter<AppState>)> {
    vec![
        ("com.atproto.space.createSpace".into(), post(create_space)),
        ("com.atproto.space.updateSpace".into(), post(update_space)),
        ("com.atproto.space.getSpace".into(), get(get_space)),
        ("com.atproto.space.deleteSpace".into(), post(delete_space)),
    ]
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSpaceInput {
    space_type: String,
    /// A TID when omitted, as the lexicon specifies.
    skey: Option<String>,
    policy: serde_json::Value,
    /// A did:key the creator holds, listed as a rotation key of the space's
    /// DID ahead of this instance's, so the creator can move the space.
    rotation_key: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateSpaceInput {
    space: String,
    policy: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpaceInput {
    space: String,
}

fn require_identity(claims: &XrpcClaims) -> Result<&crate::auth::Claims, AppError> {
    claims
        .identity
        .as_ref()
        .ok_or_else(|| AppError::Auth("This endpoint requires authentication".into()))
}

fn space_uri(space: &Space) -> String {
    format!(
        "at://{}/space/{}/{}",
        space.did, space.type_nsid, space.skey
    )
}

async fn resolve(state: &AppState, space: &str) -> Result<Space, AppError> {
    service::resolve_space(state, space)
        .await
        .map_err(|e| match e {
            AppError::NotFound(_) => AppError::XrpcError {
                status: StatusCode::BAD_REQUEST,
                code: "SpaceNotFound",
                message: "space not found".into(),
            },
            e => e,
        })
}

/// The caller owns `space`, and their session may `op` it.
async fn require_owner(
    state: &AppState,
    claims: &crate::auth::Claims,
    space: &Space,
    op: ManageOp,
) -> Result<(), AppError> {
    service::require_space_admin(state, space, claims.did())
        .await
        .map_err(|e| match e {
            AppError::Forbidden(message) => AppError::XrpcError {
                status: StatusCode::FORBIDDEN,
                code: "NotSpaceOwner",
                message,
            },
            e => e,
        })?;
    scope::require_space_scope(state, claims, space, SpaceTarget::Manage(op)).await
}

async fn create_space(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Json(input): Json<CreateSpaceInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = require_identity(&claims)?;
    let policy = SpacePolicy::parse(input.policy)?;
    let skey = input.skey.unwrap_or_else(|| {
        happyview_plugin_sdk::tid::tid_from_unix_microseconds(chrono::Utc::now().timestamp_micros())
    });
    // The space's DID does not exist yet, so only an `authority=*` grant can
    // cover creating it.
    scope::require_space_scope_at(
        &state,
        claims,
        &input.space_type,
        "*",
        &skey,
        SpaceTarget::Manage(ManageOp::Create),
    )
    .await?;

    let stored = policy.to_stored();
    let mut config = SpaceConfig::default();
    if stored.publish_policy {
        config.extra.insert("publishPolicy".into(), true.into());
    }
    if stored.publish_members {
        config.extra.insert("publishMembers".into(), true.into());
    }
    let space = service::create_space(
        &state,
        claims.did(),
        &input.space_type,
        &skey,
        None,
        None,
        Some(stored.read_policy),
        Some(stored.write_policy),
        Some(stored.app_access),
        Some(config),
        input.rotation_key.as_deref(),
    )
    .await
    .map_err(|e| match e {
        AppError::Conflict(message) => AppError::XrpcError {
            status: StatusCode::CONFLICT,
            code: "SpaceAlreadyExists",
            message,
        },
        e => e,
    })?;
    Ok(Json(serde_json::json!({ "uri": space_uri(&space) })))
}

/// Replace a space's policy wholesale. A policy of another `$type` migrates
/// the space, and the member list, which only `memberspace` keeps, is
/// discarded: migrating back does not restore it.
async fn update_space(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Json(input): Json<UpdateSpaceInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = require_identity(&claims)?;
    let mut space = resolve(&state, &input.space).await?;
    require_owner(&state, claims, &space, ManageOp::Update).await?;
    let policy = SpacePolicy::parse(input.policy)?;

    // Access rules from before policies existed match no policy; replacing
    // them keeps the member list unless the new policy has none.
    let migrating = match SpacePolicy::of(&space) {
        Ok(current) => current.kind() != policy.kind(),
        Err(_) => policy.kind() == MANAGEDSPACE,
    };
    policy.apply(&mut space);
    // One transaction, so a failed removal cannot leave a managedspace with
    // the member list of the memberspace it replaced.
    let mut tx = crate::db::begin_write(&state.db, state.db_backend)
        .await
        .map_err(|e| AppError::Internal(format!("failed to begin transaction: {e}")))?;
    db::update_space(&mut *tx, state.db_backend, &space).await?;
    if migrating {
        db::remove_all_members(&mut *tx, state.db_backend, &space.id).await?;
    }
    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("failed to commit space update: {e}")))?;
    Ok(Json(serde_json::json!({})))
}

/// Describe a space and its policy, to its owner, or to a syncer holding a
/// credential for it.
async fn get_space(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Query(query): Query<SpaceInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let space = resolve(&state, &query.space).await?;
    let owner = match &claims.identity {
        Some(identity) => {
            match service::require_space_admin(&state, &space, identity.did()).await {
                Ok(()) => true,
                Err(AppError::Forbidden(_)) => false,
                Err(e) => return Err(e),
            }
        }
        None => false,
    };
    if !owner {
        match &claims.space_credential {
            Some(token) => {
                if claims.space_audience.as_deref() != Some(space.authority_did.as_str()) {
                    return Err(AppError::XrpcError {
                        status: StatusCode::UNAUTHORIZED,
                        code: "BadSpaceAudience",
                        message: format!("request is not addressed to {}", space.authority_did),
                    });
                }
                service::verify_space_credential(&state, &space, token).await?;
            }
            None => {
                require_identity(&claims)?;
                // Not disclosed to someone who may not read it.
                return Err(AppError::XrpcError {
                    status: StatusCode::BAD_REQUEST,
                    code: "SpaceNotFound",
                    message: "space not found".into(),
                });
            }
        }
    }
    let policy = SpacePolicy::of(&space)?;
    Ok(Json(serde_json::json!({
        "uri": space_uri(&space),
        "policy": policy,
    })))
}

async fn delete_space(
    State(state): State<AppState>,
    claims: XrpcClaims,
    Json(input): Json<SpaceInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let claims = require_identity(&claims)?;
    let space = resolve(&state, &input.space).await?;
    require_owner(&state, claims, &space, ManageOp::Delete).await?;
    service::delete_space(&state, claims.did(), &input.space).await?;
    Ok(Json(serde_json::json!({})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spaces::scope::space_scope_gate_tests::session_with_scopes;

    async fn state_with_plc() -> (AppState, wiremock::MockServer) {
        let mut state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
        let plc = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .mount(&plc)
            .await;
        state.config.plc_url = plc.uri();
        (state, plc)
    }

    fn as_session(claims: crate::auth::Claims) -> XrpcClaims {
        XrpcClaims {
            identity: Some(claims),
            space_credential: None,
            space_audience: None,
            service_auth: None,
        }
    }

    async fn create(state: &AppState, scopes: &str) -> Result<String, AppError> {
        let claims = session_with_scopes(state, scopes).await;
        create_space(
            State(state.clone()),
            as_session(claims),
            Json(CreateSpaceInput {
                space_type: "com.example.forum".into(),
                skey: Some("main".into()),
                policy: serde_json::json!({ "$type": "com.atproto.memberspace.policy" }),
                rotation_key: None,
            }),
        )
        .await
        .map(|Json(body)| body["uri"].as_str().unwrap().to_string())
    }

    /// A space's DID does not exist until it is created, so creating one
    /// needs `manage=create` under `authority=*`.
    #[tokio::test]
    async fn creating_a_space_needs_manage_create_for_any_authority() {
        let (state, _plc) = state_with_plc().await;
        for scopes in [
            "space:com.example.forum?authority=*",
            "space:com.example.forum?manage=create",
            "space:com.example.other?authority=*&manage=create",
        ] {
            let err = create(&state, scopes).await.unwrap_err();
            assert!(matches!(err, AppError::Forbidden(_)), "{scopes}: {err:?}");
        }
        create(&state, "space:com.example.forum?authority=*&manage=create")
            .await
            .expect("covered");
    }

    #[tokio::test]
    async fn updating_and_deleting_need_their_own_manage_grants() {
        let (state, _plc) = state_with_plc().await;
        let space = create(&state, "space:com.example.forum?authority=*&manage=create")
            .await
            .unwrap();
        let session = |scopes: &'static str| {
            let state = state.clone();
            async move { as_session(session_with_scopes(&state, scopes).await) }
        };
        let update = |claims: XrpcClaims| {
            update_space(
                State(state.clone()),
                claims,
                Json(UpdateSpaceInput {
                    space: space.clone(),
                    policy: serde_json::json!({ "$type": "com.atproto.memberspace.policy" }),
                }),
            )
        };
        let err = update(session("space:com.example.forum?authority=*&manage=delete").await)
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");
        let _ = update(session("space:com.example.forum?authority=*&manage=update").await)
            .await
            .expect("covered");

        let delete = |claims: XrpcClaims| {
            delete_space(
                State(state.clone()),
                claims,
                Json(SpaceInput {
                    space: space.clone(),
                }),
            )
        };
        let err = delete(session("space:com.example.forum?authority=*&manage=update").await)
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");
        let _ = delete(session("space:com.example.forum?authority=*&manage=delete").await)
            .await
            .expect("covered");
    }
}

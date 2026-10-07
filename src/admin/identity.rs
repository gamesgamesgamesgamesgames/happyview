use axum::Json;
use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::error::AppError;

use super::auth::UserAuth;

#[derive(Deserialize)]
pub(super) struct ResolveQuery {
    identifier: String,
    /// Also fetch the display name and avatar from the account's profile.
    #[serde(default)]
    profile: bool,
}

#[derive(Serialize)]
pub(super) struct ResolveResponse {
    did: String,
    handle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    avatar: Option<String>,
}

/// GET /admin/identity/resolve — resolve a handle or DID for display, with
/// the handle confirmed in both directions. `profile=true` adds the display
/// name and avatar from the account's `app.bsky.actor.profile` record.
///
/// Any signed-in dashboard user may call this: it reveals nothing beyond
/// public DNS, DID documents and profile records, and every form that accepts
/// an account uses it.
pub(super) async fn resolve_identity(
    State(state): State<AppState>,
    _auth: UserAuth,
    Query(query): Query<ResolveQuery>,
) -> Result<Json<ResolveResponse>, AppError> {
    let verified =
        crate::identity::resolve_verified(&state.http, &state.config.plc_url, &query.identifier)
            .await?;
    let (display_name, avatar) = if query.profile {
        crate::profile::resolve_display_profile(&state.http, &state.config.plc_url, &verified.did)
            .await
    } else {
        (None, None)
    };
    Ok(Json(ResolveResponse {
        did: verified.did,
        handle: verified.handle,
        display_name,
        avatar,
    }))
}

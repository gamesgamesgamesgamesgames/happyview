use axum::Json;
use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::error::AppError;

use super::auth::UserAuth;

#[derive(Deserialize)]
pub(super) struct ResolveQuery {
    identifier: String,
}

#[derive(Serialize)]
pub(super) struct ResolveResponse {
    did: String,
    handle: Option<String>,
}

/// GET /admin/identity/resolve — resolve a handle or DID for display, with
/// the handle confirmed in both directions.
///
/// Any signed-in dashboard user may call this: it reveals nothing beyond
/// public DNS and DID documents, and every form that accepts an account uses
/// it.
pub(super) async fn resolve_identity(
    State(state): State<AppState>,
    _auth: UserAuth,
    Query(query): Query<ResolveQuery>,
) -> Result<Json<ResolveResponse>, AppError> {
    let verified =
        crate::identity::resolve_verified(&state.http, &state.config.plc_url, &query.identifier)
            .await?;
    Ok(Json(ResolveResponse {
        did: verified.did,
        handle: verified.handle,
    }))
}

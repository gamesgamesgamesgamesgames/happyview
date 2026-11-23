//! `GET /admin/update-status`: whether a newer HappyView is available.
//!
//! Any signed-in dashboard user may read it. The running version is already
//! public on `/config`, and every user of the dashboard should see that the
//! instance is behind, whoever can act on it.

use axum::Json;
use axum::extract::State;

use super::auth::UserAuth;
use crate::AppState;
use crate::update_check::UpdateStatus;

pub(super) async fn get(State(state): State<AppState>, _auth: UserAuth) -> Json<UpdateStatus> {
    Json(state.update_status.read().await.clone())
}

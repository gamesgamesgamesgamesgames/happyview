//! `/admin/jetstream`: what the live Jetstream consumer is doing.

use axum::Json;

use crate::error::AppError;
use crate::jetstream::status::JetstreamStatus;

use super::auth::AllowPlatform;
use super::permissions::Permission;

/// `GET /admin/jetstream/status`
pub(super) async fn status(
    AllowPlatform(auth): AllowPlatform,
) -> Result<Json<JetstreamStatus>, AppError> {
    auth.require(Permission::StatsRead).await?;
    Ok(Json(crate::jetstream::status::snapshot()))
}

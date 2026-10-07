//! Moderator access to space contents.
//!
//! Reading a private space is opt-in per instance, and each read happens under
//! a grant: a moderator names one space or one account, says why, and gets
//! access for a limited time. Grants and the reads made under them are
//! protected events (see `event_log::PROTECTED_EVENT_TYPES`).

use axum::Json;
use axum::extract::State;
use sqlx::AnyPool;

use crate::AppState;
use crate::admin::settings::get_setting;
use crate::db::DatabaseBackend;
use crate::error::AppError;
use crate::feature_flags::{self, FeatureFlag};

use super::auth::UserAuth;
use super::permissions::Permission;

pub const MAX_GRANT_SETTING: &str = "space_inspector_max_grant_minutes";
pub const DEFAULT_GRANT_MINUTES: i64 = 60;
pub const MIN_GRANT_MINUTES: i64 = 5;

pub struct InspectorConfig {
    pub enabled: bool,
    pub max_grant_minutes: i64,
}

/// The instance's maximum grant length. Unparseable values fall back to the
/// default; anything below the minimum is raised to it.
pub fn parse_max_minutes(raw: Option<&str>) -> i64 {
    raw.and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(DEFAULT_GRANT_MINUTES)
        .max(MIN_GRANT_MINUTES)
}

pub async fn load_config(pool: &AnyPool, backend: DatabaseBackend) -> InspectorConfig {
    InspectorConfig {
        enabled: feature_flags::is_enabled(pool, FeatureFlag::SPACE_INSPECTOR, backend).await,
        max_grant_minutes: parse_max_minutes(
            get_setting(pool, MAX_GRANT_SETTING, backend)
                .await
                .as_deref(),
        ),
    }
}

/// GET /admin/spaces/inspector — whether moderators can request access, and
/// for how long.
pub(super) async fn inspector_status(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<serde_json::Value>, AppError> {
    auth.require(Permission::SpacesRead).await?;
    let config = load_config(&state.db, state.db_backend).await;
    Ok(Json(serde_json::json!({
        "enabled": config.enabled,
        "default_grant_minutes": DEFAULT_GRANT_MINUTES.min(config.max_grant_minutes),
        "max_grant_minutes": config.max_grant_minutes,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_minutes_parsing() {
        assert_eq!(parse_max_minutes(None), 60);
        assert_eq!(parse_max_minutes(Some("abc")), 60);
        assert_eq!(parse_max_minutes(Some("0")), 5);
        assert_eq!(parse_max_minutes(Some("-10")), 5);
        assert_eq!(parse_max_minutes(Some(" 90 ")), 90);
        assert_eq!(parse_max_minutes(Some("100000")), 100000);
    }
}

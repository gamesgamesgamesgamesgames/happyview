use sqlx::AnyPool;

use crate::admin::settings::get_setting;
use crate::db::DatabaseBackend;

pub struct FeatureFlag;

impl FeatureFlag {
    pub const SPACES_ENABLED: &str = "feature.spaces_enabled";
    /// Opt-in, separate from `SPACES_ENABLED`: moving a user's data to their
    /// PDS is a bigger step than serving spaces, so operators enable it
    /// explicitly.
    pub const SPACES_PDS_MIGRATION: &str = "feature.spaces_pds_migration";
}

pub async fn is_enabled(pool: &AnyPool, key: &str, backend: DatabaseBackend) -> bool {
    get_setting(pool, key, backend)
        .await
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[derive(serde::Serialize)]
pub struct FeatureFlagStatus {
    pub key: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
}

pub async fn list_flags(pool: &AnyPool, backend: DatabaseBackend) -> Vec<FeatureFlagStatus> {
    let all_flags = [(
        FeatureFlag::SPACES_ENABLED,
        "Permissioned Spaces",
        "Collaborative data spaces with granular permissions, membership, and invites.",
    )];

    let mut result = Vec::new();
    for (key, name, description) in all_flags {
        let enabled = is_enabled(pool, key, backend).await;
        result.push(FeatureFlagStatus {
            key: key.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            enabled,
        });
    }
    result
}

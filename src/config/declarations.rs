//! What the configuration declares that has to exist as rows before any
//! worker or request reads the tables involved. Every step here is
//! idempotent and runs on every boot; failures are logged, not fatal,
//! because an unreachable network or a database hiccup at boot should not
//! keep the rest of the instance down.

use sqlx::AnyPool;
use tracing::{error, info};

use super::Config;
use crate::admin::auth::{SuperadminSeed, ensure_configured_superadmin};
use crate::catalog::LexiconCatalog;
use crate::db::DatabaseBackend;

pub async fn apply_at_boot(
    config: &Config,
    db: &AnyPool,
    backend: DatabaseBackend,
    http: &reqwest::Client,
    lexicons: &LexiconCatalog,
) {
    if let Some(did) = config.superadmin_did.as_deref() {
        match ensure_configured_superadmin(db, backend, did).await {
            Ok(SuperadminSeed::Created) => info!(did, "created the configured superadmin"),
            Ok(SuperadminSeed::Promoted) => info!(did, "promoted the configured superadmin"),
            Ok(SuperadminSeed::Unchanged) => {}
            Err(e) => error!(
                did,
                "could not seed the configured superadmin: {e}; no one can be bootstrapped as admin until seeding succeeds, restart to retry"
            ),
        }
    }

    let Some(project) = config.project.as_deref() else {
        return;
    };

    match crate::labeler::ensure_declared_subscriptions(db, backend, &project.labels.subscribe)
        .await
    {
        Ok(0) => {}
        Ok(added) => info!(added, "subscribed to labelers declared in happyview.toml"),
        Err(e) => error!("could not subscribe to the labelers declared in happyview.toml: {e}"),
    }

    let _ = (http, lexicons);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DatabaseBackend;
    use crate::test_support::migrated_memory_pool;

    /// The test state's `Config`, which is a complete literal; the state's
    /// own pool is not used.
    async fn config_with(superadmin: Option<&str>) -> Config {
        let mut config = crate::test_support::test_state_with_pool_on(
            crate::test_support::memory_pool().await,
            DatabaseBackend::Sqlite,
        )
        .config;
        config.superadmin_did = superadmin.map(str::to_string);
        config
    }

    #[tokio::test]
    async fn boot_subscribes_the_declared_labelers() {
        let db = migrated_memory_pool().await;
        let mut config = config_with(None).await;
        config.project = Some(std::sync::Arc::new(
            crate::config::project::parse_project_config(
                "[labels]\nsubscribe = [\"did:plc:declared\"]\n",
                std::path::Path::new("happyview.toml"),
            )
            .unwrap(),
        ));
        apply_at_boot(
            &config,
            &db,
            DatabaseBackend::Sqlite,
            &reqwest::Client::new(),
            &crate::catalog::LexiconCatalog::detached(),
        )
        .await;
        let row: Option<(String,)> =
            crate::db::query_as("SELECT did FROM happyview_labeler_subscriptions WHERE did = ?")
                .bind("did:plc:declared")
                .fetch_optional(&db)
                .await
                .unwrap();
        assert!(row.is_some());
    }

    #[tokio::test]
    async fn boot_seeds_the_configured_superadmin() {
        let db = migrated_memory_pool().await;
        let config = config_with(Some("did:plc:boss")).await;
        apply_at_boot(
            &config,
            &db,
            DatabaseBackend::Sqlite,
            &reqwest::Client::new(),
            &crate::catalog::LexiconCatalog::detached(),
        )
        .await;
        let row: Option<(i32,)> =
            crate::db::query_as("SELECT is_super FROM happyview_users WHERE did = ?")
                .bind("did:plc:boss")
                .fetch_optional(&db)
                .await
                .unwrap();
        assert_eq!(row, Some((1,)));
    }
}

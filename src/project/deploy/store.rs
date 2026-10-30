//! Where a deployed bundle is kept between boots.

use sqlx::AnyPool;

use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};

/// How many deployed bundles are kept. Only the newest is ever served; the
/// rest are there to recover by hand, and each can be 50 MiB.
const KEEP: i64 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deployment {
    pub sha256: String,
    pub deployed_at: String,
    pub deployed_by: String,
}

/// Record `bundle` as the deployed project and answer when it was deployed.
pub async fn save(
    db: &AnyPool,
    backend: DatabaseBackend,
    sha256: &str,
    bundle: &[u8],
    deployed_by: &str,
) -> Result<String, sqlx::Error> {
    let deployed_at = now_rfc3339();
    crate::db::query(&adapt_sql(
        "INSERT INTO happyview_project_deployments (sha256, bundle, deployed_at, deployed_by)
         VALUES (?, ?, ?, ?)",
        backend,
    ))
    .bind(sha256)
    .bind(bundle)
    .bind(&deployed_at)
    .bind(deployed_by)
    .execute(db)
    .await?;
    crate::db::query(&adapt_sql(
        "DELETE FROM happyview_project_deployments WHERE id NOT IN
         (SELECT id FROM happyview_project_deployments ORDER BY id DESC LIMIT ?)",
        backend,
    ))
    .bind(KEEP)
    .execute(db)
    .await?;
    Ok(deployed_at)
}

/// The project being served, if one was ever deployed.
pub async fn current(
    db: &AnyPool,
    backend: DatabaseBackend,
) -> Result<Option<Deployment>, sqlx::Error> {
    let row: Option<(String, String, String)> = crate::db::query_as(&adapt_sql(
        "SELECT sha256, deployed_at, deployed_by FROM happyview_project_deployments
         ORDER BY id DESC LIMIT 1",
        backend,
    ))
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(sha256, deployed_at, deployed_by)| Deployment {
        sha256,
        deployed_at,
        deployed_by,
    }))
}

/// The current deployment with its archive, for a boot to unpack.
pub async fn current_bundle(
    db: &AnyPool,
    backend: DatabaseBackend,
) -> Result<Option<(Deployment, Vec<u8>)>, sqlx::Error> {
    let row: Option<(String, String, String, Vec<u8>)> = crate::db::query_as(&adapt_sql(
        "SELECT sha256, deployed_at, deployed_by, bundle FROM happyview_project_deployments
         ORDER BY id DESC LIMIT 1",
        backend,
    ))
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(sha256, deployed_at, deployed_by, bundle)| {
        (
            Deployment {
                sha256,
                deployed_at,
                deployed_by,
            },
            bundle,
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::migrated_memory_pool;

    const SQLITE: DatabaseBackend = DatabaseBackend::Sqlite;

    #[tokio::test]
    async fn nothing_is_current_until_something_is_deployed() {
        let db = migrated_memory_pool().await;
        assert_eq!(current(&db, SQLITE).await.unwrap(), None);
        assert!(current_bundle(&db, SQLITE).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn the_newest_deployment_is_current() {
        let db = migrated_memory_pool().await;
        save(&db, SQLITE, "aaa", b"first", "did:plc:one")
            .await
            .unwrap();
        let at = save(&db, SQLITE, "bbb", b"second", "did:plc:two")
            .await
            .unwrap();

        let (deployment, bundle) = current_bundle(&db, SQLITE).await.unwrap().unwrap();
        assert_eq!(
            deployment,
            Deployment {
                sha256: "bbb".into(),
                deployed_at: at,
                deployed_by: "did:plc:two".into(),
            }
        );
        assert_eq!(bundle, b"second");
        assert_eq!(current(&db, SQLITE).await.unwrap(), Some(deployment));
    }

    #[tokio::test]
    async fn only_the_newest_few_are_kept() {
        let db = migrated_memory_pool().await;
        for i in 0..(KEEP + 2) {
            save(&db, SQLITE, &i.to_string(), b"x", "did:plc:one")
                .await
                .unwrap();
        }
        let (n,): (i64,) =
            crate::db::query_as("SELECT COUNT(*) FROM happyview_project_deployments")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(n, KEEP);
        assert_eq!(
            current(&db, SQLITE).await.unwrap().unwrap().sha256,
            (KEEP + 1).to_string()
        );
    }
}

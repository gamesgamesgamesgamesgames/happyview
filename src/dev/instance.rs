//! What the setup wizard would otherwise ask a local instance.

use sqlx::AnyPool;

use crate::db::DatabaseBackend;
use crate::error::AppError;
use crate::service_identity::{self, IdentityMode};

pub async fn seed(db: &AnyPool, backend: DatabaseBackend) -> Result<(), AppError> {
    let status = service_identity::get_setup_status(db, backend).await?;
    // An identity already completed, by an earlier boot or by the developer
    // through the wizard, is theirs to keep.
    if status.setup_complete {
        return Ok(());
    }
    // So is one the wizard started and did not finish: replacing it would
    // discard whatever the developer chose.
    if status.identity_mode.is_some() {
        tracing::info!("setup is unfinished; the dashboard's setup wizard can complete it");
        return Ok(());
    }
    service_identity::upsert_identity(
        db,
        backend,
        &IdentityMode::NotExposed,
        None,
        None,
        None,
        None,
    )
    .await?;
    // Written before setup is marked complete, so the early return above
    // implies the marker exists. A database created by this version has
    // nothing stranded to vacuum.
    crate::maintenance::vacuum::mark_not_needed(db, backend).await?;
    service_identity::mark_setup_complete(db, backend).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DatabaseBackend;
    use crate::service_identity::{IdentityMode, get_identity};
    use crate::test_support::migrated_memory_pool;

    #[tokio::test]
    async fn an_empty_instance_is_not_exposed_and_set_up() {
        let db = migrated_memory_pool().await;
        seed(&db, DatabaseBackend::Sqlite).await.unwrap();
        let identity = get_identity(&db, DatabaseBackend::Sqlite)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(identity.mode, IdentityMode::NotExposed);
        assert!(identity.setup_complete);
        let vacuum = crate::maintenance::vacuum::read_status(&db, DatabaseBackend::Sqlite)
            .await
            .unwrap();
        assert!(vacuum.completed_at.is_some());
    }

    /// A wizard the developer started and did not finish is left as it was.
    #[tokio::test]
    async fn an_unfinished_identity_is_left_alone() {
        let db = migrated_memory_pool().await;
        crate::service_identity::upsert_identity(
            &db,
            DatabaseBackend::Sqlite,
            &IdentityMode::AttachAccount,
            Some("did:plc:acct"),
            Some("enc-signing"),
            None,
            Some("did:plc:acct"),
        )
        .await
        .unwrap();

        seed(&db, DatabaseBackend::Sqlite).await.unwrap();

        let identity = get_identity(&db, DatabaseBackend::Sqlite)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(identity.mode, IdentityMode::AttachAccount);
        assert_eq!(identity.signing_key_enc.as_deref(), Some("enc-signing"));
        assert!(!identity.setup_complete);
    }

    /// An identity a developer completed through the wizard is theirs, and a
    /// second boot changes nothing.
    #[tokio::test]
    async fn a_completed_identity_is_kept() {
        let db = migrated_memory_pool().await;
        crate::service_identity::upsert_identity(
            &db,
            DatabaseBackend::Sqlite,
            &IdentityMode::AttachAccount,
            Some("did:plc:acct"),
            None,
            None,
            Some("did:plc:acct"),
        )
        .await
        .unwrap();
        crate::service_identity::mark_setup_complete(&db, DatabaseBackend::Sqlite)
            .await
            .unwrap();

        seed(&db, DatabaseBackend::Sqlite).await.unwrap();
        seed(&db, DatabaseBackend::Sqlite).await.unwrap();

        let identity = get_identity(&db, DatabaseBackend::Sqlite)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(identity.mode, IdentityMode::AttachAccount);
    }
}

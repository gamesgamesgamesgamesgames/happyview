//! A record source's own schema: its migrator, run after HappyView's own
//! migrations in a table of its own, `_happyview_source_<id>_migrations`, so
//! it never touches `_sqlx_migrations`.

use happyview_provider::RecordSource;
use sqlx::AnyPool;
use sqlx::migrate::{MigrateError, Migrator};

use crate::db::DatabaseBackend;

/// Run `source`'s migrator, if it has one, against `pool`.
///
/// Refuses a source whose id is not `[a-z0-9_]+`, because the id names the
/// tracking table.
pub async fn run(
    pool: &AnyPool,
    backend: DatabaseBackend,
    source: &dyn RecordSource,
) -> Result<(), MigrateError> {
    let id = source.id();
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(MigrateError::Source(
            format!("record source id {id:?} must match [a-z0-9_]+").into(),
        ));
    }
    let Some(base) = source.migrations(backend) else {
        return Ok(());
    };
    // `Migrator` is not `Clone`; share the source's migrations and change
    // only the tracking table.
    let migrator = Migrator {
        migrations: std::borrow::Cow::Borrowed(&*base.migrations),
        ignore_missing: base.ignore_missing,
        locking: base.locking,
        no_tx: base.no_tx,
        table_name: format!("_happyview_source_{id}_migrations").into(),
        create_schemas: std::borrow::Cow::Borrowed(&*base.create_schemas),
    };

    match backend {
        DatabaseBackend::Sqlite => migrator.run(pool).await,
        DatabaseBackend::Postgres => run_postgres(pool, &migrator, lock_key(id)).await,
    }
}

/// The advisory lock key for a source's migrations: HappyView's own key mixed
/// with an FNV-1a hash of the id, so sources do not wait on one another.
fn lock_key(id: &str) -> i64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in id.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (hash ^ 0x6876_736f_7572_6365) as i64
}

/// The pattern of `db::migrate_postgres`: take the lock with
/// `pg_try_advisory_lock` between short sleeps, so a waiting instance never
/// holds a statement open, then run the migrator on that connection.
async fn run_postgres(pool: &AnyPool, migrator: &Migrator, key: i64) -> Result<(), MigrateError> {
    let mut conn = pool.acquire().await?;
    loop {
        let sql = crate::db::adapt_sql("SELECT pg_try_advisory_lock(?)", DatabaseBackend::Postgres);
        let (locked,): (bool,) = crate::db::query_as(&sql)
            .bind(key)
            .fetch_one(&mut *conn)
            .await?;
        if locked {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    let outcome = migrator.run_direct(None, &mut *conn, false).await;

    let sql = crate::db::adapt_sql("SELECT pg_advisory_unlock(?)", DatabaseBackend::Postgres);
    let unlocked = crate::db::query(&sql).bind(key).execute(&mut *conn).await;
    outcome?;
    unlocked?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_pools;
    use sqlx::Row;
    use std::sync::Arc;

    static TEST_MIGRATOR: Migrator = sqlx::migrate!("./tests/fixtures/provider_migrations");

    /// A source that is nothing but an id and a migrator.
    struct Fake {
        id: &'static str,
        migrator: Option<&'static Migrator>,
    }

    #[async_trait::async_trait]
    impl RecordSource for Fake {
        fn id(&self) -> &'static str {
            self.id
        }
        fn migrations(&self, _: DatabaseBackend) -> Option<&'static Migrator> {
            self.migrator
        }
        async fn start(
            self: Arc<Self>,
            _: Arc<dyn happyview_provider::Host>,
        ) -> Result<(), happyview_provider::ProviderError> {
            Ok(())
        }
        fn admin_routes(&self) -> axum::Router {
            axum::Router::new()
        }
        async fn start_backfill(
            &self,
            _: happyview_provider::BackfillRequest,
        ) -> Result<Option<String>, happyview_provider::BackfillError> {
            Ok(None)
        }
        async fn has_backfill(&self, _: &str) -> bool {
            false
        }
        async fn backfill_running(&self, _: &str) -> bool {
            false
        }
        async fn status(&self) -> serde_json::Value {
            serde_json::Value::Null
        }
        async fn metrics(&self) -> Vec<(String, i64)> {
            Vec::new()
        }
    }

    async fn count(pool: &AnyPool, table: &str) -> i64 {
        sqlx::query(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
            .fetch_one(pool)
            .await
            .unwrap()
            .get::<i64, _>(0)
    }

    async fn applied(pool: &AnyPool) -> Vec<(i64, String)> {
        sqlx::query("SELECT version, description FROM _sqlx_migrations ORDER BY version")
            .fetch_all(pool)
            .await
            .unwrap()
            .iter()
            .map(|r| (r.get::<i64, _>(0), r.get::<String, _>(1)))
            .collect()
    }

    /// Drop the tracking table of source `id`. Each test uses an id of its
    /// own, so tests running in parallel on one Postgres database never touch
    /// each other's tracking tables. The fixture's own table is created with
    /// `IF NOT EXISTS` and never dropped, so it is safely shared.
    async fn clean(pool: &AnyPool, id: &str) {
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP TABLE IF EXISTS _happyview_source_{id}_migrations"
        )))
        .execute(pool)
        .await;
    }

    #[tokio::test]
    async fn a_source_migrator_uses_its_own_table() {
        for (pool, backend) in test_pools().await {
            clean(&pool, "test_own_table").await;
            let before = applied(&pool).await;
            let source = Fake {
                id: "test_own_table",
                migrator: Some(&TEST_MIGRATOR),
            };
            run(&pool, backend, &source).await.unwrap();
            assert_eq!(
                count(&pool, "_happyview_source_test_own_table_migrations").await,
                1,
                "{backend:?}"
            );
            assert_eq!(applied(&pool).await, before, "{backend:?}");
            clean(&pool, "test_own_table").await;
        }
    }

    #[tokio::test]
    async fn an_existing_database_upgrades_without_touching_sqlx_migrations() {
        for (pool, backend) in test_pools().await {
            clean(&pool, "test_upgrade").await;
            let snapshot = applied(&pool).await;
            assert!(!snapshot.is_empty());
            let source = Fake {
                id: "test_upgrade",
                migrator: Some(&TEST_MIGRATOR),
            };
            run(&pool, backend, &source).await.unwrap();
            run(&pool, backend, &source).await.unwrap();
            assert_eq!(applied(&pool).await, snapshot, "{backend:?}");
            assert_eq!(
                count(&pool, "_happyview_source_test_upgrade_migrations").await,
                1
            );
            clean(&pool, "test_upgrade").await;
        }
    }

    #[tokio::test]
    async fn no_migrator_is_a_no_op() {
        for (pool, backend) in test_pools().await {
            clean(&pool, "test_noop").await;
            let source = Fake {
                id: "test_noop",
                migrator: None,
            };
            run(&pool, backend, &source).await.unwrap();
            assert!(
                sqlx::query("SELECT COUNT(*) FROM _happyview_source_test_noop_migrations")
                    .fetch_one(&pool)
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn an_id_that_would_name_an_odd_table_is_refused() {
        let (pool, backend) = test_pools().await.remove(0);
        for id in ["", "Test", "a-b", "x; DROP TABLE records", "a b"] {
            let source = Fake {
                id,
                migrator: Some(&TEST_MIGRATOR),
            };
            assert!(run(&pool, backend, &source).await.is_err(), "{id:?}");
        }
    }

    #[test]
    fn lock_keys_differ_per_source_and_from_the_core_key() {
        assert_ne!(lock_key("a"), lock_key("b"));
        assert_ne!(lock_key("a"), 0x6876_6d69_6772_6174);
    }
}

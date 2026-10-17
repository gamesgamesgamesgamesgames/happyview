//! An external [`RecordStorage`] for tests: a second in-memory SQLite
//! database migrated with HappyView's own migrations, so its `read_route`
//! satisfies the relation contract and core's reads run on it unchanged.
//! It claims a fixed set of collections and records every call core makes.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use happyview::db::DatabaseBackend;
use happyview::record_store::{PreparedRecord, RecordStore};
use happyview_provider::{Delta, ReadRoute, RecordKey, RecordStorage};

pub struct FakeStorage {
    claimed: HashSet<String>,
    pool: sqlx::AnyPool,
    /// A pool whose database is closed: what `read_route` hands out while
    /// the storage is `down`.
    closed: sqlx::AnyPool,
    pub down: AtomicBool,
    pub deltas: Mutex<Vec<(RecordKey, Delta, Option<String>)>>,
    pub forgotten: Mutex<Vec<(String, Option<Vec<String>>)>>,
    pub collection_deletes: Mutex<Vec<(String, i64)>>,
    pub asked_live: Mutex<Vec<String>>,
}

impl FakeStorage {
    pub async fn claiming(collections: &[&str]) -> Self {
        let closed = happyview::test_support::memory_pool().await;
        closed.close().await;
        Self {
            claimed: collections.iter().map(|c| c.to_string()).collect(),
            pool: happyview::test_support::migrated_memory_pool().await,
            closed,
            down: AtomicBool::new(false),
            deltas: Mutex::new(Vec::new()),
            forgotten: Mutex::new(Vec::new()),
            collection_deletes: Mutex::new(Vec::new()),
            asked_live: Mutex::new(Vec::new()),
        }
    }

    /// The fake's own database, where its records live.
    pub fn pool(&self) -> &sqlx::AnyPool {
        &self.pool
    }

    /// Store records in the fake's own database, as the claiming store's
    /// feed would have.
    pub async fn seed(&self, records: &[PreparedRecord]) {
        RecordStore::new(self.pool.clone(), DatabaseBackend::Sqlite)
            .write_network(&self.pool, records)
            .await
            .expect("seed the fake storage");
    }

    pub fn deltas(&self) -> Vec<(RecordKey, Delta, Option<String>)> {
        self.deltas.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl RecordStorage for FakeStorage {
    fn claims(&self, collection: &str) -> bool {
        self.claimed.contains(collection)
    }

    fn read_route(&self, _collection: &str) -> ReadRoute {
        let pool = if self.down.load(Ordering::SeqCst) {
            self.closed.clone()
        } else {
            self.pool.clone()
        };
        ReadRoute {
            pool,
            backend: DatabaseBackend::Sqlite,
        }
    }

    async fn apply_delta(
        &self,
        key: &RecordKey,
        delta: Delta,
        for_rev: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        if self.down.load(Ordering::SeqCst) {
            return Err(sqlx::Error::PoolClosed);
        }
        self.deltas
            .lock()
            .unwrap()
            .push((key.clone(), delta, for_rev.map(str::to_string)));
        Ok(())
    }

    async fn forget_account(
        &self,
        did: &str,
        collections: Option<&[String]>,
    ) -> Result<u64, sqlx::Error> {
        self.forgotten
            .lock()
            .unwrap()
            .push((did.to_string(), collections.map(<[String]>::to_vec)));
        Ok(0)
    }

    async fn delete_collection_batch(
        &self,
        collection: &str,
        limit: i64,
    ) -> Result<u64, sqlx::Error> {
        self.collection_deletes
            .lock()
            .unwrap()
            .push((collection.to_string(), limit));
        Ok(0)
    }

    async fn is_live(&self, uri: &str) -> Result<bool, sqlx::Error> {
        self.asked_live.lock().unwrap().push(uri.to_string());
        Ok(RecordStore::new(self.pool.clone(), DatabaseBackend::Sqlite)
            .get(uri)
            .await?
            .is_some())
    }

    fn claimed(&self) -> Vec<String> {
        let mut claimed: Vec<String> = self.claimed.iter().cloned().collect();
        claimed.sort();
        claimed
    }
}

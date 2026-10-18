//! An external [`RecordStorage`] for tests: a second in-memory SQLite
//! database migrated with HappyView's own migrations, so its `read_route`
//! satisfies the relation contract and core's reads run on it unchanged.
//! It claims a fixed set of collections and records every call core makes.
//!
//! Built with [`FakeStorage::applying`], it also acts on those calls as a
//! real store would: it keeps the records its own feed stored
//! ([`FakeStorage::store`]) apart from the view it serves, and each delta,
//! account purge and collection delete changes that view.

use std::collections::{HashMap, HashSet};
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
    /// Whether calls change what is served (see [`FakeStorage::applying`]).
    applying: bool,
    /// The records the store's feed stored, by URI, before any delta.
    stored: Mutex<HashMap<String, PreparedRecord>>,
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
            applying: false,
            stored: Mutex::new(HashMap::new()),
        }
    }

    /// A fake whose served view follows the calls core makes: what a
    /// claimed collection's provider is shown to behave like by the
    /// conformance cases.
    pub async fn applying(collections: &[&str]) -> Self {
        Self {
            applying: true,
            ..Self::claiming(collections).await
        }
    }

    /// The store's own feed storing `record`: kept, and served as it is
    /// until a delta says otherwise. An older rev than the one held is
    /// refused, as a store ordering commits by rev refuses it.
    pub async fn store(&self, record: PreparedRecord) {
        let uri = record.uri();
        {
            let mut stored = self.stored.lock().unwrap();
            if let Some(held) = stored.get(&uri)
                && held.rev > record.rev
            {
                return;
            }
            stored.insert(uri, record.clone());
        }
        self.serve(&record).await;
    }

    /// The store's own feed removing the record at `uri`.
    pub async fn unstore(&self, uri: &str) {
        self.stored.lock().unwrap().remove(uri);
        self.unserve(uri).await;
    }

    fn records(&self) -> RecordStore {
        RecordStore::new(self.pool.clone(), DatabaseBackend::Sqlite)
    }

    async fn serve(&self, record: &PreparedRecord) {
        self.unserve(&record.uri()).await;
        self.records()
            .write_network(&self.pool, std::slice::from_ref(record))
            .await
            .expect("serve a record from the fake storage");
    }

    async fn unserve(&self, uri: &str) {
        self.records()
            .delete(uri)
            .await
            .expect("hide a record in the fake storage");
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
            .push((key.clone(), delta.clone(), for_rev.map(str::to_string)));
        if !self.applying {
            return Ok(());
        }
        let uri = key.uri();
        let held = self.stored.lock().unwrap().get(&uri).cloned();
        // A delta computed from a version the store no longer holds.
        if let Some(for_rev) = for_rev
            && held.as_ref().and_then(|held| held.rev.as_deref()) != Some(for_rev)
        {
            return Ok(());
        }
        match delta {
            Delta::Proceed => match held {
                Some(held) => self.serve(&held).await,
                None => self.unserve(&uri).await,
            },
            Delta::Replace { cid, rev, body } | Delta::Pending { cid, rev, body, .. } => {
                self.serve(&PreparedRecord {
                    did: key.did.clone(),
                    collection: key.collection.clone(),
                    rkey: key.rkey.clone(),
                    record_json: body,
                    cid,
                    rev,
                })
                .await
            }
            Delta::Hide => self.unserve(&uri).await,
        }
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
        if !self.applying {
            return Ok(0);
        }
        let gone: Vec<String> = self
            .stored
            .lock()
            .unwrap()
            .values()
            .filter(|record| record.did == did)
            .filter(|record| collections.is_none_or(|c| c.contains(&record.collection)))
            .map(PreparedRecord::uri)
            .collect();
        for uri in &gone {
            self.unstore(uri).await;
        }
        Ok(gone.len() as u64)
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

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use async_trait::async_trait;
use tokio::sync::watch;

use crate::db::DatabaseBackend;
use crate::types::*;

/// The instance database, for a provider's own tables.
#[derive(Clone)]
pub struct ProviderDb {
    pub pool: sqlx::AnyPool,
    /// A second pool sized for backfill work (`BACKFILL_CONCURRENT_*`).
    pub backfill_pool: sqlx::AnyPool,
    pub backend: DatabaseBackend,
}

#[derive(Default)]
pub struct IngestCounters {
    pub received: AtomicU64,
    pub matched: AtomicU64,
    pub skipped: AtomicU64,
}

/// What HappyView gives a provider. Everything a provider may use in
/// HappyView is here.
#[async_trait]
pub trait Host: Send + Sync + 'static {
    async fn ingest(&self, event: SourceEvent) -> IngestOutcome;
    /// In order, in one write transaction where the storage allows.
    async fn ingest_batch(&self, events: Vec<SourceEvent>) -> BatchOutcome;
    fn tracked_collections(&self) -> watch::Receiver<Vec<String>>;
    async fn has_record_hook(&self, collection: &str, action: Action) -> bool;
    /// DIDs holding records in `collections`, after `after_did`, ascending.
    async fn held_dids_page(
        &self,
        collections: &[String],
        after_did: Option<&str>,
        limit: i64,
    ) -> Result<Vec<String>, sqlx::Error>;
    fn database(&self) -> ProviderDb;
    async fn setting(&self, key: &str) -> Option<String>;
    async fn put_setting(&self, key: &str, value: &str) -> Result<(), sqlx::Error>;
    async fn log_event(&self, event: ProviderEvent);
    fn http(&self) -> reqwest::Client;
    fn instance(&self) -> InstanceInfo;
    async fn resolve_pds(&self, did: &str) -> Result<String, ResolveError>;
    fn counters(&self) -> Arc<IngestCounters>;
}

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
    /// [`Host::ingest`], with the error text behind a failed account or sync
    /// event, for a source that reports it once it gives the event up.
    async fn ingest_with_error(&self, event: SourceEvent) -> (IngestOutcome, Option<String>) {
        (self.ingest(event).await, None)
    }
    /// In order, in one write transaction where the storage allows.
    async fn ingest_batch(&self, events: Vec<SourceEvent>) -> BatchOutcome;
    /// [`Host::ingest_batch`], with the error text of a write that failed and
    /// stopped the batch, for a source that reports or retries it.
    async fn ingest_batch_with_error(
        &self,
        events: Vec<SourceEvent>,
    ) -> (BatchOutcome, Option<String>) {
        (self.ingest_batch(events).await, None)
    }
    fn tracked_collections(&self) -> watch::Receiver<Vec<String>>;
    async fn has_record_hook(&self, collection: &str, action: Action) -> bool;
    /// DIDs holding records in `collections`, after `after_did`, ascending.
    async fn held_dids_page(
        &self,
        collections: &[String],
        after_did: Option<&str>,
        limit: i64,
    ) -> Result<Vec<String>, sqlx::Error>;
    /// Whether `did` holds records in `collections`: whether
    /// [`Host::held_dids_page`] would list it. The default pages through
    /// that list; a host that can answer for one DID directly should.
    async fn holds_records(&self, did: &str, collections: &[String]) -> Result<bool, sqlx::Error> {
        const PAGE: i64 = 1000;
        let mut after: Option<String> = None;
        loop {
            let page = self
                .held_dids_page(collections, after.as_deref(), PAGE)
                .await?;
            if page.iter().any(|held| held == did) {
                return Ok(true);
            }
            if (page.len() as i64) < PAGE {
                return Ok(false);
            }
            after = page.last().cloned();
        }
    }
    fn database(&self) -> ProviderDb;
    async fn setting(&self, key: &str) -> Option<String>;
    async fn put_setting(&self, key: &str, value: &str) -> Result<(), sqlx::Error>;
    async fn log_event(&self, event: ProviderEvent);
    fn http(&self) -> reqwest::Client;
    fn instance(&self) -> InstanceInfo;
    /// One attempt at the PDS endpoint of `did`. Never retries: the error
    /// says whether a retry could help.
    async fn resolve_pds(&self, did: &str) -> Result<String, ResolveError>;
    /// The DID a handle names (a leading `@` allowed), or a DID itself,
    /// checked and returned as it is.
    async fn resolve_handle(&self, handle: &str) -> Result<String, ResolveError> {
        Err(ResolveError::new(format!(
            "could not resolve {handle}: this host does not resolve handles"
        )))
    }
    fn counters(&self) -> Arc<IngestCounters>;
}

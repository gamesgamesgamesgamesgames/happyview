use std::sync::Arc;

use async_trait::async_trait;

use crate::{host::Host, types::*};

#[async_trait]
pub trait RecordSource: Send + Sync + 'static {
    fn id(&self) -> &'static str;
    fn contract_version(&self) -> u32 {
        CONTRACT_VERSION
    }
    /// The source's own schema for `backend`, run by the host after its own
    /// migrations in `_happyview_source_<id>_migrations`.
    fn migrations(
        &self,
        backend: crate::db::DatabaseBackend,
    ) -> Option<&'static sqlx::migrate::Migrator>;
    /// Spawn live ingest and resume interrupted jobs. Returns once spawned.
    async fn start(self: Arc<Self>, host: Arc<dyn Host>) -> Result<(), ProviderError>;
    /// Mounted under `/admin` behind the host's auth layer.
    fn admin_routes(&self) -> axum::Router;
    async fn start_backfill(&self, req: BackfillRequest) -> Result<Option<String>, BackfillError>;
    async fn has_backfill(&self, collection: &str) -> bool;
    async fn backfill_running(&self, collection: &str) -> bool;
    async fn status(&self) -> serde_json::Value;
    async fn metrics(&self) -> Vec<(String, i64)>;
}

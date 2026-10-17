//! `JetstreamSource`: the Jetstream record source behind the `RecordSource`
//! contract. The live consumer is this crate's own. Backfill is still
//! HappyView's in-tree code, reached through the `backfill` source HappyView
//! hands over, until it moves here too.

use std::sync::Arc;

use async_trait::async_trait;
use happyview_provider::{
    BackfillError, BackfillRequest, Host, ProviderError, RecordSource, db::DatabaseBackend,
};

use crate::consumer::{self, GapReplay, config::Settings};

pub struct JetstreamSource {
    settings: Settings,
    gap_replay: Option<GapReplay>,
    /// Answers every backfill method. Its `start` must start backfill work
    /// only: the live consumer is this source's.
    backfill: Arc<dyn RecordSource>,
}

impl JetstreamSource {
    /// A source consuming Jetstream by `settings`, replaying a refused
    /// cursor's gap by `gap_replay` (none: resume without the archive), with
    /// backfill answered by `backfill`.
    pub fn new(
        settings: Settings,
        gap_replay: Option<GapReplay>,
        backfill: Arc<dyn RecordSource>,
    ) -> Arc<Self> {
        Arc::new(Self {
            settings,
            gap_replay,
            backfill,
        })
    }
}

#[async_trait]
impl RecordSource for JetstreamSource {
    fn id(&self) -> &'static str {
        "jetstream"
    }

    fn migrations(&self, backend: DatabaseBackend) -> Option<&'static sqlx::migrate::Migrator> {
        self.backfill.migrations(backend)
    }

    async fn start(self: Arc<Self>, host: Arc<dyn Host>) -> Result<(), ProviderError> {
        consumer::spawn_with_gap_replay(
            Arc::clone(&host),
            self.settings.clone(),
            None,
            self.gap_replay.clone(),
        );
        Arc::clone(&self.backfill).start(host).await
    }

    fn admin_routes(&self) -> axum::Router {
        self.backfill.admin_routes()
    }

    async fn start_backfill(&self, req: BackfillRequest) -> Result<Option<String>, BackfillError> {
        self.backfill.start_backfill(req).await
    }

    async fn has_backfill(&self, collection: &str) -> bool {
        self.backfill.has_backfill(collection).await
    }

    async fn backfill_running(&self, collection: &str) -> bool {
        self.backfill.backfill_running(collection).await
    }

    async fn status(&self) -> serde_json::Value {
        serde_json::to_value(consumer::status::snapshot()).unwrap_or_default()
    }

    async fn metrics(&self) -> Vec<(String, i64)> {
        self.backfill.metrics().await
    }
}

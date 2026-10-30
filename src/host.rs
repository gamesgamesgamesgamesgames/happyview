//! `AppHost`: what HappyView gives a record provider (`happyview_provider::Host`),
//! over `AppState`.

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use tokio::sync::watch;

use happyview_provider::{
    Action, BackfillError, BackfillRequest, BatchOutcome, Host, IngestCounters, IngestOutcome,
    InstanceInfo, JobHookStats, ProviderDb, ProviderError, ProviderEvent, RecordSource,
    ResolveError, Severity, SourceEvent,
};
use happyview_source_jetstream::JetstreamSource;
use happyview_source_jetstream::archive::ArchiveConfig;
use happyview_source_jetstream::consumer::config::Settings;

use crate::AppState;

pub struct AppHost {
    state: AppState,
}

impl AppHost {
    pub fn new(state: AppState) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Host for AppHost {
    async fn ingest(&self, event: SourceEvent) -> IngestOutcome {
        crate::ingest::apply(&self.state, event).await
    }

    async fn ingest_with_error(&self, event: SourceEvent) -> (IngestOutcome, Option<String>) {
        let applied = crate::ingest::apply_in(&self.state, event).await;
        (applied.outcome, applied.error)
    }

    async fn ingest_batch(&self, events: Vec<SourceEvent>) -> BatchOutcome {
        crate::ingest::apply_batch(&self.state, events).await
    }

    async fn ingest_batch_with_error(
        &self,
        events: Vec<SourceEvent>,
    ) -> (BatchOutcome, Option<String>) {
        crate::ingest::apply_batch_in(&self.state, events).await
    }

    fn tracked_collections(&self) -> watch::Receiver<Vec<String>> {
        self.state.collections_tx.subscribe()
    }

    async fn has_record_hook(&self, collection: &str, action: Action) -> bool {
        self.state
            .scripts
            .resolve_record_event_in(&self.state.project.load_full(), collection, action.as_str())
            .await
            .is_some()
    }

    async fn held_dids_page(
        &self,
        collections: &[String],
        after_did: Option<&str>,
        limit: i64,
    ) -> Result<Vec<String>, sqlx::Error> {
        self.state
            .records
            .held_dids_page(&self.state.backfill_db, collections, after_did, limit)
            .await
    }

    async fn holds_records(&self, did: &str, collections: &[String]) -> Result<bool, sqlx::Error> {
        self.state
            .records
            .holds_local_network_records(did, Some(collections))
            .await
    }

    fn database(&self) -> ProviderDb {
        ProviderDb {
            pool: self.state.db.clone(),
            backfill_pool: self.state.backfill_db.clone(),
            backend: self.state.db_backend,
        }
    }

    async fn setting(&self, key: &str) -> Option<String> {
        crate::admin::settings::get_setting(&self.state.db, key, self.state.db_backend).await
    }

    async fn put_setting(&self, key: &str, value: &str) -> Result<(), sqlx::Error> {
        crate::admin::settings::put_setting(&self.state.db, key, value, self.state.db_backend).await
    }

    async fn log_event(&self, event: ProviderEvent) {
        crate::event_log::log_event(
            &self.state.db,
            crate::event_log::EventLog {
                event_type: event.event_type,
                severity: match event.severity {
                    Severity::Info => crate::event_log::Severity::Info,
                    Severity::Warn => crate::event_log::Severity::Warn,
                    Severity::Error => crate::event_log::Severity::Error,
                },
                actor_did: event.actor_did,
                subject: event.subject,
                detail: event.detail,
            },
            self.state.db_backend,
        )
        .await;
    }

    fn http(&self) -> reqwest::Client {
        self.state.http.clone()
    }

    fn instance(&self) -> InstanceInfo {
        InstanceInfo {
            plc_url: self.state.config.plc_url.clone(),
            relay_url: self.state.config.relay_url.clone(),
            public_url: self.state.config.public_url.clone(),
            user_agent: crate::version::user_agent(),
        }
    }

    async fn resolve_pds(&self, did: &str) -> Result<String, ResolveError> {
        crate::profile::resolve_pds_endpoint_once(&self.state.http, &self.state.config.plc_url, did)
            .await
            .map_err(ResolveError)
    }

    /// `identity::resolve_identifier`: a DID is checked and returned, a
    /// handle resolved. Its message is the one an admin request shows.
    async fn resolve_handle(&self, handle: &str) -> Result<String, ResolveError> {
        crate::identity::resolve_identifier(handle)
            .await
            .map(|resolved| resolved.did)
            .map_err(|e| match e {
                crate::error::AppError::BadRequest(message) => ResolveError::new(message),
                other => ResolveError::new(other.to_string()),
            })
    }

    fn counters(&self) -> Arc<IngestCounters> {
        Arc::clone(&self.state.telemetry_counters.ingest)
    }

    fn observe_job_stage(
        &self,
        job_id: &str,
        stage: &'static str,
        elapsed: std::time::Duration,
        n: u64,
    ) {
        self.state.metrics.observe_job(job_id, stage, elapsed, n);
    }

    fn note_job_progress(&self, job_id: &str, processed: i64, total: Option<i64>) {
        self.state.metrics.job_progress(job_id, processed, total);
    }

    fn job_hook_stats_due(&self, job_id: &str) -> Option<JobHookStats> {
        self.state.metrics.job_stats_due(job_id)
    }

    fn job_hook_stats(&self, job_id: &str) -> Option<JobHookStats> {
        self.state.metrics.job_stats(job_id)
    }

    fn finish_job_hook_stats(&self, job_id: &str) -> Option<JobHookStats> {
        self.state.metrics.finish_job(job_id)
    }
}

/// Make a detached Jetstream source over `state` its record source, with
/// replay settings `archive` and `state.config.jetstream_url`: its backfill
/// jobs and admin routes work on `state`, and nothing is started. For a state
/// built without `run`'s boot (tests and the test runner), and again after
/// such a state's pools or settings change.
///
/// The state the source's host holds reaches back to the source weakly, so
/// the two don't keep each other (and their pools) alive.
pub fn attach_jetstream(state: &mut AppState, archive: ArchiveConfig) -> Arc<JetstreamSource> {
    let source = JetstreamSource::builder()
        .settings(Settings::from_env(&state.config.jetstream_url))
        .archive(archive)
        .build_detached();
    let mut held = state.clone();
    held.source = Arc::new(WeakSource {
        source: Arc::downgrade(&source),
        running: source.running(),
    });
    source.attach(AppHost::new(held));
    state.source = source.clone();
    source
}

/// A source's host's handle back to it. Once the source is gone it answers
/// on the safe side: a job it was running still counts as running, and a
/// collection may have been backfilled, so nothing starts one on a guess.
struct WeakSource {
    source: Weak<JetstreamSource>,
    running: happyview_source_jetstream::backfill::RunningJobs,
}

#[async_trait]
impl RecordSource for WeakSource {
    fn id(&self) -> &'static str {
        "jetstream"
    }

    fn migrations(
        &self,
        _backend: happyview_provider::db::DatabaseBackend,
    ) -> Option<&'static sqlx::migrate::Migrator> {
        None
    }

    async fn start(self: Arc<Self>, host: Arc<dyn Host>) -> Result<(), ProviderError> {
        match self.source.upgrade() {
            Some(source) => source.start(host).await,
            None => Ok(()),
        }
    }

    fn admin_routes(&self) -> axum::Router {
        match self.source.upgrade() {
            Some(source) => source.admin_routes(),
            None => axum::Router::new(),
        }
    }

    async fn start_backfill(&self, req: BackfillRequest) -> Result<Option<String>, BackfillError> {
        match self.source.upgrade() {
            Some(source) => source.start_backfill(req).await,
            None => Err(BackfillError::Internal(
                "the jetstream source has no host yet".into(),
            )),
        }
    }

    async fn has_backfill(&self, collection: &str) -> bool {
        match self.source.upgrade() {
            Some(source) => source.has_backfill(collection).await,
            None => true,
        }
    }

    async fn backfill_running(&self, collection: &str) -> bool {
        self.running.covers(collection)
    }

    async fn status(&self) -> serde_json::Value {
        match self.source.upgrade() {
            Some(source) => source.status().await,
            None => serde_json::Value::Null,
        }
    }

    async fn metrics(&self) -> Vec<(String, i64)> {
        match self.source.upgrade() {
            Some(source) => source.metrics().await,
            None => Vec::new(),
        }
    }
}

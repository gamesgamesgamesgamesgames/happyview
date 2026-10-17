//! `AppHost`: what HappyView gives a record provider (`happyview_provider::Host`),
//! over `AppState`.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::watch;

use happyview_provider::{
    Action, BatchOutcome, Host, IngestCounters, IngestOutcome, InstanceInfo, ProviderDb,
    ProviderEvent, ResolveError, Severity, SourceEvent,
};

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
}

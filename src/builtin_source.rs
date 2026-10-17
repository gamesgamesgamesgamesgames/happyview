//! `BuiltinSource`: the Jetstream source (`happyview_source_jetstream`) with
//! HappyView's archive replay plugged into it, behind the `RecordSource`
//! contract (plan ruling C1: route HappyView through the contract first,
//! move the code into its crate after).
//!
//! Replay still takes an `AppState`, so the source is built from one,
//! attached by whoever built the state: `run` before it calls `start`, and
//! the test state builders. The state it holds reaches back to the source
//! through a weak handle, so an instance and its source do not keep each
//! other (and their pools) alive.

use std::sync::{Arc, Mutex, RwLock, Weak};

use async_trait::async_trait;
use futures_util::FutureExt;

use happyview_provider::{
    BackfillError, BackfillKind, BackfillRequest, Host, ProviderError, RecordSource,
};
use happyview_source_jetstream::JetstreamSource;
use happyview_source_jetstream::backfill::JobState;

use crate::AppState;
use crate::jetstream::GapReplay;
use crate::jetstream::archive::SourceChoice;

/// Builtin sources `run` may need to attach a state to, found by address
/// since `Providers` hands the source over as a trait object.
static ATTACHABLE: Mutex<Vec<Weak<BuiltinSource>>> = Mutex::new(Vec::new());

pub struct BuiltinSource {
    /// `false` for a test state's source: `start` spawns nothing.
    live: bool,
    attached: RwLock<Option<Attached>>,
}

/// What attaching a state builds.
#[derive(Clone)]
struct Attached {
    state: AppState,
    source: Arc<JetstreamSource>,
}

impl BuiltinSource {
    /// The source a served instance runs: `run` attaches its state.
    pub fn new() -> Arc<Self> {
        let source = Arc::new(Self::with_live(true));
        let mut attachable = ATTACHABLE.lock().unwrap_or_else(|e| e.into_inner());
        attachable.retain(|weak| weak.strong_count() > 0);
        attachable.push(Arc::downgrade(&source));
        source
    }

    /// A source whose `start` is a no-op, holding no state until one is
    /// attached.
    pub fn detached() -> Self {
        Self::with_live(false)
    }

    /// A detached source attached to `state`, for a test that changed its
    /// state after building it and needs the source to see the change.
    pub fn attached(state: &AppState) -> Arc<Self> {
        let source = Arc::new(Self::detached());
        source.attach(state);
        source
    }

    fn with_live(live: bool) -> Self {
        Self {
            live,
            attached: RwLock::new(None),
        }
    }

    /// Build the Jetstream source over `state` (its own handle to this
    /// source made weak), with HappyView's archive replay.
    pub fn attach(self: &Arc<Self>, state: &AppState) {
        let mut held = state.clone();
        held.source = Arc::new(SourceRef(Arc::downgrade(self)));
        let jobs = job_state(&held);
        let source = JetstreamSource::new(
            crate::jetstream::config::Settings::from_env(&held.config.jetstream_url),
            gap_replay(&held, &jobs),
            jobs,
        );
        *self.attached.write().unwrap_or_else(|e| e.into_inner()) = Some(Attached {
            state: held,
            source,
        });
    }

    fn current(&self) -> Option<Attached> {
        self.attached
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The Jetstream source this one runs, once a state is attached.
    pub fn jetstream(&self) -> Option<Arc<JetstreamSource>> {
        self.current().map(|attached| attached.source)
    }
}

/// Attach `state` to `source` when it is a [`BuiltinSource`] made by
/// [`BuiltinSource::new`]. Any other source is left alone.
pub(crate) fn attach_if_builtin(source: &Arc<dyn RecordSource>, state: &AppState) -> bool {
    let address = Arc::as_ptr(source) as *const ();
    let found = ATTACHABLE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|weak| weak.as_ptr() as *const () == address)
        .and_then(Weak::upgrade);
    match found {
        Some(builtin) => {
            builtin.attach(state);
            true
        }
        None => false,
    }
}

/// A backfill job system over `state`, with HappyView's archive replay and
/// a job event channel of its own.
pub fn job_state(state: &AppState) -> JobState {
    JobState::new(
        crate::host::AppHost::new(state.clone()),
        state.archive.default_source,
        Arc::new(crate::admin::backfill::ArchiveReplay::new(state.clone())),
    )
}

/// The consumer's gap replay for `state`: the in-tree archive replay, run as
/// a job of `jobs`, or `None` when backfill only crawls, which never asks
/// the archive (P8).
pub fn gap_replay(state: &AppState, jobs: &JobState) -> Option<GapReplay> {
    if state.archive.default_source == SourceChoice::Crawl {
        return None;
    }
    let state = state.clone();
    let jobs = jobs.clone();
    Some(Arc::new(move |after_seq| {
        let state = state.clone();
        let jobs = jobs.clone();
        async move {
            crate::admin::backfill::replay::start_gap_replay(&state, &jobs, after_seq).await
        }
        .boxed()
    }))
}

fn detached_error() -> BackfillError {
    BackfillError::Internal("the builtin record source has no instance attached".into())
}

#[async_trait]
impl RecordSource for BuiltinSource {
    fn id(&self) -> &'static str {
        "jetstream"
    }

    /// Its tables are still in HappyView's own migrations.
    fn migrations(
        &self,
        _backend: happyview_provider::db::DatabaseBackend,
    ) -> Option<&'static sqlx::migrate::Migrator> {
        None
    }

    async fn start(self: Arc<Self>, host: Arc<dyn Host>) -> Result<(), ProviderError> {
        if !self.live {
            return Ok(());
        }
        let Some(Attached { state, source }) = self.current() else {
            return Err(ProviderError::Other(
                "the builtin record source was started with no instance attached".into(),
            ));
        };
        // Spawns the consumer and the retention sweep, and resumes the jobs
        // that were running.
        source.start(host).await?;

        // After the resume, which re-spawns running jobs and would otherwise run
        // one started here twice.
        if state.config.project_root.is_some() {
            crate::project::boot::start_declared_backfills(&state).await;
        }
        Ok(())
    }

    fn admin_routes(&self) -> axum::Router {
        match self.jetstream() {
            Some(source) => source.admin_routes(),
            None => axum::Router::new(),
        }
    }

    async fn start_backfill(&self, req: BackfillRequest) -> Result<Option<String>, BackfillError> {
        let Attached { state, source } = self.current().ok_or_else(detached_error)?;
        match req.kind {
            // Whatever `BACKFILL_SOURCE` says, as before.
            BackfillKind::Gap { after_seq } => {
                crate::admin::backfill::replay::start_gap_replay(&state, source.jobs(), after_seq)
                    .await
                    .map(|(job_id, _tip)| Some(job_id))
                    .map_err(BackfillError::Internal)
            }
            _ => source.start_backfill(req).await,
        }
    }

    async fn has_backfill(&self, collection: &str) -> bool {
        match self.jetstream() {
            Some(source) => source.has_backfill(collection).await,
            None => false,
        }
    }

    async fn backfill_running(&self, collection: &str) -> bool {
        match self.jetstream() {
            Some(source) => source.backfill_running(collection).await,
            None => false,
        }
    }

    async fn status(&self) -> serde_json::Value {
        serde_json::to_value(crate::jetstream::status::snapshot()).unwrap_or_default()
    }

    async fn metrics(&self) -> Vec<(String, i64)> {
        match self.jetstream() {
            Some(source) => source.metrics().await,
            None => Vec::new(),
        }
    }
}

/// The handle a [`BuiltinSource`]'s held state has to the source itself:
/// weak, so the two don't keep each other alive. Once the source is gone it
/// answers as a detached source would.
struct SourceRef(Weak<BuiltinSource>);

#[async_trait]
impl RecordSource for SourceRef {
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
        match self.0.upgrade() {
            Some(source) => source.start(host).await,
            None => Ok(()),
        }
    }

    fn admin_routes(&self) -> axum::Router {
        match self.0.upgrade() {
            Some(source) => source.admin_routes(),
            None => axum::Router::new(),
        }
    }

    async fn start_backfill(&self, req: BackfillRequest) -> Result<Option<String>, BackfillError> {
        match self.0.upgrade() {
            Some(source) => source.start_backfill(req).await,
            None => Err(detached_error()),
        }
    }

    async fn has_backfill(&self, collection: &str) -> bool {
        match self.0.upgrade() {
            Some(source) => source.has_backfill(collection).await,
            None => false,
        }
    }

    async fn backfill_running(&self, collection: &str) -> bool {
        match self.0.upgrade() {
            Some(source) => source.backfill_running(collection).await,
            None => false,
        }
    }

    async fn status(&self) -> serde_json::Value {
        match self.0.upgrade() {
            Some(source) => source.status().await,
            None => serde_json::Value::Null,
        }
    }

    async fn metrics(&self) -> Vec<(String, i64)> {
        match self.0.upgrade() {
            Some(source) => source.metrics().await,
            None => Vec::new(),
        }
    }
}

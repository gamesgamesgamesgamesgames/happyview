//! `BuiltinSource`: the Jetstream consumer (from `happyview_source_jetstream`)
//! and the in-tree backfill behind the `RecordSource` contract (plan ruling
//! C1: route HappyView through the contract first, move the code into its
//! crate after).
//!
//! The backfill code it wraps still takes an `AppState`, so the source holds one,
//! attached by whoever built the state: `run` before it calls `start`, and the
//! test state builders. The state it holds reaches back to the source through
//! a weak handle, so an instance and its source do not keep each other (and
//! their pools) alive.

use std::sync::{Arc, Mutex, RwLock, Weak};

use async_trait::async_trait;
use futures_util::FutureExt;

use happyview_provider::{
    BackfillError, BackfillKind, BackfillRequest, Host, ProviderError, RecordSource,
};

use crate::AppState;
use crate::error::AppError;
use crate::jetstream::GapReplay;
use crate::jetstream::archive::SourceChoice;

/// Tables counted into telemetry `totals`, as (payload key, table name).
const COUNTED_TABLES: &[(&str, &str)] = &[
    ("backfill_repos", "happyview_backfill_repos"),
    ("backfill_queue", "happyview_backfill_queue"),
    ("backfill_completions", "happyview_backfill_completions"),
    ("backfill_pds_stats", "happyview_backfill_pds_stats"),
    ("backfill_cursors", "happyview_backfill_cursors"),
];

/// Builtin sources `run` may need to attach a state to, found by address
/// since `Providers` hands the source over as a trait object.
static ATTACHABLE: Mutex<Vec<Weak<BuiltinSource>>> = Mutex::new(Vec::new());

pub struct BuiltinSource {
    /// `false` for a test state's source: `start` spawns nothing.
    live: bool,
    state: RwLock<Option<AppState>>,
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
            state: RwLock::new(None),
        }
    }

    /// Hold `state` (its own handle to this source made weak) for the
    /// in-tree code this source calls.
    pub fn attach(self: &Arc<Self>, state: &AppState) {
        let mut held = state.clone();
        held.source = Arc::new(SourceRef(Arc::downgrade(self)));
        *self.state.write().unwrap_or_else(|e| e.into_inner()) = Some(held);
    }

    fn state(&self) -> Option<AppState> {
        self.state.read().unwrap_or_else(|e| e.into_inner()).clone()
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

/// The consumer's gap replay for `state`: the in-tree archive replay, or
/// `None` when backfill only crawls, which never asks the archive (P8).
pub fn gap_replay(state: &AppState) -> Option<GapReplay> {
    if state.archive.default_source == SourceChoice::Crawl {
        return None;
    }
    let state = state.clone();
    Some(Arc::new(move |after_seq| {
        let state = state.clone();
        async move { crate::admin::backfill::replay::start_gap_replay(&state, after_seq).await }
            .boxed()
    }))
}

fn backfill_error(e: AppError) -> BackfillError {
    match e {
        AppError::BadRequest(_) => BackfillError::Invalid(e.to_string()),
        AppError::Conflict(_) => BackfillError::Conflict(e.to_string()),
        _ => BackfillError::Internal(e.to_string()),
    }
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
        let Some(state) = self.state() else {
            return Err(ProviderError::Other(
                "the builtin record source was started with no instance attached".into(),
            ));
        };
        crate::jetstream::spawn_with_gap_replay(
            host,
            crate::jetstream::config::Settings::from_env(&state.config.jetstream_url),
            None,
            gap_replay(&state),
        );

        crate::admin::backfill::resume_backfill_jobs(&state).await;

        // After the resume, which re-spawns running jobs and would otherwise run
        // one started here twice.
        if state.config.project_root.is_some() {
            crate::project::boot::start_declared_backfills(&state).await;
        }

        tokio::spawn(async move {
            crate::admin::backfill::run_backfill_retention_cleanup(&state).await;
        });
        Ok(())
    }

    fn admin_routes(&self) -> axum::Router {
        match self.state() {
            Some(state) => crate::admin::builtin_source_routes().with_state(state),
            None => axum::Router::new(),
        }
    }

    async fn start_backfill(&self, req: BackfillRequest) -> Result<Option<String>, BackfillError> {
        let state = self.state().ok_or_else(detached_error)?;
        match req.kind {
            BackfillKind::Automatic => {
                let collection = req.collection.ok_or_else(|| {
                    BackfillError::Invalid("an automatic backfill needs a collection".into())
                })?;
                crate::admin::backfill::start_automatic_backfill(&state, collection, &req.actor)
                    .await
                    .map_err(backfill_error)
            }
            BackfillKind::Manual => {
                let source = match req.source.as_deref() {
                    Some(raw) => crate::jetstream::archive::parse_source(raw).ok_or_else(|| {
                        BackfillError::Invalid(format!("unknown backfill source: {raw}"))
                    })?,
                    None => state.archive.default_source,
                };
                crate::admin::backfill::start_backfill_with(
                    &state,
                    req.collection,
                    req.dids,
                    &req.actor,
                    source,
                )
                .await
                .map(Some)
                .map_err(backfill_error)
            }
            BackfillKind::Gap { after_seq } => {
                crate::admin::backfill::replay::start_gap_replay(&state, after_seq)
                    .await
                    .map(|(job_id, _tip)| Some(job_id))
                    .map_err(BackfillError::Internal)
            }
        }
    }

    /// Whether `collection` has ever had a network backfill job. A job for
    /// named accounts doesn't count: it fetched those accounts' records, not
    /// the collection's. When it cannot tell, it says yes, so no caller starts
    /// one on a guess.
    async fn has_backfill(&self, collection: &str) -> bool {
        let Some(state) = self.state() else {
            return false;
        };
        let sql = crate::db::adapt_sql(
            "SELECT COUNT(*) FROM happyview_backfill_jobs WHERE collection = ? AND scope = 'network'",
            state.db_backend,
        );
        let seen: Result<(i64,), _> = crate::db::query_as(&sql)
            .bind(collection)
            .fetch_one(&state.db)
            .await;
        match seen {
            Ok((0,)) => false,
            Ok(_) => true,
            Err(e) => {
                tracing::warn!(
                    collection = %collection,
                    error = %e,
                    "could not tell whether this collection was backfilled; not starting one"
                );
                true
            }
        }
    }

    /// Whether a backfill covering `collection` may still write: running, or
    /// pausing or cancelling (both stop once in-flight pages are written).
    /// When it cannot tell, it says yes, so a delete still leaves its
    /// tombstone.
    async fn backfill_running(&self, collection: &str) -> bool {
        let Some(state) = self.state() else {
            return false;
        };
        let sql = crate::db::adapt_sql(
            "SELECT 1 FROM happyview_backfill_jobs \
             WHERE status IN ('running', 'pausing', 'cancelling') \
             AND (collection IS NULL OR collection = ?) LIMIT 1",
            state.db_backend,
        );
        match crate::db::query(&sql)
            .bind(collection)
            .fetch_optional(&state.db)
            .await
        {
            Ok(row) => row.is_some(),
            Err(e) => {
                tracing::warn!(
                    collection = %collection,
                    error = %e,
                    "could not tell whether a backfill is running; assuming one is"
                );
                true
            }
        }
    }

    async fn status(&self) -> serde_json::Value {
        serde_json::to_value(crate::jetstream::status::snapshot()).unwrap_or_default()
    }

    /// The backfill tables' row counts (a table that cannot be counted is
    /// left out), and `feature.backfill.ever` / `feature.backfill.active`
    /// (any job, and any job in the last seven days).
    async fn metrics(&self) -> Vec<(String, i64)> {
        let Some(state) = self.state() else {
            return Vec::new();
        };
        let (pool, backend) = (&state.db, state.db_backend);
        let mut out = Vec::new();
        for (key, table) in COUNTED_TABLES {
            if let Some(n) =
                crate::telemetry::collect::database::count_table(pool, backend, table).await
            {
                out.push(((*key).to_string(), n));
            }
        }
        let ever = crate::telemetry::collect::features::count(
            pool,
            backend,
            "SELECT COUNT(*) FROM happyview_backfill_jobs",
            None,
        )
        .await
            >= 1;
        let active = ever
            && crate::telemetry::collect::features::count(
                pool,
                backend,
                "SELECT COUNT(*) FROM happyview_backfill_jobs WHERE created_at >= ?",
                Some(&crate::telemetry::collect::features::window_start()),
            )
            .await
                >= 1;
        out.push(("feature.backfill.ever".to_string(), i64::from(ever)));
        out.push(("feature.backfill.active".to_string(), i64::from(active)));
        out
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

//! Script execution metrics, kept in memory by the process that runs the
//! scripts.
//!
//! Every run through `script::dispatch` becomes one [`RunSample`], added to
//! its script's rolling windows. Nothing is written per run: once a minute
//! [`persist`] writes the minutes that have closed, and a restart reads the
//! last day back.

pub mod ingest;
pub mod job;
pub mod persist;
pub mod retention;
pub mod run;
pub mod sketch;
pub mod stages;
pub mod window;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use serde::Serialize;
use sqlx::AnyPool;

use crate::admin::settings::try_get_setting;
use crate::db::DatabaseBackend;
use crate::plugin::{ExecutionError, ScriptExecuteOutput, ScriptKind};
use crate::script::DispatchError;

use happyview_provider::JobHookStats;
use ingest::{IngestBucket, IngestSummary, RecordSample};
use job::JobStats;
use run::RunTimings;
use window::{Bucket, History, Stats};

pub const ENABLED_KEY: &str = "script_metrics_enabled";

/// How long a change made on another instance sharing the database takes to
/// reach this one.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(10);

/// The most scripts kept at once. An instance's keys are its triggers, so
/// this is far past any real one; it bounds memory against a flood of
/// one-off trigger ids. A key past it is counted and dropped.
pub const MAX_KEYS: usize = 1_024;

/// How a run ended. The interpreter's own error kinds and the host's
/// failures share names where they mean the same thing: a budget the
/// interpreter enforced and one the host enforced are both a `timeout`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Syntax,
    Runtime,
    Timeout,
    Memory,
    MissingHandle,
    /// The guest trapped without reporting why.
    Trap,
    NoInterpreter,
    /// Any other failure of the host to run the script.
    Error,
}

/// Every outcome's name, in [`Outcome::index`] order.
pub const OUTCOMES: [&str; 9] = [
    "ok",
    "syntax",
    "runtime",
    "timeout",
    "memory",
    "missing_handle",
    "trap",
    "no_interpreter",
    "error",
];

impl Outcome {
    pub fn index(self) -> usize {
        self as usize
    }

    pub fn as_str(self) -> &'static str {
        OUTCOMES[self.index()]
    }

    pub fn of(result: &Result<ScriptExecuteOutput, DispatchError>) -> Self {
        use crate::plugin::ScriptErrorKind as K;
        match result {
            Ok(ScriptExecuteOutput::Returned { .. }) => Self::Ok,
            Ok(ScriptExecuteOutput::Error { kind, .. }) => match kind {
                K::Syntax => Self::Syntax,
                K::Runtime => Self::Runtime,
                K::Timeout => Self::Timeout,
                K::Memory => Self::Memory,
                K::MissingHandle => Self::MissingHandle,
            },
            Err(DispatchError::NoInterpreter { .. }) => Self::NoInterpreter,
            Err(DispatchError::Execution(e)) => match e {
                ExecutionError::Timeout => Self::Timeout,
                ExecutionError::MemoryLimit { .. } => Self::Memory,
                ExecutionError::Trap(_) => Self::Trap,
                _ => Self::Error,
            },
        }
    }
}

/// One run, as `dispatch` saw it.
#[derive(Clone, Debug)]
pub struct RunSample {
    pub outcome: Outcome,
    /// The whole dispatch, from building the input to the result.
    pub wall: Duration,
    /// Building the input and attaching a prepared artifact.
    pub prepare: Duration,
    pub timings: RunTimings,
}

/// Where a run's script came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Layer {
    Project,
    Dashboard,
}

impl Layer {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Dashboard => "dashboard",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    trigger: String,
    kind: ScriptKind,
    layer: Layer,
}

/// The windows metrics are read over. The hour and the day are held in
/// memory; a week and a month are read from the table [`persist`] writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    Hour,
    Day,
    Week,
    Month,
}

impl Window {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "1h" => Some(Self::Hour),
            "24h" => Some(Self::Day),
            "7d" => Some(Self::Week),
            "30d" => Some(Self::Month),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hour => "1h",
            Self::Day => "24h",
            Self::Week => "7d",
            Self::Month => "30d",
        }
    }

    pub fn secs(self) -> u64 {
        match self {
            Self::Hour => 3_600,
            Self::Day => 86_400,
            Self::Week => 7 * 86_400,
            Self::Month => 30 * 86_400,
        }
    }

    /// Whether the window is read from the table rather than memory.
    pub fn is_stored(self) -> bool {
        matches!(self, Self::Week | Self::Month)
    }

    /// The window in words, for a message.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Hour => "hour",
            Self::Day => "day",
            Self::Week => "7 days",
            Self::Month => "30 days",
        }
    }
}

/// The widths a series can be read at: the minute and the half hour from
/// memory, the hour and the day from the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Minute,
    HalfHour,
    Hour,
    Day,
}

impl Step {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "1m" => Some(Self::Minute),
            "30m" => Some(Self::HalfHour),
            "1h" => Some(Self::Hour),
            "1d" => Some(Self::Day),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Minute => "1m",
            Self::HalfHour => "30m",
            Self::Hour => "1h",
            Self::Day => "1d",
        }
    }

    pub fn secs(self) -> u64 {
        match self {
            Self::Minute => 60,
            Self::HalfHour => 1_800,
            Self::Hour => 3_600,
            Self::Day => 86_400,
        }
    }

    pub fn is_stored(self) -> bool {
        matches!(self, Self::Hour | Self::Day)
    }

    pub fn default_for(window: Window) -> Self {
        match window {
            Window::Hour => Self::Minute,
            Window::Day => Self::HalfHour,
            Window::Week => Self::Hour,
            Window::Month => Self::Day,
        }
    }
}

/// The kinds a gauge is kept for. A test file is the test runner's.
const GAUGED: [ScriptKind; 5] = [
    ScriptKind::XrpcQuery,
    ScriptKind::XrpcProcedure,
    ScriptKind::RecordEvent,
    ScriptKind::Label,
    ScriptKind::Job,
];

fn gauge_index(kind: ScriptKind) -> Option<usize> {
    GAUGED.iter().position(|k| *k == kind)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct IngestKey {
    collection: String,
    action: &'static str,
}

pub struct ScriptMetrics {
    enabled: AtomicBool,
    keys: RwLock<HashMap<Key, Arc<Mutex<History>>>>,
    /// Ingest's records, by collection and action. Bounded by [`MAX_KEYS`]
    /// as the scripts are, and counted in the same overflow.
    ingest: RwLock<HashMap<IngestKey, Arc<Mutex<History<IngestBucket>>>>>,
    /// Backfill jobs' hook stats, by job id. Few jobs run at once, and each
    /// is dropped an hour after its last record.
    jobs: Mutex<HashMap<String, JobStats>>,
    overflow: AtomicU64,
    active: [AtomicU64; GAUGED.len()],
    /// Every minute bucket starting before this (seconds since the epoch) is
    /// in the table, and none after it. Zero until something is written or
    /// read back.
    persisted_to: AtomicU64,
}

impl Default for ScriptMetrics {
    fn default() -> Self {
        Self {
            enabled: AtomicBool::new(true),
            keys: RwLock::default(),
            ingest: RwLock::default(),
            jobs: Mutex::default(),
            overflow: AtomicU64::new(0),
            active: Default::default(),
            persisted_to: AtomicU64::new(0),
        }
    }
}

/// One job's hook stats in the live view.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct LiveJob {
    pub job_id: String,
    /// Seconds since the job's last record or report.
    pub idle_s: u64,
    pub hooks: JobHookStats,
}

/// One script's row in a window.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct ScriptSummary {
    pub trigger: String,
    pub kind: &'static str,
    pub layer: &'static str,
    #[serde(flatten)]
    pub stats: Stats,
}

/// One step of a script's series.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct SeriesPoint {
    pub start: String,
    pub runs: u64,
    pub outcomes: std::collections::BTreeMap<&'static str, u64>,
    pub wall_ms: PointPercentiles,
    pub guest_ms: PointPercentiles,
    pub host_ms_mean: Option<f64>,
    pub instantiate_ms_mean: Option<f64>,
    pub link_ms_mean: Option<f64>,
    pub allocate_ms_mean: Option<f64>,
    pub teardown_ms_mean: Option<f64>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct PointPercentiles {
    pub p50: Option<f64>,
    pub p95: Option<f64>,
    pub max: Option<f64>,
}

/// One script's series, merged over every layer it ran from.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct ScriptSeries {
    pub trigger: String,
    pub kind: &'static str,
    pub layers: Vec<&'static str>,
    #[serde(flatten)]
    pub stats: Stats,
    pub points: Vec<SeriesPoint>,
}

/// The active-runs gauge of one run, given back when the run ends however
/// it ends.
pub struct ActiveRun<'a> {
    gauge: Option<&'a AtomicU64>,
}

impl Drop for ActiveRun<'_> {
    fn drop(&mut self) {
        if let Some(gauge) = self.gauge {
            gauge.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// Seconds since the epoch.
pub fn unix_now() -> u64 {
    unix_now_ms() / 1_000
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn rfc3339(secs: u64) -> String {
    chrono::DateTime::from_timestamp(i64::try_from(secs).unwrap_or(i64::MAX), 0)
        .unwrap_or_default()
        .to_rfc3339()
}

impl ScriptMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Keys refused because [`MAX_KEYS`] were held.
    pub fn overflow(&self) -> u64 {
        self.overflow.load(Ordering::Relaxed)
    }

    /// Count a run of `kind` as active until the guard drops. Counted whether
    /// or not metrics are on: two atomics, and the live view is what an
    /// operator turning metrics off to measure their cost still looks at.
    pub fn start_run(&self, kind: ScriptKind) -> ActiveRun<'_> {
        let gauge = gauge_index(kind).map(|i| &self.active[i]);
        if let Some(gauge) = gauge {
            gauge.fetch_add(1, Ordering::Relaxed);
        }
        ActiveRun { gauge }
    }

    /// Runs in progress, by kind.
    pub fn active_runs(&self) -> Vec<(&'static str, u64)> {
        GAUGED
            .iter()
            .zip(&self.active)
            .map(|(kind, n)| (kind.as_str(), n.load(Ordering::Relaxed)))
            .collect()
    }

    pub fn record(&self, trigger: &str, kind: ScriptKind, layer: Layer, sample: &RunSample) {
        self.record_at(unix_now(), trigger, kind, layer, sample);
    }

    /// As [`record`](Self::record), at `at` seconds since the epoch. Test
    /// files are not recorded, and nothing is while metrics are off.
    pub fn record_at(
        &self,
        at: u64,
        trigger: &str,
        kind: ScriptKind,
        layer: Layer,
        sample: &RunSample,
    ) {
        if kind == ScriptKind::Test || !self.enabled() {
            return;
        }
        let key = Key {
            trigger: trigger.to_string(),
            kind,
            layer,
        };
        let held = self
            .keys
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned();
        let history = match held {
            Some(history) => history,
            None => match self.insert(key, at, true) {
                Some(history) => history,
                None => return,
            },
        };
        history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .add(at, sample);
    }

    /// A key's history, created unless the registry is full. A full registry
    /// first drops keys with nothing left inside the longest window; a key
    /// still refused counts in [`overflow`](Self::overflow) when `count` is
    /// set.
    fn insert(&self, key: Key, at: u64, count: bool) -> Option<Arc<Mutex<History>>> {
        insert_capped(&self.keys, key, at, count.then_some(&self.overflow))
    }

    fn insert_ingest(
        &self,
        key: IngestKey,
        at: u64,
        count: bool,
    ) -> Option<Arc<Mutex<History<IngestBucket>>>> {
        insert_capped(&self.ingest, key, at, count.then_some(&self.overflow))
    }

    fn ingest_entries(&self) -> Vec<(IngestKey, Arc<Mutex<History<IngestBucket>>>)> {
        self.ingest
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    fn persisted_to(&self) -> u64 {
        self.persisted_to.load(Ordering::Relaxed)
    }

    fn entries(&self) -> Vec<(Key, Arc<Mutex<History>>)> {
        self.keys
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn scripts(&self, window: Window) -> Vec<ScriptSummary> {
        self.scripts_at(unix_now(), window)
    }

    /// Every script that ran inside `window` ending at `now`, busiest first.
    pub fn scripts_at(&self, now: u64, window: Window) -> Vec<ScriptSummary> {
        let mut out: Vec<ScriptSummary> = self
            .entries()
            .into_iter()
            .filter_map(|(key, history)| {
                let bucket = merged(
                    &history.lock().unwrap_or_else(|e| e.into_inner()),
                    now,
                    window,
                );
                (bucket.runs > 0).then(|| ScriptSummary {
                    trigger: key.trigger,
                    kind: key.kind.as_str(),
                    layer: key.layer.as_str(),
                    stats: bucket.stats(),
                })
            })
            .collect();
        sort_scripts(&mut out);
        out
    }

    pub fn series(&self, trigger: &str, window: Window, step: Step) -> Option<ScriptSeries> {
        self.series_at(unix_now(), trigger, window, step)
    }

    /// `trigger`'s history inside `window`, one point per `step`, merged over
    /// the layers it ran from. `None` when it has not run inside the window.
    pub fn series_at(
        &self,
        now: u64,
        trigger: &str,
        window: Window,
        step: Step,
    ) -> Option<ScriptSeries> {
        let mut histories: Vec<(Key, History)> = self
            .entries()
            .into_iter()
            .filter(|(key, _)| key.trigger == trigger)
            .map(|(key, history)| {
                let history = history.lock().unwrap_or_else(|e| e.into_inner()).clone();
                (key, history)
            })
            .collect();
        histories.sort_by_key(|(key, _)| key.layer.as_str());
        let mut total = Bucket::default();
        let mut layers = Vec::new();
        for (key, history) in &histories {
            let bucket = merged(history, now, window);
            if bucket.runs > 0 {
                layers.push(key.layer.as_str());
                total.merge(&bucket);
            }
        }
        if total.runs == 0 {
            return None;
        }
        let ring = |h: &History| match step {
            Step::Minute => Some(h.minutes.clone()),
            Step::HalfHour => Some(h.half_hours.clone()),
            Step::Hour | Step::Day => None,
        };
        // Memory holds no hour or day buckets: those are the table's.
        let rings: Vec<window::Ring> = histories
            .iter()
            .map(|(_, h)| ring(h))
            .collect::<Option<_>>()?;
        let points = rings[0]
            .starts(now, window.secs())
            .map(|start| {
                let mut bucket = Bucket::default();
                for ring in &rings {
                    if let Some(b) = ring.get(start) {
                        bucket.merge(b);
                    }
                }
                point(start, &bucket)
            })
            .collect();
        Some(ScriptSeries {
            trigger: trigger.to_string(),
            kind: histories[0].0.kind.as_str(),
            layers,
            stats: total.stats(),
            points,
        })
    }

    pub fn record_ingest(
        &self,
        collection: &str,
        action: &'static str,
        job: Option<&str>,
        sample: &RecordSample,
    ) {
        self.record_ingest_at(unix_now_ms(), collection, action, job, sample);
    }

    /// One record of `collection`'s `action`, finished at `at_ms`, added to
    /// its windows and to backfill job `job`'s stats.
    pub fn record_ingest_at(
        &self,
        at_ms: u64,
        collection: &str,
        action: &'static str,
        job: Option<&str>,
        sample: &RecordSample,
    ) {
        if !self.enabled() {
            return;
        }
        let at = at_ms / 1_000;
        if let Some(job) = job {
            self.with_job(at_ms, job, |stats| stats.add(at_ms, sample));
        }
        let key = IngestKey {
            collection: collection.to_string(),
            action,
        };
        let held = self
            .ingest
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned();
        let history = match held {
            Some(history) => history,
            None => match self.insert_ingest(key, at, true) {
                Some(history) => history,
                None => return,
            },
        };
        history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .add(at, sample);
    }

    pub fn ingest(&self, window: Window) -> Vec<IngestSummary> {
        self.ingest_at(unix_now(), window)
    }

    /// Every collection and action that saw records inside `window` ending
    /// at `now`, busiest first.
    pub fn ingest_at(&self, now: u64, window: Window) -> Vec<IngestSummary> {
        let mut out: Vec<IngestSummary> = self
            .ingest_entries()
            .into_iter()
            .filter_map(|(key, history)| {
                let history = history.lock().unwrap_or_else(|e| e.into_inner());
                let ring = ring_for(&history, window);
                let bucket = ring.merged(now, window.secs());
                let oldest = ring.oldest_in(now, window.secs())?;
                (bucket.records > 0).then(|| {
                    bucket.summary(&key.collection, key.action, now.saturating_sub(oldest) + 1)
                })
            })
            .collect();
        sort_ingest(&mut out);
        out
    }

    /// Run `f` on `job`'s stats, creating them, after dropping every job
    /// idle past [`job::KEPT_MS`].
    fn with_job<T>(&self, now_ms: u64, job: &str, f: impl FnOnce(&mut JobStats) -> T) -> T {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        jobs.retain(|_, stats| stats.is_live(now_ms));
        f(jobs
            .entry(job.to_string())
            .or_insert_with(|| JobStats::new(now_ms)))
    }

    /// Run `f` on `job`'s stats if they are held.
    fn held_job<T>(&self, now_ms: u64, job: &str, f: impl FnOnce(&mut JobStats) -> T) -> Option<T> {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        jobs.retain(|_, stats| stats.is_live(now_ms));
        jobs.get_mut(job).map(f)
    }

    /// `n` records' worth of `elapsed` in `job`'s stage `stage`, as its
    /// executor measured it.
    pub fn observe_job(&self, job: &str, stage: &'static str, elapsed: Duration, n: u64) {
        self.observe_job_at(unix_now_ms(), job, stage, elapsed, n);
    }

    pub fn observe_job_at(
        &self,
        now_ms: u64,
        job: &str,
        stage: &'static str,
        elapsed: Duration,
        n: u64,
    ) {
        if self.enabled() {
            self.with_job(now_ms, job, |stats| {
                stats.observe(now_ms, stage, elapsed, n)
            });
        }
    }

    /// `processed` of `total` done for `job`, for its ETA.
    pub fn job_progress(&self, job: &str, processed: i64, total: Option<i64>) {
        if self.enabled() {
            let now_ms = unix_now_ms();
            self.with_job(now_ms, job, |stats| {
                stats.progress(now_ms, processed, total)
            });
        }
    }

    /// `job`'s stats now, while they are held.
    pub fn job_stats(&self, job: &str) -> Option<JobHookStats> {
        let now_ms = unix_now_ms();
        self.held_job(now_ms, job, |stats| stats.stats(now_ms))
    }

    /// `job`'s stats if they are due to go out ([`JobStats::due`]).
    pub fn job_stats_due(&self, job: &str) -> Option<JobHookStats> {
        self.job_stats_due_at(unix_now_ms(), job)
    }

    pub fn job_stats_due_at(&self, now_ms: u64, job: &str) -> Option<JobHookStats> {
        self.held_job(now_ms, job, |stats| stats.due(now_ms))
            .flatten()
    }

    /// `job`'s final stats. They stay readable until dropped, but are never
    /// due again.
    pub fn finish_job(&self, job: &str) -> Option<JobHookStats> {
        let now_ms = unix_now_ms();
        self.held_job(now_ms, job, |stats| {
            stats.finished = true;
            stats.stats(now_ms)
        })
    }

    /// Every held job that has not finished, most recently active first.
    pub fn live_jobs(&self) -> Vec<LiveJob> {
        let now_ms = unix_now_ms();
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        jobs.retain(|_, stats| stats.is_live(now_ms));
        let mut out: Vec<LiveJob> = jobs
            .iter()
            .filter(|(_, stats)| !stats.finished)
            .map(|(id, stats)| LiveJob {
                job_id: id.clone(),
                idle_s: now_ms.saturating_sub(stats.last_ms) / 1_000,
                hooks: stats.stats(now_ms),
            })
            .collect();
        out.sort_by(|a, b| {
            a.idle_s
                .cmp(&b.idle_s)
                .then_with(|| a.job_id.cmp(&b.job_id))
        });
        out
    }

    /// Re-read the switch. A stored value is validated on the way in, so an
    /// unacceptable one can only come from the environment or a hand-edited
    /// row; it is reported and the default stands. A database that cannot be
    /// read leaves the switch where it is.
    pub async fn refresh(&self, pool: &AnyPool, backend: DatabaseBackend) {
        match try_get_setting(pool, ENABLED_KEY, backend).await {
            Ok(raw) => self.set_enabled(match raw {
                None => true,
                Some(raw) => parse_enabled(&raw).unwrap_or_else(|| {
                    tracing::warn!(
                        key = ENABLED_KEY,
                        value = %raw,
                        "not \"true\" or \"false\"; script metrics stay on"
                    );
                    true
                }),
            }),
            Err(e) => tracing::warn!(
                error = %e,
                "could not read script_metrics_enabled; keeping the current value"
            ),
        }
    }
}

/// The ring a window reads from memory. Memory holds a day at most, so a
/// longer window reads the day here; the admin routes read those from the
/// table instead.
fn ring_for<B: window::Accumulate>(history: &History<B>, window: Window) -> &window::Ring<B> {
    match window {
        Window::Hour => &history.minutes,
        Window::Day | Window::Week | Window::Month => &history.half_hours,
    }
}

fn merged(history: &History, now: u64, window: Window) -> Bucket {
    ring_for(history, window).merged(now, window.secs())
}

/// A key's history in `keys`, created unless [`MAX_KEYS`] are held. A full
/// map first drops keys with nothing left inside the longest window, and a
/// key still refused is counted in `overflow` when one is given.
fn insert_capped<K, B>(
    keys: &RwLock<HashMap<K, Arc<Mutex<History<B>>>>>,
    key: K,
    at: u64,
    overflow: Option<&AtomicU64>,
) -> Option<Arc<Mutex<History<B>>>>
where
    K: std::hash::Hash + Eq,
    B: window::Accumulate,
{
    let mut keys = keys.write().unwrap_or_else(|e| e.into_inner());
    if let Some(history) = keys.get(&key) {
        return Some(history.clone());
    }
    if keys.len() >= MAX_KEYS {
        keys.retain(|_, history| {
            history
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_live(at)
        });
    }
    if keys.len() >= MAX_KEYS {
        if let Some(overflow) = overflow {
            overflow.fetch_add(1, Ordering::Relaxed);
        }
        return None;
    }
    let history = Arc::new(Mutex::new(History::default()));
    keys.insert(key, history.clone());
    Some(history)
}

/// Busiest first.
fn sort_scripts(rows: &mut [ScriptSummary]) {
    rows.sort_by(|a, b| {
        b.stats
            .runs
            .cmp(&a.stats.runs)
            .then_with(|| a.trigger.cmp(&b.trigger))
            .then_with(|| a.layer.cmp(b.layer))
    });
}

/// Busiest first.
fn sort_ingest(rows: &mut [IngestSummary]) {
    rows.sort_by(|a, b| {
        b.records
            .cmp(&a.records)
            .then_with(|| a.collection.cmp(&b.collection))
            .then_with(|| a.action.cmp(b.action))
    });
}

/// One step of a series, starting at `start`.
fn point(start: u64, bucket: &Bucket) -> SeriesPoint {
    let q = |sketch: &sketch::Sketch| PointPercentiles {
        p50: sketch.quantile(0.5).map(|us| us as f64 / 1_000.0),
        p95: sketch.quantile(0.95).map(|us| us as f64 / 1_000.0),
        max: sketch.max().map(|us| us as f64 / 1_000.0),
    };
    let stats = bucket.stats();
    SeriesPoint {
        start: rfc3339(start),
        runs: bucket.runs,
        outcomes: stats.outcomes,
        wall_ms: q(&bucket.wall),
        guest_ms: q(&bucket.guest),
        host_ms_mean: stats.host_ms_mean,
        instantiate_ms_mean: stats.instantiate_ms_mean,
        link_ms_mean: stats.link_ms_mean,
        allocate_ms_mean: stats.allocate_ms_mean,
        teardown_ms_mean: stats.teardown_ms_mean,
    }
}

fn parse_enabled(raw: &str) -> Option<bool> {
    match raw.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// The rule a stored `script_metrics_enabled` must satisfy. Any other key
/// passes.
pub fn validate(key: &str, value: &str) -> Result<(), String> {
    if key == ENABLED_KEY && parse_enabled(value).is_none() {
        return Err(format!("{ENABLED_KEY} must be \"true\" or \"false\""));
    }
    Ok(())
}

/// Keep `metrics`' switch converged with the database for the life of the
/// process.
pub async fn run_refresh_loop(
    metrics: Arc<ScriptMetrics>,
    pool: AnyPool,
    backend: DatabaseBackend,
) {
    loop {
        tokio::time::sleep(REFRESH_INTERVAL).await;
        metrics.refresh(&pool, backend).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{ScriptErrorKind, ScriptValueKind};

    const T0: u64 = 1_793_318_400;

    fn sample(outcome: Outcome) -> RunSample {
        RunSample {
            outcome,
            wall: Duration::from_millis(5),
            prepare: Duration::from_micros(200),
            timings: RunTimings {
                guest_call: Some(Duration::from_millis(4)),
                ..Default::default()
            },
        }
    }

    #[test]
    fn runs_are_kept_per_trigger_kind_and_layer() {
        let m = ScriptMetrics::new();
        let s = sample(Outcome::Ok);
        for _ in 0..3 {
            m.record_at(
                T0,
                "record.create:a.b.c",
                ScriptKind::RecordEvent,
                Layer::Dashboard,
                &s,
            );
        }
        m.record_at(
            T0,
            "record.create:a.b.c",
            ScriptKind::RecordEvent,
            Layer::Project,
            &s,
        );
        m.record_at(
            T0,
            "xrpc.query:a.b.d",
            ScriptKind::XrpcQuery,
            Layer::Dashboard,
            &s,
        );
        let rows = m.scripts_at(T0 + 1, Window::Hour);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].trigger, "record.create:a.b.c");
        assert_eq!(rows[0].layer, "dashboard");
        assert_eq!(rows[0].stats.runs, 3);
        assert_eq!(rows[0].kind, "record_event");

        let series = m
            .series_at(T0 + 1, "record.create:a.b.c", Window::Hour, Step::Minute)
            .unwrap();
        assert_eq!(series.stats.runs, 4);
        assert_eq!(series.layers, vec!["dashboard", "project"]);
        assert_eq!(series.points.len(), 60);
        assert_eq!(series.points.last().unwrap().runs, 4);
        assert!(
            m.series_at(T0 + 1, "nope", Window::Hour, Step::Minute)
                .is_none()
        );
    }

    #[test]
    fn a_script_idle_for_the_window_is_not_listed() {
        let m = ScriptMetrics::new();
        m.record_at(
            T0,
            "t",
            ScriptKind::Label,
            Layer::Dashboard,
            &sample(Outcome::Ok),
        );
        assert_eq!(m.scripts_at(T0 + 2 * 3_600, Window::Hour), vec![]);
        assert_eq!(m.scripts_at(T0 + 2 * 3_600, Window::Day).len(), 1);
    }

    #[test]
    fn test_files_are_not_recorded() {
        let m = ScriptMetrics::new();
        m.record_at(
            T0,
            "test:a",
            ScriptKind::Test,
            Layer::Project,
            &sample(Outcome::Ok),
        );
        assert!(m.scripts_at(T0, Window::Hour).is_empty());
    }

    #[test]
    fn nothing_is_recorded_while_switched_off() {
        let m = ScriptMetrics::new();
        m.set_enabled(false);
        m.record_at(
            T0,
            "t",
            ScriptKind::Job,
            Layer::Dashboard,
            &sample(Outcome::Ok),
        );
        assert!(m.scripts_at(T0, Window::Hour).is_empty());
        m.set_enabled(true);
        m.record_at(
            T0,
            "t",
            ScriptKind::Job,
            Layer::Dashboard,
            &sample(Outcome::Ok),
        );
        assert_eq!(m.scripts_at(T0, Window::Hour).len(), 1);
    }

    #[test]
    fn keys_past_the_cap_are_counted_not_kept() {
        let m = ScriptMetrics::new();
        let s = sample(Outcome::Ok);
        for i in 0..MAX_KEYS + 5 {
            m.record_at(T0, &format!("t{i}"), ScriptKind::Job, Layer::Dashboard, &s);
        }
        assert_eq!(m.overflow(), 5);
        assert_eq!(m.scripts_at(T0, Window::Hour).len(), MAX_KEYS);
        // A key already held still records.
        m.record_at(T0, "t0", ScriptKind::Job, Layer::Dashboard, &s);
        assert_eq!(m.overflow(), 5);

        // Once every held key has left the day, a new one takes their room.
        let later = T0 + 2 * 86_400;
        m.record_at(later, "fresh", ScriptKind::Job, Layer::Dashboard, &s);
        assert_eq!(m.overflow(), 5);
        assert_eq!(m.scripts_at(later, Window::Hour).len(), 1);
    }

    #[test]
    fn the_active_gauge_is_given_back_however_a_run_ends() {
        let m = ScriptMetrics::new();
        let count = |m: &ScriptMetrics| {
            m.active_runs()
                .into_iter()
                .find(|(k, _)| *k == "xrpc_query")
                .unwrap()
                .1
        };
        {
            let _a = m.start_run(ScriptKind::XrpcQuery);
            let _b = m.start_run(ScriptKind::XrpcQuery);
            assert_eq!(count(&m), 2);
        }
        assert_eq!(count(&m), 0);
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _a = m.start_run(ScriptKind::XrpcQuery);
            panic!("the run blew up");
        }));
        assert!(caught.is_err());
        assert_eq!(count(&m), 0);
        let _t = m.start_run(ScriptKind::Test);
        assert!(m.active_runs().iter().all(|(_, n)| *n == 0));
    }

    #[test]
    fn outcomes_name_the_interpreter_and_the_host_failure_alike() {
        let returned = Ok(ScriptExecuteOutput::Returned {
            value: serde_json::Value::Null,
            value_kind: ScriptValueKind::None,
        });
        assert_eq!(Outcome::of(&returned), Outcome::Ok);
        for (kind, outcome) in [
            (ScriptErrorKind::Syntax, "syntax"),
            (ScriptErrorKind::Runtime, "runtime"),
            (ScriptErrorKind::Timeout, "timeout"),
            (ScriptErrorKind::Memory, "memory"),
            (ScriptErrorKind::MissingHandle, "missing_handle"),
        ] {
            let failed = Ok(ScriptExecuteOutput::Error {
                kind,
                message: String::new(),
                line: None,
                raw: String::new(),
                chunk: None,
            });
            assert_eq!(Outcome::of(&failed).as_str(), outcome);
            assert_eq!(kind.as_str(), outcome);
        }
        let host = |e: ExecutionError| Outcome::of(&Err(DispatchError::Execution(e))).as_str();
        assert_eq!(host(ExecutionError::Timeout), "timeout");
        assert_eq!(
            host(ExecutionError::MemoryLimit {
                requested: 1,
                ceiling: 0
            }),
            "memory"
        );
        assert_eq!(
            host(ExecutionError::Trap(wasmtime::Error::msg("x"))),
            "trap"
        );
        assert_eq!(host(ExecutionError::PluginNotFound("p".into())), "error");
        assert_eq!(
            Outcome::of(&Err(DispatchError::NoInterpreter {
                language: "lua".into()
            })),
            Outcome::NoInterpreter
        );
    }

    #[test]
    fn a_jobs_stats_are_dropped_an_hour_after_its_last_report() {
        let m = ScriptMetrics::new();
        let now = unix_now_ms();
        m.observe_job_at(
            now - job::KEPT_MS - 1,
            "old",
            "page_read",
            Duration::ZERO,
            1,
        );
        m.observe_job_at(now, "new", "page_read", Duration::from_millis(1), 1);
        assert!(m.job_stats("old").is_none());
        assert_eq!(m.job_stats("new").unwrap().stages["page_read"].n, 1);
        assert_eq!(m.live_jobs().len(), 1);
        // A finished job keeps its stats but leaves the live view, and is
        // never due again.
        assert!(m.finish_job("new").is_some());
        assert!(m.live_jobs().is_empty());
        assert!(m.job_stats("new").is_some());
        m.observe_job_at(now + 5_000, "new", "page_read", Duration::ZERO, 1);
        assert!(m.job_stats_due_at(now + 5_000, "new").is_none());
    }

    #[test]
    fn a_due_job_goes_out_once_a_second() {
        let m = ScriptMetrics::new();
        let now = unix_now_ms();
        m.observe_job_at(now, "j", "write", Duration::from_millis(3), 1);
        assert!(m.job_stats_due_at(now, "j").is_some());
        m.observe_job_at(now + 10, "j", "write", Duration::from_millis(3), 1);
        assert!(m.job_stats_due_at(now + 10, "j").is_none());
        assert_eq!(
            m.job_stats_due_at(now + 1_000, "j").unwrap().stages["write"].n,
            2
        );
        assert!(m.job_stats_due_at(now + 9_000, "nope").is_none());
    }

    #[test]
    fn the_switch_takes_only_true_or_false() {
        assert_eq!(validate(ENABLED_KEY, "true"), Ok(()));
        assert_eq!(validate(ENABLED_KEY, "false"), Ok(()));
        for bad in ["yes", "1", "", "TRUE", "off"] {
            assert!(validate(ENABLED_KEY, bad).is_err(), "{bad:?}");
        }
        assert_eq!(validate("app_name", "yes"), Ok(()));
    }

    /// The grant runs against a database that already has users, so it is
    /// run again here over one: whoever reads events gains metrics, nobody
    /// else does, and running it twice changes nothing.
    #[tokio::test]
    async fn the_migration_grants_metrics_read_to_event_readers() {
        let pool = crate::test_support::migrated_memory_pool().await;
        for (id, perms) in [("u1", &["events:read"][..]), ("u2", &["stats:read"][..])] {
            crate::db::query(
                "INSERT INTO happyview_users (id, did, is_super, created_at) VALUES (?, ?, 0, ?)",
            )
            .bind(id)
            .bind(format!("did:plc:{id}"))
            .bind("2026-01-01T00:00:00+00:00")
            .execute(&pool)
            .await
            .unwrap();
            for perm in perms {
                crate::db::query(
                    "INSERT INTO happyview_user_permissions (user_id, permission, granted_at) VALUES (?, ?, ?)",
                )
                .bind(id)
                .bind(*perm)
                .bind("2026-01-01T00:00:00+00:00")
                .execute(&pool)
                .await
                .unwrap();
            }
        }
        let grant =
            include_str!("../../migrations/sqlite/20261031000000_metrics_read_permission.sql");
        for _ in 0..2 {
            crate::db::query(grant).execute(&pool).await.unwrap();
        }
        let holders: Vec<(String,)> = crate::db::query_as(
            "SELECT user_id FROM happyview_user_permissions WHERE permission = 'metrics:read'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(holders, vec![("u1".to_string(),)]);
        assert_eq!(
            include_str!("../../migrations/postgres/20261031000000_metrics_read_permission.sql"),
            grant
        );
    }

    #[tokio::test]
    async fn the_switch_resolves_from_the_database() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let m = ScriptMetrics::new();
        m.refresh(&pool, DatabaseBackend::Sqlite).await;
        assert!(m.enabled());
        crate::admin::settings::put_setting(&pool, ENABLED_KEY, "false", DatabaseBackend::Sqlite)
            .await
            .unwrap();
        m.refresh(&pool, DatabaseBackend::Sqlite).await;
        assert!(!m.enabled());
    }
}

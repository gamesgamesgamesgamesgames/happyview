//! What one record costs on its way through ingest, around and inside its
//! hook.
//!
//! Ingest starts a [`RecordTimer`] for each create, update or delete and
//! runs the record's hook inside it with [`RecordTimer::hooking`]. The
//! lookup and the dispatch underneath report into the task-local collector
//! that opens, through the free functions here, which do nothing outside a
//! record, so a dispatch that is not a record hook pays one failed read. The
//! write comes later, often for a whole batch at once, and is handed to
//! [`RecordTimer::finish`].

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use happyview_provider::{Origin, RecordEvent, StageStats};
use serde::Serialize;

use super::run::RunTimings;
use super::sketch::Sketch;
use super::stages::{self, Stages};
use super::window::{Accumulate, Percentiles, mean_ms, micros};
use super::{Outcome, ScriptMetrics};

/// What ingest decided for a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordDecision {
    /// Stored, or deleted, as it arrived.
    Proceed,
    /// Stored with the body the hook returned.
    Replace,
    /// Not stored: the hook returned nothing.
    Skip,
    /// Not stored: the body does not hash to its claimed CID.
    CidMismatch,
}

/// Every decision's name, in [`RecordDecision::index`] order.
pub const DECISIONS: [&str; 4] = ["proceed", "replace", "skip", "cid_mismatch"];

impl RecordDecision {
    pub fn index(self) -> usize {
        self as usize
    }

    pub fn as_str(self) -> &'static str {
        DECISIONS[self.index()]
    }

    pub fn stored(self) -> bool {
        matches!(self, Self::Proceed | Self::Replace)
    }
}

/// One record, finished.
#[derive(Clone, Debug)]
pub struct RecordSample {
    pub decision: RecordDecision,
    /// Its hook phase and its share of the write. Time it spent waiting for
    /// the rest of its batch is not in it.
    pub record: Duration,
    /// The hook's guest code and the host calls it made, over every attempt.
    /// `None` when no hook ran.
    pub script: Option<Duration>,
    pub stages: Vec<(&'static str, Duration)>,
    pub failed: bool,
    pub timed_out: bool,
    pub dead_lettered: bool,
}

#[derive(Debug, Default)]
struct HookTimes {
    resolve: Duration,
    /// Whether the lookup found a script.
    resolved: bool,
    script: Option<Duration>,
    /// The runs' instance costs, over every attempt.
    link: Duration,
    allocate: Duration,
    teardown: Duration,
    failed: bool,
    timed_out: bool,
    dead_lettered: bool,
}

#[derive(Debug, Default)]
pub struct RecordCollector(Mutex<HookTimes>);

impl RecordCollector {
    fn lock(&self) -> std::sync::MutexGuard<'_, HookTimes> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

tokio::task_local! {
    static RECORD: Arc<RecordCollector>;
}

fn with(f: impl FnOnce(&mut HookTimes)) {
    let _ = RECORD.try_with(|collector| f(&mut collector.lock()));
}

/// The hook's script lookup took `elapsed`, and found one or not.
pub fn note_resolve(elapsed: Duration, found: bool) {
    with(|t| {
        t.resolve += elapsed;
        t.resolved |= found;
    });
}

/// One attempt of the hook ended in `outcome`, having measured `timings`.
/// Its script time is its guest code plus the host calls it made.
pub fn note_run(outcome: Outcome, timings: &RunTimings) {
    with(|t| {
        if let Some(guest) = timings.guest() {
            t.script = Some(t.script.unwrap_or_default() + guest + timings.host_total);
        }
        t.link += timings.link.unwrap_or_default();
        t.allocate += timings.allocate.unwrap_or_default();
        t.teardown += timings.teardown.unwrap_or_default();
        if outcome != Outcome::Ok {
            t.failed = true;
        }
        if outcome == Outcome::Timeout {
            t.timed_out = true;
        }
    });
}

/// The hook was given up on and dead-lettered.
pub fn note_dead_lettered() {
    with(|t| {
        t.failed = true;
        t.dead_lettered = true;
    });
}

/// One record being measured, or nothing while metrics are off.
pub struct RecordTimer(Option<Box<Timing>>);

struct Timing {
    collection: String,
    action: &'static str,
    job: Option<String>,
    started: Instant,
    collector: Arc<RecordCollector>,
    stages: Vec<(&'static str, Duration)>,
    decision: RecordDecision,
    /// How long the record took up to its decision.
    hooked: Option<Duration>,
}

impl RecordTimer {
    pub fn start(metrics: &ScriptMetrics, ev: &RecordEvent) -> Self {
        if !metrics.enabled() {
            return Self(None);
        }
        Self(Some(Box::new(Timing {
            collection: ev.key.collection.clone(),
            action: ev.action.as_str(),
            job: match &ev.origin {
                Origin::Backfill { job_id } => Some(job_id.clone()),
                Origin::Live | Origin::Retry => None,
            },
            started: Instant::now(),
            collector: Arc::default(),
            stages: Vec::new(),
            decision: RecordDecision::Proceed,
            hooked: None,
        })))
    }

    /// A timer that measures nothing.
    pub fn off() -> Self {
        Self(None)
    }

    /// Run the record's hook, `fut`: its lookup is `resolve`, the rest is
    /// `hook` when a script was found.
    pub async fn hooking<F: Future>(&mut self, fut: F) -> F::Output {
        let Some(timing) = &mut self.0 else {
            return fut.await;
        };
        let started = Instant::now();
        let out = RECORD.scope(timing.collector.clone(), fut).await;
        let elapsed = started.elapsed();
        let times = timing.collector.lock();
        timing.stages.push((stages::RESOLVE, times.resolve));
        if times.resolved {
            timing
                .stages
                .push((stages::HOOK, elapsed.saturating_sub(times.resolve)));
            // Parts of `hook` rather than more of it: what building and
            // dropping the runs' instances cost, which is the platform's.
            for (name, d) in [
                (stages::HOOK_LINK, times.link),
                (stages::HOOK_ALLOCATE, times.allocate),
                (stages::HOOK_TEARDOWN, times.teardown),
            ] {
                if !d.is_zero() {
                    timing.stages.push((name, d));
                }
            }
        }
        drop(times);
        out
    }

    pub fn stage(&mut self, name: &'static str, elapsed: Duration) {
        if let Some(timing) = &mut self.0 {
            timing.stages.push((name, elapsed));
        }
    }

    /// The hook phase ends with `decision`.
    pub fn decided(&mut self, decision: RecordDecision) {
        if let Some(timing) = &mut self.0 {
            timing.decision = decision;
            timing.hooked = Some(timing.started.elapsed());
        }
    }

    /// The record is done, with `write` the stage and time of its write when
    /// it had one.
    pub fn finish(self, metrics: &ScriptMetrics, write: Option<(&'static str, Duration)>) {
        let Some(timing) = self.0 else {
            return;
        };
        let Timing {
            collection,
            action,
            job,
            started,
            collector,
            mut stages,
            decision,
            hooked,
        } = *timing;
        let hooked = hooked.unwrap_or_else(|| started.elapsed());
        let mut record = hooked;
        if let Some((name, elapsed)) = write {
            stages.push((name, elapsed));
            record += elapsed;
        }
        let times = collector.lock();
        let sample = RecordSample {
            decision,
            record,
            script: times.script,
            stages,
            failed: times.failed,
            timed_out: times.timed_out,
            dead_lettered: times.dead_lettered,
        };
        drop(times);
        metrics.record_ingest(&collection, action, job.as_deref(), &sample);
    }
}

/// Everything recorded for one collection and action over one bucket's span.
#[derive(Clone, Debug, Default)]
pub struct IngestBucket {
    pub records: u64,
    pub decisions: [u64; DECISIONS.len()],
    pub failed: u64,
    pub timeouts: u64,
    pub dead_lettered: u64,
    pub record_us: u64,
    pub record: Sketch,
    /// Summed over every record, a record no hook ran for adding nothing, so
    /// that over `record_us` it is the share of the pipeline's time scripts
    /// had.
    pub script_us: u64,
    pub scripted: u64,
    pub stages: Stages,
}

impl Accumulate for IngestBucket {
    type Sample = RecordSample;

    fn add_sample(&mut self, sample: &RecordSample) {
        self.records += 1;
        self.decisions[sample.decision.index()] += 1;
        self.failed += u64::from(sample.failed);
        self.timeouts += u64::from(sample.timed_out);
        self.dead_lettered += u64::from(sample.dead_lettered);
        let record = micros(sample.record);
        self.record_us = self.record_us.saturating_add(record);
        self.record.insert(record);
        if let Some(script) = sample.script {
            self.script_us = self.script_us.saturating_add(micros(script));
            self.scripted += 1;
        }
        for (name, elapsed) in &sample.stages {
            self.stages.add(name, *elapsed, 1);
        }
    }

    fn merge_from(&mut self, other: &Self) {
        self.records += other.records;
        for (mine, theirs) in self.decisions.iter_mut().zip(other.decisions) {
            *mine += theirs;
        }
        self.failed += other.failed;
        self.timeouts += other.timeouts;
        self.dead_lettered += other.dead_lettered;
        self.record_us = self.record_us.saturating_add(other.record_us);
        self.record.merge(&other.record);
        self.script_us = self.script_us.saturating_add(other.script_us);
        self.scripted += other.scripted;
        self.stages.merge(&other.stages);
    }
}

pub(crate) fn decision_counts(decisions: &[u64; DECISIONS.len()]) -> BTreeMap<&'static str, u64> {
    DECISIONS
        .iter()
        .zip(decisions)
        .filter(|(_, n)| **n > 0)
        .map(|(name, n)| (*name, *n))
        .collect()
}

/// Script time over record time, when any record time was measured.
pub(crate) fn share(script_us: u64, record_us: u64) -> Option<f64> {
    (record_us > 0).then(|| (script_us as f64 / record_us as f64).min(1.0))
}

/// One collection and action's row in a window.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct IngestSummary {
    pub collection: String,
    pub action: &'static str,
    pub records: u64,
    /// Over the part of the window that saw records: from the start of its
    /// oldest bucket that holds any, so an instance up for ten minutes is
    /// not averaged over the hour.
    pub records_per_s: f64,
    pub record_ms: Percentiles,
    pub stages: BTreeMap<String, StageStats>,
    pub outcomes: BTreeMap<&'static str, u64>,
    pub failed: u64,
    pub timeouts: u64,
    pub dead_lettered: u64,
    /// Over the records a hook ran for.
    pub script_ms_mean: Option<f64>,
    pub script_share: Option<f64>,
    /// Everything else in the record's time: the lookup, the CID check, the
    /// write, and in the hook, building the input and the instance, the
    /// linker, settling and teardown.
    pub platform_share: Option<f64>,
}

impl IngestBucket {
    pub fn summary(&self, collection: &str, action: &'static str, span_secs: u64) -> IngestSummary {
        IngestSummary {
            collection: collection.to_string(),
            action,
            records: self.records,
            records_per_s: self.records as f64 / span_secs.max(1) as f64,
            record_ms: Percentiles::of(&self.record, self.record_us),
            stages: self.stages.stats(),
            outcomes: decision_counts(&self.decisions),
            failed: self.failed,
            timeouts: self.timeouts,
            dead_lettered: self.dead_lettered,
            script_ms_mean: mean_ms(self.script_us, self.scripted),
            script_share: share(self.script_us, self.record_us),
            platform_share: share(self.script_us, self.record_us).map(|s| 1.0 - s),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use happyview_provider::{Action, RecordKey};

    fn event(origin: Origin) -> RecordEvent {
        RecordEvent {
            key: RecordKey {
                did: "did:plc:a".into(),
                collection: "app.test.a".into(),
                rkey: "r".into(),
            },
            action: Action::Create,
            cid: None,
            rev: None,
            body: None,
            time_us: None,
            origin,
            stored: false,
        }
    }

    #[tokio::test]
    async fn a_hooked_record_is_timed_by_stage() {
        let metrics = ScriptMetrics::new();
        let mut timer = RecordTimer::start(
            &metrics,
            &event(Origin::Backfill {
                job_id: "job".into(),
            }),
        );
        timer.stage(stages::CID_CHECK, Duration::from_micros(40));
        timer
            .hooking(async {
                note_resolve(Duration::from_micros(100), true);
                tokio::time::sleep(Duration::from_millis(5)).await;
                let run = |guest_us, link_us| RunTimings {
                    guest_call: Some(Duration::from_micros(guest_us)),
                    link: Some(Duration::from_micros(link_us)),
                    allocate: Some(Duration::from_micros(400)),
                    teardown: Some(Duration::from_micros(50)),
                    ..Default::default()
                };
                note_run(Outcome::Runtime, &run(300, 100));
                note_run(Outcome::Ok, &run(200, 100));
            })
            .await;
        timer.decided(RecordDecision::Replace);
        timer.finish(&metrics, Some((stages::WRITE, Duration::from_millis(2))));

        let rows = metrics.ingest(super::super::Window::Hour);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.collection, "app.test.a");
        assert_eq!(row.action, "create");
        assert_eq!(row.records, 1);
        assert_eq!(row.outcomes, BTreeMap::from([("replace", 1)]));
        assert_eq!(row.failed, 1);
        assert_eq!(row.timeouts, 0);
        assert_eq!(row.script_ms_mean, Some(0.5));
        assert_eq!(row.stages["resolve"].ms_mean, Some(0.1));
        assert_eq!(row.stages["cid_check"].ms_mean, Some(0.04));
        assert_eq!(row.stages["write"].ms_mean, Some(2.0));
        assert!(row.stages["hook"].ms_mean.unwrap() >= 4.9);
        assert_eq!(row.stages["hook.link"].ms_mean, Some(0.2));
        assert_eq!(row.stages["hook.allocate"].ms_mean, Some(0.8));
        assert_eq!(row.stages["hook.teardown"].ms_mean, Some(0.1));
        let script = row.script_share.unwrap();
        assert!((row.platform_share.unwrap() - (1.0 - script)).abs() < 1e-9);
        assert!(row.record_ms.p50.unwrap() >= 7.0);

        let job = metrics.job_stats("job").expect("the job's stats");
        assert_eq!(job.done, 1);
        assert_eq!(job.failed, 1);
        assert_eq!(job.outcomes["replace"], 1);
    }

    #[tokio::test]
    async fn a_record_with_no_hook_has_no_hook_stage_and_no_script_time() {
        let metrics = ScriptMetrics::new();
        let mut timer = RecordTimer::start(&metrics, &event(Origin::Live));
        timer
            .hooking(async { note_resolve(Duration::from_micros(10), false) })
            .await;
        timer.decided(RecordDecision::Proceed);
        timer.finish(&metrics, None);
        let row = &metrics.ingest(super::super::Window::Hour)[0];
        assert!(!row.stages.contains_key("hook"));
        assert_eq!(row.stages["resolve"].n, 1);
        assert_eq!(row.script_ms_mean, None);
        assert_eq!(row.script_share, Some(0.0));
        assert!(metrics.live_jobs().is_empty());
    }

    #[tokio::test]
    async fn nothing_is_measured_while_metrics_are_off() {
        let metrics = ScriptMetrics::new();
        metrics.set_enabled(false);
        let mut timer = RecordTimer::start(&metrics, &event(Origin::Live));
        timer.hooking(async { note_dead_lettered() }).await;
        timer.finish(&metrics, None);
        assert!(metrics.ingest(super::super::Window::Hour).is_empty());
    }

    #[tokio::test]
    async fn reports_outside_a_record_change_nothing() {
        note_resolve(Duration::from_millis(1), true);
        note_run(Outcome::Timeout, &RunTimings::default());
        note_dead_lettered();
        assert!(RECORD.try_with(|_| ()).is_err());
    }

    #[test]
    fn a_dead_letter_is_a_failure_and_a_timeout_is_counted_apart() {
        let mut bucket = IngestBucket::default();
        let sample = |failed, timed_out, dead_lettered| RecordSample {
            decision: RecordDecision::Proceed,
            record: Duration::from_millis(10),
            script: Some(Duration::from_millis(4)),
            stages: vec![],
            failed,
            timed_out,
            dead_lettered,
        };
        bucket.add_sample(&sample(true, true, true));
        bucket.add_sample(&sample(false, false, false));
        let summary = bucket.summary("c", "create", 10);
        assert_eq!(
            (summary.failed, summary.timeouts, summary.dead_lettered),
            (1, 1, 1)
        );
        assert_eq!(summary.records_per_s, 0.2);
        assert_eq!(summary.script_share, Some(0.4));
    }
}

//! A backfill job's hook stats: its records' costs, its rate and its ETA.
//!
//! Times are milliseconds since the epoch, passed in, so the rate and the
//! throttle can be driven by a test.

use std::time::Duration;

use happyview_provider::JobHookStats;

use super::ingest::{DECISIONS, RecordSample, decision_counts, share};
use super::sketch::Sketch;
use super::stages::Stages;
use super::window::{micros, ms};

/// How far back the rate looks, roughly: the EWMA's time constant.
pub const RATE_WINDOW_MS: u64 = 30_000;
/// The rate is folded at most this often, so a burst of records within one
/// tick is a count rather than an instantaneous rate of millions a second.
pub const RATE_TICK_MS: u64 = 1_000;
/// Hook stats go out at most this often per job.
pub const PUBLISH_EVERY_MS: u64 = 1_000;
/// A job's stats are kept this long after its last record, then dropped.
pub const KEPT_MS: u64 = 3_600_000;

/// An exponentially weighted rate per second, folded once a tick.
#[derive(Clone, Debug)]
pub struct Ewma {
    rate: Option<f64>,
    tick_ms: u64,
    pending: f64,
}

impl Ewma {
    pub fn new(now_ms: u64) -> Self {
        Self {
            rate: None,
            tick_ms: now_ms,
            pending: 0.0,
        }
    }

    /// `n` more as of `now_ms`, counted in the tick that ends then.
    pub fn add(&mut self, now_ms: u64, n: f64) {
        self.pending += n;
        self.fold(now_ms);
    }

    /// Fold what arrived since the last tick in, once a tick has passed. A
    /// long gap folds as one low reading, which is what decays the rate of a
    /// job that has gone quiet.
    fn fold(&mut self, now_ms: u64) {
        let dt = now_ms.saturating_sub(self.tick_ms);
        if dt < RATE_TICK_MS {
            return;
        }
        let instant = self.pending * 1_000.0 / dt as f64;
        let alpha = 1.0 - (-(dt as f64) / RATE_WINDOW_MS as f64).exp();
        self.rate = Some(match self.rate {
            None => instant,
            Some(rate) => rate + alpha * (instant - rate),
        });
        self.pending = 0.0;
        self.tick_ms = now_ms;
    }

    /// The rate as of `now_ms`, `None` before the first tick.
    pub fn read(&self, now_ms: u64) -> Option<f64> {
        let mut at = self.clone();
        at.fold(now_ms);
        at.rate
    }
}

/// Where the job said it was, in its own units ([`JobStats::progress`]).
#[derive(Clone, Debug)]
struct Progress {
    processed: i64,
    total: Option<i64>,
    rate: Ewma,
    first_ms: u64,
    first_processed: i64,
}

#[derive(Clone, Debug)]
pub struct JobStats {
    first_ms: u64,
    pub(crate) last_ms: u64,
    done: u64,
    decisions: [u64; DECISIONS.len()],
    failed: u64,
    timeouts: u64,
    dead_lettered: u64,
    record_us: u64,
    record: Sketch,
    script_us: u64,
    script: Sketch,
    stages: Stages,
    rate: Ewma,
    progress: Option<Progress>,
    published_ms: Option<u64>,
    changed: bool,
    pub(crate) finished: bool,
}

impl JobStats {
    pub fn new(now_ms: u64) -> Self {
        Self {
            first_ms: now_ms,
            last_ms: now_ms,
            done: 0,
            decisions: Default::default(),
            failed: 0,
            timeouts: 0,
            dead_lettered: 0,
            record_us: 0,
            record: Sketch::default(),
            script_us: 0,
            script: Sketch::default(),
            stages: Stages::default(),
            rate: Ewma::new(now_ms),
            progress: None,
            published_ms: None,
            changed: false,
            finished: false,
        }
    }

    fn touch(&mut self, now_ms: u64) {
        self.last_ms = self.last_ms.max(now_ms);
        self.changed = true;
    }

    pub fn add(&mut self, now_ms: u64, sample: &RecordSample) {
        self.touch(now_ms);
        self.done += 1;
        self.decisions[sample.decision.index()] += 1;
        self.failed += u64::from(sample.failed);
        self.timeouts += u64::from(sample.timed_out);
        self.dead_lettered += u64::from(sample.dead_lettered);
        let record = micros(sample.record);
        self.record_us = self.record_us.saturating_add(record);
        self.record.insert(record);
        if let Some(script) = sample.script {
            let script = micros(script);
            self.script_us = self.script_us.saturating_add(script);
            self.script.insert(script);
        }
        for (name, elapsed) in &sample.stages {
            self.stages.add(name, *elapsed, 1);
        }
        self.rate.add(now_ms, 1.0);
    }

    /// A stage the job's executor measured itself.
    pub fn observe(&mut self, now_ms: u64, stage: &'static str, elapsed: Duration, n: u64) {
        self.touch(now_ms);
        self.stages.add(stage, elapsed, n);
    }

    /// `processed` of `total` done, in whatever the job counts. The ETA is
    /// reckoned from how fast `processed` moves rather than from the hook
    /// rate, so it is right whatever the unit.
    pub fn progress(&mut self, now_ms: u64, processed: i64, total: Option<i64>) {
        self.touch(now_ms);
        match &mut self.progress {
            Some(p) => {
                let moved = processed.saturating_sub(p.processed).max(0);
                p.rate.add(now_ms, moved as f64);
                p.processed = processed;
                p.total = total.or(p.total);
            }
            None => {
                self.progress = Some(Progress {
                    processed,
                    total,
                    rate: Ewma::new(now_ms),
                    first_ms: now_ms,
                    first_processed: processed,
                });
            }
        }
    }

    fn eta_s(&self, now_ms: u64) -> Option<f64> {
        let p = self.progress.as_ref()?;
        let left = p.total?.saturating_sub(p.processed);
        if left <= 0 {
            return Some(0.0);
        }
        let elapsed_s = now_ms.saturating_sub(p.first_ms) as f64 / 1_000.0;
        let moved = p.processed.saturating_sub(p.first_processed);
        let rate = p
            .rate
            .read(now_ms)
            .filter(|r| *r > 0.0)
            .or_else(|| (elapsed_s > 0.0 && moved > 0).then(|| moved as f64 / elapsed_s))?;
        Some(left as f64 / rate)
    }

    pub fn stats(&self, now_ms: u64) -> JobHookStats {
        // Floored at a second, so a job a few milliseconds old does not
        // report thousands of records a second.
        let elapsed_s = (now_ms.saturating_sub(self.first_ms) as f64 / 1_000.0).max(1.0);
        let avg = self.done as f64 / elapsed_s;
        let q50 = |sketch: &Sketch| sketch.quantile(0.5).map(ms);
        JobHookStats {
            done: self.done,
            failed: self.failed,
            timeouts: self.timeouts,
            dead_lettered: self.dead_lettered,
            skipped: self.decisions[super::ingest::RecordDecision::Skip.index()]
                + self.decisions[super::ingest::RecordDecision::CidMismatch.index()],
            outcomes: decision_counts(&self.decisions)
                .into_iter()
                .map(|(name, n)| (name.to_string(), n))
                .collect(),
            rate_per_s: self.rate.read(now_ms).unwrap_or(avg),
            avg_rate_per_s: avg,
            eta_s: self.eta_s(now_ms),
            record_ms_p50: q50(&self.record),
            script_ms_p50: q50(&self.script),
            script_share: share(self.script_us, self.record_us),
            stages: self.stages.stats(),
        }
    }

    /// The stats, when something changed since they last went out and
    /// [`PUBLISH_EVERY_MS`] has passed since then. Never once finished.
    pub fn due(&mut self, now_ms: u64) -> Option<JobHookStats> {
        if self.finished || !self.changed {
            return None;
        }
        if self
            .published_ms
            .is_some_and(|at| now_ms.saturating_sub(at) < PUBLISH_EVERY_MS)
        {
            return None;
        }
        self.published_ms = Some(now_ms);
        self.changed = false;
        Some(self.stats(now_ms))
    }

    /// Whether to keep the stats at `now_ms`.
    pub fn is_live(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.last_ms) < KEPT_MS
    }
}

#[cfg(test)]
mod tests {
    use super::super::ingest::RecordDecision;
    use super::*;

    const T0: u64 = 1_793_318_400_000;

    fn record(decision: RecordDecision, record_ms: u64, script_ms: Option<u64>) -> RecordSample {
        RecordSample {
            decision,
            record: Duration::from_millis(record_ms),
            script: script_ms.map(Duration::from_millis),
            stages: vec![("write", Duration::from_millis(1))],
            failed: false,
            timed_out: false,
            dead_lettered: false,
        }
    }

    #[test]
    fn the_rate_settles_on_a_steady_pace_and_decays_when_it_stops() {
        let mut job = JobStats::new(T0);
        // 400 records a second for a minute.
        for ms in 0..60_000 {
            if ms % 5 == 0 {
                for _ in 0..2 {
                    job.add(T0 + ms, &record(RecordDecision::Proceed, 2, Some(0)));
                }
            }
        }
        let stats = job.stats(T0 + 60_000);
        assert!(
            (stats.rate_per_s - 400.0).abs() < 1.0,
            "{}",
            stats.rate_per_s
        );
        assert!((stats.avg_rate_per_s - 400.0).abs() < 1.0);

        // Thirty seconds of nothing: the rate falls to about a third, and the
        // average since the start only to two thirds.
        let quiet = job.stats(T0 + 90_000);
        assert!(
            quiet.rate_per_s > 140.0 && quiet.rate_per_s < 160.0,
            "{}",
            quiet.rate_per_s
        );
        assert!((quiet.avg_rate_per_s - 266.7).abs() < 1.0);
    }

    #[test]
    fn before_a_tick_the_rate_is_the_average() {
        let mut job = JobStats::new(T0);
        for _ in 0..3 {
            job.add(T0 + 10, &record(RecordDecision::Proceed, 1, None));
        }
        let stats = job.stats(T0 + 20);
        assert_eq!(stats.rate_per_s, 3.0);
        assert_eq!(stats.avg_rate_per_s, 3.0);
    }

    #[test]
    fn the_eta_comes_from_the_jobs_own_progress() {
        let mut job = JobStats::new(T0);
        assert_eq!(job.stats(T0).eta_s, None);
        job.progress(T0, 0, Some(1_000));
        assert_eq!(job.stats(T0).eta_s, None, "nothing has moved yet");
        for s in 1..=10 {
            job.progress(T0 + s * 1_000, s as i64 * 10, Some(1_000));
        }
        // 10 a second with 900 left.
        let eta = job.stats(T0 + 10_000).eta_s.unwrap();
        assert!((eta - 90.0).abs() < 0.5, "{eta}");

        job.progress(T0 + 11_000, 1_000, None);
        assert_eq!(job.stats(T0 + 11_000).eta_s, Some(0.0));

        let mut crawl = JobStats::new(T0);
        crawl.progress(T0, 5, None);
        crawl.progress(T0 + 2_000, 50, None);
        assert_eq!(crawl.stats(T0 + 2_000).eta_s, None, "no total, no ETA");
    }

    #[test]
    fn stats_go_out_at_most_once_a_second_and_only_when_changed() {
        let mut job = JobStats::new(T0);
        assert!(job.due(T0).is_none(), "nothing yet");
        job.add(T0, &record(RecordDecision::Proceed, 1, None));
        assert_eq!(job.due(T0).unwrap().done, 1);
        job.add(T0 + 500, &record(RecordDecision::Skip, 1, None));
        assert!(job.due(T0 + 999).is_none(), "under a second since");
        let due = job.due(T0 + 1_000).unwrap();
        assert_eq!((due.done, due.skipped), (2, 1));
        assert!(job.due(T0 + 5_000).is_none(), "nothing changed");
        job.observe(T0 + 6_000, "page_read", Duration::from_millis(50), 500);
        assert!(job.due(T0 + 6_000).is_some());
        job.add(T0 + 8_000, &record(RecordDecision::Proceed, 1, None));
        job.finished = true;
        assert!(job.due(T0 + 9_000).is_none(), "a finished job says no more");
    }

    #[test]
    fn a_skipped_stage_does_not_drag_the_write_mean() {
        let mut job = JobStats::new(T0);
        job.observe(T0, "write", Duration::from_millis(30), 1);
        job.observe(T0, "write", Duration::from_millis(10), 1);
        job.observe(T0, "write_skipped", Duration::ZERO, 998);
        let stages = job.stats(T0).stages;
        assert_eq!(stages["write"].n, 2);
        assert_eq!(stages["write"].ms_mean, Some(20.0));
        assert_eq!(stages["write_skipped"].n, 998);
        assert_eq!(stages["write_skipped"].ms_mean, None);
    }

    #[test]
    fn script_share_and_percentiles_come_from_the_records() {
        let mut job = JobStats::new(T0);
        for _ in 0..10 {
            job.add(T0, &record(RecordDecision::Proceed, 10, Some(1)));
        }
        let stats = job.stats(T0);
        assert_eq!(stats.record_ms_p50, Some(10.0));
        assert_eq!(stats.script_ms_p50, Some(1.0));
        assert_eq!(stats.script_share, Some(0.1));
        assert_eq!(stats.stages["write"].n, 10);
    }

    #[test]
    fn a_job_is_kept_for_an_hour_after_its_last_record() {
        let mut job = JobStats::new(T0);
        job.add(T0 + 1_000, &record(RecordDecision::Proceed, 1, None));
        assert!(job.is_live(T0 + 1_000 + KEPT_MS - 1));
        assert!(!job.is_live(T0 + 1_000 + KEPT_MS));
    }
}

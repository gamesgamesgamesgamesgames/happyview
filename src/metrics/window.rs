//! Rolling windows of run metrics: buckets that close as time moves on.
//!
//! A ring of fixed-width buckets is rotated on write. A write into a slot
//! whose bucket started in an earlier lap replaces it, and a read skips any
//! bucket that has fallen out of the window, so nothing needs a timer.

use serde::Serialize;

use super::run::RunTimings;
use super::sketch::Sketch;
use super::{OUTCOMES, Outcome, RunSample};

pub const MINUTE_SECS: u64 = 60;
pub const HALF_HOUR_SECS: u64 = 30 * 60;
/// The last hour, a minute at a time.
pub const MINUTES_KEPT: usize = 60;
/// The last day, half an hour at a time.
pub const HALF_HOURS_KEPT: usize = 48;

pub(crate) fn micros(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

/// Everything recorded for one key over one bucket's span. Every mean is
/// over the runs that reached the field, so a run that found no interpreter
/// does not pull the instantiation mean towards zero.
#[derive(Clone, Debug, Default)]
pub struct Bucket {
    pub runs: u64,
    pub outcomes: [u64; OUTCOMES.len()],
    pub wall_us: u64,
    pub prepare_us: u64,
    pub instantiate_us: u64,
    /// `link` and `allocate` are summed over the runs that were instantiated,
    /// as `instantiate` is.
    pub link_us: u64,
    pub allocate_us: u64,
    pub instantiated: u64,
    pub teardown_us: u64,
    pub torn_down: u64,
    /// Guest and host time are summed over the runs that called the guest.
    pub guest_us: u64,
    pub host_us: u64,
    pub called: u64,
    pub settle_us: u64,
    pub settled: u64,
    pub host_imports: Vec<HostImportSum>,
    pub memory_peak_max: u64,
    pub memory_peak_sum: u64,
    pub memory_measured: u64,
    pub fuel_sum: u64,
    pub fuel_measured: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub wall: Sketch,
    pub guest: Sketch,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HostImportSum {
    pub name: &'static str,
    pub us: u64,
    pub calls: u64,
}

impl Bucket {
    pub fn add(&mut self, sample: &RunSample) {
        let t: &RunTimings = &sample.timings;
        self.runs += 1;
        self.outcomes[sample.outcome.index()] += 1;
        let wall = micros(sample.wall);
        self.wall_us = self.wall_us.saturating_add(wall);
        self.wall.insert(wall);
        self.prepare_us = self.prepare_us.saturating_add(micros(sample.prepare));
        if let Some(d) = t.instantiate {
            self.instantiate_us = self.instantiate_us.saturating_add(micros(d));
            self.link_us = self
                .link_us
                .saturating_add(micros(t.link.unwrap_or_default()));
            self.allocate_us = self
                .allocate_us
                .saturating_add(micros(t.allocate.unwrap_or(d)));
            self.instantiated += 1;
        }
        if let Some(d) = t.teardown {
            self.teardown_us = self.teardown_us.saturating_add(micros(d));
            self.torn_down += 1;
        }
        if let Some(guest) = t.guest() {
            let guest = micros(guest);
            self.guest_us = self.guest_us.saturating_add(guest);
            self.host_us = self.host_us.saturating_add(micros(t.host_total));
            self.called += 1;
            self.guest.insert(guest);
        }
        if let Some(d) = t.settle {
            self.settle_us = self.settle_us.saturating_add(micros(d));
            self.settled += 1;
        }
        for (name, total, calls) in &t.host_by_import {
            self.add_import(name, micros(*total), *calls);
        }
        if let Some(bytes) = t.memory_peak_bytes {
            self.memory_peak_max = self.memory_peak_max.max(bytes);
            self.memory_peak_sum = self.memory_peak_sum.saturating_add(bytes);
            self.memory_measured += 1;
        }
        if let Some(fuel) = t.fuel_consumed {
            self.fuel_sum = self.fuel_sum.saturating_add(fuel);
            self.fuel_measured += 1;
        }
        match t.module_cache_hit {
            Some(true) => self.cache_hits += 1,
            Some(false) => self.cache_misses += 1,
            None => {}
        }
    }

    fn add_import(&mut self, name: &'static str, us: u64, calls: u64) {
        match self.host_imports.iter_mut().find(|i| i.name == name) {
            Some(sum) => {
                sum.us = sum.us.saturating_add(us);
                sum.calls += calls;
            }
            None => self.host_imports.push(HostImportSum { name, us, calls }),
        }
    }

    pub fn merge(&mut self, other: &Bucket) {
        self.runs += other.runs;
        for (mine, theirs) in self.outcomes.iter_mut().zip(other.outcomes) {
            *mine += theirs;
        }
        self.wall_us = self.wall_us.saturating_add(other.wall_us);
        self.prepare_us = self.prepare_us.saturating_add(other.prepare_us);
        self.instantiate_us = self.instantiate_us.saturating_add(other.instantiate_us);
        self.link_us = self.link_us.saturating_add(other.link_us);
        self.allocate_us = self.allocate_us.saturating_add(other.allocate_us);
        self.instantiated += other.instantiated;
        self.teardown_us = self.teardown_us.saturating_add(other.teardown_us);
        self.torn_down += other.torn_down;
        self.guest_us = self.guest_us.saturating_add(other.guest_us);
        self.host_us = self.host_us.saturating_add(other.host_us);
        self.called += other.called;
        self.settle_us = self.settle_us.saturating_add(other.settle_us);
        self.settled += other.settled;
        for import in &other.host_imports {
            self.add_import(import.name, import.us, import.calls);
        }
        self.memory_peak_max = self.memory_peak_max.max(other.memory_peak_max);
        self.memory_peak_sum = self.memory_peak_sum.saturating_add(other.memory_peak_sum);
        self.memory_measured += other.memory_measured;
        self.fuel_sum = self.fuel_sum.saturating_add(other.fuel_sum);
        self.fuel_measured += other.fuel_measured;
        self.cache_hits += other.cache_hits;
        self.cache_misses += other.cache_misses;
        self.wall.merge(&other.wall);
        self.guest.merge(&other.guest);
    }

    pub fn outcome_counts(&self) -> std::collections::BTreeMap<&'static str, u64> {
        OUTCOMES
            .iter()
            .zip(self.outcomes)
            .filter(|(_, n)| *n > 0)
            .map(|(name, n)| (*name, n))
            .collect()
    }

    pub fn count(&self, outcome: Outcome) -> u64 {
        self.outcomes[outcome.index()]
    }
}

/// What a ring's buckets hold: samples added one at a time, and buckets
/// merged into a window.
pub trait Accumulate: Clone + Default {
    type Sample;
    fn add_sample(&mut self, sample: &Self::Sample);
    fn merge_from(&mut self, other: &Self);
}

impl Accumulate for Bucket {
    type Sample = RunSample;

    fn add_sample(&mut self, sample: &RunSample) {
        self.add(sample);
    }

    fn merge_from(&mut self, other: &Self) {
        self.merge(other);
    }
}

/// A bucket ring of `width`-second buckets.
#[derive(Clone, Debug)]
pub struct Ring<B = Bucket> {
    width: u64,
    slots: Vec<Option<(u64, B)>>,
}

impl<B: Accumulate> Ring<B> {
    pub fn new(width: u64, len: usize) -> Self {
        Self {
            width,
            slots: vec![None; len],
        }
    }

    pub fn width(&self) -> u64 {
        self.width
    }

    fn start_of(&self, at: u64) -> u64 {
        at - at % self.width
    }

    fn slot_of(&self, start: u64) -> usize {
        ((start / self.width) % self.slots.len() as u64) as usize
    }

    pub fn add(&mut self, at: u64, sample: &B::Sample) {
        let start = self.start_of(at);
        let i = self.slot_of(start);
        match &mut self.slots[i] {
            Some((held, bucket)) if *held == start => bucket.add_sample(sample),
            slot => {
                let mut bucket = B::default();
                bucket.add_sample(sample);
                *slot = Some((start, bucket));
            }
        }
    }

    /// The bucket starts a window of `secs` ending at `now` covers, oldest
    /// first: every bucket overlapping the window that the ring can still
    /// hold. The newest is the one still open.
    pub fn starts(&self, now: u64, secs: u64) -> impl Iterator<Item = u64> + use<B> {
        let newest = self.start_of(now);
        let held = newest.saturating_sub((self.slots.len() as u64 - 1) * self.width);
        let overlapping = self.start_of(now.saturating_sub(secs.saturating_sub(1)));
        let oldest = held.max(overlapping);
        (oldest..=newest).step_by(self.width as usize)
    }

    /// The bucket that started at `start`, if it is still held.
    pub fn get(&self, start: u64) -> Option<&B> {
        match &self.slots[self.slot_of(start)] {
            Some((held, bucket)) if *held == start => Some(bucket),
            _ => None,
        }
    }

    /// Everything a window of `secs` ending at `now` holds, merged.
    pub fn merged(&self, now: u64, secs: u64) -> B {
        let mut out = B::default();
        for start in self.starts(now, secs) {
            if let Some(bucket) = self.get(start) {
                out.merge_from(bucket);
            }
        }
        out
    }

    /// When the oldest bucket a window of `secs` ending at `now` holds
    /// started, if it holds any.
    pub fn oldest_in(&self, now: u64, secs: u64) -> Option<u64> {
        self.starts(now, secs)
            .find(|start| self.get(*start).is_some())
    }

    /// When the newest bucket the ring holds started.
    pub fn newest(&self) -> Option<u64> {
        self.slots.iter().flatten().map(|(start, _)| *start).max()
    }
}

/// One key's history: the last hour by the minute, the last day by the half
/// hour. Each run is added to both, so neither is derived from the other.
#[derive(Clone, Debug)]
pub struct History<B = Bucket> {
    pub minutes: Ring<B>,
    pub half_hours: Ring<B>,
}

impl<B: Accumulate> Default for History<B> {
    fn default() -> Self {
        Self {
            minutes: Ring::new(MINUTE_SECS, MINUTES_KEPT),
            half_hours: Ring::new(HALF_HOUR_SECS, HALF_HOURS_KEPT),
        }
    }
}

impl<B: Accumulate> History<B> {
    pub fn add(&mut self, at: u64, sample: &B::Sample) {
        self.minutes.add(at, sample);
        self.half_hours.add(at, sample);
    }

    /// Whether anything this key holds is still inside the longest window.
    pub fn is_live(&self, now: u64) -> bool {
        self.half_hours
            .newest()
            .is_some_and(|start| start + HALF_HOUR_SECS * HALF_HOURS_KEPT as u64 > now)
    }
}

/// Milliseconds, from microseconds.
pub(crate) fn ms(us: u64) -> f64 {
    us as f64 / 1_000.0
}

pub(crate) fn mean_ms(sum_us: u64, n: u64) -> Option<f64> {
    (n > 0).then(|| ms(sum_us) / n as f64)
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Percentiles {
    pub p50: Option<f64>,
    pub p95: Option<f64>,
    pub p99: Option<f64>,
    pub max: Option<f64>,
    pub mean: Option<f64>,
}

impl Percentiles {
    pub(crate) fn of(sketch: &Sketch, sum_us: u64) -> Self {
        let q = |q: f64| sketch.quantile(q).map(ms);
        Self {
            p50: q(0.5),
            p95: q(0.95),
            p99: q(0.99),
            max: sketch.max().map(ms),
            mean: mean_ms(sum_us, sketch.count()),
        }
    }
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct HostImportStats {
    pub name: &'static str,
    pub calls: u64,
    pub ms_mean: f64,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct MemoryStats {
    pub max: Option<u64>,
    pub mean: Option<f64>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
}

/// What a window says about one key.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Stats {
    pub runs: u64,
    pub outcomes: std::collections::BTreeMap<&'static str, u64>,
    pub wall_ms: Percentiles,
    pub guest_ms: Percentiles,
    pub prepare_ms_mean: Option<f64>,
    pub instantiate_ms_mean: Option<f64>,
    pub link_ms_mean: Option<f64>,
    pub allocate_ms_mean: Option<f64>,
    pub teardown_ms_mean: Option<f64>,
    pub host_ms_mean: Option<f64>,
    pub settle_ms_mean: Option<f64>,
    /// Mean per call, sorted by total time spent.
    pub host_imports: Vec<HostImportStats>,
    pub memory_peak_bytes: MemoryStats,
    /// `None` unless the script is a compiled module.
    pub fuel_mean: Option<f64>,
    pub module_cache: CacheStats,
}

impl Bucket {
    pub fn stats(&self) -> Stats {
        let mut imports = self.host_imports.clone();
        imports.sort_by(|a, b| b.us.cmp(&a.us).then(a.name.cmp(b.name)));
        Stats {
            runs: self.runs,
            outcomes: self.outcome_counts(),
            wall_ms: Percentiles::of(&self.wall, self.wall_us),
            guest_ms: Percentiles::of(&self.guest, self.guest_us),
            prepare_ms_mean: mean_ms(self.prepare_us, self.runs),
            instantiate_ms_mean: mean_ms(self.instantiate_us, self.instantiated),
            link_ms_mean: mean_ms(self.link_us, self.instantiated),
            allocate_ms_mean: mean_ms(self.allocate_us, self.instantiated),
            teardown_ms_mean: mean_ms(self.teardown_us, self.torn_down),
            host_ms_mean: mean_ms(self.host_us, self.called),
            settle_ms_mean: mean_ms(self.settle_us, self.settled),
            host_imports: imports
                .iter()
                .map(|i| HostImportStats {
                    name: i.name,
                    calls: i.calls,
                    ms_mean: mean_ms(i.us, i.calls).unwrap_or(0.0),
                })
                .collect(),
            memory_peak_bytes: MemoryStats {
                max: (self.memory_measured > 0).then_some(self.memory_peak_max),
                mean: (self.memory_measured > 0)
                    .then(|| self.memory_peak_sum as f64 / self.memory_measured as f64),
            },
            fuel_mean: (self.fuel_measured > 0)
                .then(|| self.fuel_sum as f64 / self.fuel_measured as f64),
            module_cache: CacheStats {
                hits: self.cache_hits,
                misses: self.cache_misses,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn sample(wall_ms: u64, outcome: Outcome) -> RunSample {
        RunSample {
            outcome,
            wall: Duration::from_millis(wall_ms),
            prepare: Duration::from_millis(1),
            timings: RunTimings {
                instantiate: Some(Duration::from_millis(1)),
                link: Some(Duration::from_micros(250)),
                allocate: Some(Duration::from_micros(750)),
                teardown: Some(Duration::from_micros(500)),
                module_cache_hit: Some(true),
                guest_call: Some(Duration::from_millis(wall_ms.saturating_sub(2))),
                host_total: Duration::from_millis(1),
                host_by_import: vec![("host_db_query", Duration::from_millis(1), 2)],
                settle: Some(Duration::ZERO),
                memory_peak_bytes: Some(1 << 20),
                fuel_consumed: None,
            },
        }
    }

    /// A half-hour boundary.
    const T0: u64 = 1_793_318_400;

    #[test]
    fn a_new_minute_opens_a_new_bucket() {
        let mut ring: Ring = Ring::new(MINUTE_SECS, MINUTES_KEPT);
        ring.add(T0 + 5, &sample(10, Outcome::Ok));
        ring.add(T0 + 59, &sample(10, Outcome::Ok));
        ring.add(T0 + 60, &sample(10, Outcome::Runtime));
        assert_eq!(ring.get(T0).unwrap().runs, 2);
        let next = ring.get(T0 + 60).unwrap();
        assert_eq!(next.runs, 1);
        assert_eq!(next.count(Outcome::Runtime), 1);
    }

    #[test]
    fn a_window_merges_the_buckets_inside_it() {
        let mut ring: Ring = Ring::new(MINUTE_SECS, MINUTES_KEPT);
        for minute in 0..10 {
            ring.add(T0 + minute * 60, &sample(10 + minute, Outcome::Ok));
        }
        let merged = ring.merged(T0 + 9 * 60 + 30, 3_600);
        assert_eq!(merged.runs, 10);
        let stats = merged.stats();
        assert_eq!(stats.outcomes.get("ok"), Some(&10));
        assert_eq!(stats.wall_ms.max, Some(19.0));
        assert_eq!(stats.host_imports[0].calls, 20);
        assert_eq!(stats.host_imports[0].ms_mean, 0.5);
    }

    #[test]
    fn a_bucket_past_the_window_is_not_read() {
        let mut ring: Ring = Ring::new(MINUTE_SECS, MINUTES_KEPT);
        ring.add(T0, &sample(10, Outcome::Ok));
        ring.add(T0 + 30 * 60, &sample(10, Outcome::Ok));
        // Sixty minutes on, the first bucket has left the hour.
        let merged = ring.merged(T0 + 60 * 60, 3_600);
        assert_eq!(merged.runs, 1);
        // And a write a lap later replaces it rather than adding to it.
        ring.add(T0 + 60 * 60, &sample(10, Outcome::Ok));
        assert!(ring.get(T0).is_none());
        assert_eq!(ring.get(T0 + 60 * 60).unwrap().runs, 1);
    }

    #[test]
    fn the_day_is_kept_by_the_half_hour() {
        let mut history: History = History::default();
        history.add(T0, &sample(10, Outcome::Ok));
        history.add(T0 + 29 * 60, &sample(10, Outcome::Ok));
        history.add(T0 + 30 * 60, &sample(10, Outcome::Timeout));
        history.add(T0 + 5 * 3_600, &sample(10, Outcome::Ok));
        let now = T0 + 5 * 3_600 + 1;
        assert_eq!(history.half_hours.get(T0).unwrap().runs, 2);
        assert_eq!(history.half_hours.merged(now, 86_400).runs, 4);
        // Only the last run is inside the hour.
        assert_eq!(history.minutes.merged(now, 3_600).runs, 1);
        assert!(history.is_live(now));
        assert!(!history.is_live(T0 + 5 * 3_600 + 86_400 + 1_800));
    }

    #[test]
    fn a_window_names_every_bucket_start_it_covers() {
        let ring: Ring = Ring::new(MINUTE_SECS, MINUTES_KEPT);
        let starts: Vec<u64> = ring.starts(T0 + 3_600 + 10, 3_600).collect();
        assert_eq!(starts.len(), 60);
        assert_eq!(starts.last(), Some(&(T0 + 3_600)));
        assert_eq!(starts.first(), Some(&(T0 + 60)));

        let day: Ring = Ring::new(HALF_HOUR_SECS, HALF_HOURS_KEPT);
        assert_eq!(day.starts(T0 + 86_400, 86_400).count(), 48);
        assert_eq!(day.starts(T0 + 10, 3_600).count(), 3);
    }

    #[test]
    fn a_run_that_never_reached_the_guest_moves_only_its_own_means() {
        let mut bucket = Bucket::default();
        bucket.add(&sample(10, Outcome::Ok));
        bucket.add(&RunSample {
            outcome: Outcome::NoInterpreter,
            wall: Duration::from_millis(1),
            prepare: Duration::ZERO,
            timings: RunTimings::default(),
        });
        let stats = bucket.stats();
        assert_eq!(stats.runs, 2);
        assert_eq!(stats.instantiate_ms_mean, Some(1.0));
        assert_eq!(stats.link_ms_mean, Some(0.25));
        assert_eq!(stats.allocate_ms_mean, Some(0.75));
        assert_eq!(stats.teardown_ms_mean, Some(0.5));
        assert_eq!(stats.guest_ms.mean, Some(7.0));
        assert_eq!(stats.module_cache, CacheStats { hits: 1, misses: 0 });
        assert_eq!(stats.memory_peak_bytes.max, Some(1 << 20));
        assert_eq!(stats.fuel_mean, None);
    }
}

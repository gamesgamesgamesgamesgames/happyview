//! A mergeable quantile sketch over microsecond durations.
//!
//! Values are counted in logarithmic buckets whose bounds grow by [`GAMMA`],
//! so any quantile it answers is within about 1% of a value that was
//! inserted, whatever the distribution. Two sketches merge by adding their
//! buckets, which is what lets a window's percentiles be read from its
//! minute buckets without keeping a single sample.

use std::collections::BTreeMap;

/// The ratio between consecutive bucket bounds. The relative error is
/// `(GAMMA - 1) / (GAMMA + 1)`, just under 1%.
pub const GAMMA: f64 = 1.02;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sketch {
    /// Bucket `i` counts values in `(GAMMA^(i-1), GAMMA^i]`. Sparse: a
    /// latency distribution occupies a few dozen of the ~1,400 buckets
    /// between a microsecond and a day.
    buckets: BTreeMap<i32, u64>,
    /// Values too small to have a logarithm: a run that took under a
    /// microsecond.
    zeros: u64,
    count: u64,
    min: u64,
    max: u64,
}

fn index_of(us: u64) -> i32 {
    ((us as f64).ln() / GAMMA.ln()).ceil() as i32
}

/// The value a bucket stands for: the point whose relative distance to
/// either bound is the same.
fn value_of(index: i32) -> f64 {
    2.0 * GAMMA.powi(index) / (GAMMA + 1.0)
}

impl Sketch {
    pub fn insert(&mut self, us: u64) {
        if self.count == 0 {
            self.min = us;
            self.max = us;
        } else {
            self.min = self.min.min(us);
            self.max = self.max.max(us);
        }
        self.count += 1;
        if us == 0 {
            self.zeros += 1;
        } else {
            *self.buckets.entry(index_of(us)).or_default() += 1;
        }
    }

    pub fn merge(&mut self, other: &Sketch) {
        if other.count == 0 {
            return;
        }
        if self.count == 0 {
            self.min = other.min;
            self.max = other.max;
        } else {
            self.min = self.min.min(other.min);
            self.max = self.max.max(other.max);
        }
        self.count += other.count;
        self.zeros += other.zeros;
        for (index, n) in &other.buckets {
            *self.buckets.entry(*index).or_default() += n;
        }
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    /// The largest value inserted, exactly.
    pub fn max(&self) -> Option<u64> {
        (self.count > 0).then_some(self.max)
    }

    /// The value at quantile `q` (0 to 1). The lowest and highest ranks are
    /// the exact minimum and maximum, and every estimate between is clamped
    /// to them.
    pub fn quantile(&self, q: f64) -> Option<u64> {
        if self.count == 0 {
            return None;
        }
        let q = q.clamp(0.0, 1.0);
        let rank = (q * (self.count - 1) as f64).floor() as u64;
        if rank == 0 {
            return Some(self.min);
        }
        if rank == self.count - 1 {
            return Some(self.max);
        }
        if rank < self.zeros {
            return Some(0);
        }
        let mut seen = self.zeros;
        for (index, n) in &self.buckets {
            seen += n;
            if seen > rank {
                let estimate = value_of(*index).round() as u64;
                return Some(estimate.clamp(self.min, self.max));
            }
        }
        Some(self.max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn within(estimate: u64, exact: u64, tolerance: f64) -> bool {
        let error = (estimate as f64 - exact as f64).abs() / exact as f64;
        error <= tolerance
    }

    /// The exact value at quantile `q` of a sorted slice, ranked as the
    /// sketch ranks.
    fn exact(sorted: &[u64], q: f64) -> u64 {
        sorted[(q * (sorted.len() - 1) as f64).floor() as usize]
    }

    fn check(values: &mut [u64]) {
        let mut sketch = Sketch::default();
        for v in values.iter() {
            sketch.insert(*v);
        }
        values.sort_unstable();
        for q in [0.5, 0.9, 0.95, 0.99] {
            let estimate = sketch.quantile(q).unwrap();
            let truth = exact(values, q);
            assert!(
                within(estimate, truth, 0.02),
                "q={q}: {estimate} against {truth}"
            );
        }
        assert_eq!(sketch.max(), values.last().copied());
        assert_eq!(sketch.count(), values.len() as u64);
    }

    #[test]
    fn a_uniform_distribution_is_read_within_two_percent() {
        let mut values: Vec<u64> = (1..=100_000).collect();
        check(&mut values);
    }

    #[test]
    fn a_long_tailed_distribution_is_read_within_two_percent() {
        // A deterministic exponential-ish spread from 10 µs to about a second,
        // which is what latency looks like.
        let mut values: Vec<u64> = (0..50_000)
            .map(|i| (10.0 * (1.0002_f64).powi(i)) as u64)
            .collect();
        check(&mut values);
    }

    #[test]
    fn a_constant_is_answered_exactly() {
        let mut sketch = Sketch::default();
        for _ in 0..1_000 {
            sketch.insert(2_500);
        }
        for q in [0.0, 0.5, 0.99, 1.0] {
            assert_eq!(sketch.quantile(q), Some(2_500));
        }
    }

    #[test]
    fn merging_equals_inserting_everything_into_one() {
        let mut a = Sketch::default();
        let mut b = Sketch::default();
        let mut both = Sketch::default();
        for v in 0..5_000u64 {
            let value = v * 7 % 10_000;
            if v % 3 == 0 {
                a.insert(value);
            } else {
                b.insert(value);
            }
            both.insert(value);
        }
        a.merge(&b);
        assert_eq!(a, both);
    }

    #[test]
    fn merging_into_an_empty_sketch_copies_the_other() {
        let mut a = Sketch::default();
        let mut b = Sketch::default();
        b.insert(40);
        b.insert(90);
        a.merge(&b);
        assert_eq!(a, b);
        a.merge(&Sketch::default());
        assert_eq!(a, b);
    }

    #[test]
    fn an_empty_sketch_answers_nothing() {
        let sketch = Sketch::default();
        assert_eq!(sketch.quantile(0.5), None);
        assert_eq!(sketch.max(), None);
        assert_eq!(sketch.count(), 0);
    }

    #[test]
    fn sub_microsecond_runs_count_as_zero() {
        let mut sketch = Sketch::default();
        for _ in 0..9 {
            sketch.insert(0);
        }
        sketch.insert(1_000);
        assert_eq!(sketch.quantile(0.5), Some(0));
        assert_eq!(sketch.quantile(1.0), Some(1_000));
        assert_eq!(sketch.max(), Some(1_000));
    }
}

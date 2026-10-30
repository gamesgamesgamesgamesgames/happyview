//! Named stages of a record's way through ingest, summed.

use std::collections::BTreeMap;
use std::time::Duration;

use happyview_provider::StageStats;

use super::window::{mean_ms, micros};

/// The record hook's script lookup.
pub const RESOLVE: &str = "resolve";
/// The check that a body hashes to its claimed CID.
pub const CID_CHECK: &str = "cid_check";
/// The record hook, retries and their backoff included, less its lookup.
pub const HOOK: &str = "hook";
/// Parts of [`HOOK`], not more of it: building the linker, then the store
/// and instance, and dropping them, summed over the hook's attempts. They are
/// platform time a script cannot change.
pub const HOOK_LINK: &str = "hook.link";
pub const HOOK_ALLOCATE: &str = "hook.allocate";
pub const HOOK_TEARDOWN: &str = "hook.teardown";
/// The write into the instance database. A batch's write is shared evenly
/// over the records it wrote.
pub const WRITE: &str = "write";
/// A claiming storage's `apply_delta`, as ingest waited on it: no-ops
/// included, since ingest cannot tell one from a round trip. A storage that
/// can reports its own writes through the job's `observe` as `write` and
/// `write_skipped`.
pub const DELTA: &str = "delta";

/// Whether `stage` is a count rather than a time: a name ending in
/// `_skipped`, for work that was skipped and took none.
pub fn is_count(stage: &str) -> bool {
    stage.ends_with("_skipped")
}

#[derive(Clone, Debug, PartialEq)]
pub struct StageSum {
    pub name: &'static str,
    pub n: u64,
    pub us: u64,
}

/// Each stage's records and total time. A record reaches a handful of
/// stages, so a list beats a map.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stages(Vec<StageSum>);

impl Stages {
    /// `n` records' worth of `elapsed` in `name`. A count's time is dropped.
    pub fn add(&mut self, name: &'static str, elapsed: Duration, n: u64) {
        let us = if is_count(name) { 0 } else { micros(elapsed) };
        self.add_us(name, us, n);
    }

    fn add_us(&mut self, name: &'static str, us: u64, n: u64) {
        match self.0.iter_mut().find(|s| s.name == name) {
            Some(sum) => {
                sum.n = sum.n.saturating_add(n);
                sum.us = sum.us.saturating_add(us);
            }
            None => self.0.push(StageSum { name, n, us }),
        }
    }

    pub fn merge(&mut self, other: &Stages) {
        for sum in &other.0 {
            self.add_us(sum.name, sum.us, sum.n);
        }
    }

    pub fn stats(&self) -> BTreeMap<String, StageStats> {
        self.0
            .iter()
            .map(|sum| {
                (
                    sum.name.to_string(),
                    StageStats {
                        n: sum.n,
                        ms_mean: if is_count(sum.name) {
                            None
                        } else {
                            mean_ms(sum.us, sum.n)
                        },
                    },
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_skipped_stage_is_counted_and_never_timed() {
        let mut stages = Stages::default();
        stages.add(WRITE, Duration::from_millis(20), 1);
        stages.add(WRITE, Duration::from_millis(40), 1);
        stages.add("write_skipped", Duration::from_millis(500), 98);
        let stats = stages.stats();
        assert_eq!(
            stats["write"],
            StageStats {
                n: 2,
                ms_mean: Some(30.0)
            }
        );
        assert_eq!(
            stats["write_skipped"],
            StageStats {
                n: 98,
                ms_mean: None
            }
        );
    }

    #[test]
    fn stages_merge_by_name() {
        let mut a = Stages::default();
        a.add(RESOLVE, Duration::from_micros(10), 1);
        let mut b = Stages::default();
        b.add(RESOLVE, Duration::from_micros(30), 1);
        b.add("page_read", Duration::from_millis(50), 500);
        a.merge(&b);
        let stats = a.stats();
        assert_eq!(stats["resolve"].n, 2);
        assert_eq!(stats["resolve"].ms_mean, Some(0.02));
        assert_eq!(stats["page_read"].ms_mean, Some(0.1));
    }
}

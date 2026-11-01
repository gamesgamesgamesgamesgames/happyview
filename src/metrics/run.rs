//! The timings of the one script run in progress on this task.
//!
//! `dispatch` opens a collector around the executor call with [`run_under`];
//! the executor reports instantiation, the guest call, settling, memory and
//! instructions into it through the free functions here, which do nothing outside a
//! scope, so a run nobody is measuring (a validation, a test file, a run with
//! metrics switched off) pays one failed task-local read per report.
//!
//! Host imports report through the collector the run's store holds rather
//! than the task-local: a library call runs on the same task as the script
//! that made it, so its own imports would otherwise be counted a second time
//! inside the `host_call_library` that already covers them.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What one run measured. Every field the run never reached stays `None`: a
/// run with no interpreter was never instantiated.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunTimings {
    /// Everything from asking for an instance to holding one: `link` plus
    /// `allocate`.
    pub instantiate: Option<Duration>,
    /// Building the linker the instance's imports are resolved through.
    pub link: Option<Duration>,
    /// The rest of `instantiate`: finding the module, building the store's
    /// state, the store and the instance, its memory and fiber stack, and
    /// the guest's `_initialize`.
    pub allocate: Option<Duration>,
    /// Dropping the store and its instance once the run is over.
    pub teardown: Option<Duration>,
    /// Whether this run's module came from the runtime's memory rather than
    /// disk or a compile.
    pub module_cache_hit: Option<bool>,
    /// Wall time inside `execute`, host imports included.
    pub guest_call: Option<Duration>,
    pub host_total: Duration,
    /// Per import: total time and calls. A run makes a handful of distinct
    /// imports, so a list beats a map.
    pub host_by_import: Vec<(&'static str, Duration, u64)>,
    pub settle: Option<Duration>,
    pub memory_peak_bytes: Option<u64>,
    /// A compiled module's fuel in instructions, or what an interpreter said
    /// it executed. `None` from an interpreter that does not count.
    pub instructions: Option<u64>,
}

impl RunTimings {
    /// Time the guest ran its own code: the call less the imports it made.
    pub fn guest(&self) -> Option<Duration> {
        self.guest_call
            .map(|call| call.saturating_sub(self.host_total))
    }
}

#[derive(Debug, Default)]
pub struct RunCollector {
    timings: Mutex<RunTimings>,
}

impl RunCollector {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn record_host(&self, name: &'static str, elapsed: Duration) {
        let mut t = self.lock();
        t.host_total += elapsed;
        match t.host_by_import.iter_mut().find(|(n, _, _)| *n == name) {
            Some((_, total, calls)) => {
                *total += elapsed;
                *calls += 1;
            }
            None => t.host_by_import.push((name, elapsed, 1)),
        }
    }

    pub fn snapshot(&self) -> RunTimings {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RunTimings> {
        self.timings.lock().unwrap_or_else(|e| e.into_inner())
    }
}

tokio::task_local! {
    static RUN: Arc<RunCollector>;
}

/// Run `fut` with `collector` as the run in progress.
pub async fn run_under<F: Future>(collector: Arc<RunCollector>, fut: F) -> F::Output {
    RUN.scope(collector, fut).await
}

/// The collector of the run in progress on this task, if one is measured.
pub fn current() -> Option<Arc<RunCollector>> {
    RUN.try_with(Arc::clone).ok()
}

fn with(f: impl FnOnce(&mut RunTimings)) {
    let _ = RUN.try_with(|collector| f(&mut collector.lock()));
}

/// Instantiation took `elapsed`, `link` of it building the linker.
pub fn record_instantiate(elapsed: Duration, link: Duration, module_cache_hit: bool) {
    with(|t| {
        t.instantiate = Some(elapsed);
        t.link = Some(link);
        t.allocate = Some(elapsed.saturating_sub(link));
        t.module_cache_hit = Some(module_cache_hit);
    });
}

pub fn record_teardown(elapsed: Duration) {
    with(|t| t.teardown = Some(elapsed));
}

pub fn record_guest_call(elapsed: Duration) {
    with(|t| t.guest_call = Some(elapsed));
}

pub fn record_settle(elapsed: Duration) {
    with(|t| t.settle = Some(elapsed));
}

pub fn record_memory_peak(bytes: u64) {
    with(|t| t.memory_peak_bytes = Some(bytes));
}

pub fn record_instructions(used: u64) {
    with(|t| t.instructions = Some(used));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reports_outside_a_scope_change_nothing() {
        record_instantiate(Duration::from_millis(1), Duration::ZERO, true);
        record_guest_call(Duration::from_millis(1));
        assert!(current().is_none());
    }

    #[tokio::test]
    async fn a_scope_collects_what_the_run_reports() {
        let collector = RunCollector::new();
        run_under(collector.clone(), async {
            record_instantiate(Duration::from_micros(300), Duration::from_micros(80), false);
            record_guest_call(Duration::from_micros(1_000));
            let current = current().expect("inside the scope");
            current.record_host("host_db_query", Duration::from_micros(200));
            current.record_host("host_db_query", Duration::from_micros(100));
            current.record_host("host_http_fetch", Duration::from_micros(50));
            record_settle(Duration::from_micros(20));
            record_teardown(Duration::from_micros(15));
            record_memory_peak(65_536);
            record_instructions(4_000);
        })
        .await;
        let t = collector.snapshot();
        assert_eq!(t.instantiate, Some(Duration::from_micros(300)));
        assert_eq!(t.link, Some(Duration::from_micros(80)));
        assert_eq!(t.allocate, Some(Duration::from_micros(220)));
        assert_eq!(t.teardown, Some(Duration::from_micros(15)));
        assert_eq!(t.module_cache_hit, Some(false));
        assert_eq!(t.host_total, Duration::from_micros(350));
        assert_eq!(
            t.host_by_import,
            vec![
                ("host_db_query", Duration::from_micros(300), 2),
                ("host_http_fetch", Duration::from_micros(50), 1),
            ]
        );
        assert_eq!(t.guest(), Some(Duration::from_micros(650)));
        assert_eq!(t.settle, Some(Duration::from_micros(20)));
        assert_eq!(t.memory_peak_bytes, Some(65_536));
        assert_eq!(t.instructions, Some(4_000));
    }

    #[test]
    fn guest_time_never_goes_below_zero() {
        let t = RunTimings {
            guest_call: Some(Duration::from_micros(10)),
            host_total: Duration::from_micros(15),
            ..Default::default()
        };
        assert_eq!(t.guest(), Some(Duration::ZERO));
    }
}

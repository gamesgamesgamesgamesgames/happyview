//! In-process counters, reset by every restart.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::db::now_rfc3339;
use crate::telemetry::payload;

pub(crate) fn add_saturating(atomic: &AtomicU64, delta: u64) {
    let mut current = atomic.load(Ordering::Relaxed);
    loop {
        let new = current.saturating_add(delta);
        match atomic.compare_exchange_weak(current, new, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(actual) => current = actual,
        }
    }
}

#[derive(Debug)]
pub struct Counters {
    pub jetstream_received: AtomicU64,
    pub jetstream_matched: AtomicU64,
    pub jetstream_skipped: AtomicU64,
    pub http_bytes_out: AtomicU64,
    pub http_bytes_in: AtomicU64,
    pub xrpc_requests: AtomicU64,
    pub xrpc_requests_with_credentials: AtomicU64,
    pub script_executions: AtomicU64,
    pub script_runtime_us: AtomicU64,
    pub job_wait_ms: AtomicU64,
    process_started_at: String,
}

impl Default for Counters {
    fn default() -> Self {
        Self::new()
    }
}

impl Counters {
    pub fn new() -> Self {
        Self {
            jetstream_received: AtomicU64::new(0),
            jetstream_matched: AtomicU64::new(0),
            jetstream_skipped: AtomicU64::new(0),
            http_bytes_out: AtomicU64::new(0),
            http_bytes_in: AtomicU64::new(0),
            xrpc_requests: AtomicU64::new(0),
            xrpc_requests_with_credentials: AtomicU64::new(0),
            script_executions: AtomicU64::new(0),
            script_runtime_us: AtomicU64::new(0),
            job_wait_ms: AtomicU64::new(0),
            process_started_at: now_rfc3339(),
        }
    }

    pub fn process_started_at(&self) -> &str {
        &self.process_started_at
    }

    pub fn snapshot(&self) -> payload::Counters {
        let read =
            |a: &AtomicU64| -> i64 { i64::try_from(a.load(Ordering::Relaxed)).unwrap_or(i64::MAX) };

        let mut out = payload::Counters::new();
        out.insert(
            "jetstream_events_received".into(),
            read(&self.jetstream_received),
        );
        out.insert(
            "jetstream_events_matched".into(),
            read(&self.jetstream_matched),
        );
        out.insert(
            "jetstream_events_skipped".into(),
            read(&self.jetstream_skipped),
        );
        out.insert("http_bytes_out".into(), read(&self.http_bytes_out));
        out.insert("http_bytes_in".into(), read(&self.http_bytes_in));
        out.insert("xrpc_requests".into(), read(&self.xrpc_requests));
        out.insert(
            "xrpc_requests_with_credentials".into(),
            read(&self.xrpc_requests_with_credentials),
        );
        out.insert("script_executions".into(), read(&self.script_executions));
        out.insert(
            "script_runtime_ms".into(),
            read(&self.script_runtime_us) / 1_000,
        );
        out.insert("job_wait_ms".into(), read(&self.job_wait_ms));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_zero_and_counts_up() {
        let c = Counters::new();
        assert_eq!(c.snapshot().get("jetstream_events_received"), Some(&0));

        c.jetstream_received.fetch_add(3, Ordering::Relaxed);
        c.jetstream_skipped.fetch_add(2, Ordering::Relaxed);

        let snap = c.snapshot();
        assert_eq!(snap.get("jetstream_events_received"), Some(&3));
        assert_eq!(snap.get("jetstream_events_skipped"), Some(&2));
        assert_eq!(snap.get("jetstream_events_matched"), Some(&0));
    }

    #[test]
    fn snapshot_reports_every_counter_even_at_zero() {
        let snap = Counters::new().snapshot();
        for key in [
            "jetstream_events_received",
            "jetstream_events_matched",
            "jetstream_events_skipped",
            "http_bytes_out",
            "http_bytes_in",
            "xrpc_requests",
            "xrpc_requests_with_credentials",
            "script_executions",
            "script_runtime_ms",
            "job_wait_ms",
        ] {
            assert!(snap.contains_key(key), "missing counter: {key}");
        }
    }

    #[test]
    fn xrpc_and_script_and_job_wait_counters_count_independently() {
        let c = Counters::new();
        c.xrpc_requests.fetch_add(5, Ordering::Relaxed);
        c.xrpc_requests_with_credentials
            .fetch_add(2, Ordering::Relaxed);
        c.script_executions.fetch_add(7, Ordering::Relaxed);
        c.script_runtime_us.fetch_add(1_234_000, Ordering::Relaxed);
        c.job_wait_ms.fetch_add(9000, Ordering::Relaxed);

        let snap = c.snapshot();
        assert_eq!(snap.get("xrpc_requests"), Some(&5));
        assert_eq!(snap.get("xrpc_requests_with_credentials"), Some(&2));
        assert_eq!(snap.get("script_executions"), Some(&7));
        assert_eq!(snap.get("script_runtime_ms"), Some(&1234));
        assert_eq!(snap.get("job_wait_ms"), Some(&9000));
    }

    #[test]
    fn sub_millisecond_script_runs_accumulate_instead_of_rounding_to_zero() {
        let c = Counters::new();
        for _ in 0..20 {
            add_saturating(&c.script_runtime_us, 400);
        }

        assert_eq!(c.script_runtime_us.load(Ordering::Relaxed), 8_000);
        assert_eq!(c.snapshot().get("script_runtime_ms"), Some(&8));
    }

    #[test]
    fn add_saturating_saturates_instead_of_wrapping() {
        let a = AtomicU64::new(u64::MAX - 1);
        add_saturating(&a, 10);
        assert_eq!(a.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn add_saturating_accumulates_normally_below_the_ceiling() {
        let a = AtomicU64::new(100);
        add_saturating(&a, 50);
        assert_eq!(a.load(Ordering::Relaxed), 150);
    }

    #[test]
    fn http_bytes_in_and_out_count_independently() {
        let c = Counters::new();
        c.http_bytes_out.fetch_add(4096, Ordering::Relaxed);
        c.http_bytes_in.fetch_add(128, Ordering::Relaxed);

        let snap = c.snapshot();
        assert_eq!(snap.get("http_bytes_out"), Some(&4096));
        assert_eq!(snap.get("http_bytes_in"), Some(&128));
    }

    #[test]
    fn process_start_is_fixed_at_construction() {
        let c = Counters::new();
        let first = c.process_started_at().to_string();
        let second = c.process_started_at().to_string();
        assert_eq!(first, second);
        assert!(
            first.ends_with("+00:00"),
            "must match db::now_rfc3339 offset form"
        );
    }

    #[test]
    fn saturates_rather_than_wrapping_into_a_negative() {
        let c = Counters::new();
        c.jetstream_received.store(u64::MAX, Ordering::Relaxed);
        assert_eq!(
            c.snapshot().get("jetstream_events_received"),
            Some(&i64::MAX)
        );
    }
}

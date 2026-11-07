//! What the Jetstream consumer is doing now, for the admin status endpoint.
//! Process-wide: there is one consumer per process.

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use serde::Serialize;

use super::config::Protocol;
use super::event::CursorPoint;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct JetstreamStatus {
    /// `"v2"` or `"v1"`; `None` before the first connection.
    pub protocol: Option<&'static str>,
    pub host: Option<String>,
    pub connected: bool,
    pub compression: bool,
    pub dictionary_id: Option<u32>,
    /// The applied watermark's seq (v2 only).
    pub applied_seq: Option<i64>,
    /// When Jetstream witnessed the applied watermark, in unix microseconds.
    pub applied_time_us: Option<i64>,
    /// Milliseconds from `applied_time_us` to now.
    pub lag_ms: Option<i64>,
    /// Events waiting in each worker's queue.
    pub worker_queue_depths: Vec<usize>,
    /// Per worker, when the job it is applying (or retrying) started, in unix
    /// milliseconds; `None` when idle. Queue depths do not count that job.
    pub worker_busy_since_ms: Vec<Option<i64>>,
    /// Server time at this snapshot, in unix milliseconds, so a client can age
    /// `worker_busy_since_ms` without trusting its own clock.
    pub now_ms: i64,
}

#[derive(Default)]
struct Live {
    status: JetstreamStatus,
    depths: Option<Arc<Vec<AtomicUsize>>>,
    busy_since: Option<Arc<Vec<AtomicI64>>>,
}

static LIVE: LazyLock<Mutex<Live>> = LazyLock::new(Mutex::default);

fn live() -> MutexGuard<'static, Live> {
    LIVE.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn connected(protocol: Protocol, host: &str, dictionary_id: Option<u32>) {
    let mut live = live();
    live.status.protocol = Some(protocol.as_str());
    live.status.host = Some(host.to_string());
    live.status.connected = true;
    live.status.compression = dictionary_id.is_some();
    live.status.dictionary_id = dictionary_id;
}

pub(crate) fn disconnected() {
    live().status.connected = false;
}

pub(crate) fn applied(point: &CursorPoint) {
    let mut live = live();
    live.status.applied_seq = point.seq();
    live.status.applied_time_us = Some(point.time_us());
}

pub(crate) fn set_workers(depths: Arc<Vec<AtomicUsize>>, busy_since: Arc<Vec<AtomicI64>>) {
    let mut live = live();
    live.depths = Some(depths);
    live.busy_since = Some(busy_since);
}

/// Forget the worker counters, once the pool they belong to is gone.
pub(crate) fn clear_workers() {
    let mut live = live();
    live.depths = None;
    live.busy_since = None;
}

pub(crate) fn snapshot() -> JetstreamStatus {
    snapshot_at(chrono::Utc::now().timestamp_micros())
}

fn snapshot_at(now_us: i64) -> JetstreamStatus {
    let live = live();
    let mut status = live.status.clone();
    status.now_ms = now_us / 1000;
    status.lag_ms = status.applied_time_us.map(|t| ((now_us - t) / 1000).max(0));
    status.worker_queue_depths = live
        .depths
        .as_ref()
        .map(|depths| depths.iter().map(|d| d.load(Ordering::Relaxed)).collect())
        .unwrap_or_default();
    status.worker_busy_since_ms = live
        .busy_since
        .as_ref()
        .map(|busy| super::workers::busy_since_values(busy))
        .unwrap_or_default();
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_reports_lag_and_queue_depths() {
        set_workers(
            Arc::new(vec![AtomicUsize::new(3), AtomicUsize::new(0)]),
            Arc::new(vec![
                AtomicI64::new(1_700_000_000_000),
                AtomicI64::new(i64::MIN),
            ]),
        );
        connected(Protocol::V2, "js.example.com", Some(7));
        applied(&CursorPoint::V2 {
            seq: 42,
            witnessed_us: 1_000_000,
        });

        let status = snapshot_at(3_500_000);
        assert_eq!(status.protocol, Some("v2"));
        assert_eq!(status.host.as_deref(), Some("js.example.com"));
        assert!(status.connected);
        assert!(status.compression);
        assert_eq!(status.dictionary_id, Some(7));
        assert_eq!(status.applied_seq, Some(42));
        assert_eq!(status.lag_ms, Some(2_500));
        assert_eq!(status.worker_queue_depths, vec![3, 0]);
        assert_eq!(
            status.worker_busy_since_ms,
            vec![Some(1_700_000_000_000), None]
        );

        assert_eq!(status.now_ms, 3_500);

        disconnected();
        assert!(!snapshot_at(3_500_000).connected);

        clear_workers();
        let status = snapshot_at(3_500_000);
        assert!(status.worker_queue_depths.is_empty());
        assert!(status.worker_busy_since_ms.is_empty());
    }
}

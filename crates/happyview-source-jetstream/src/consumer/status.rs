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
    /// When the current connection last delivered a data frame, in unix
    /// milliseconds; `None` before its first one.
    pub last_frame_ms: Option<i64>,
    /// Milliseconds since the current connection last delivered a data frame,
    /// or since it connected when none has arrived yet; `None` while
    /// disconnected. Unlike `lag_ms`, it grows on a stalled connection even
    /// before anything was applied.
    pub idle_ms: Option<i64>,
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
    /// When the current connection was made, in unix milliseconds.
    connected_ms: Option<i64>,
    depths: Option<Arc<Vec<AtomicUsize>>>,
    busy_since: Option<Arc<Vec<AtomicI64>>>,
}

static LIVE: LazyLock<Mutex<Live>> = LazyLock::new(Mutex::default);

/// The last data frame's arrival, in unix milliseconds; `NO_FRAME` for none
/// on this connection. An atomic, not part of `LIVE`, because it is written
/// once per frame.
static LAST_FRAME_MS: AtomicI64 = AtomicI64::new(NO_FRAME);
const NO_FRAME: i64 = i64::MIN;

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
    live.connected_ms = Some(chrono::Utc::now().timestamp_millis());
    LAST_FRAME_MS.store(NO_FRAME, Ordering::Relaxed);
}

/// A data frame arrived on the current connection.
pub(crate) fn frame_received() {
    LAST_FRAME_MS.store(chrono::Utc::now().timestamp_millis(), Ordering::Relaxed);
}

pub(crate) fn disconnected() {
    let mut live = live();
    live.status.connected = false;
    live.connected_ms = None;
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
    let now_ms = now_us / 1000;
    let last_frame = LAST_FRAME_MS.load(Ordering::Relaxed);
    status.last_frame_ms = (status.connected && last_frame != NO_FRAME).then_some(last_frame);
    status.idle_ms = if status.connected {
        status
            .last_frame_ms
            .or(live.connected_ms)
            .map(|since| (now_ms - since).max(0))
    } else {
        None
    };
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

        // Connected with no frame yet: idle since the connection, and no
        // last frame. (The connection's time is the real clock, so only
        // its presence is checked here.)
        assert_eq!(status.last_frame_ms, None);
        let now_us = chrono::Utc::now().timestamp_micros();
        assert!(snapshot_at(now_us).idle_ms.is_some());
        // A frame at 2.0 s, seen at 3.5 s: idle 1.5 s, while lag still
        // counts from what was applied.
        LAST_FRAME_MS.store(2_000, Ordering::Relaxed);
        let status = snapshot_at(3_500_000);
        assert_eq!(status.last_frame_ms, Some(2_000));
        assert_eq!(status.idle_ms, Some(1_500));
        assert_eq!(status.lag_ms, Some(2_500));
        // A new connection starts without a last frame.
        connected(Protocol::V2, "js.example.com", Some(7));
        assert_eq!(snapshot_at(3_500_000).last_frame_ms, None);

        disconnected();
        let status = snapshot_at(3_500_000);
        assert!(!status.connected);
        assert_eq!(status.last_frame_ms, None);
        assert_eq!(status.idle_ms, None);

        clear_workers();
        let status = snapshot_at(3_500_000);
        assert!(status.worker_queue_depths.is_empty());
        assert!(status.worker_busy_since_ms.is_empty());
    }
}

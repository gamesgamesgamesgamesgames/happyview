//! Per-DID ordered application across a fixed set of workers.
//!
//! Jetstream preserves each DID's order but not a global one. Each event goes
//! to worker `hash(did) % workers`, which applies its queue in order, so one
//! DID's events never race while different DIDs still apply in parallel.
//! Queues are bounded: a full one makes `dispatch` wait, which stops the
//! socket being read until the worker catches up.
//!
//! A failed event is retried in place, so the worker's later events (and so
//! that DID's) wait behind it. Once the retries run out it is handed to
//! `on_give_up` and its ticket completes, so the watermark never stalls.
//! An event that failed only because the infrastructure is unavailable is
//! retried until it applies: its ticket stays open, the watermark holds, and
//! the full queue backpressures the reader until the outage ends.

use std::any::Any;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::event::Event;
use super::watermark::WatermarkHandle;

/// What applying one event came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied,
    /// Worth a few retries; the string says why it failed.
    Failed(String),
    /// A transient infrastructure failure (the database is unreachable, say),
    /// retried until it applies and never given up.
    Unavailable(String),
}

/// What a worker does with each event.
pub type ApplyFn = Arc<dyn Fn(Event) -> BoxFuture<'static, ApplyOutcome> + Send + Sync>;

/// Called once for an event that still failed after every retry, with the
/// last error. It runs inline before the event's ticket completes, so it
/// must not hang: a hang stops that worker and the watermark for good.
pub type GiveUpFn = Arc<dyn Fn(Event, String) -> BoxFuture<'static, ()> + Send + Sync>;

/// The longest wait between retries of an `Unavailable` event.
pub const MAX_UNAVAILABLE_DELAY: Duration = Duration::from_secs(30);

/// How a worker retries an event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// The waits before each retry of a `Failed` event; one retry per delay.
    /// `Unavailable` retries walk the same delays, then repeat the last.
    pub delays: Vec<Duration>,
    /// The cap on an `Unavailable` retry's wait.
    pub max_unavailable_delay: Duration,
}

impl RetryPolicy {
    /// The wait before the `retry`th (from 0) retry of an `Unavailable` event.
    fn unavailable_delay(&self, retry: usize) -> Duration {
        let delay = self
            .delays
            .get(retry)
            .or(self.delays.last())
            .copied()
            .unwrap_or(self.max_unavailable_delay);
        delay.min(self.max_unavailable_delay)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            delays: [1, 2, 4, 8, 16].map(Duration::from_secs).to_vec(),
            max_unavailable_delay: MAX_UNAVAILABLE_DELAY,
        }
    }
}

pub struct Job {
    pub ticket: u64,
    pub event: Event,
}

pub struct WorkerPool {
    senders: Vec<mpsc::Sender<Job>>,
    depths: Arc<Vec<AtomicUsize>>,
    busy_since: Arc<Vec<AtomicI64>>,
    watermark: Arc<WatermarkHandle>,
    tasks: Vec<JoinHandle<()>>,
}

/// Dropping the pool stops its workers, so a stopped consumer leaves nothing
/// writing in the background. An apply cut off mid-transaction rolls back.
impl Drop for WorkerPool {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// The worker that owns `did`'s events.
pub fn worker_for(did: &str, workers: usize) -> usize {
    let mut hasher = DefaultHasher::new();
    did.hash(&mut hasher);
    (hasher.finish() % workers.max(1) as u64) as usize
}

impl WorkerPool {
    pub fn spawn(
        workers: usize,
        capacity: usize,
        apply: ApplyFn,
        watermark: Arc<WatermarkHandle>,
        retry: RetryPolicy,
        on_give_up: GiveUpFn,
    ) -> Self {
        let workers = workers.max(1);
        let depths: Arc<Vec<AtomicUsize>> =
            Arc::new((0..workers).map(|_| AtomicUsize::new(0)).collect());
        let busy_since: Arc<Vec<AtomicI64>> =
            Arc::new((0..workers).map(|_| AtomicI64::new(IDLE)).collect());
        let (senders, tasks) = (0..workers)
            .map(|index| {
                let (tx, rx) = mpsc::channel(capacity.max(1));
                let task = tokio::spawn(run_worker(Worker {
                    index,
                    rx,
                    apply: Arc::clone(&apply),
                    watermark: Arc::clone(&watermark),
                    depths: Arc::clone(&depths),
                    busy_since: Arc::clone(&busy_since),
                    retry: retry.clone(),
                    on_give_up: Arc::clone(&on_give_up),
                }));
                (tx, task)
            })
            .unzip();
        Self {
            senders,
            depths,
            busy_since,
            watermark,
            tasks,
        }
    }

    /// Queue `job` on its DID's worker, waiting while that queue is full.
    pub async fn dispatch(&self, job: Job) {
        let index = worker_for(&job.event.did, self.senders.len());
        self.depths[index].fetch_add(1, Ordering::Relaxed);
        if let Err(mpsc::error::SendError(job)) = self.senders[index].send(job).await {
            self.depths[index].fetch_sub(1, Ordering::Relaxed);
            // Nothing will ever apply it, so retire the ticket here rather
            // than hold the watermark back for good.
            self.watermark.complete(job.ticket);
            tracing::error!(
                worker = index,
                did = %job.event.did,
                "a jetstream worker has stopped; dropping an event it was sent"
            );
        }
    }

    /// The live queue depths, for the status page.
    pub fn depth_counters(&self) -> Arc<Vec<AtomicUsize>> {
        Arc::clone(&self.depths)
    }

    /// When each worker's current job started, in unix milliseconds. A job
    /// being applied or retried is not in the queue depth, so this is what
    /// shows a worker stuck on one event.
    pub fn busy_since_counters(&self) -> Arc<Vec<AtomicI64>> {
        Arc::clone(&self.busy_since)
    }

    pub fn busy_since(&self) -> Vec<Option<i64>> {
        busy_since_values(&self.busy_since)
    }

    pub fn depths(&self) -> Vec<usize> {
        self.depths
            .iter()
            .map(|depth| depth.load(Ordering::Relaxed))
            .collect()
    }
}

/// The `busy_since` value of a worker with no job in hand.
const IDLE: i64 = i64::MIN;

/// Reads a `busy_since` set as `None` for idle workers.
pub fn busy_since_values(busy_since: &[AtomicI64]) -> Vec<Option<i64>> {
    busy_since
        .iter()
        .map(|since| match since.load(Ordering::Relaxed) {
            IDLE => None,
            ms => Some(ms),
        })
        .collect()
}

struct Worker {
    index: usize,
    rx: mpsc::Receiver<Job>,
    apply: ApplyFn,
    watermark: Arc<WatermarkHandle>,
    depths: Arc<Vec<AtomicUsize>>,
    busy_since: Arc<Vec<AtomicI64>>,
    retry: RetryPolicy,
    on_give_up: GiveUpFn,
}

async fn run_worker(mut worker: Worker) {
    while let Some(Job { ticket, event }) = worker.rx.recv().await {
        worker.depths[worker.index].fetch_sub(1, Ordering::Relaxed);
        worker.busy_since[worker.index]
            .store(chrono::Utc::now().timestamp_millis(), Ordering::Relaxed);
        apply_with_retries(&worker, event).await;
        worker.busy_since[worker.index].store(IDLE, Ordering::Relaxed);
        worker.watermark.complete(ticket);
    }
}

/// Apply `event`, retrying after each delay, and give it up once a `Failed`
/// event runs out of them. `Unavailable` attempts never count toward that and
/// are retried until the event applies. A panic counts as a failure: it must
/// not kill the worker, or its tickets would never complete and the saved
/// cursor would stop for good.
async fn apply_with_retries(worker: &Worker, event: Event) {
    let mut delays = worker.retry.delays.iter();
    let mut unavailable_retries = 0;
    let error = loop {
        let error = match AssertUnwindSafe((worker.apply)(event.clone()))
            .catch_unwind()
            .await
        {
            Ok(ApplyOutcome::Applied) => {
                if unavailable_retries > 0 {
                    tracing::info!(
                        worker = worker.index,
                        did = %event.did,
                        unavailable_retries,
                        "applied a jetstream event once it became available"
                    );
                }
                return;
            }
            Ok(ApplyOutcome::Unavailable(error)) => {
                let delay = worker.retry.unavailable_delay(unavailable_retries);
                if unavailable_retries % 10 == 0 {
                    tracing::warn!(
                        worker = worker.index,
                        did = %event.did,
                        %error,
                        unavailable_retries,
                        retry_in_ms = delay.as_millis() as u64,
                        "a jetstream event cannot be applied while unavailable; retrying until it can"
                    );
                }
                unavailable_retries += 1;
                tokio::time::sleep(delay).await;
                continue;
            }
            Ok(ApplyOutcome::Failed(error)) => error,
            Err(panic) => format!("panicked: {}", panic_message(&*panic)),
        };
        let Some(delay) = delays.next() else {
            break error;
        };
        tracing::warn!(
            worker = worker.index,
            did = %event.did,
            %error,
            retry_in_ms = delay.as_millis() as u64,
            "applying a jetstream event failed; retrying"
        );
        tokio::time::sleep(*delay).await;
    };
    tracing::error!(
        worker = worker.index,
        did = %event.did,
        %error,
        "applying a jetstream event kept failing; giving up on it"
    );
    if AssertUnwindSafe((worker.on_give_up)(event, error))
        .catch_unwind()
        .await
        .is_err()
    {
        tracing::error!(
            worker = worker.index,
            "giving up on a jetstream event panicked"
        );
    }
}

fn panic_message(panic: &(dyn Any + Send)) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else {
        "a non-string panic".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use crate::jetstream::event::{CursorPoint, EventKind};

    fn event(did: &str, seq: i64) -> Event {
        Event {
            did: did.into(),
            cursor: CursorPoint::V2 {
                seq,
                witnessed_us: seq,
            },
            kind: EventKind::Identity { handle: None },
        }
    }

    async fn send(pool: &WorkerPool, watermark: &WatermarkHandle, event: Event) {
        let ticket = watermark.begin(event.cursor);
        pool.dispatch(Job { ticket, event }).await;
    }

    fn fast_retry(attempts_after_the_first: usize) -> RetryPolicy {
        RetryPolicy {
            delays: vec![Duration::from_millis(1); attempts_after_the_first],
            max_unavailable_delay: Duration::from_millis(2),
        }
    }

    type GaveUp = Arc<Mutex<Vec<(i64, String)>>>;

    fn recording_give_up() -> (GiveUpFn, GaveUp) {
        let gave_up: GaveUp = Arc::default();
        let on_give_up: GiveUpFn = {
            let gave_up = Arc::clone(&gave_up);
            Arc::new(move |event: Event, error: String| {
                let gave_up = Arc::clone(&gave_up);
                async move {
                    let seq = event.cursor.seq().expect("seq");
                    gave_up.lock().unwrap().push((seq, error));
                }
                .boxed()
            })
        };
        (on_give_up, gave_up)
    }

    #[test]
    fn a_did_always_maps_to_the_same_worker() {
        for did in ["did:plc:a", "did:plc:b", "did:web:example.com"] {
            let worker = worker_for(did, 8);
            assert!(worker < 8);
            assert_eq!(worker, worker_for(did, 8));
        }
        assert_eq!(worker_for("did:plc:a", 1), 0);
        assert_eq!(worker_for("did:plc:a", 0), 0);
    }

    #[test]
    fn the_default_retry_backs_off_from_one_to_sixteen_seconds() {
        let policy = RetryPolicy::default();
        assert_eq!(
            policy.delays,
            [1, 2, 4, 8, 16].map(Duration::from_secs).to_vec()
        );
        assert_eq!(policy.max_unavailable_delay, MAX_UNAVAILABLE_DELAY);
    }

    #[test]
    fn unavailable_retries_repeat_the_last_delay_up_to_the_cap() {
        let policy = RetryPolicy {
            delays: [1, 2, 40].map(Duration::from_secs).to_vec(),
            max_unavailable_delay: MAX_UNAVAILABLE_DELAY,
        };
        let delays: Vec<u64> = (0..5)
            .map(|retry| policy.unavailable_delay(retry).as_secs())
            .collect();
        assert_eq!(delays, vec![1, 2, 30, 30, 30]);
        let walked: Vec<u64> = (0..7)
            .map(|retry| RetryPolicy::default().unavailable_delay(retry).as_secs())
            .collect();
        assert_eq!(walked, vec![1, 2, 4, 8, 16, 16, 16]);
    }

    async fn wait_for(what: &str, condition: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
    }

    #[tokio::test]
    async fn an_unavailable_event_holds_the_watermark_until_it_applies() {
        let healthy = Arc::new(AtomicBool::new(false));
        let attempts: Arc<Mutex<Vec<i64>>> = Arc::default();
        let apply: ApplyFn = {
            let healthy = Arc::clone(&healthy);
            let attempts = Arc::clone(&attempts);
            Arc::new(move |event: Event| {
                let healthy = Arc::clone(&healthy);
                let attempts = Arc::clone(&attempts);
                async move {
                    attempts
                        .lock()
                        .unwrap()
                        .push(event.cursor.seq().expect("seq"));
                    if healthy.load(Ordering::SeqCst) {
                        ApplyOutcome::Applied
                    } else {
                        ApplyOutcome::Unavailable("the database is unreachable".into())
                    }
                }
                .boxed()
            })
        };
        let (on_give_up, gave_up) = recording_give_up();
        let watermark = Arc::new(WatermarkHandle::new());
        let pool = WorkerPool::spawn(
            1,
            8,
            apply,
            Arc::clone(&watermark),
            fast_retry(2),
            on_give_up,
        );
        send(&pool, &watermark, event("did:plc:same", 1)).await;
        send(&pool, &watermark, event("did:plc:same", 2)).await;

        wait_for("ten unavailable attempts", || {
            attempts.lock().unwrap().len() >= 10
        })
        .await;
        assert!(
            attempts.lock().unwrap().iter().all(|seq| *seq == 1),
            "seq 2 waits behind seq 1"
        );
        assert_eq!(watermark.applied(), None, "the watermark holds");
        assert_eq!(watermark.in_flight(), 2);

        healthy.store(true, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(5), watermark.wait_idle())
            .await
            .expect("drained");
        assert!(gave_up.lock().unwrap().is_empty());
        assert_eq!(
            watermark.applied(),
            Some(CursorPoint::V2 {
                seq: 2,
                witnessed_us: 2
            })
        );
    }

    /// Plays `script` out for seq 1, one outcome per attempt, then applies.
    fn scripted(script: Vec<ApplyOutcome>) -> (ApplyFn, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let script = Arc::new(script);
        let apply: ApplyFn = {
            let calls = Arc::clone(&calls);
            Arc::new(move |_event: Event| {
                let call = calls.fetch_add(1, Ordering::SeqCst);
                let outcome = script.get(call).cloned().unwrap_or(ApplyOutcome::Applied);
                async move { outcome }.boxed()
            })
        };
        (apply, calls)
    }

    #[tokio::test]
    async fn a_long_outage_never_gives_up() {
        let (apply, calls) = scripted(vec![ApplyOutcome::Unavailable("down".into()); 50]);
        let (on_give_up, gave_up) = recording_give_up();
        let watermark = Arc::new(WatermarkHandle::new());
        let pool = WorkerPool::spawn(
            1,
            8,
            apply,
            Arc::clone(&watermark),
            fast_retry(2),
            on_give_up,
        );
        send(&pool, &watermark, event("did:plc:same", 1)).await;
        tokio::time::timeout(Duration::from_secs(10), watermark.wait_idle())
            .await
            .expect("drained");
        assert_eq!(calls.load(Ordering::SeqCst), 51);
        assert!(gave_up.lock().unwrap().is_empty());
        assert_eq!(
            watermark.applied(),
            Some(CursorPoint::V2 {
                seq: 1,
                witnessed_us: 1
            })
        );
    }

    #[tokio::test]
    async fn only_failed_attempts_count_toward_giving_up() {
        let u = || ApplyOutcome::Unavailable("down".into());
        let f = |n: u32| ApplyOutcome::Failed(format!("failure {n}"));
        // Two retries allow three failed attempts; the unavailable ones
        // between them do not count.
        let (apply, calls) = scripted(vec![u(), f(1), u(), u(), f(2), u(), f(3)]);
        let (on_give_up, gave_up) = recording_give_up();
        let watermark = Arc::new(WatermarkHandle::new());
        let pool = WorkerPool::spawn(
            1,
            8,
            apply,
            Arc::clone(&watermark),
            fast_retry(2),
            on_give_up,
        );
        send(&pool, &watermark, event("did:plc:same", 1)).await;
        tokio::time::timeout(Duration::from_secs(5), watermark.wait_idle())
            .await
            .expect("drained");
        assert_eq!(calls.load(Ordering::SeqCst), 7);
        assert_eq!(*gave_up.lock().unwrap(), vec![(1, "failure 3".to_string())]);
    }

    #[tokio::test]
    async fn dropping_the_pool_stops_a_retrying_worker() {
        let (apply, calls) = scripted(vec![ApplyOutcome::Unavailable("down".into()); 100_000]);
        let (on_give_up, _gave_up) = recording_give_up();
        let watermark = Arc::new(WatermarkHandle::new());
        let pool = WorkerPool::spawn(
            1,
            8,
            apply,
            Arc::clone(&watermark),
            fast_retry(1),
            on_give_up,
        );
        send(&pool, &watermark, event("did:plc:same", 1)).await;
        wait_for("the worker to start retrying", || {
            calls.load(Ordering::SeqCst) >= 3
        })
        .await;

        drop(pool);
        tokio::time::sleep(Duration::from_millis(20)).await;
        let after_drop = calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            after_drop,
            "the worker kept applying after the pool was dropped"
        );
        assert_eq!(watermark.applied(), None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn each_dids_events_apply_in_order_across_eight_workers() {
        let seen: Arc<Mutex<HashMap<String, Vec<i64>>>> = Arc::default();
        let apply: ApplyFn = {
            let seen = Arc::clone(&seen);
            Arc::new(move |event: Event| {
                let seen = Arc::clone(&seen);
                async move {
                    let seq = event.cursor.seq().expect("seq");
                    // Uneven work, so workers finish out of read order.
                    tokio::time::sleep(Duration::from_micros((seq % 7) as u64 * 50)).await;
                    seen.lock().unwrap().entry(event.did).or_default().push(seq);
                    ApplyOutcome::Applied
                }
                .boxed()
            })
        };
        let (on_give_up, gave_up) = recording_give_up();
        let watermark = Arc::new(WatermarkHandle::new());
        // A queue of 4 makes dispatch wait, exercising backpressure.
        let pool = WorkerPool::spawn(
            8,
            4,
            apply,
            Arc::clone(&watermark),
            fast_retry(1),
            on_give_up,
        );

        let mut seq = 0;
        for _ in 0..40 {
            for d in 0..50 {
                seq += 1;
                send(&pool, &watermark, event(&format!("did:plc:{d}"), seq)).await;
            }
        }
        tokio::time::timeout(Duration::from_secs(30), watermark.wait_idle())
            .await
            .expect("drained");

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 50);
        for (did, seqs) in seen.iter() {
            assert_eq!(seqs.len(), 40, "{did}");
            assert!(
                seqs.windows(2).all(|w| w[0] < w[1]),
                "{did} applied out of order: {seqs:?}"
            );
        }
        assert_eq!(
            watermark.applied(),
            Some(CursorPoint::V2 {
                seq: 2000,
                witnessed_us: 2000
            })
        );
        assert_eq!(pool.depths(), vec![0; 8]);
        assert!(gave_up.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_failed_event_is_retried_in_place_until_it_applies() {
        let attempts: Arc<Mutex<Vec<i64>>> = Arc::default();
        let apply: ApplyFn = {
            let attempts = Arc::clone(&attempts);
            Arc::new(move |event: Event| {
                let attempts = Arc::clone(&attempts);
                async move {
                    let seq = event.cursor.seq().expect("seq");
                    let mut attempts = attempts.lock().unwrap();
                    attempts.push(seq);
                    let tries = attempts.iter().filter(|s| **s == seq).count();
                    if seq == 1 && tries < 3 {
                        ApplyOutcome::Failed(format!("try {tries}"))
                    } else {
                        ApplyOutcome::Applied
                    }
                }
                .boxed()
            })
        };
        let (on_give_up, gave_up) = recording_give_up();
        let watermark = Arc::new(WatermarkHandle::new());
        let pool = WorkerPool::spawn(
            1,
            8,
            apply,
            Arc::clone(&watermark),
            fast_retry(5),
            on_give_up,
        );
        send(&pool, &watermark, event("did:plc:same", 1)).await;
        send(&pool, &watermark, event("did:plc:same", 2)).await;
        tokio::time::timeout(Duration::from_secs(5), watermark.wait_idle())
            .await
            .expect("drained");

        assert_eq!(
            *attempts.lock().unwrap(),
            vec![1, 1, 1, 2],
            "seq 2 waits behind seq 1's retries"
        );
        assert!(gave_up.lock().unwrap().is_empty());
        assert_eq!(
            watermark.applied(),
            Some(CursorPoint::V2 {
                seq: 2,
                witnessed_us: 2
            })
        );
    }

    #[tokio::test]
    async fn an_event_that_keeps_failing_is_given_up_once_and_completed() {
        let attempts: Arc<Mutex<Vec<i64>>> = Arc::default();
        let apply: ApplyFn = {
            let attempts = Arc::clone(&attempts);
            Arc::new(move |event: Event| {
                let attempts = Arc::clone(&attempts);
                async move {
                    let seq = event.cursor.seq().expect("seq");
                    attempts.lock().unwrap().push(seq);
                    if seq == 1 {
                        ApplyOutcome::Failed("the database is down".into())
                    } else {
                        ApplyOutcome::Applied
                    }
                }
                .boxed()
            })
        };
        let (on_give_up, gave_up) = recording_give_up();
        let watermark = Arc::new(WatermarkHandle::new());
        let pool = WorkerPool::spawn(
            1,
            8,
            apply,
            Arc::clone(&watermark),
            fast_retry(3),
            on_give_up,
        );
        send(&pool, &watermark, event("did:plc:same", 1)).await;
        send(&pool, &watermark, event("did:plc:same", 2)).await;
        tokio::time::timeout(Duration::from_secs(5), watermark.wait_idle())
            .await
            .expect("drained");

        assert_eq!(
            *attempts.lock().unwrap(),
            vec![1, 1, 1, 1, 2],
            "one attempt plus one per retry delay, then the next event"
        );
        assert_eq!(
            *gave_up.lock().unwrap(),
            vec![(1, "the database is down".to_string())]
        );
        assert_eq!(
            watermark.applied(),
            Some(CursorPoint::V2 {
                seq: 2,
                witnessed_us: 2
            })
        );
    }

    #[tokio::test]
    async fn a_panicking_event_does_not_stall_the_watermark() {
        let applied: Arc<Mutex<Vec<i64>>> = Arc::default();
        let apply: ApplyFn = {
            let applied = Arc::clone(&applied);
            Arc::new(move |event: Event| {
                let applied = Arc::clone(&applied);
                async move {
                    let seq = event.cursor.seq().expect("seq");
                    if seq == 2 {
                        panic!("a bad event");
                    }
                    applied.lock().unwrap().push(seq);
                    ApplyOutcome::Applied
                }
                .boxed()
            })
        };
        let (on_give_up, gave_up) = recording_give_up();
        let watermark = Arc::new(WatermarkHandle::new());
        let pool = WorkerPool::spawn(
            2,
            8,
            apply,
            Arc::clone(&watermark),
            fast_retry(2),
            on_give_up,
        );
        for seq in 1..=3 {
            send(&pool, &watermark, event("did:plc:same", seq)).await;
        }
        tokio::time::timeout(Duration::from_secs(5), watermark.wait_idle())
            .await
            .expect("the worker survived the panic");
        assert_eq!(*applied.lock().unwrap(), vec![1, 3]);
        let gave_up = gave_up.lock().unwrap();
        assert_eq!(gave_up.len(), 1, "a panic retries, then gives up once");
        assert_eq!(gave_up[0].0, 2);
        assert!(
            gave_up[0].1.contains("a bad event"),
            "the panic message is the error: {:?}",
            gave_up[0].1
        );
        assert_eq!(
            watermark.applied(),
            Some(CursorPoint::V2 {
                seq: 3,
                witnessed_us: 3
            })
        );
    }

    #[tokio::test]
    async fn a_job_for_a_stopped_worker_still_completes_its_ticket() {
        let watermark = Arc::new(WatermarkHandle::new());
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let pool = WorkerPool {
            senders: vec![tx],
            depths: Arc::new(vec![AtomicUsize::new(0)]),
            busy_since: Arc::new(vec![AtomicI64::new(IDLE)]),
            watermark: Arc::clone(&watermark),
            tasks: Vec::new(),
        };
        send(&pool, &watermark, event("did:plc:same", 1)).await;
        assert_eq!(watermark.in_flight(), 0);
        assert_eq!(
            watermark.applied(),
            Some(CursorPoint::V2 {
                seq: 1,
                witnessed_us: 1
            })
        );
        assert_eq!(pool.depths(), vec![0]);
    }

    #[tokio::test]
    async fn a_worker_retrying_an_event_reports_when_it_started() {
        let healthy = Arc::new(AtomicBool::new(false));
        let apply: ApplyFn = {
            let healthy = Arc::clone(&healthy);
            Arc::new(move |_event: Event| {
                let healthy = Arc::clone(&healthy);
                async move {
                    if healthy.load(Ordering::SeqCst) {
                        ApplyOutcome::Applied
                    } else {
                        ApplyOutcome::Unavailable("down".into())
                    }
                }
                .boxed()
            })
        };
        let (on_give_up, _gave_up) = recording_give_up();
        let watermark = Arc::new(WatermarkHandle::new());
        let pool = WorkerPool::spawn(
            2,
            8,
            apply,
            Arc::clone(&watermark),
            fast_retry(2),
            on_give_up,
        );
        assert_eq!(pool.busy_since(), vec![None, None]);

        let before = chrono::Utc::now().timestamp_millis();
        let did = "did:plc:stuck";
        send(&pool, &watermark, event(did, 1)).await;
        let stuck = worker_for(did, 2);
        wait_for("the worker to take the job", || {
            pool.busy_since()[stuck].is_some()
        })
        .await;

        // The queue is empty, but the worker is busy with the retried job.
        assert_eq!(pool.depths(), vec![0, 0]);
        let busy = pool.busy_since();
        assert!(busy[stuck].is_some_and(|ms| ms >= before));
        assert_eq!(busy[1 - stuck], None);

        healthy.store(true, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(5), watermark.wait_idle())
            .await
            .expect("drained");
        wait_for("the worker to go idle", || {
            pool.busy_since() == vec![None, None]
        })
        .await;
    }
}

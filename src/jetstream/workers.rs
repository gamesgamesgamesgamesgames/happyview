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

use std::any::Any;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use tokio::sync::mpsc;

use super::event::Event;
use super::watermark::WatermarkHandle;

/// What applying one event came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied,
    /// Worth retrying; the string says why it failed.
    Failed(String),
}

/// What a worker does with each event.
pub type ApplyFn = Arc<dyn Fn(Event) -> BoxFuture<'static, ApplyOutcome> + Send + Sync>;

/// Called once for an event that still failed after every retry, with the
/// last error.
pub type GiveUpFn = Arc<dyn Fn(Event, String) -> BoxFuture<'static, ()> + Send + Sync>;

/// The waits before each retry of a failed event; one retry per delay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    pub delays: Vec<Duration>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            delays: [1, 2, 4, 8, 16].map(Duration::from_secs).to_vec(),
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
    watermark: Arc<WatermarkHandle>,
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
        let senders = (0..workers)
            .map(|index| {
                let (tx, rx) = mpsc::channel(capacity.max(1));
                tokio::spawn(run_worker(Worker {
                    index,
                    rx,
                    apply: Arc::clone(&apply),
                    watermark: Arc::clone(&watermark),
                    depths: Arc::clone(&depths),
                    retry: retry.clone(),
                    on_give_up: Arc::clone(&on_give_up),
                }));
                tx
            })
            .collect();
        Self {
            senders,
            depths,
            watermark,
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

    pub fn depths(&self) -> Vec<usize> {
        self.depths
            .iter()
            .map(|depth| depth.load(Ordering::Relaxed))
            .collect()
    }
}

struct Worker {
    index: usize,
    rx: mpsc::Receiver<Job>,
    apply: ApplyFn,
    watermark: Arc<WatermarkHandle>,
    depths: Arc<Vec<AtomicUsize>>,
    retry: RetryPolicy,
    on_give_up: GiveUpFn,
}

async fn run_worker(mut worker: Worker) {
    while let Some(Job { ticket, event }) = worker.rx.recv().await {
        worker.depths[worker.index].fetch_sub(1, Ordering::Relaxed);
        apply_with_retries(&worker, event).await;
        worker.watermark.complete(ticket);
    }
}

/// Apply `event`, retrying after each delay, and give it up once they run
/// out. A panic counts as a failure: it must not kill the worker, or its
/// tickets would never complete and the saved cursor would stop for good.
async fn apply_with_retries(worker: &Worker, event: Event) {
    let mut delays = worker.retry.delays.iter();
    let error = loop {
        let error = match AssertUnwindSafe((worker.apply)(event.clone()))
            .catch_unwind()
            .await
        {
            Ok(ApplyOutcome::Applied) => return,
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
        assert_eq!(
            RetryPolicy::default().delays,
            [1, 2, 4, 8, 16].map(Duration::from_secs).to_vec()
        );
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
            watermark: Arc::clone(&watermark),
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
}

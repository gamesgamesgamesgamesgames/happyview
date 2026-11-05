//! Library calls a guest has started through `host_call_library_start` and not
//! yet collected through `host_call_library_wait_any`.
//!
//! Each call runs as its own tokio task on its own store, so nothing here
//! borrows the guest's store and the guest keeps running while they do. The
//! tasks belong to the instance that started them: dropping the instance
//! drops this, and dropping this aborts every call still outstanding, so no
//! call outlives the run that asked for it.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use serde_json::Value;
use tokio::sync::Semaphore;
use tokio::task::{JoinError, JoinHandle};

use crate::plugin::PluginResponse;
use crate::plugin::library::MAX_CONCURRENT_LIBRARY_CALLS;

/// One started call's outcome: the `{ok}`/`{error}` envelope
/// `host_call_library` would have answered with.
pub(crate) type CallOutcome = PluginResponse<Value>;

/// A spawned call that is aborted when its handle is dropped, which is what
/// ties a call's life to the instance holding it.
pub(crate) struct AbortOnDrop(JoinHandle<CallOutcome>);

impl AbortOnDrop {
    fn is_finished(&self) -> bool {
        self.0.is_finished()
    }
}

impl Future for AbortOnDrop {
    type Output = Result<CallOutcome, JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx)
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The calls one instance has in flight, queued, or settled and unclaimed.
pub struct PendingCalls {
    next_handle: u32,
    slots: Arc<Semaphore>,
    calls: HashMap<u32, AbortOnDrop>,
}

impl Default for PendingCalls {
    fn default() -> Self {
        Self {
            next_handle: 1,
            slots: Arc::new(Semaphore::new(MAX_CONCURRENT_LIBRARY_CALLS)),
            calls: HashMap::new(),
        }
    }
}

impl PendingCalls {
    /// Spawn `call` behind a concurrency slot and return its handle. The
    /// handle is issued at once whether or not a slot is free; the task waits
    /// for one before it does anything, so a call queued past the cap has
    /// not instantiated its library yet.
    pub(crate) fn start<F>(&mut self, call: F) -> Option<u32>
    where
        F: Future<Output = CallOutcome> + Send + 'static,
    {
        let slots = self.slots.clone();
        self.spawn(async move {
            // The semaphore is never closed, so this only fails if that
            // changes; say so rather than run past the cap.
            let Ok(_slot) = slots.acquire_owned().await else {
                return error("HOST_ERROR", "the library call queue is closed");
            };
            call.await
        })
    }

    /// A handle whose call failed before it could run — bad arguments, a
    /// missing capability. It takes no slot, and settles the way a call does,
    /// so a guest has one place to read every failure from.
    pub(crate) fn settled(&mut self, outcome: CallOutcome) -> Option<u32> {
        self.spawn(async move { outcome })
    }

    fn spawn<F>(&mut self, task: F) -> Option<u32>
    where
        F: Future<Output = CallOutcome> + Send + 'static,
    {
        // The import returns an `i32`, and 0 is its failure, so a handle has
        // to stay a positive `i32`. Two billion calls in one run is not a
        // limit anyone reaches.
        let handle = self.next_handle;
        if handle > i32::MAX as u32 {
            return None;
        }
        self.next_handle += 1;
        self.calls.insert(handle, AbortOnDrop(tokio::spawn(task)));
        Some(handle)
    }

    /// Take the listed calls out to be waited on, lowest handle first. A list
    /// that is empty or names a handle that is not outstanding is refused
    /// whole and takes nothing, because a wait that could never return must
    /// answer now rather than hang.
    pub(crate) fn take(&mut self, handles: &[u32]) -> Result<Waiting, String> {
        let mut handles = handles.to_vec();
        handles.sort_unstable();
        handles.dedup();
        if handles.is_empty() {
            return Err("wait_any needs at least one handle".into());
        }
        if let Some(missing) = handles.iter().find(|h| !self.calls.contains_key(h)) {
            return Err(format!(
                "handle {missing} is not outstanding: it was never issued or has already been waited on"
            ));
        }
        let calls = handles
            .into_iter()
            .map(|handle| (handle, self.calls.remove(&handle).unwrap()))
            .collect();
        Ok(Waiting { calls })
    }

    /// Put back the calls a wait did not consume.
    pub(crate) fn restore(&mut self, waiting: Waiting) {
        self.calls.extend(waiting.calls);
    }

    /// Every call still outstanding, for the end of a run.
    pub(crate) fn drain(&mut self) -> Vec<(u32, AbortOnDrop)> {
        let mut calls: Vec<_> = self.calls.drain().collect();
        calls.sort_unstable_by_key(|(handle, _)| *handle);
        calls
    }
}

/// Calls taken out of [`PendingCalls`] for one wait, sorted by handle.
pub(crate) struct Waiting {
    calls: Vec<(u32, AbortOnDrop)>,
}

impl Waiting {
    /// Wait for the first of these calls to settle and return its handle and
    /// outcome, leaving the rest in `self`. When some have already settled,
    /// the lowest of those wins, so a guest that waits on calls that are all
    /// finished sees them in handle order rather than in a scheduler's.
    pub(crate) async fn first(&mut self) -> (u32, CallOutcome) {
        if let Some(index) = self.calls.iter().position(|(_, call)| call.is_finished()) {
            let (handle, call) = self.calls.remove(index);
            return (handle, outcome_of(call.await));
        }
        let pending = self.calls.iter_mut().map(|(_, call)| call);
        let (joined, index, rest) = futures_util::future::select_all(pending).await;
        drop(rest);
        // The winner has already yielded its output, and a `JoinHandle` is
        // not polled twice; dropping it aborts a task that has finished.
        let (handle, _) = self.calls.remove(index);
        (handle, outcome_of(joined))
    }
}

/// A task that panicked or was aborted settles as a failed call.
pub(crate) fn outcome_of(joined: Result<CallOutcome, JoinError>) -> CallOutcome {
    joined.unwrap_or_else(|e| error("LIBRARY_ERROR", format!("library call did not finish: {e}")))
}

fn error(code: &str, message: impl Into<String>) -> CallOutcome {
    PluginResponse::Err {
        error: crate::plugin::PluginEnvelopeError::new(code, message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn ok(value: Value) -> CallOutcome {
        PluginResponse::Ok { ok: value }
    }

    #[tokio::test]
    async fn handles_start_at_one_and_count_up() {
        let mut calls = PendingCalls::default();
        assert_eq!(calls.settled(ok(Value::Null)), Some(1));
        assert_eq!(calls.start(async { ok(Value::Null) }), Some(2));
    }

    #[tokio::test]
    async fn an_empty_or_unknown_wait_is_refused_and_takes_nothing() {
        let mut calls = PendingCalls::default();
        let handle = calls.settled(ok(Value::Null)).unwrap();
        assert!(calls.take(&[]).is_err());
        let err = calls.take(&[handle, 99]).err().unwrap();
        assert!(err.contains("99"), "{err}");
        // The refusal took nothing: the real handle is still waitable.
        let mut waiting = calls.take(&[handle]).unwrap();
        assert_eq!(waiting.first().await.0, handle);
    }

    #[tokio::test]
    async fn a_consumed_handle_cannot_be_waited_again() {
        let mut calls = PendingCalls::default();
        let handle = calls.settled(ok(Value::Null)).unwrap();
        let mut waiting = calls.take(&[handle]).unwrap();
        waiting.first().await;
        calls.restore(waiting);
        assert!(calls.take(&[handle]).is_err());
    }

    #[tokio::test]
    async fn settled_calls_come_back_lowest_handle_first() {
        let mut calls = PendingCalls::default();
        let handles: Vec<u32> = (0..3)
            .map(|n| calls.settled(ok(Value::from(n))).unwrap())
            .collect();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let mut waiting = calls.take(&[handles[2], handles[0], handles[1]]).unwrap();
        let mut seen = Vec::new();
        for _ in 0..3 {
            seen.push(waiting.first().await.0);
        }
        assert_eq!(seen, handles);
    }

    #[tokio::test]
    async fn the_first_to_finish_wins_when_none_has_yet() {
        let mut calls = PendingCalls::default();
        let slow = calls
            .start(async {
                tokio::time::sleep(Duration::from_millis(300)).await;
                ok(Value::from("slow"))
            })
            .unwrap();
        let fast = calls
            .start(async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                ok(Value::from("fast"))
            })
            .unwrap();
        let mut waiting = calls.take(&[slow, fast]).unwrap();
        let (handle, outcome) = waiting.first().await;
        assert_eq!(handle, fast);
        assert_eq!(outcome.into_result().unwrap(), "fast");
        calls.restore(waiting);
        // The loser went back and is still waitable.
        let mut waiting = calls.take(&[slow]).unwrap();
        assert_eq!(waiting.first().await.0, slow);
    }

    #[tokio::test]
    async fn no_more_than_the_cap_run_at_once() {
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut calls = PendingCalls::default();
        let handles: Vec<u32> = (0..MAX_CONCURRENT_LIBRARY_CALLS + 4)
            .map(|_| {
                let (running, peak) = (running.clone(), peak.clone());
                calls
                    .start(async move {
                        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(30)).await;
                        running.fetch_sub(1, Ordering::SeqCst);
                        ok(Value::Null)
                    })
                    .unwrap()
            })
            .collect();
        let mut waiting = calls.take(&handles).unwrap();
        for _ in 0..handles.len() {
            waiting.first().await;
        }
        assert_eq!(peak.load(Ordering::SeqCst), MAX_CONCURRENT_LIBRARY_CALLS);
    }

    #[tokio::test]
    async fn dropping_the_calls_aborts_them() {
        let finished = Arc::new(AtomicUsize::new(0));
        let mut calls = PendingCalls::default();
        let counter = finished.clone();
        calls.start(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            counter.fetch_add(1, Ordering::SeqCst);
            ok(Value::Null)
        });
        drop(calls);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(finished.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_panicked_call_settles_as_a_library_error() {
        let mut calls = PendingCalls::default();
        let handle = calls
            .start(async { panic!("the callee fell over") })
            .unwrap();
        let mut waiting = calls.take(&[handle]).unwrap();
        let (_, outcome) = waiting.first().await;
        assert_eq!(outcome.into_result().unwrap_err().code, "LIBRARY_ERROR");
    }
}

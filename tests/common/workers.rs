//! Job workers a test can stop cleanly.
//!
//! Aborting a worker's task leaves the thread its job runs on still running.
//! A test that sees the job's terminal status and returns can then drop its
//! runtime while that thread still awaits a write the job makes after its
//! status (its event log), and the drop waits on the thread forever. So a
//! test stops its workers with [`Workers::stop`], which joins them.

use std::time::Duration;

use happyview::AppState;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// How long [`Workers::stop`] waits for a job still running.
const STOP_WITHIN: Duration = Duration::from_secs(60);

pub struct Workers {
    stop: watch::Sender<bool>,
    handles: Vec<JoinHandle<()>>,
}

impl Workers {
    /// One worker, as `run_worker` runs.
    pub fn one(state: &AppState) -> Self {
        let (stop, rx) = watch::channel(false);
        let handle = tokio::spawn(happyview::jobs::worker::run_worker_until(state.clone(), rx));
        Self {
            stop,
            handles: vec![handle],
        }
    }

    /// One worker claiming as `owner`, renewing each job's lease every
    /// `renew_every`.
    pub fn renewing(state: &AppState, owner: &str, renew_every: Duration) -> Self {
        let (stop, rx) = watch::channel(false);
        let handle = tokio::spawn(happyview::jobs::worker::run_worker_renewing(
            state.clone(),
            owner.to_string(),
            renew_every,
            rx,
        ));
        Self {
            stop,
            handles: vec![handle],
        }
    }

    /// `count` workers, as `spawn_workers` starts them.
    pub fn spawn(state: &AppState, count: usize) -> Self {
        let (stop, rx) = watch::channel(false);
        let handles = happyview::jobs::worker::spawn_workers_until(state.clone(), count, rx);
        Self { stop, handles }
    }

    pub fn len(&self) -> usize {
        self.handles.len()
    }

    /// Stop the workers, letting a job already running finish, and wait for
    /// them. Panics if one is still running after [`STOP_WITHIN`].
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        for handle in self.handles {
            tokio::time::timeout(STOP_WITHIN, handle)
                .await
                .expect("a job worker was still running its job a minute after being stopped")
                .expect("a job worker panicked");
        }
    }
}

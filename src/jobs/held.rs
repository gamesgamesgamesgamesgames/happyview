//! The lease a job run holds, handed to the run itself.
//!
//! A job's progress writes and its `should_stop` check come from wherever the
//! run reaches them (a script's host imports, the Lua context, a native job)
//! with only the job's id. The run's own lease rides along as a task-local,
//! set around the run's future by [`run_under`], so they find this run's
//! lease and never another's: two runs of one job in one process (a lease
//! that expired here, and the job re-claimed by another local worker) each
//! see their own attempt, and the stale one's writes fail its fence. When
//! renewal finds the job taken over it marks the lease lost, every write the
//! run makes from then on changes nothing, `should_stop` answers true, and a
//! guest is interrupted at its next host import.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub use super::db::Lease;

#[derive(Clone)]
pub struct Held {
    pub id: String,
    pub lease: Lease,
    pub lost: Arc<AtomicBool>,
}

impl Held {
    pub fn new(id: &str, lease: Lease) -> Self {
        Self {
            id: id.to_string(),
            lease,
            lost: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    pub fn mark_lost(&self) {
        self.lost.store(true, Ordering::Release);
    }
}

tokio::task_local! {
    static RUN: Held;
}

/// Run `fut` as the run of job `held.id` under `held`.
pub async fn run_under<F: Future>(held: Held, fut: F) -> F::Output {
    RUN.scope(held, fut).await
}

/// The lease of the run in progress on this task, when it is a run of job
/// `id`.
pub fn current(id: &str) -> Option<Held> {
    RUN.try_with(|held| (held.id == id).then(|| held.clone()))
        .ok()
        .flatten()
}

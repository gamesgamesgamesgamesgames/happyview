//! The leases this process holds on the jobs it is running, by job id.
//!
//! A job's progress writes and its `should_stop` check come from wherever the
//! run reaches them (a script's host imports, the Lua context, a native job)
//! with only the job's id. This is where they find the lease to fence on, and
//! whether it has been lost: when renewal finds the job taken over, it marks
//! the lease lost here, every write the run makes from then on changes
//! nothing, `should_stop` answers true, and a guest is interrupted at its
//! next host import.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use super::db::Lease;

#[derive(Clone)]
pub struct Held {
    pub lease: Lease,
    pub lost: Arc<AtomicBool>,
}

impl Held {
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    pub fn mark_lost(&self) {
        self.lost.store(true, Ordering::Release);
    }
}

static HELD: LazyLock<Mutex<HashMap<String, Held>>> = LazyLock::new(Default::default);

fn held() -> std::sync::MutexGuard<'static, HashMap<String, Held>> {
    HELD.lock().unwrap_or_else(|e| e.into_inner())
}

/// Record that this process runs job `id` under `lease`, until the returned
/// guard is dropped.
pub fn hold(id: &str, lease: Lease) -> HoldGuard {
    let entry = Held {
        lease,
        lost: Arc::new(AtomicBool::new(false)),
    };
    held().insert(id.to_string(), entry.clone());
    HoldGuard {
        id: id.to_string(),
        held: entry,
    }
}

/// The lease this process holds on job `id`, if it is running it.
pub fn get(id: &str) -> Option<Held> {
    held().get(id).cloned()
}

pub struct HoldGuard {
    id: String,
    held: Held,
}

impl HoldGuard {
    pub fn held(&self) -> &Held {
        &self.held
    }
}

impl Drop for HoldGuard {
    fn drop(&mut self) {
        let mut map = held();
        // Only this hold's own entry: a later hold of the same id replaces it.
        if map
            .get(&self.id)
            .is_some_and(|h| Arc::ptr_eq(&h.lost, &self.held.lost))
        {
            map.remove(&self.id);
        }
    }
}

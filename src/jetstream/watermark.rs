//! The applied watermark: the furthest point in the stream such that every
//! event at or before it has been applied.
//!
//! Workers finish events out of order (one slow DID does not hold up the
//! others), so the last event read can be well past the last one every
//! worker has finished. Saving the read position would let a crash skip the
//! unfinished ones. This hands out tickets in read order and only advances
//! over a contiguous run of finished tickets.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard};

use tokio::sync::Notify;

use super::event::CursorPoint;

#[derive(Debug, Default)]
pub struct Watermark {
    /// The ticket of `pending[0]`.
    front: u64,
    /// Every ticket from `front` on, in read order, and whether it finished.
    pending: VecDeque<(CursorPoint, bool)>,
    applied: Option<CursorPoint>,
}

impl Watermark {
    /// Track an event about to be dispatched; returns its ticket.
    pub fn begin(&mut self, point: CursorPoint) -> u64 {
        let ticket = self.front + self.pending.len() as u64;
        self.pending.push_back((point, false));
        ticket
    }

    /// Mark a ticket finished, and advance over every finished ticket at the
    /// front. Unknown and already-retired tickets are ignored.
    pub fn complete(&mut self, ticket: u64) {
        let Some(index) = ticket.checked_sub(self.front) else {
            return;
        };
        let Some(slot) = self.pending.get_mut(index as usize) else {
            return;
        };
        slot.1 = true;
        while self.pending.front().is_some_and(|(_, done)| *done) {
            let (point, _) = self.pending.pop_front().expect("front exists");
            self.applied = Some(point);
            self.front += 1;
        }
    }

    pub fn applied(&self) -> Option<CursorPoint> {
        self.applied
    }

    /// Tickets not yet retired: unfinished, or finished behind one that is.
    pub fn in_flight(&self) -> usize {
        self.pending.len()
    }
}

/// A `Watermark` shared between the reader and the workers.
#[derive(Default)]
pub struct WatermarkHandle {
    inner: Mutex<Watermark>,
    idle: Notify,
}

impl WatermarkHandle {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Watermark> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn begin(&self, point: CursorPoint) -> u64 {
        self.lock().begin(point)
    }

    pub fn complete(&self, ticket: u64) {
        let idle = {
            let mut watermark = self.lock();
            watermark.complete(ticket);
            watermark.in_flight() == 0
        };
        if idle {
            self.idle.notify_waiters();
        }
    }

    pub fn applied(&self) -> Option<CursorPoint> {
        self.lock().applied()
    }

    pub fn in_flight(&self) -> usize {
        self.lock().in_flight()
    }

    /// Wait until every dispatched event has been applied.
    pub async fn wait_idle(&self) {
        loop {
            let notified = self.idle.notified();
            tokio::pin!(notified);
            // Registered before the check, so a completion between the check
            // and the await still wakes this.
            notified.as_mut().enable();
            if self.in_flight() == 0 {
                return;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    fn p(seq: i64) -> CursorPoint {
        CursorPoint::V2 {
            seq,
            witnessed_us: seq,
        }
    }

    #[test]
    fn the_watermark_waits_for_the_oldest_unfinished_event() {
        let mut w = Watermark::default();
        let t: Vec<u64> = (1..=5).map(|s| w.begin(p(s))).collect();
        w.complete(t[1]);
        w.complete(t[2]);
        assert_eq!(
            w.applied(),
            None,
            "seq 1 is unfinished, so nothing is applied yet"
        );
        w.complete(t[0]);
        assert_eq!(w.applied(), Some(p(3)));
        w.complete(t[4]);
        assert_eq!(w.applied(), Some(p(3)), "seq 4 is unfinished");
        assert_eq!(w.in_flight(), 2);
        w.complete(t[3]);
        assert_eq!(w.applied(), Some(p(5)));
        assert_eq!(w.in_flight(), 0);
    }

    #[test]
    fn stale_and_unknown_tickets_are_ignored() {
        let mut w = Watermark::default();
        let t = w.begin(p(1));
        w.complete(t);
        w.complete(t);
        w.complete(99);
        assert_eq!(w.applied(), Some(p(1)));
        assert_eq!(w.in_flight(), 0);
    }

    /// Completed in a scrambled order, the watermark is always exactly the
    /// event before the first unfinished one: never past it.
    #[test]
    fn the_watermark_never_passes_an_unfinished_event() {
        let mut w = Watermark::default();
        let tickets: Vec<u64> = (1..=200).map(|s| w.begin(p(s))).collect();
        let mut order: Vec<usize> = (0..200).collect();
        let mut x: u64 = 0x2545_F491_4F6C_DD1D;
        for i in (1..order.len()).rev() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            order.swap(i, (x % (i as u64 + 1)) as usize);
        }
        let mut done = [false; 200];
        for i in order {
            w.complete(tickets[i]);
            done[i] = true;
            let expected = match done.iter().position(|d| !d) {
                Some(0) => None,
                Some(n) => Some(p(n as i64)),
                None => Some(p(200)),
            };
            assert_eq!(w.applied(), expected);
        }
    }

    #[tokio::test]
    async fn wait_idle_returns_once_everything_completes() {
        let handle = Arc::new(WatermarkHandle::new());
        handle.wait_idle().await;
        let ticket = handle.begin(p(1));
        let waiter = {
            let handle = Arc::clone(&handle);
            tokio::spawn(async move { handle.wait_idle().await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiter.is_finished());
        handle.complete(ticket);
        tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("idle")
            .expect("join");
    }
}

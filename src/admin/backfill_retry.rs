use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// First backoff step; doubles per consecutive failure.
const BASE_BACKOFF: Duration = Duration::from_millis(500);

/// Ceiling on any single cooldown. Matches the cap `parse_retry_after` applies
/// to `RateLimit-Reset`, so neither a hostile header nor a long failure streak
/// can park a job for an unbounded time.
pub const MAX_BACKOFF: Duration = Duration::from_secs(120);

/// Consecutive failures against one host after which it is treated as down and
/// its queued work is given up on rather than retried one cooldown at a time.
/// See `HostCooldowns::is_saturated`.
pub const SATURATED_CONSECUTIVE_FAILURES: u32 = 5;

/// Exponential backoff for the Nth consecutive failure against a host.
/// No jitter: cooldowns are keyed by host, so every DID behind one host already
/// shares a single timestamp and there is no herd to disperse.
pub fn backoff_delay(consecutive_failures: u32) -> Duration {
    if consecutive_failures == 0 {
        return Duration::ZERO;
    }
    let shift = consecutive_failures.saturating_sub(1).min(20);
    BASE_BACKOFF
        .checked_mul(1u32 << shift)
        .unwrap_or(MAX_BACKOFF)
        .min(MAX_BACKOFF)
}

#[derive(Debug)]
struct Cooldown {
    next_eligible_at: Instant,
    consecutive_failures: u32,
}

/// Per-host retry gate.
///
/// A 429 or 5xx is a property of the server, not of the repo that happened to
/// trigger it. Holding the cooldown per host means one failure teaches every
/// DID routed through that host to wait, instead of ninety-nine of them
/// re-learning it one request at a time.
#[derive(Debug, Default)]
pub struct HostCooldowns {
    entries: HashMap<String, Cooldown>,
}

impl HostCooldowns {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn eligible_at(&self, host: &str) -> Option<Instant> {
        self.entries.get(host).map(|c| c.next_eligible_at)
    }

    /// How many times in a row this host has failed without a success between.
    pub fn consecutive_failures(&self, host: &str) -> u32 {
        self.entries
            .get(host)
            .map(|c| c.consecutive_failures)
            .unwrap_or(0)
    }

    /// Whether this host has failed often enough to stop asking it.
    ///
    /// Cooldowns are per host and every deferred item behind a host shares
    /// one, so a host that never recovers gates the whole drain at one item
    /// per cooldown — 500 DIDs at three attempts against a dead PDS is ~1000
    /// pops, and by the ninth consecutive failure each pop waits the full
    /// `MAX_BACKOFF`. Declaring the host dead bounds that at
    /// O(hosts × MAX_BACKOFF) instead of O(items × MAX_BACKOFF).
    ///
    /// `record_success` clears the entry outright, so a host that answers even
    /// once is never saturated and the healthy path is untouched.
    pub fn is_saturated(&self, host: &str) -> bool {
        self.consecutive_failures(host) >= SATURATED_CONSECUTIVE_FAILURES
    }

    pub fn record_failure(&mut self, host: &str, retry_after: Option<Duration>, now: Instant) {
        let entry = self.entries.entry(host.to_string()).or_insert(Cooldown {
            next_eligible_at: now,
            consecutive_failures: 0,
        });
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        // An explicit RateLimit-Reset is real information; prefer it to a guess.
        let delay = retry_after
            .map(|d| d.min(MAX_BACKOFF))
            .unwrap_or_else(|| backoff_delay(entry.consecutive_failures));
        entry.next_eligible_at = now + delay;
    }

    pub fn record_success(&mut self, host: &str) {
        self.entries.remove(host);
    }
}

#[derive(Debug, Clone)]
pub struct DeferredItem<T> {
    pub payload: T,
    pub host: String,
    pub attempts: u32,
    pub eligible_at: Instant,
}

#[derive(Debug)]
pub enum PopResult<T> {
    Ready(DeferredItem<T>),
    WaitUntil(Instant),
    Empty,
}

/// Work that failed retryably and goes to the back of the line.
///
/// A worker blocked on a 120-second reset is a worker not resolving the
/// thousands of DIDs it could be handling instead. Deferral turns waiting into
/// ordering.
#[derive(Debug)]
pub struct DeferredQueue<T> {
    items: VecDeque<DeferredItem<T>>,
}

impl<T> Default for DeferredQueue<T> {
    fn default() -> Self {
        Self {
            items: VecDeque::new(),
        }
    }
}

impl<T> DeferredQueue<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, item: DeferredItem<T>) {
        self.items.push_back(item);
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Remove and return every queued item for one host.
    ///
    /// Used when a host is declared down: its items are handed to the error
    /// recorder in one step instead of being re-offered to it one cooldown at
    /// a time. Items for every other host stay queued and eligible.
    pub fn drain_host(&mut self, host: &str) -> Vec<DeferredItem<T>> {
        let mut taken = Vec::new();
        let mut kept = VecDeque::with_capacity(self.items.len());
        for item in self.items.drain(..) {
            if item.host == host {
                taken.push(item);
            } else {
                kept.push_back(item);
            }
        }
        self.items = kept;
        taken
    }

    /// An item is ready when both its own eligibility and its host's cooldown
    /// have passed. When nothing is ready, report the earliest moment anything
    /// will be, so the caller sleeps exactly once instead of spinning.
    pub fn pop_eligible(&mut self, cooldowns: &HostCooldowns, now: Instant) -> PopResult<T> {
        let mut earliest: Option<Instant> = None;

        for idx in 0..self.items.len() {
            let item = &self.items[idx];
            let host_gate = cooldowns.eligible_at(&item.host);
            let gate = match host_gate {
                Some(h) => h.max(item.eligible_at),
                None => item.eligible_at,
            };
            if gate <= now {
                return PopResult::Ready(self.items.remove(idx).expect("index in range"));
            }
            earliest = Some(match earliest {
                Some(e) => e.min(gate),
                None => gate,
            });
        }

        match earliest {
            Some(at) => PopResult::WaitUntil(at),
            None => PopResult::Empty,
        }
    }
}

#[derive(Debug)]
pub enum DrainStep<T> {
    /// This item is ready; the caller retries it.
    Retry(DeferredItem<T>),
    /// Nothing was ready, so we waited a slice. The caller should check for
    /// cancellation and call again.
    Slept,
    /// Queue is empty; draining is finished.
    Done,
}

/// One turn of a deferred-queue drain.
///
/// Sleeps at most `max_slice` even when the cooldown is far longer, so the
/// caller regains control often enough to notice a cancel or pause. Both the
/// resolve and fetch phases drain this way; only the retry operation differs,
/// which is why that stays with the caller.
pub async fn next_drain_step<T>(
    queue: &mut DeferredQueue<T>,
    cooldowns: &HostCooldowns,
    max_slice: Duration,
) -> DrainStep<T> {
    let now = Instant::now();
    match queue.pop_eligible(cooldowns, now) {
        PopResult::Empty => DrainStep::Done,
        PopResult::Ready(item) => DrainStep::Retry(item),
        PopResult::WaitUntil(at) => {
            let slice = at.saturating_duration_since(now).min(max_slice);
            tokio::time::sleep(slice).await;
            DrainStep::Slept
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff_delay(1), Duration::from_millis(500));
        assert_eq!(backoff_delay(2), Duration::from_secs(1));
        assert_eq!(backoff_delay(3), Duration::from_secs(2));
        assert_eq!(backoff_delay(4), Duration::from_secs(4));
        // Capped at the same 120s ceiling parse_retry_after uses, so a host
        // that keeps failing can never stall a job indefinitely.
        assert_eq!(backoff_delay(30), MAX_BACKOFF);
    }

    #[test]
    fn unknown_host_has_no_cooldown() {
        let c = HostCooldowns::new();
        assert_eq!(c.eligible_at("example.com"), None);
    }

    #[test]
    fn failure_sets_a_cooldown_that_grows() {
        let now = t0();
        let mut c = HostCooldowns::new();
        c.record_failure("example.com", None, now);
        assert_eq!(c.eligible_at("example.com"), Some(now + backoff_delay(1)));
        c.record_failure("example.com", None, now);
        assert_eq!(c.eligible_at("example.com"), Some(now + backoff_delay(2)));
    }

    #[test]
    fn explicit_retry_after_wins_over_backoff() {
        // RateLimit-Reset is better information than a guess.
        let now = t0();
        let mut c = HostCooldowns::new();
        c.record_failure("example.com", Some(Duration::from_secs(45)), now);
        assert_eq!(
            c.eligible_at("example.com"),
            Some(now + Duration::from_secs(45))
        );
    }

    #[test]
    fn success_clears_the_cooldown_and_the_failure_count() {
        let now = t0();
        let mut c = HostCooldowns::new();
        c.record_failure("example.com", None, now);
        c.record_success("example.com");
        assert_eq!(c.eligible_at("example.com"), None);
        c.record_failure("example.com", None, now);
        assert_eq!(c.eligible_at("example.com"), Some(now + backoff_delay(1)));
    }

    #[test]
    fn cooldown_is_shared_by_every_did_on_the_host() {
        // The whole point: one 429 from plc.directory must not be re-learned
        // ninety-nine more times.
        let now = t0();
        let mut cooldowns = HostCooldowns::new();
        cooldowns.record_failure("plc.directory", Some(Duration::from_secs(60)), now);

        let mut q = DeferredQueue::new();
        for did in ["did:plc:a", "did:plc:b", "did:plc:c"] {
            q.push(DeferredItem {
                payload: did.to_string(),
                host: "plc.directory".to_string(),
                attempts: 1,
                eligible_at: now,
            });
        }

        match q.pop_eligible(&cooldowns, now) {
            PopResult::WaitUntil(at) => assert_eq!(at, now + Duration::from_secs(60)),
            other => panic!("expected WaitUntil, got {other:?}"),
        }
        assert_eq!(q.len(), 3, "nothing should have been popped");
    }

    #[test]
    fn item_becomes_ready_once_both_clocks_pass() {
        let now = t0();
        let mut cooldowns = HostCooldowns::new();
        cooldowns.record_failure("pds.example.com", Some(Duration::from_secs(10)), now);

        let mut q = DeferredQueue::new();
        q.push(DeferredItem {
            payload: "did:plc:a".to_string(),
            host: "pds.example.com".to_string(),
            attempts: 1,
            eligible_at: now + Duration::from_secs(5),
        });

        // Item clock passed, host clock has not.
        match q.pop_eligible(&cooldowns, now + Duration::from_secs(6)) {
            PopResult::WaitUntil(at) => assert_eq!(at, now + Duration::from_secs(10)),
            other => panic!("expected WaitUntil, got {other:?}"),
        }
        // Both passed.
        match q.pop_eligible(&cooldowns, now + Duration::from_secs(11)) {
            PopResult::Ready(item) => assert_eq!(item.payload, "did:plc:a"),
            other => panic!("expected Ready, got {other:?}"),
        }
        assert!(q.is_empty());
    }

    #[test]
    fn wait_until_reports_the_earliest_of_several_hosts() {
        let now = t0();
        let mut cooldowns = HostCooldowns::new();
        cooldowns.record_failure("slow.example.com", Some(Duration::from_secs(90)), now);
        cooldowns.record_failure("fast.example.com", Some(Duration::from_secs(5)), now);

        let mut q: DeferredQueue<String> = DeferredQueue::new();
        q.push(DeferredItem {
            payload: "a".into(),
            host: "slow.example.com".into(),
            attempts: 1,
            eligible_at: now,
        });
        q.push(DeferredItem {
            payload: "b".into(),
            host: "fast.example.com".into(),
            attempts: 1,
            eligible_at: now,
        });

        match q.pop_eligible(&cooldowns, now) {
            PopResult::WaitUntil(at) => assert_eq!(at, now + Duration::from_secs(5)),
            other => panic!("expected WaitUntil, got {other:?}"),
        }
    }

    #[test]
    fn ready_items_are_returned_in_fifo_order() {
        let now = t0();
        let cooldowns = HostCooldowns::new();
        let mut q: DeferredQueue<String> = DeferredQueue::new();
        q.push(DeferredItem {
            payload: "first".into(),
            host: "a.example.com".into(),
            attempts: 1,
            eligible_at: now,
        });
        q.push(DeferredItem {
            payload: "second".into(),
            host: "b.example.com".into(),
            attempts: 1,
            eligible_at: now,
        });

        let a = match q.pop_eligible(&cooldowns, now) {
            PopResult::Ready(i) => i.payload,
            other => panic!("expected Ready, got {other:?}"),
        };
        assert_eq!(a, "first");
    }

    #[test]
    fn empty_queue_reports_empty_not_wait() {
        let q_cool = HostCooldowns::new();
        let mut q: DeferredQueue<String> = DeferredQueue::new();
        assert!(matches!(q.pop_eligible(&q_cool, t0()), PopResult::Empty));
    }

    #[test]
    fn a_host_saturates_only_after_repeated_failures() {
        let now = t0();
        let mut c = HostCooldowns::new();
        assert!(!c.is_saturated("dead.example.com"));
        for _ in 0..(SATURATED_CONSECUTIVE_FAILURES - 1) {
            c.record_failure("dead.example.com", None, now);
            assert!(!c.is_saturated("dead.example.com"));
        }
        c.record_failure("dead.example.com", None, now);
        assert!(c.is_saturated("dead.example.com"));
        assert_eq!(
            c.consecutive_failures("dead.example.com"),
            SATURATED_CONSECUTIVE_FAILURES
        );
    }

    #[test]
    fn one_success_unsaturates_a_host() {
        // The healthy path: a transient rate limit that resolves must not
        // leave the host anywhere near "declared down".
        let now = t0();
        let mut c = HostCooldowns::new();
        for _ in 0..10 {
            c.record_failure("busy.example.com", None, now);
        }
        assert!(c.is_saturated("busy.example.com"));
        c.record_success("busy.example.com");
        assert!(!c.is_saturated("busy.example.com"));
        assert_eq!(c.consecutive_failures("busy.example.com"), 0);
    }

    #[test]
    fn a_saturated_hosts_queue_is_drained_in_one_step() {
        // The bug this exists to prevent: 500 DIDs behind one dead PDS, each
        // gated behind that host's own fresh cooldown, is ~33 hours of drain.
        let now = t0();
        let mut cooldowns = HostCooldowns::new();
        let mut q: DeferredQueue<String> = DeferredQueue::new();
        for i in 0..500 {
            q.push(DeferredItem {
                payload: format!("did:plc:{i}"),
                host: "dead.example.com".into(),
                attempts: 1,
                eligible_at: now,
            });
        }
        q.push(DeferredItem {
            payload: "did:plc:other".into(),
            host: "live.example.com".into(),
            attempts: 1,
            eligible_at: now,
        });

        for _ in 0..SATURATED_CONSECUTIVE_FAILURES {
            cooldowns.record_failure("dead.example.com", None, now);
        }
        assert!(cooldowns.is_saturated("dead.example.com"));

        let abandoned = q.drain_host("dead.example.com");
        assert_eq!(abandoned.len(), 500, "every queued DID must come back once");
        assert!(
            abandoned.iter().all(|i| i.host == "dead.example.com"),
            "drain_host must not take another host's work"
        );
        assert_eq!(q.len(), 1, "the live host's item stays queued");

        // ...and it is still eligible, not collaterally gated.
        match q.pop_eligible(&cooldowns, now) {
            PopResult::Ready(item) => assert_eq!(item.payload, "did:plc:other"),
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn a_host_that_succeeds_on_retry_keeps_its_remaining_items() {
        // The healthy path end to end: one 429, one successful retry, and the
        // rest of that host's queue drains at full speed.
        let now = t0();
        let mut cooldowns = HostCooldowns::new();
        let mut q: DeferredQueue<String> = DeferredQueue::new();
        for i in 0..3 {
            q.push(DeferredItem {
                payload: format!("did:plc:{i}"),
                host: "busy.example.com".into(),
                attempts: 1,
                eligible_at: now,
            });
        }
        cooldowns.record_failure("busy.example.com", Some(Duration::from_secs(30)), now);
        assert!(!cooldowns.is_saturated("busy.example.com"));

        // First item pops once the reset passes, and succeeds.
        let at = now + Duration::from_secs(31);
        match q.pop_eligible(&cooldowns, at) {
            PopResult::Ready(item) => assert_eq!(item.payload, "did:plc:0"),
            other => panic!("expected Ready, got {other:?}"),
        }
        cooldowns.record_success("busy.example.com");

        // The remaining two are immediately eligible — no fresh cooldown.
        assert_eq!(q.len(), 2);
        for expected in ["did:plc:1", "did:plc:2"] {
            match q.pop_eligible(&cooldowns, at) {
                PopResult::Ready(item) => assert_eq!(item.payload, expected),
                other => panic!("expected Ready, got {other:?}"),
            }
        }
        assert!(q.is_empty());
    }

    #[test]
    fn draining_an_unknown_host_is_a_no_op() {
        let now = t0();
        let mut q: DeferredQueue<String> = DeferredQueue::new();
        q.push(DeferredItem {
            payload: "a".into(),
            host: "a.example.com".into(),
            attempts: 1,
            eligible_at: now,
        });
        assert!(q.drain_host("b.example.com").is_empty());
        assert_eq!(q.len(), 1);
    }

    #[tokio::test]
    async fn drain_step_returns_done_on_empty_queue() {
        let cooldowns = HostCooldowns::new();
        let mut q: DeferredQueue<String> = DeferredQueue::new();
        let step = next_drain_step(&mut q, &cooldowns, Duration::from_millis(10)).await;
        assert!(matches!(step, DrainStep::Done));
    }

    #[tokio::test]
    async fn drain_step_sleeps_in_bounded_slices() {
        // A 120s cooldown must not become a 120s unresponsive stretch: the
        // caller has to get control back often enough to notice a cancel.
        let now = Instant::now();
        let mut cooldowns = HostCooldowns::new();
        cooldowns.record_failure("slow.example.com", Some(Duration::from_secs(120)), now);

        let mut q = DeferredQueue::new();
        q.push(DeferredItem {
            payload: "did:plc:a".to_string(),
            host: "slow.example.com".to_string(),
            attempts: 1,
            eligible_at: now,
        });

        let started = Instant::now();
        let step = next_drain_step(&mut q, &cooldowns, Duration::from_millis(50)).await;
        assert!(matches!(step, DrainStep::Slept));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "slept past the slice cap: {:?}",
            started.elapsed()
        );
        assert_eq!(q.len(), 1, "the item must stay queued while it waits");
    }

    #[tokio::test]
    async fn drain_step_returns_the_item_once_eligible() {
        let cooldowns = HostCooldowns::new();
        let mut q = DeferredQueue::new();
        q.push(DeferredItem {
            payload: "did:plc:a".to_string(),
            host: "a.example.com".to_string(),
            attempts: 1,
            eligible_at: Instant::now(),
        });
        match next_drain_step(&mut q, &cooldowns, Duration::from_millis(50)).await {
            DrainStep::Retry(item) => assert_eq!(item.payload, "did:plc:a"),
            other => panic!("expected Retry, got {other:?}"),
        }
    }
}

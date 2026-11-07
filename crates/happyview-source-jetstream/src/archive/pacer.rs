//! Pacing to the archive's byte quota. Bluesky's archive meters each key
//! with a token bucket of bytes, and says so on every response:
//!
//! ```text
//! headwind-quota-burst-bytes: 1000000000
//! headwind-quota-refill-bytes: 2000000
//! headwind-quota-refill-period-seconds: 1
//! ```
//!
//! Once it is empty, `getBlock` answers 429. `BytePacer` keeps a copy of
//! that bucket so requests wait for bytes instead of spending their retries
//! on 429s. It is shared by every client of one `ArchiveConfig`, so every
//! job on one key draws from one bucket. Until it has seen the headers it
//! never waits: a self-hosted Jetstream has no quota.

use std::sync::Mutex;
use std::time::Duration;

use reqwest::header::HeaderMap;
use tokio::time::Instant;

const BURST_HEADER: &str = "headwind-quota-burst-bytes";
const REFILL_HEADER: &str = "headwind-quota-refill-bytes";
const PERIOD_HEADER: &str = "headwind-quota-refill-period-seconds";

/// The quota an archive advertised.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quota {
    pub burst_bytes: f64,
    pub refill_per_sec: f64,
}

#[derive(Debug)]
struct State {
    quota: Option<Quota>,
    /// Bytes left; negative when a body overspent the bucket.
    tokens: f64,
    /// When `tokens` was last refilled.
    at: Instant,
}

impl State {
    fn refill(&mut self, now: Instant) {
        if let Some(quota) = self.quota {
            let earned =
                now.saturating_duration_since(self.at).as_secs_f64() * quota.refill_per_sec;
            self.tokens = (self.tokens + earned).min(quota.burst_bytes);
        }
        self.at = now;
    }
}

#[derive(Debug)]
pub struct BytePacer {
    state: Mutex<State>,
}

impl Default for BytePacer {
    fn default() -> Self {
        Self::new()
    }
}

impl BytePacer {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                quota: None,
                tokens: 0.0,
                at: Instant::now(),
            }),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The quota last advertised, if any.
    pub fn quota(&self) -> Option<Quota> {
        self.state().quota
    }

    /// Take the quota from an archive response's headers. A response without
    /// all three, or with unusable ones, changes nothing. A first quota
    /// starts the bucket full; a changed one keeps what is left, up to the
    /// new burst.
    pub fn learn(&self, headers: &HeaderMap) {
        let Some(quota) = parse_quota(headers) else {
            return;
        };
        let mut state = self.state();
        if state.quota == Some(quota) {
            return;
        }
        let now = Instant::now();
        state.refill(now);
        state.tokens = match state.quota {
            None => quota.burst_bytes,
            Some(_) => state.tokens.min(quota.burst_bytes),
        };
        state.quota = Some(quota);
    }

    /// Spend `bytes` of body received. The bucket may go into debt: a body's
    /// size isn't known until it has arrived.
    pub fn charge(&self, bytes: u64) {
        let mut state = self.state();
        if state.quota.is_some() {
            state.refill(Instant::now());
            state.tokens -= bytes as f64;
        }
    }

    /// The archive answered 429: empty the bucket so that every request
    /// waits `retry_after` before the next one goes. `false` when there is no
    /// quota to empty, and the caller must wait itself.
    pub fn throttled(&self, retry_after: Duration) -> bool {
        let mut state = self.state();
        let Some(quota) = state.quota else {
            return false;
        };
        state.refill(Instant::now());
        let debt = -(quota.refill_per_sec * retry_after.as_secs_f64());
        state.tokens = state.tokens.min(debt);
        true
    }

    /// Wait until the bucket is out of debt.
    pub async fn acquire(&self) {
        loop {
            let wait = {
                let mut state = self.state();
                let Some(quota) = state.quota else {
                    return;
                };
                state.refill(Instant::now());
                if state.tokens >= 0.0 {
                    return;
                }
                Duration::from_secs_f64(-state.tokens / quota.refill_per_sec)
            };
            tokio::time::sleep(wait).await;
        }
    }
}

fn parse_quota(headers: &HeaderMap) -> Option<Quota> {
    let number = |name: &str| {
        headers
            .get(name)?
            .to_str()
            .ok()?
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite() && *n > 0.0)
    };
    let (burst, refill, period) = (
        number(BURST_HEADER)?,
        number(REFILL_HEADER)?,
        number(PERIOD_HEADER)?,
    );
    Some(Quota {
        burst_bytes: burst,
        refill_per_sec: refill / period,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    fn headers(burst: &str, refill: &str, period: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(BURST_HEADER, HeaderValue::from_str(burst).unwrap());
        headers.insert(REFILL_HEADER, HeaderValue::from_str(refill).unwrap());
        headers.insert(PERIOD_HEADER, HeaderValue::from_str(period).unwrap());
        headers
    }

    fn paced(burst: &str, refill: &str) -> BytePacer {
        let pacer = BytePacer::new();
        pacer.learn(&headers(burst, refill, "1"));
        pacer
    }

    #[test]
    fn the_headwind_headers_are_the_quota() {
        let pacer = BytePacer::new();
        assert_eq!(pacer.quota(), None);
        pacer.learn(&headers("1000000000", "2000000", "1"));
        assert_eq!(
            pacer.quota(),
            Some(Quota {
                burst_bytes: 1e9,
                refill_per_sec: 2e6,
            })
        );
        pacer.learn(&headers("1000", "500", "5"));
        assert_eq!(
            pacer.quota(),
            Some(Quota {
                burst_bytes: 1000.0,
                refill_per_sec: 100.0,
            }),
            "a changed quota replaces the old one"
        );
    }

    #[test]
    fn missing_or_unusable_headers_teach_nothing() {
        let pacer = BytePacer::new();
        pacer.learn(&HeaderMap::new());
        pacer.learn(&headers("lots", "2000000", "1"));
        pacer.learn(&headers("1000", "0", "1"));
        pacer.learn(&headers("1000", "100", "0"));
        assert_eq!(pacer.quota(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_quota_nothing_waits() {
        let pacer = BytePacer::new();
        pacer.charge(10_000_000_000);
        let start = Instant::now();
        pacer.acquire().await;
        assert_eq!(start.elapsed(), Duration::ZERO);
        assert!(!pacer.throttled(Duration::from_secs(1)));
        pacer.acquire().await;
        assert_eq!(start.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_bucket_starts_full_and_debt_waits_to_refill() {
        let pacer = paced("1000", "100");
        let start = Instant::now();
        pacer.charge(1000);
        pacer.acquire().await;
        assert_eq!(start.elapsed(), Duration::ZERO, "the burst is free");
        pacer.charge(500);
        pacer.acquire().await;
        let waited = start.elapsed();
        assert!(
            waited >= Duration::from_secs(5) && waited < Duration::from_millis(5100),
            "{waited:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn overspending_a_full_bucket_waits_for_the_debt() {
        let pacer = paced("1000", "100");
        let start = Instant::now();
        pacer.charge(1500);
        pacer.acquire().await;
        let waited = start.elapsed();
        assert!(
            waited >= Duration::from_secs(5) && waited < Duration::from_millis(5100),
            "{waited:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_429_makes_every_waiter_wait_out_retry_after() {
        let pacer = std::sync::Arc::new(paced("1000", "100"));
        let start = Instant::now();
        assert!(pacer.throttled(Duration::from_secs(3)));
        let waiters: Vec<_> = (0..4)
            .map(|_| {
                let pacer = pacer.clone();
                tokio::spawn(async move {
                    pacer.acquire().await;
                    start.elapsed()
                })
            })
            .collect();
        for waiter in waiters {
            let waited = waiter.await.unwrap();
            assert!(
                waited >= Duration::from_secs(3) && waited < Duration::from_millis(3100),
                "{waited:?}"
            );
        }
    }
}

//! Per-host pacing for a crawl's record fetches.
//!
//! A PDS limits how fast one client IP may call it. The atproto reference
//! PDS allows 3,000 requests per five minutes per IP across its read
//! endpoints, and says so on every response: `RateLimit-Limit`,
//! `RateLimit-Remaining`, `RateLimit-Reset` (a Unix time in seconds) and,
//! on a 429, `Retry-After` (seconds). A crawl that ignores these spends the
//! budget in the first seconds of each window, and then every request
//! until the reset is a 429.
//!
//! [`HostPacer`] is shared by every fetch a PDS worker runs, so it knows
//! the host's budget. Each request waits at its gate first:
//!
//! - it spaces requests so the remaining budget lasts until the reset;
//! - once the budget is spent, or after a 429, it holds every request
//!   until the host's reset, `Retry-After`, or an exponential backoff with
//!   jitter when the host names neither;
//! - after a transport failure or a 5xx it backs off the same way;
//! - it adapts how many units the worker runs at once: halved on a 429 or
//!   a failure, raised by `1/limit` on each success (AIMD).
//!
//! Waiting here holds no job-wide request permit, so other hosts keep their
//! share of the job's concurrency while this one cools.
//!
//! A host is treated as down after [`DOWN_AFTER_FAILURES`] failures in a
//! row, or after nothing but 429s for [`RATE_LIMIT_GIVE_UP`]. Requests then
//! stop waiting and give up with the host's last failure, except one probe
//! per cooldown, so a host that comes back is used again.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use tokio::time::Instant;

use super::errors::{BackfillErrorKind, BackfillFailure};
use super::retry::{SATURATED_CONSECUTIVE_FAILURES, backoff_delay};

/// The longest a host's own rate-limit headers may hold its requests. The
/// reference PDS's window is five minutes; this bounds a hostile header.
pub const MAX_RATE_LIMIT_WAIT: Duration = Duration::from_secs(600);

/// The widest gap pacing puts between two requests to one host.
pub const MAX_REQUEST_SPACING: Duration = Duration::from_secs(10);

/// How long a host may answer nothing but 429s before it is treated as down.
pub const RATE_LIMIT_GIVE_UP: Duration = Duration::from_secs(60 * 60);

/// Failures in a row (transport errors and 5xx, not 429s) after which a host
/// is treated as down.
pub const DOWN_AFTER_FAILURES: u32 = SATURATED_CONSECUTIVE_FAILURES;

/// Added to a `RateLimit-Reset`, which is rounded down to the second.
const RESET_MARGIN: Duration = Duration::from_secs(1);

/// What a response's rate-limit headers say.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RateLimitHeaders {
    pub limit: Option<u64>,
    pub remaining: Option<u64>,
    /// Time until the window resets.
    pub reset_in: Option<Duration>,
    /// `Retry-After`, in seconds.
    pub retry_after: Option<Duration>,
}

fn header_u64(headers: &HeaderMap, name: &str) -> Option<u64> {
    let raw = headers.get(name)?.to_str().ok()?;
    // `RateLimit-Limit` may carry a policy after the number ("3000, 3000;w=300").
    let digits: String = raw
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// Read the rate-limit headers of a response received at `now_unix`.
///
/// `RateLimit-Reset` is a Unix time on atproto servers and a number of
/// seconds in the IETF draft. A value past 10^9 can only be the former.
pub fn parse_rate_limit_headers(headers: &HeaderMap, now_unix: u64) -> RateLimitHeaders {
    let reset_in = header_u64(headers, "ratelimit-reset").map(|reset| {
        if reset > 1_000_000_000 {
            Duration::from_secs(reset.saturating_sub(now_unix))
        } else {
            Duration::from_secs(reset)
        }
    });
    RateLimitHeaders {
        limit: header_u64(headers, "ratelimit-limit"),
        remaining: header_u64(headers, "ratelimit-remaining"),
        reset_in,
        retry_after: header_u64(headers, "retry-after").map(Duration::from_secs),
    }
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// [`backoff_delay`] with equal jitter: half the delay, plus up to half
/// again at random, so hosts that failed together do not retry together.
pub fn jittered_backoff(consecutive_failures: u32, unit_random: f64) -> Duration {
    let full = backoff_delay(consecutive_failures);
    let half = full / 2;
    half + half.mul_f64(unit_random.clamp(0.0, 1.0))
}

/// What a request gets at a host's gate.
#[derive(Debug, Clone)]
pub enum Gate {
    /// Send it. `probe` is a request let through to a host that is down.
    Go { probe: bool },
    /// The host is down: give up with this failure, without a request.
    Down(BackfillFailure),
}

impl PartialEq for Gate {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Go { probe: a }, Self::Go { probe: b }) => a == b,
            (Self::Down(a), Self::Down(b)) => a.kind == b.kind && a.message == b.message,
            _ => false,
        }
    }
}

/// A host's activity since the last [`HostPacer::take_summary`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PacerSummary {
    pub requests: u64,
    pub rate_limited: u64,
    pub failures: u64,
    /// How long the host's requests are held from now, if they are.
    pub cooling_for: Option<Duration>,
    /// Units the worker may run at once.
    pub concurrency: usize,
    pub down: bool,
}

#[derive(Debug)]
struct PacerState {
    cooling_until: Option<Instant>,
    next_slot: Instant,
    spacing: Duration,
    consecutive_failures: u32,
    consecutive_rate_limits: u32,
    rate_limited_since: Option<Instant>,
    last_failure: Option<BackfillFailure>,
    probe_in_flight: bool,
    limit: f64,
    max_limit: usize,
    requests: u64,
    rate_limited: u64,
    failures: u64,
}

impl PacerState {
    fn down_failure(&self, now: Instant) -> Option<BackfillFailure> {
        let failing = self.consecutive_failures >= DOWN_AFTER_FAILURES;
        let throttled = self
            .rate_limited_since
            .is_some_and(|since| now.saturating_duration_since(since) >= RATE_LIMIT_GIVE_UP);
        if failing || throttled {
            self.last_failure.clone()
        } else {
            None
        }
    }

    fn cool_until(&mut self, until: Instant) {
        self.cooling_until = Some(self.cooling_until.map_or(until, |c| c.max(until)));
    }

    fn back_off_concurrency(&mut self) {
        self.limit = (self.limit / 2.0).max(1.0);
    }

    /// Pace from what the host said its budget is.
    fn pace(&mut self, headers: &RateLimitHeaders, now: Instant) {
        match (headers.remaining, headers.reset_in) {
            (Some(0), Some(reset_in)) => {
                self.cool_until(now + (reset_in + RESET_MARGIN).min(MAX_RATE_LIMIT_WAIT));
            }
            (Some(remaining), Some(reset_in)) => {
                self.spacing = (reset_in / u32::try_from(remaining).unwrap_or(u32::MAX))
                    .min(MAX_REQUEST_SPACING);
            }
            _ => {}
        }
    }
}

/// One host's gate. See the module documentation.
#[derive(Debug)]
pub struct HostPacer {
    state: Mutex<PacerState>,
}

impl HostPacer {
    /// A pacer for a worker that may run up to `max_concurrency` units.
    pub fn new(max_concurrency: usize) -> Self {
        let max_limit = max_concurrency.max(1);
        Self {
            state: Mutex::new(PacerState {
                cooling_until: None,
                next_slot: Instant::now(),
                spacing: Duration::ZERO,
                consecutive_failures: 0,
                consecutive_rate_limits: 0,
                rate_limited_since: None,
                last_failure: None,
                probe_in_flight: false,
                limit: max_limit as f64,
                max_limit,
                requests: 0,
                rate_limited: 0,
                failures: 0,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PacerState> {
        // The state is plain counters, so one a panicking holder left behind
        // is still usable.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Units the worker may run at once now.
    pub fn concurrency(&self) -> usize {
        let st = self.lock();
        (st.limit.floor() as usize).clamp(1, st.max_limit)
    }

    /// Whether the host is down at `now`.
    pub fn is_down(&self, now: Instant) -> bool {
        self.lock().down_failure(now).is_some()
    }

    /// When the host's cooldown ends, if it is cooling.
    pub fn open_at(&self) -> Option<Instant> {
        self.lock().cooling_until
    }

    /// Pass the gate at `now`, or learn when to try again.
    pub fn try_pass(&self, now: Instant) -> Result<Gate, Instant> {
        let mut st = self.lock();
        let open_at = st
            .cooling_until
            .map_or(st.next_slot, |c| c.max(st.next_slot));
        if let Some(failure) = st.down_failure(now) {
            if open_at <= now && !st.probe_in_flight {
                st.probe_in_flight = true;
                st.next_slot = now + st.spacing;
                st.requests += 1;
                return Ok(Gate::Go { probe: true });
            }
            return Ok(Gate::Down(failure));
        }
        if open_at <= now {
            st.next_slot = now + st.spacing;
            st.requests += 1;
            return Ok(Gate::Go { probe: false });
        }
        Err(open_at)
    }

    /// Wait at the gate until a request may go, in slices of at most a
    /// second so a stop is noticed. `None` once `cancelled` is set.
    pub async fn wait_turn(&self, cancelled: &AtomicBool) -> Option<Gate> {
        loop {
            if cancelled.load(Ordering::Relaxed) {
                return None;
            }
            let now = Instant::now();
            match self.try_pass(now) {
                Ok(gate) => return Some(gate),
                Err(at) => {
                    let wait = at
                        .saturating_duration_since(now)
                        .min(Duration::from_secs(1));
                    tokio::time::sleep(wait).await;
                }
            }
        }
    }

    /// The host answered with `status`: pace from its headers. A success
    /// clears the host's failure streaks and raises its concurrency.
    pub fn on_response(&self, status: StatusCode, headers: &HeaderMap, now: Instant) {
        let parsed = parse_rate_limit_headers(headers, unix_now());
        let mut st = self.lock();
        st.probe_in_flight = false;
        // Any answer says the host is up.
        st.consecutive_failures = 0;
        if status.is_success() {
            st.consecutive_rate_limits = 0;
            st.rate_limited_since = None;
            st.limit = (st.limit + 1.0 / st.limit).min(st.max_limit as f64);
        }
        st.pace(&parsed, now);
    }

    /// The host answered 429. Hold its requests until `Retry-After`, else
    /// its reset, else an exponential backoff with jitter.
    pub fn on_rate_limited(&self, failure: &BackfillFailure, headers: &HeaderMap, now: Instant) {
        self.rate_limited_at(failure, &parse_rate_limit_headers(headers, unix_now()), now);
    }

    pub fn rate_limited_at(
        &self,
        failure: &BackfillFailure,
        headers: &RateLimitHeaders,
        now: Instant,
    ) {
        let mut st = self.lock();
        st.probe_in_flight = false;
        st.rate_limited += 1;
        st.consecutive_rate_limits = st.consecutive_rate_limits.saturating_add(1);
        st.rate_limited_since.get_or_insert(now);
        st.last_failure = Some(failure.clone());
        st.back_off_concurrency();
        let wait = headers
            .retry_after
            .or(headers.reset_in.map(|r| r + RESET_MARGIN))
            .unwrap_or_else(|| jittered_backoff(st.consecutive_rate_limits, rand::random()))
            .min(MAX_RATE_LIMIT_WAIT);
        st.cool_until(now + wait);
        // The window's remaining budget is spent; spacing resumes from the
        // next answer.
    }

    /// A request to the host failed retryably without a 429: a transport
    /// error or a 5xx.
    pub fn on_failure(&self, failure: &BackfillFailure, now: Instant) {
        let mut st = self.lock();
        st.probe_in_flight = false;
        st.failures += 1;
        st.consecutive_failures = st.consecutive_failures.saturating_add(1);
        st.last_failure = Some(failure.clone());
        st.back_off_concurrency();
        let wait = failure
            .retry_after
            .unwrap_or_else(|| jittered_backoff(st.consecutive_failures, rand::random()))
            .min(MAX_RATE_LIMIT_WAIT);
        st.cool_until(now + wait);
    }

    /// The host's activity since the last summary, and its state now.
    pub fn take_summary(&self, now: Instant) -> PacerSummary {
        let mut st = self.lock();
        let summary = PacerSummary {
            requests: st.requests,
            rate_limited: st.rate_limited,
            failures: st.failures,
            cooling_for: st
                .cooling_until
                .filter(|until| *until > now)
                .map(|until| until - now),
            concurrency: (st.limit.floor() as usize).clamp(1, st.max_limit),
            down: st.down_failure(now).is_some(),
        };
        st.requests = 0;
        st.rate_limited = 0;
        st.failures = 0;
        summary
    }
}

/// A 429 failure, for tests and for a host that gives no message.
pub fn rate_limited_failure(message: &str) -> BackfillFailure {
    BackfillFailure {
        kind: BackfillErrorKind::RateLimited,
        message: message.to_string(),
        retry_after: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    fn server_error() -> BackfillFailure {
        BackfillFailure {
            kind: BackfillErrorKind::PdsServerError,
            message: "PDS returned 503".to_string(),
            retry_after: None,
        }
    }

    #[test]
    fn atproto_headers_parse_with_an_epoch_reset() {
        let h = headers(&[
            ("ratelimit-limit", "3000"),
            ("ratelimit-remaining", "12"),
            ("ratelimit-reset", "1700000300"),
            ("retry-after", "42"),
        ]);
        assert_eq!(
            parse_rate_limit_headers(&h, 1_700_000_000),
            RateLimitHeaders {
                limit: Some(3000),
                remaining: Some(12),
                reset_in: Some(Duration::from_secs(300)),
                retry_after: Some(Duration::from_secs(42)),
            }
        );
    }

    #[test]
    fn a_delta_reset_and_a_policy_suffix_parse() {
        let h = headers(&[
            ("ratelimit-limit", "100, 100;w=60"),
            ("ratelimit-reset", "30"),
        ]);
        let parsed = parse_rate_limit_headers(&h, 1_700_000_000);
        assert_eq!(parsed.limit, Some(100));
        assert_eq!(parsed.reset_in, Some(Duration::from_secs(30)));
    }

    #[test]
    fn a_reset_in_the_past_means_now() {
        let h = headers(&[("ratelimit-reset", "1699999990")]);
        assert_eq!(
            parse_rate_limit_headers(&h, 1_700_000_000).reset_in,
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn jitter_stays_between_half_and_the_full_backoff() {
        for n in 1..10 {
            let full = backoff_delay(n);
            assert_eq!(jittered_backoff(n, 0.0), full / 2);
            assert_eq!(jittered_backoff(n, 1.0), full / 2 + full / 2);
            let mid = jittered_backoff(n, 0.5);
            assert!(mid >= full / 2 && mid <= full);
        }
    }

    #[test]
    fn a_fresh_host_lets_requests_through() {
        let pacer = HostPacer::new(3);
        let now = Instant::now();
        assert_eq!(pacer.try_pass(now), Ok(Gate::Go { probe: false }));
        assert_eq!(pacer.try_pass(now), Ok(Gate::Go { probe: false }));
        assert_eq!(pacer.concurrency(), 3);
    }

    #[test]
    fn a_429_holds_the_host_until_retry_after() {
        let pacer = HostPacer::new(3);
        let now = Instant::now();
        pacer.rate_limited_at(
            &rate_limited_failure("429"),
            &RateLimitHeaders {
                retry_after: Some(Duration::from_secs(30)),
                reset_in: Some(Duration::from_secs(200)),
                ..Default::default()
            },
            now,
        );
        assert_eq!(pacer.try_pass(now), Err(now + Duration::from_secs(30)));
        assert_eq!(
            pacer.try_pass(now + Duration::from_secs(29)),
            Err(now + Duration::from_secs(30))
        );
        assert_eq!(
            pacer.try_pass(now + Duration::from_secs(30)),
            Ok(Gate::Go { probe: false })
        );
        assert_eq!(pacer.concurrency(), 1, "3 halved, floored, at least 1");
    }

    #[test]
    fn a_429_without_retry_after_waits_for_the_reset() {
        let pacer = HostPacer::new(3);
        let now = Instant::now();
        pacer.rate_limited_at(
            &rate_limited_failure("429"),
            &RateLimitHeaders {
                reset_in: Some(Duration::from_secs(200)),
                ..Default::default()
            },
            now,
        );
        assert_eq!(pacer.try_pass(now), Err(now + Duration::from_secs(201)));
    }

    #[test]
    fn a_429_with_no_headers_backs_off_exponentially() {
        let pacer = HostPacer::new(3);
        let now = Instant::now();
        let mut previous = Duration::ZERO;
        for n in 1..=4 {
            pacer.rate_limited_at(
                &rate_limited_failure("429"),
                &RateLimitHeaders::default(),
                now,
            );
            let until = pacer.try_pass(now).expect_err("cooling");
            let wait = until - now;
            assert!(wait >= backoff_delay(n) / 2, "attempt {n}: {wait:?}");
            assert!(wait >= previous, "a cooldown never shortens");
            previous = wait;
        }
    }

    #[test]
    fn a_hostile_header_is_capped() {
        let pacer = HostPacer::new(3);
        let now = Instant::now();
        pacer.rate_limited_at(
            &rate_limited_failure("429"),
            &RateLimitHeaders {
                retry_after: Some(Duration::from_secs(1_000_000)),
                ..Default::default()
            },
            now,
        );
        assert_eq!(pacer.try_pass(now), Err(now + MAX_RATE_LIMIT_WAIT));
    }

    #[test]
    fn requests_are_spaced_so_the_budget_lasts_until_the_reset() {
        let pacer = HostPacer::new(3);
        let now = Instant::now();
        let mut st = pacer.lock();
        st.pace(
            &RateLimitHeaders {
                remaining: Some(100),
                reset_in: Some(Duration::from_secs(50)),
                ..Default::default()
            },
            now,
        );
        drop(st);
        assert_eq!(pacer.try_pass(now), Ok(Gate::Go { probe: false }));
        assert_eq!(pacer.try_pass(now), Err(now + Duration::from_millis(500)));
        assert_eq!(
            pacer.try_pass(now + Duration::from_millis(500)),
            Ok(Gate::Go { probe: false })
        );
    }

    #[test]
    fn a_spent_budget_holds_the_host_until_the_reset() {
        let pacer = HostPacer::new(3);
        let now = Instant::now();
        pacer.lock().pace(
            &RateLimitHeaders {
                remaining: Some(0),
                reset_in: Some(Duration::from_secs(20)),
                ..Default::default()
            },
            now,
        );
        assert_eq!(pacer.try_pass(now), Err(now + Duration::from_secs(21)));
    }

    #[test]
    fn concurrency_halves_on_a_429_and_climbs_back_on_success() {
        let pacer = HostPacer::new(4);
        let now = Instant::now();
        pacer.rate_limited_at(
            &rate_limited_failure("429"),
            &RateLimitHeaders::default(),
            now,
        );
        assert_eq!(pacer.concurrency(), 2);
        let ok = HeaderMap::new();
        let mut successes = 0;
        while pacer.concurrency() < 4 {
            pacer.on_response(StatusCode::OK, &ok, now);
            successes += 1;
            assert!(successes < 100, "concurrency never recovered");
        }
        assert!(successes > 2, "additive increase is gradual: {successes}");
        pacer.on_response(StatusCode::OK, &ok, now);
        assert_eq!(pacer.concurrency(), 4, "never past the worker's maximum");
    }

    #[test]
    fn a_host_failing_in_a_row_goes_down_and_is_probed() {
        let pacer = HostPacer::new(3);
        let mut now = Instant::now();
        for _ in 0..DOWN_AFTER_FAILURES {
            pacer.on_failure(&server_error(), now);
        }
        assert!(pacer.is_down(now));
        // While cooling, requests give up without waiting.
        assert_eq!(pacer.try_pass(now), Ok(Gate::Down(server_error())));
        // Once the cooldown passes, one probe goes through; the rest still
        // give up while it is out.
        now += MAX_RATE_LIMIT_WAIT;
        assert_eq!(pacer.try_pass(now), Ok(Gate::Go { probe: true }));
        assert_eq!(pacer.try_pass(now), Ok(Gate::Down(server_error())));
        // An answer brings the host back.
        pacer.on_response(StatusCode::OK, &HeaderMap::new(), now);
        assert!(!pacer.is_down(now));
        assert_eq!(pacer.try_pass(now), Ok(Gate::Go { probe: false }));
    }

    #[test]
    fn a_host_that_only_429s_for_an_hour_goes_down() {
        let pacer = HostPacer::new(3);
        let start = Instant::now();
        pacer.rate_limited_at(
            &rate_limited_failure("429"),
            &RateLimitHeaders::default(),
            start,
        );
        assert!(!pacer.is_down(start + Duration::from_secs(60)));
        pacer.rate_limited_at(
            &rate_limited_failure("429"),
            &RateLimitHeaders::default(),
            start + RATE_LIMIT_GIVE_UP,
        );
        assert!(pacer.is_down(start + RATE_LIMIT_GIVE_UP));
        pacer.on_response(
            StatusCode::OK,
            &HeaderMap::new(),
            start + RATE_LIMIT_GIVE_UP,
        );
        assert!(!pacer.is_down(start + RATE_LIMIT_GIVE_UP));
    }

    #[test]
    fn a_summary_reports_and_resets_the_period() {
        let pacer = HostPacer::new(3);
        let now = Instant::now();
        let _ = pacer.try_pass(now);
        pacer.rate_limited_at(
            &rate_limited_failure("429"),
            &RateLimitHeaders {
                retry_after: Some(Duration::from_secs(10)),
                ..Default::default()
            },
            now,
        );
        let summary = pacer.take_summary(now);
        assert_eq!(
            summary,
            PacerSummary {
                requests: 1,
                rate_limited: 1,
                failures: 0,
                cooling_for: Some(Duration::from_secs(10)),
                concurrency: 1,
                down: false,
            }
        );
        let next = pacer.take_summary(now + Duration::from_secs(11));
        assert_eq!(
            (next.requests, next.rate_limited, next.cooling_for),
            (0, 0, None)
        );
    }
}

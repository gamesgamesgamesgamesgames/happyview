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
//! Some operators rate-limit many hosts as one budget per client IP.
//! [`RateLimitDomains`] groups such hosts (Bluesky's `*.host.bsky.network`
//! shards by default) under one shared pacer that owns their spacing, 429
//! cooldowns and an AIMD cap on requests in flight; each host's own pacer
//! keeps its failure backoff.
//!
//! A host is treated as down after [`DOWN_AFTER_FAILURES`] transport
//! failures or 5xx in a row. Requests then stop waiting and give up with
//! the host's last failure, except one probe per cooldown, so a host that
//! comes back is used again. 429s never take a host down: a host that keeps
//! answering 429 is up and pacing us, so its requests keep waiting for its
//! reset however long that takes, and its units stay queued.

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

/// The shortest a 429 holds the gate, whatever `Retry-After` says: a
/// `Retry-After: 0` must not turn into a request loop.
pub const MIN_RATE_LIMIT_WAIT: Duration = Duration::from_secs(1);

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

/// A pacer's state at a moment, for a summary.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PacerSummary {
    /// How long the gate is held from now, if it is.
    pub cooling_for: Option<Duration>,
    /// Units a host's worker may run at once, or, for a domain, requests
    /// in flight across its hosts.
    pub concurrency: usize,
    /// Requests through a domain's gate that have not been answered yet.
    pub in_flight: usize,
    pub down: bool,
}

#[derive(Debug)]
struct PacerState {
    cooling_until: Option<Instant>,
    next_slot: Instant,
    spacing: Duration,
    consecutive_failures: u32,
    consecutive_rate_limits: u32,
    last_failure: Option<BackfillFailure>,
    probe_in_flight: bool,
    limit: f64,
    max_limit: usize,
    /// Set on a domain's pacer: `limit` caps requests in flight through the
    /// gate, rather than a worker's units.
    caps_requests: bool,
    in_flight: usize,
}

impl PacerState {
    fn down_failure(&self, _now: Instant) -> Option<BackfillFailure> {
        if self.consecutive_failures >= DOWN_AFTER_FAILURES {
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
        Self::build(max_concurrency, false)
    }

    /// A rate-limit domain's pacer, shared by the workers of every host in
    /// it: at most `max_in_flight` requests through its gate at once, cut
    /// by AIMD. See [`RateLimitDomains`].
    pub fn shared(max_in_flight: usize) -> Self {
        Self::build(max_in_flight, true)
    }

    fn build(max_concurrency: usize, caps_requests: bool) -> Self {
        let max_limit = max_concurrency.max(1);
        Self {
            state: Mutex::new(PacerState {
                cooling_until: None,
                next_slot: Instant::now(),
                spacing: Duration::ZERO,
                consecutive_failures: 0,
                consecutive_rate_limits: 0,
                last_failure: None,
                probe_in_flight: false,
                limit: max_limit as f64,
                max_limit,
                caps_requests,
                in_flight: 0,
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

    /// Pass the gate at `now`, or learn when to try again. On a domain's
    /// pacer a request that passes holds one of its in-flight slots until
    /// [`HostPacer::release`]; [`InFlight`] does that on drop.
    pub fn try_pass(&self, now: Instant) -> Result<Gate, Instant> {
        let mut st = self.lock();
        let open_at = st
            .cooling_until
            .map_or(st.next_slot, |c| c.max(st.next_slot));
        if let Some(failure) = st.down_failure(now) {
            if open_at <= now && !st.probe_in_flight {
                st.probe_in_flight = true;
                st.next_slot = now + st.spacing;
                if st.caps_requests {
                    st.in_flight += 1;
                }
                return Ok(Gate::Go { probe: true });
            }
            return Ok(Gate::Down(failure));
        }
        if open_at <= now {
            if st.caps_requests {
                let limit = (st.limit.floor() as usize).clamp(1, st.max_limit);
                if st.in_flight >= limit {
                    // Answers free slots; look again shortly.
                    return Err(now + IN_FLIGHT_POLL);
                }
                st.in_flight += 1;
            }
            st.next_slot = now + st.spacing;
            return Ok(Gate::Go { probe: false });
        }
        Err(open_at)
    }

    /// Give back a domain's in-flight slot.
    pub fn release(&self) {
        let mut st = self.lock();
        st.in_flight = st.in_flight.saturating_sub(1);
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
    pub fn on_response(&self, status: StatusCode, headers: &RateLimitHeaders, now: Instant) {
        let mut st = self.lock();
        st.probe_in_flight = false;
        // Any answer other than a 429 says the host is up and not pacing us.
        st.consecutive_failures = 0;
        st.consecutive_rate_limits = 0;
        if status.is_success() {
            st.limit = (st.limit + 1.0 / st.limit).min(st.max_limit as f64);
        }
        st.pace(headers, now);
    }

    /// The host answered 429. Hold its requests until `Retry-After`, else
    /// its reset, else an exponential backoff with jitter.
    pub fn on_rate_limited(
        &self,
        failure: &BackfillFailure,
        headers: &RateLimitHeaders,
        now: Instant,
    ) {
        let mut st = self.lock();
        st.probe_in_flight = false;
        st.consecutive_rate_limits = st.consecutive_rate_limits.saturating_add(1);
        // Not the host's `last_failure`: that is what a down host's units are
        // given up with, and a 429 never takes a host down.
        let _ = failure;
        st.back_off_concurrency();
        let wait = headers
            .retry_after
            .or(headers.reset_in.map(|r| r + RESET_MARGIN))
            .unwrap_or_else(|| jittered_backoff(st.consecutive_rate_limits, rand::random()))
            .clamp(MIN_RATE_LIMIT_WAIT, MAX_RATE_LIMIT_WAIT);
        st.cool_until(now + wait);
        // The window's remaining budget is spent; spacing resumes from the
        // next answer.
    }

    /// A request to the host failed retryably without a 429: a transport
    /// error or a 5xx.
    pub fn on_failure(&self, failure: &BackfillFailure, now: Instant) {
        let mut st = self.lock();
        st.probe_in_flight = false;
        st.consecutive_failures = st.consecutive_failures.saturating_add(1);
        st.last_failure = Some(failure.clone());
        st.back_off_concurrency();
        let wait = failure
            .retry_after
            .unwrap_or_else(|| jittered_backoff(st.consecutive_failures, rand::random()))
            .min(MAX_RATE_LIMIT_WAIT);
        st.cool_until(now + wait);
    }

    /// The pacer's state at `now`.
    pub fn summary(&self, now: Instant) -> PacerSummary {
        let st = self.lock();
        PacerSummary {
            cooling_for: st
                .cooling_until
                .filter(|until| *until > now)
                .map(|until| until - now),
            concurrency: (st.limit.floor() as usize).clamp(1, st.max_limit),
            in_flight: st.in_flight,
            down: st.down_failure(now).is_some(),
        }
    }
}

/// How often a request waiting only for a domain's in-flight slot looks
/// again.
const IN_FLIGHT_POLL: Duration = Duration::from_millis(25);

/// A domain's in-flight slot, given back on drop.
pub struct InFlight<'a>(pub &'a HostPacer);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// What one host answered since the last [`HostObserved::take`]: its own
/// counts, and the rate-limit headers it sent last. Kept per host even when
/// the host shares a domain's gate, so a summary shows whether hosts'
/// budgets fall together.
#[derive(Debug, Default)]
pub struct HostObserved {
    inner: Mutex<ObservedPeriod>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ObservedPeriod {
    pub requests: u64,
    pub rate_limited: u64,
    pub failures: u64,
    /// The last rate-limit headers the host sent, if it sent any.
    pub last: Option<RateLimitHeaders>,
}

impl HostObserved {
    fn lock(&self) -> std::sync::MutexGuard<'_, ObservedPeriod> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// An answer with these headers; `rate_limited` for a 429.
    pub fn answered(&self, headers: &RateLimitHeaders, rate_limited: bool) {
        let mut o = self.lock();
        o.requests += 1;
        if rate_limited {
            o.rate_limited += 1;
        }
        if headers.limit.is_some() || headers.remaining.is_some() || headers.reset_in.is_some() {
            o.last = Some(headers.clone());
        }
    }

    /// A request that got no answer, or a 5xx.
    pub fn failed(&self) {
        let mut o = self.lock();
        o.requests += 1;
        o.failures += 1;
    }

    /// The period's counts, starting the next. The last headers carry over.
    pub fn take(&self) -> ObservedPeriod {
        let mut o = self.lock();
        let period = o.clone();
        o.requests = 0;
        o.rate_limited = 0;
        o.failures = 0;
        period
    }
}

/// The rate-limit domain hosts are grouped into by default: Bluesky's PDS
/// shards, which appear to share one budget per client IP.
pub const DEFAULT_RATE_LIMIT_DOMAINS: &str = "host.bsky.network";

/// Hosts whose rate limits are one budget. A PDS operator can run many
/// hosts behind one rate-limit store, keyed by client IP, so pacing each
/// host to its own headers would send the sum of every host's share. A host
/// named `suffix` or `*.suffix` for a configured suffix shares one
/// [`HostPacer::shared`] gate (spacing, 429 cooldowns and in-flight AIMD)
/// with the domain's other hosts. Its own failure backoff stays per host.
#[derive(Debug)]
pub struct RateLimitDomains {
    suffixes: Vec<String>,
    max_in_flight: usize,
    pacers: Mutex<std::collections::HashMap<String, std::sync::Arc<HostPacer>>>,
}

impl RateLimitDomains {
    /// `raw` is the `backfill_rate_limit_domains` setting: comma-separated
    /// suffixes, [`DEFAULT_RATE_LIMIT_DOMAINS`] when unset, none when empty
    /// or `none`.
    pub fn new(raw: Option<&str>, max_in_flight: usize) -> Self {
        let raw = raw.unwrap_or(DEFAULT_RATE_LIMIT_DOMAINS).trim();
        let suffixes = if raw.eq_ignore_ascii_case("none") {
            Vec::new()
        } else {
            raw.split(',')
                .map(|s| {
                    s.trim()
                        .trim_start_matches("*.")
                        .trim_start_matches('.')
                        .to_ascii_lowercase()
                })
                .filter(|s| !s.is_empty())
                .collect()
        };
        Self {
            suffixes,
            max_in_flight: max_in_flight.max(1),
            pacers: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// The domain `host` (with or without a port) belongs to, if any.
    pub fn domain_of(&self, host: &str) -> Option<&str> {
        let name = host
            .rsplit_once(':')
            .filter(|(_, port)| port.chars().all(|c| c.is_ascii_digit()))
            .map_or(host, |(name, _)| name)
            .to_ascii_lowercase();
        self.suffixes
            .iter()
            .find(|suffix| name == **suffix || name.ends_with(&format!(".{suffix}")))
            .map(String::as_str)
    }

    /// The shared pacer of `host`'s domain, if it is in one.
    pub fn pacer_for(&self, host: &str) -> Option<std::sync::Arc<HostPacer>> {
        let domain = self.domain_of(host)?.to_string();
        let mut pacers = self.pacers.lock().unwrap_or_else(|e| e.into_inner());
        Some(std::sync::Arc::clone(pacers.entry(domain).or_insert_with(
            || std::sync::Arc::new(HostPacer::shared(self.max_in_flight)),
        )))
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
        pacer.on_rate_limited(
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
        pacer.on_rate_limited(
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
            pacer.on_rate_limited(
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
        pacer.on_rate_limited(
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
        pacer.on_rate_limited(
            &rate_limited_failure("429"),
            &RateLimitHeaders::default(),
            now,
        );
        assert_eq!(pacer.concurrency(), 2);
        let ok = RateLimitHeaders::default();
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
        pacer.on_response(StatusCode::OK, &RateLimitHeaders::default(), now);
        assert!(!pacer.is_down(now));
        assert_eq!(pacer.try_pass(now), Ok(Gate::Go { probe: false }));
    }

    #[test]
    fn a_host_that_only_429s_never_goes_down() {
        let pacer = HostPacer::new(3);
        let mut now = Instant::now();
        for _ in 0..100 {
            pacer.on_rate_limited(
                &rate_limited_failure("429"),
                &RateLimitHeaders::default(),
                now,
            );
            now += Duration::from_secs(3600);
        }
        assert!(!pacer.is_down(now));
        // It keeps waiting, and passes once its cooldown is over.
        assert_eq!(pacer.try_pass(now), Ok(Gate::Go { probe: false }));
    }

    #[test]
    fn a_429_between_failures_does_not_hide_the_host_failing() {
        let pacer = HostPacer::new(3);
        let now = Instant::now();
        for _ in 0..DOWN_AFTER_FAILURES - 1 {
            pacer.on_failure(&server_error(), now);
        }
        pacer.on_rate_limited(
            &rate_limited_failure("429"),
            &RateLimitHeaders::default(),
            now,
        );
        pacer.on_failure(&server_error(), now);
        assert!(pacer.is_down(now));
        assert_eq!(
            pacer.try_pass(now),
            Ok(Gate::Down(server_error())),
            "a down host gives up with its own failure, not a 429"
        );
    }

    #[test]
    fn a_definitive_answer_ends_a_429_streak() {
        let pacer = HostPacer::new(3);
        let now = Instant::now();
        for _ in 0..6 {
            pacer.on_rate_limited(
                &rate_limited_failure("429"),
                &RateLimitHeaders::default(),
                now,
            );
        }
        let later = now + MAX_RATE_LIMIT_WAIT;
        pacer.on_response(StatusCode::BAD_REQUEST, &RateLimitHeaders::default(), later);
        pacer.on_rate_limited(
            &rate_limited_failure("429"),
            &RateLimitHeaders::default(),
            later,
        );
        let wait = pacer.try_pass(later).expect_err("cooling") - later;
        assert!(
            wait <= backoff_delay(1).max(MIN_RATE_LIMIT_WAIT),
            "the backoff started over: {wait:?}"
        );
    }

    #[test]
    fn retry_after_zero_still_waits_a_second() {
        let pacer = HostPacer::new(3);
        let now = Instant::now();
        pacer.on_rate_limited(
            &rate_limited_failure("429"),
            &RateLimitHeaders {
                retry_after: Some(Duration::ZERO),
                ..Default::default()
            },
            now,
        );
        assert_eq!(pacer.try_pass(now), Err(now + MIN_RATE_LIMIT_WAIT));
    }

    #[test]
    fn a_summary_reports_the_cooldown_and_concurrency() {
        let pacer = HostPacer::new(3);
        let now = Instant::now();
        pacer.on_rate_limited(
            &rate_limited_failure("429"),
            &RateLimitHeaders {
                retry_after: Some(Duration::from_secs(10)),
                ..Default::default()
            },
            now,
        );
        assert_eq!(
            pacer.summary(now),
            PacerSummary {
                cooling_for: Some(Duration::from_secs(10)),
                concurrency: 1,
                in_flight: 0,
                down: false,
            }
        );
        assert_eq!(
            pacer.summary(now + Duration::from_secs(11)).cooling_for,
            None
        );
    }

    #[test]
    fn a_domain_gate_caps_requests_in_flight_and_halves_on_a_429() {
        let domain = HostPacer::shared(4);
        let now = Instant::now();
        for _ in 0..4 {
            assert_eq!(domain.try_pass(now), Ok(Gate::Go { probe: false }));
        }
        assert!(domain.try_pass(now).is_err(), "all four slots are out");
        domain.release();
        assert_eq!(domain.try_pass(now), Ok(Gate::Go { probe: false }));
        for _ in 0..4 {
            domain.release();
        }
        domain.on_rate_limited(
            &rate_limited_failure("429"),
            &RateLimitHeaders {
                retry_after: Some(Duration::from_secs(1)),
                ..Default::default()
            },
            now,
        );
        let later = now + Duration::from_secs(2);
        assert_eq!(domain.try_pass(later), Ok(Gate::Go { probe: false }));
        assert_eq!(domain.try_pass(later), Ok(Gate::Go { probe: false }));
        assert!(domain.try_pass(later).is_err(), "halved to two");
    }

    #[test]
    fn an_in_flight_slot_is_given_back_on_drop() {
        let domain = HostPacer::shared(1);
        let now = Instant::now();
        assert_eq!(domain.try_pass(now), Ok(Gate::Go { probe: false }));
        {
            let _slot = InFlight(&domain);
            assert!(domain.try_pass(now).is_err());
        }
        assert_eq!(domain.try_pass(now), Ok(Gate::Go { probe: false }));
    }

    #[test]
    fn bluesky_shards_share_a_domain_and_others_do_not() {
        let domains = RateLimitDomains::new(None, 8);
        assert_eq!(
            domains.domain_of("amanita.us-east.host.bsky.network"),
            Some("host.bsky.network")
        );
        assert_eq!(domains.domain_of("pds.example.com"), None);
        assert_eq!(domains.domain_of("evilhost.bsky.network"), None);
        let a = domains
            .pacer_for("amanita.us-east.host.bsky.network")
            .unwrap();
        let b = domains
            .pacer_for("shiitake.us-west.host.bsky.network:443")
            .unwrap();
        assert!(std::sync::Arc::ptr_eq(&a, &b));

        let custom = RateLimitDomains::new(Some(" *.pds.example , other.test"), 8);
        assert_eq!(custom.domain_of("a.pds.example"), Some("pds.example"));
        assert_eq!(custom.domain_of("other.test:8080"), Some("other.test"));
        assert_eq!(custom.domain_of("a.host.bsky.network"), None);
        assert_eq!(
            RateLimitDomains::new(Some("none"), 8).domain_of("a.host.bsky.network"),
            None
        );
    }

    #[test]
    fn a_host_reports_its_own_answers_and_last_headers() {
        let observed = HostObserved::default();
        let h = RateLimitHeaders {
            limit: Some(3000),
            remaining: Some(7),
            reset_in: Some(Duration::from_secs(12)),
            retry_after: None,
        };
        observed.answered(&h, false);
        observed.answered(&RateLimitHeaders::default(), true);
        observed.failed();
        assert_eq!(
            observed.take(),
            ObservedPeriod {
                requests: 3,
                rate_limited: 1,
                failures: 1,
                last: Some(h.clone()),
            }
        );
        let next = observed.take();
        assert_eq!((next.requests, next.last), (0, Some(h)));
    }
}

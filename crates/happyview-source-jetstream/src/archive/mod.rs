//! The Jetstream v2 archive: settings, the HTTP client, the segment reader
//! and the conversion of archive rows into the events live ingest applies.

use std::path::PathBuf;
use std::sync::Arc;

use pacer::BytePacer;

pub mod client;
pub mod pacer;
pub mod rows;
pub mod segment;

pub const DEFAULT_CONCURRENCY: usize = 8;
pub const MAX_CONCURRENCY: usize = 64;
/// The archive client's read timeout: as long as a whole `planSnapshot` or
/// `getBlock` may take, and as long as a segment body may go silent.
pub const ARCHIVE_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// How long one archive request may keep being answered 429 before it
/// fails. The archive's quota refills steadily, so a 429 is a reason to wait,
/// not to give up; only one that lasts this long is treated as an outage.
pub const MAX_THROTTLED_FOR: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// How long the live consumer waits for the archive to plan a gap replay
/// before resuming without it.
pub const GAP_PLAN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How far back from the archive's tip a network replay sweeps account and
/// sync markers: three of Jetstream's default 4-hour compaction intervals.
/// Below that, compaction has already removed what a deleted account or a
/// `#sync` supersedes.
pub const DEFAULT_MARKER_TAIL: std::time::Duration = std::time::Duration::from_secs(12 * 60 * 60);

/// Where a backfill job's records come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceChoice {
    /// Replay when the Jetstream has an archive, otherwise crawl.
    Auto,
    Replay,
    Crawl,
}

impl SourceChoice {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Replay => "replay",
            Self::Crawl => "crawl",
        }
    }
}

/// `auto`, `replay` or `crawl`, any case. `None` for anything else.
pub fn parse_source(raw: &str) -> Option<SourceChoice> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "auto" => Some(SourceChoice::Auto),
        "replay" => Some(SourceChoice::Replay),
        "crawl" => Some(SourceChoice::Crawl),
        _ => None,
    }
}

/// Whether a job's `source` is one of the source's own backfills (`auto`,
/// `replay` or `crawl`, as stored); anything else names an executor.
pub fn is_builtin_source(source: &str) -> bool {
    matches!(source, "auto" | "replay" | "crawl")
}

pub fn parse_concurrency(raw: Option<&str>) -> usize {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return DEFAULT_CONCURRENCY;
    };
    match raw.parse::<usize>() {
        Ok(n) if n >= 1 => n.min(MAX_CONCURRENCY),
        _ => {
            tracing::warn!(
                value = raw,
                default = DEFAULT_CONCURRENCY,
                "BACKFILL_REPLAY_CONCURRENCY must be a positive integer; using the default"
            );
            DEFAULT_CONCURRENCY
        }
    }
}

/// `BACKFILL_REPLAY_MARKER_TAIL_HOURS`: whole hours, `0` for the whole
/// range. Unset, blank or anything else (logged) means
/// `DEFAULT_MARKER_TAIL`.
pub fn parse_marker_tail(raw: Option<&str>) -> std::time::Duration {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return DEFAULT_MARKER_TAIL;
    };
    match raw.parse::<u64>() {
        Ok(hours) => std::time::Duration::from_secs(hours.saturating_mul(60 * 60)),
        Err(_) => {
            tracing::warn!(
                value = raw,
                default = DEFAULT_MARKER_TAIL.as_secs() / 3600,
                "BACKFILL_REPLAY_MARKER_TAIL_HOURS must be a whole number of hours; using the default"
            );
            DEFAULT_MARKER_TAIL
        }
    }
}

/// `BACKFILL_SOURCE`: unset or blank means `crawl`; anything unrecognised
/// logs a warning and means `crawl`. Replay is an opt-in: the archive holds
/// only what it holds, and some repos' history in it starts after their
/// first record.
pub fn parse_default_source(raw: Option<&str>) -> SourceChoice {
    match raw {
        None => SourceChoice::Crawl,
        Some(raw) if raw.trim().is_empty() => SourceChoice::Crawl,
        Some(raw) => parse_source(raw).unwrap_or_else(|| {
            tracing::warn!(
                value = raw,
                "BACKFILL_SOURCE must be auto, replay or crawl; using crawl"
            );
            SourceChoice::Crawl
        }),
    }
}

/// The key, trimmed; blank means none.
pub fn parse_api_key(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Replay settings, read once at startup. The archive's host is
/// `JETSTREAM_URL`'s, taken from `Config` when a job starts.
///
/// Both `Default` and `from_env` crawl unless told otherwise, so nothing
/// probes a Jetstream archive unless `BACKFILL_SOURCE` or a job asks for it.
#[derive(Clone, Debug)]
pub struct ArchiveConfig {
    /// The client archive requests go through, built once: see
    /// `archive_client`.
    pub http: reqwest::Client,
    /// The archive's byte quota, learned from its responses and shared by
    /// every client built from this config: one key, one bucket.
    pub pacer: Arc<BytePacer>,
    /// `JETSTREAM_API_KEY`, sent only to the archive endpoints.
    pub api_key: Option<String>,
    /// `BACKFILL_REPLAY_CONCURRENCY`: concurrent `getBlock` requests per job.
    pub concurrency: usize,
    /// `BACKFILL_REPLAY_DIR`: where segment downloads are spooled.
    pub download_dir: PathBuf,
    /// `BACKFILL_SOURCE`: the source of a job created without one.
    pub default_source: SourceChoice,
    /// The first backoff step after a 5xx or a transport error; doubles per
    /// attempt. One second in production; tests shorten it.
    pub retry_unit: std::time::Duration,
    /// How long one request may keep being throttled (429) before it fails.
    /// `MAX_THROTTLED_FOR` in production; tests shorten it.
    pub max_throttled_for: std::time::Duration,
    /// The longest a gap replay's `planSnapshot` may take, retries included,
    /// before the live consumer resumes without it. `GAP_PLAN_TIMEOUT` in
    /// production; tests shorten it.
    pub gap_plan_timeout: std::time::Duration,
    /// `BACKFILL_REPLAY_MARKER_TAIL_HOURS`: how far back from the archive's
    /// tip a network replay sweeps account and sync markers; zero sweeps the
    /// job's whole range. `DEFAULT_MARKER_TAIL` unless set.
    pub marker_tail: std::time::Duration,
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self {
            http: archive_client(DEFAULT_USER_AGENT),
            pacer: Arc::default(),
            api_key: None,
            concurrency: DEFAULT_CONCURRENCY,
            download_dir: std::env::temp_dir().join("happyview-replay"),
            default_source: SourceChoice::Crawl,
            retry_unit: std::time::Duration::from_secs(1),
            max_throttled_for: MAX_THROTTLED_FOR,
            gap_plan_timeout: GAP_PLAN_TIMEOUT,
            marker_tail: DEFAULT_MARKER_TAIL,
        }
    }
}

impl ArchiveConfig {
    pub fn from_env(user_agent: &str) -> Self {
        let var = |name: &str| std::env::var(name).ok();
        let default_source = parse_default_source(var("BACKFILL_SOURCE").as_deref());
        Self {
            http: archive_client(user_agent),
            pacer: Arc::default(),
            api_key: parse_api_key(var("JETSTREAM_API_KEY").as_deref()),
            concurrency: parse_concurrency(var("BACKFILL_REPLAY_CONCURRENCY").as_deref()),
            download_dir: var("BACKFILL_REPLAY_DIR")
                .map(|d| d.trim().to_string())
                .filter(|d| !d.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| Self::default().download_dir),
            default_source,
            retry_unit: std::time::Duration::from_secs(1),
            max_throttled_for: MAX_THROTTLED_FOR,
            gap_plan_timeout: GAP_PLAN_TIMEOUT,
            marker_tail: parse_marker_tail(var("BACKFILL_REPLAY_MARKER_TAIL_HOURS").as_deref()),
        }
    }
}

/// The `User-Agent` of a config built without one, as in tests. The host
/// passes its own to `from_env`.
const DEFAULT_USER_AGENT: &str = concat!("HappyView-jetstream/", env!("CARGO_PKG_VERSION"));

/// How long to wait for TCP + TLS before giving up on the archive host.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The client for the Jetstream archive: the shared client's connect timeout
/// and user agent, but a `read` timeout long enough for the archive. Its
/// network-wide plans can take 30 s before the first byte, past the shared
/// client's read timeout. Redirects follow reqwest's default policy, which
/// drops `Authorization` when a redirect leaves the host (a `getBlock` 307 to
/// a CDN), so the archive key never travels further.
fn archive_client(user_agent: &str) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(ARCHIVE_READ_TIMEOUT)
        .user_agent(user_agent)
        .build()
        // Only fails if the TLS backend can't initialise, in which case no
        // outbound request would work anyway.
        .expect("failed to build HTTP client")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_parse_in_any_case_and_refuse_the_rest() {
        assert_eq!(parse_source("auto"), Some(SourceChoice::Auto));
        assert_eq!(parse_source(" Replay "), Some(SourceChoice::Replay));
        assert_eq!(parse_source("CRAWL"), Some(SourceChoice::Crawl));
        assert_eq!(parse_source("relay"), None);
        assert_eq!(parse_source(""), None);
        for choice in [
            SourceChoice::Auto,
            SourceChoice::Replay,
            SourceChoice::Crawl,
        ] {
            assert_eq!(parse_source(choice.as_str()), Some(choice));
        }
    }

    #[test]
    fn backfill_source_defaults_to_crawl() {
        assert_eq!(parse_default_source(None), SourceChoice::Crawl);
        assert_eq!(parse_default_source(Some("  ")), SourceChoice::Crawl);
        assert_eq!(parse_default_source(Some("relay")), SourceChoice::Crawl);
        assert_eq!(parse_default_source(Some("Auto")), SourceChoice::Auto);
        assert_eq!(parse_default_source(Some("replay")), SourceChoice::Replay);
        assert_eq!(parse_default_source(Some("crawl")), SourceChoice::Crawl);
    }

    #[test]
    fn concurrency_defaults_and_clamps() {
        assert_eq!(parse_concurrency(None), 8);
        assert_eq!(parse_concurrency(Some("")), 8);
        assert_eq!(parse_concurrency(Some("0")), 8);
        assert_eq!(parse_concurrency(Some("-3")), 8);
        assert_eq!(parse_concurrency(Some("lots")), 8);
        assert_eq!(parse_concurrency(Some("3")), 3);
        assert_eq!(parse_concurrency(Some("1000")), 64);
    }

    #[test]
    fn the_marker_tail_defaults_to_twelve_hours() {
        let hours = |h: u64| std::time::Duration::from_secs(h * 60 * 60);
        assert_eq!(parse_marker_tail(None), hours(12));
        assert_eq!(parse_marker_tail(Some(" ")), hours(12));
        assert_eq!(parse_marker_tail(Some("-1")), hours(12));
        assert_eq!(parse_marker_tail(Some("1.5")), hours(12));
        assert_eq!(parse_marker_tail(Some("soon")), hours(12));
        assert_eq!(parse_marker_tail(Some("0")), std::time::Duration::ZERO);
        assert_eq!(parse_marker_tail(Some(" 24 ")), hours(24));
        assert_eq!(ArchiveConfig::default().marker_tail, hours(12));
    }

    #[test]
    fn the_default_config_crawls() {
        // `from_env` is not tested here: tests must not set process env. Its
        // parse is covered by `sources_parse_in_any_case_and_refuse_the_rest`.
        assert_eq!(ArchiveConfig::default().default_source, SourceChoice::Crawl);
    }

    #[test]
    fn a_blank_api_key_is_no_key() {
        assert_eq!(parse_api_key(None), None);
        assert_eq!(parse_api_key(Some("   ")), None);
        assert_eq!(parse_api_key(Some(" k3y \n")), Some("k3y".to_string()));
    }
}

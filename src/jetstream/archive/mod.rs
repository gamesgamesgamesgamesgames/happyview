//! The Jetstream v2 archive: settings, the HTTP client, the segment reader
//! and the conversion of archive rows into the events live ingest applies.

use std::path::PathBuf;

pub mod rows;
pub mod segment;

pub const DEFAULT_CONCURRENCY: usize = 8;
pub const MAX_CONCURRENCY: usize = 64;

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

/// The key, trimmed; blank means none.
pub fn parse_api_key(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Replay settings, read once at startup. The archive's host is
/// `JETSTREAM_URL`'s, taken from `Config` when a job starts.
///
/// `Default` is for tests and embedded states: it crawls, so nothing probes a
/// Jetstream archive unless a job asks for replay. `from_env` defaults to
/// `auto`.
#[derive(Clone, Debug)]
pub struct ArchiveConfig {
    /// `JETSTREAM_API_KEY`, sent only to the archive endpoints.
    pub api_key: Option<String>,
    /// `BACKFILL_REPLAY_CONCURRENCY`: concurrent `getBlock` requests per job.
    pub concurrency: usize,
    /// `BACKFILL_REPLAY_DIR`: where segment downloads are spooled.
    pub download_dir: PathBuf,
    /// `BACKFILL_SOURCE`: the source of a job created without one.
    pub default_source: SourceChoice,
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            concurrency: DEFAULT_CONCURRENCY,
            download_dir: std::env::temp_dir().join("happyview-replay"),
            default_source: SourceChoice::Crawl,
        }
    }
}

impl ArchiveConfig {
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok();
        let default_source = match var("BACKFILL_SOURCE") {
            None => SourceChoice::Auto,
            Some(raw) if raw.trim().is_empty() => SourceChoice::Auto,
            Some(raw) => parse_source(&raw).unwrap_or_else(|| {
                tracing::warn!(
                    value = raw,
                    "BACKFILL_SOURCE must be auto, replay or crawl; using auto"
                );
                SourceChoice::Auto
            }),
        };
        Self {
            api_key: parse_api_key(var("JETSTREAM_API_KEY").as_deref()),
            concurrency: parse_concurrency(var("BACKFILL_REPLAY_CONCURRENCY").as_deref()),
            download_dir: var("BACKFILL_REPLAY_DIR")
                .map(|d| d.trim().to_string())
                .filter(|d| !d.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| Self::default().download_dir),
            default_source,
        }
    }
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

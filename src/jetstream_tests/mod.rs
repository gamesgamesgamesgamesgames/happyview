//! The Jetstream source's database-backed tests. They need a host, which
//! `happyview_source_jetstream` cannot build, so they stay here and drive
//! it through `AppHost`.

use happyview_source_jetstream::archive::ArchiveConfig;
use happyview_source_jetstream::backfill::{JobState, RelayRetry};
use happyview_source_jetstream::consumer::config::parse_endpoint;

mod apply;
mod backfill;
mod cursor;
mod replay_fold;
mod replay_sweeps;

/// A job system over `state`, with default replay settings, against
/// `state.config.jetstream_url`.
///
/// A failed relay page is retried as in production, but a few milliseconds
/// apart rather than seconds.
pub(crate) fn jobs(state: &crate::AppState) -> JobState {
    let mut jobs = JobState::new(
        crate::host::AppHost::new(state.clone()),
        ArchiveConfig::default(),
        parse_endpoint(&state.config.jetstream_url),
    );
    jobs.config.relay_retry = TEST_RELAY_RETRY;
    jobs
}

/// The relay retry policy `jobs` uses.
pub(crate) const TEST_RELAY_RETRY: RelayRetry = RelayRetry {
    max_attempts: 4,
    base_delay: std::time::Duration::from_millis(5),
    max_delay: std::time::Duration::from_millis(20),
};

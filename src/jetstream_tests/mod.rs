//! The Jetstream source's database-backed tests. They need a host, which
//! `happyview_source_jetstream` cannot build, so they stay here and drive
//! it through `AppHost`.

use happyview_source_jetstream::archive::ArchiveConfig;
use happyview_source_jetstream::backfill::JobState;
use happyview_source_jetstream::consumer::config::parse_endpoint;

mod apply;
mod backfill;
mod cursor;
mod replay_fold;
mod replay_sweeps;

/// A job system over `state`, with default replay settings, against
/// `state.config.jetstream_url`.
pub(crate) fn jobs(state: &crate::AppState) -> JobState {
    JobState::new(
        crate::host::AppHost::new(state.clone()),
        ArchiveConfig::default(),
        parse_endpoint(&state.config.jetstream_url),
    )
}

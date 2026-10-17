//! Archive replay, the part of backfill still in HappyView. The job system
//! that runs it is the Jetstream source's
//! (`happyview_source_jetstream::backfill`), which hands replay jobs here
//! through [`ArchiveReplay`].

use async_trait::async_trait;
use happyview_source_jetstream::backfill::{JobState, Replay, ReplayOutcome};

use crate::AppState;

pub(crate) mod replay;
mod replay_fold;
mod replay_sweeps;

#[cfg(test)]
mod tests;

/// HappyView's archive replay, as the Jetstream source's job system asks for
/// it.
pub struct ArchiveReplay {
    state: AppState,
}

impl ArchiveReplay {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }
}

#[async_trait]
impl Replay for ArchiveReplay {
    async fn run(
        &self,
        jobs: &JobState,
        job_id: &str,
        collections: &[String],
        scope_dids: Option<Vec<String>>,
        source: &str,
    ) -> ReplayOutcome {
        replay::run(&self.state, jobs, job_id, collections, scope_dids, source).await
    }

    async fn job_dids(&self, _jobs: &JobState, job_id: &str) -> Result<Vec<String>, sqlx::Error> {
        replay_sweeps::job_dids(&self.state, job_id).await
    }

    async fn insert_retries(
        &self,
        _jobs: &JobState,
        job_id: &str,
        blocks: &[(String, i32)],
    ) -> Result<(), sqlx::Error> {
        replay_sweeps::insert_retries(&self.state, job_id, blocks).await
    }
}

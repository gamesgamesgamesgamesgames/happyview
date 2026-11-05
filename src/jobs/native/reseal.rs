//! `happyview.reseal`: re-seal every stored value under the current
//! `TOKEN_ENCRYPTION_KEY`, the second half of a key rotation (see
//! [`crate::reseal`]).
//!
//! Boot starts it while anything still needs it, and an operator can start it
//! from `POST /admin/encryption/rotation`. A pass that is paused, cancelled or
//! lost to a restart is safe to run again: values already under the current
//! key are skipped.

use crate::AppState;

use super::super::{Job, db};
use super::NativeOutcome;

pub const JOB_TYPE: &str = "happyview.reseal";

/// Who boot names as the job's creator.
pub const BOOT_ACTOR: &str = "happyview";

pub async fn run(state: &AppState, job: &Job) -> NativeOutcome {
    let Some(keys) = state.config.keyring() else {
        return NativeOutcome::Failed("TOKEN_ENCRYPTION_KEY is not set".into());
    };
    let result = crate::reseal::reseal(&state.db, state.db_backend, &keys, || {
        db::should_stop(state, &job.id)
    })
    .await;
    match result {
        Ok((stores, stopped)) => {
            let report = serde_json::json!({ "stores": stores, "stopped": stopped });
            let _ = db::update_progress(state, &job.id, &report).await;
            NativeOutcome::Completed(report)
        }
        Err(e) => NativeOutcome::Failed(e.to_string()),
    }
}

/// The re-seal job that is pending or running, if there is one.
pub async fn active(state: &AppState) -> Result<Option<Job>, crate::error::AppError> {
    let statuses = ["pending", "running", "pausing", "paused"].map(String::from);
    Ok(db::list_jobs_any(state, &statuses, Some(JOB_TYPE), 1)
        .await?
        .into_iter()
        .next())
}

/// Start a re-seal unless one is already under way. Returns its id.
pub async fn start(state: &AppState, created_by: &str) -> Result<String, crate::error::AppError> {
    if let Some(job) = active(state).await? {
        return Ok(job.id);
    }
    db::create_job(
        state,
        JOB_TYPE,
        &serde_json::json!({}),
        created_by,
        false,
        None,
        None,
    )
    .await
}

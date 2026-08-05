//! Built-in job types implemented in Rust rather than Lua.
//!
//! The `happyview.` prefix is **reserved**, and that reservation is an
//! authorization boundary rather than a naming convention: a native job runs
//! privileged Rust with no script mediating its input, so if a Lua script could
//! enqueue `happyview.delete-collection` it would bypass the
//! `records:delete-collection` permission check that the admin endpoint
//! performs. Only internal Rust callers may enqueue these.

pub mod delete_collection;

use crate::AppState;

use super::Job;

/// Job types beginning with this prefix may only be enqueued by internal Rust
/// callers, and may not be used as script trigger ids.
pub const RESERVED_PREFIX: &str = "happyview.";

/// Every implemented native job type.
const NATIVE_TYPES: &[&str] = &["happyview.delete-collection"];

/// Whether a job type is reserved. Reserved-but-unimplemented types are still
/// refused, so adding a handler later can never be shadowed by a user script
/// created in the meantime.
pub fn is_reserved(job_type: &str) -> bool {
    job_type.starts_with(RESERVED_PREFIX)
}

/// Whether a job type has a native handler.
pub fn is_native(job_type: &str) -> bool {
    NATIVE_TYPES.contains(&job_type)
}

/// What a native handler produced.
pub enum NativeOutcome {
    Completed(serde_json::Value),
    Failed(String),
}

/// Dispatch to the handler for this job type.
pub async fn execute(state: &AppState, job: &Job) -> NativeOutcome {
    match job.job_type.as_str() {
        "happyview.delete-collection" => delete_collection::run(state, job).await,
        other => NativeOutcome::Failed(format!("no native handler for job type '{other}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happyview_prefix_is_reserved() {
        assert!(is_reserved("happyview.delete-collection"));
        assert!(is_reserved("happyview.anything"));
    }

    #[test]
    fn user_job_types_are_not_reserved() {
        assert!(!is_reserved("pet.trezy.reset-account"));
        assert!(!is_reserved("happyviewish.thing"));
        assert!(!is_reserved("my.happyview.thing"));
    }

    #[test]
    fn only_known_reserved_types_are_native() {
        assert!(is_native("happyview.delete-collection"));
        // Reserved but unimplemented: still refused to callers, but not dispatched.
        assert!(!is_native("happyview.not-a-real-job"));
        assert!(!is_native("pet.trezy.reset-account"));
    }
}

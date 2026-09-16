pub(crate) mod db;
pub(crate) mod logs;
pub mod native;
pub mod worker;

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub job_type: String,
    pub status: String,
    pub input: serde_json::Value,
    pub progress: serde_json::Value,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    pub created_by: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: String,
    pub inherit_auth: bool,
    pub api_client_id: Option<String>,
    pub dpop_key_id: Option<String>,
}

static JOB_TYPE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9._-]*$").unwrap());

/// Whether `job_type` may be enqueued: 1–128 characters matching
/// `/^[a-z0-9][a-z0-9._-]*$/` and outside the `happyview.` prefix reserved for
/// native jobs. Shared by the trigger grammar (`job.run:<type>`), the
/// `jobs.create` Lua global, and the `happyview.jobs` library import, so a
/// type accepted in one is accepted in all three.
pub fn validate_job_type(job_type: &str) -> Result<(), String> {
    if job_type.is_empty() || job_type.len() > 128 {
        return Err(format!(
            "invalid job type '{job_type}': must be 1–128 characters"
        ));
    }
    if !JOB_TYPE_RE.is_match(job_type) {
        return Err(format!(
            "invalid job type '{job_type}': must match /^[a-z0-9][a-z0-9._-]*$/"
        ));
    }
    if native::is_reserved(job_type) {
        return Err(format!(
            "invalid job type '{job_type}': the '{}' prefix is reserved for built-in jobs",
            native::RESERVED_PREFIX
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_job_type_accepts_a_normal_type() {
        assert!(validate_job_type("test.export").is_ok());
    }

    #[test]
    fn validate_job_type_refuses_empty() {
        let err = validate_job_type("").unwrap_err();
        assert!(err.contains("1–128 characters"), "{err}");
    }

    #[test]
    fn validate_job_type_refuses_129_chars() {
        let too_long = "a".repeat(129);
        let err = validate_job_type(&too_long).unwrap_err();
        assert!(err.contains("1–128 characters"), "{err}");
    }

    #[test]
    fn validate_job_type_refuses_uppercase() {
        let err = validate_job_type("Test.Export").unwrap_err();
        assert!(err.contains("must match"), "{err}");
    }

    #[test]
    fn validate_job_type_refuses_the_reserved_prefix() {
        let err = validate_job_type("happyview.x").unwrap_err();
        assert!(err.contains("reserved"), "{err}");
    }
}

//! Startup audit for stored config the NSID consolidation tightened rules
//! around.
//!
//! Validation on trigger ids and proxy patterns only tightened on *write*
//! paths, so a row written under looser rules keeps working after an
//! upgrade — but it can no longer be re-saved. That is a latent lockout: an
//! operator who deletes a legacy script to recreate it, or who toggles proxy
//! mode without touching its pattern list, gets rejected on a config that
//! was fine a moment ago.
//!
//! This scan makes that latent state visible at boot. It is read-only (two
//! queries, no writes) and fail-open: any database error or unexpected shape
//! logs a warning and lets the process continue booting. A config that
//! currently works must never be the reason startup fails.

use sqlx::AnyPool;
use tracing::warn;

use crate::db::{DatabaseBackend, adapt_sql};
use crate::lua::scripts::ParsedTrigger;
use crate::proxy_config::ProxyConfig;

/// A `happyview_scripts.id` that would be rejected if re-saved today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyTriggerFinding {
    pub trigger_id: String,
    pub error: String,
}

/// An `xrpc_proxy_config` pattern that would be rejected if the config were
/// re-saved today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyProxyPatternFinding {
    pub pattern: String,
    pub error: String,
}

/// Classify a single trigger id: `None` if it parses cleanly under the
/// current grammar, or if it belongs to a family this audit does not cover;
/// `Some` naming why it would now be rejected.
///
/// `job.run:*` job types and the literal `labeler.apply:_actor` are skipped
/// outright rather than run through [`ParsedTrigger::parse`] — they are
/// valid trigger ids, just not NSIDs, so a malformed one is a different
/// problem than the one the NSID consolidation created.
pub fn classify_trigger_id(id: &str) -> Option<LegacyTriggerFinding> {
    if id.starts_with("job.run:") || id == "labeler.apply:_actor" {
        return None;
    }

    match ParsedTrigger::parse(id) {
        Ok(_) => None,
        Err(error) => Some(LegacyTriggerFinding {
            trigger_id: id.to_string(),
            error,
        }),
    }
}

/// Classify a single proxy-config NSID pattern the same way.
pub fn classify_proxy_pattern(pattern: &str) -> Option<LegacyProxyPatternFinding> {
    match happyview_nsid::validate_nsid_pattern(pattern) {
        Ok(()) => None,
        Err(error) => Some(LegacyProxyPatternFinding {
            pattern: pattern.to_string(),
            error: error.to_string(),
        }),
    }
}

/// Scan `happyview_scripts.id` for trigger ids the current NSID grammar
/// would reject, and warn about each one found.
async fn audit_scripts(pool: &AnyPool, backend: DatabaseBackend) -> Result<usize, sqlx::Error> {
    let sql = adapt_sql("SELECT id FROM happyview_scripts", backend);
    let rows: Vec<(String,)> = crate::db::query_as(&sql).fetch_all(pool).await?;

    let mut found = 0;
    for (id,) in rows {
        if let Some(finding) = classify_trigger_id(&id) {
            warn!(
                trigger_id = %finding.trigger_id,
                error = %finding.error,
                "legacy trigger id '{}' would be rejected if re-saved ({}). It continues to \
                 fire and can still be edited, but it cannot be recreated after deletion.",
                finding.trigger_id, finding.error
            );
            found += 1;
        }
    }
    Ok(found)
}

/// Scan the stored `xrpc_proxy_config` patterns for ones the current NSID
/// grammar would reject, and warn about each one found.
///
/// Fail-open on a missing setting (nothing configured yet) or a value that
/// doesn't deserialize as `ProxyConfig` — the latter would be surprising,
/// but it is not this scan's job to diagnose it, only to not crash on it.
async fn audit_proxy_config(
    pool: &AnyPool,
    backend: DatabaseBackend,
) -> Result<usize, sqlx::Error> {
    let sql = adapt_sql(
        "SELECT value FROM happyview_instance_settings WHERE key = 'xrpc_proxy_config'",
        backend,
    );
    let row: Option<(String,)> = crate::db::query_as(&sql).fetch_optional(pool).await?;

    let Some((raw,)) = row else {
        return Ok(0);
    };

    let config: ProxyConfig = match serde_json::from_str(&raw) {
        Ok(c) => c,
        Err(e) => {
            warn!(
                error = %e,
                "stored xrpc_proxy_config could not be parsed; skipping the NSID audit for it"
            );
            return Ok(0);
        }
    };

    let mut found = 0;
    for pattern in &config.nsids {
        if let Some(finding) = classify_proxy_pattern(pattern) {
            warn!(
                pattern = %finding.pattern,
                error = %finding.error,
                "legacy proxy pattern '{}' would be rejected if the config were re-saved ({}). \
                 It still matches at runtime, but the config cannot be saved while it is present.",
                finding.pattern, finding.error
            );
            found += 1;
        }
    }
    Ok(found)
}

/// Run both scans. Never returns an error to the caller and never blocks
/// startup: any failure is logged at WARN and treated as "nothing found" so
/// a database hiccup at boot cannot be mistaken for a clean scan, but also
/// cannot stop the process from booting.
pub async fn run(pool: &AnyPool, backend: DatabaseBackend) {
    let scripts_found = match audit_scripts(pool, backend).await {
        Ok(n) => n,
        Err(e) => {
            warn!(error = %e, "NSID audit: failed to scan happyview_scripts; skipping");
            0
        }
    };

    let proxy_found = match audit_proxy_config(pool, backend).await {
        Ok(n) => n,
        Err(e) => {
            warn!(
                error = %e,
                "NSID audit: failed to scan xrpc_proxy_config; skipping"
            );
            0
        }
    };

    if scripts_found == 0 && proxy_found == 0 {
        tracing::info!(
            "NSID audit: no legacy trigger ids or proxy patterns found; all stored config \
             satisfies the current NSID grammar"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_trigger_id_produces_no_finding() {
        assert_eq!(
            classify_trigger_id("xrpc.query:com.example.getPhotos"),
            None
        );
        assert_eq!(classify_trigger_id("record.index:com.example.thing"), None);
        assert_eq!(
            classify_trigger_id("labeler.apply:app.bsky.feed.post"),
            None
        );
    }

    #[test]
    fn legacy_two_segment_trigger_id_produces_a_finding() {
        let finding = classify_trigger_id("xrpc.query:com.example")
            .expect("two-segment NSID should be flagged");
        assert_eq!(finding.trigger_id, "xrpc.query:com.example");
        assert!(finding.error.contains("com.example"));
    }

    #[test]
    fn hyphenated_name_segment_produces_a_finding() {
        let finding = classify_trigger_id("xrpc.query:com.example.get-photos")
            .expect("hyphenated name segment should be flagged");
        assert_eq!(finding.trigger_id, "xrpc.query:com.example.get-photos");
        assert!(finding.error.contains("com.example.get-photos"));
    }

    #[test]
    fn job_run_triggers_are_skipped_regardless_of_shape() {
        assert_eq!(classify_trigger_id("job.run:happyview.export"), None);
        // Would fail job-type validation too (uppercase), but this audit is
        // about NSIDs, not job types, so it is skipped outright.
        assert_eq!(classify_trigger_id("job.run:UPPER"), None);
    }

    #[test]
    fn labeler_apply_actor_is_skipped() {
        assert_eq!(classify_trigger_id("labeler.apply:_actor"), None);
    }

    #[test]
    fn malformed_trigger_id_with_no_separator_still_produces_a_finding() {
        // Not skipped, since it isn't job.run or the literal _actor case.
        assert!(classify_trigger_id("garbage").is_some());
    }

    #[test]
    fn valid_proxy_pattern_produces_no_finding() {
        assert_eq!(classify_proxy_pattern("com.example.feed.getHot"), None);
        assert_eq!(classify_proxy_pattern("com.example.*"), None);
    }

    #[test]
    fn legacy_proxy_pattern_produces_a_finding() {
        let finding =
            classify_proxy_pattern("1.foo.*").expect("digit-leading TLD should be flagged");
        assert_eq!(finding.pattern, "1.foo.*");
        assert!(finding.error.contains("1.foo.*"));
    }
}

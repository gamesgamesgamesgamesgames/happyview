//! Telling a refusal from an outage when a linked repo's session fails.
//!
//! A grant needs reauthorization only when the account's authorization server
//! refused it, or its stored session can't be used again: the grant was
//! rejected or revoked, the session is gone or no longer reads back, or the
//! key it was made with is no longer held. Anything else, such as a 5xx, a
//! rate limit, a timeout or a dropped connection, leaves the grant active, so
//! a passing outage doesn't make an operator authorize it again, until it has
//! failed for [`GIVE_UP_AFTER`].

/// What a failed session restore or refresh means for the grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshFailure {
    /// The authorization server refused the grant, or the stored session can't
    /// be read back. It needs reauthorization.
    Refused,
    /// The authorization server or the network failed. Retry later.
    Upstream,
    /// This instance's own session store failed, such as a locked database.
    /// Retry later.
    Local,
}

impl RefreshFailure {
    /// Whether the grant should stay active and be retried.
    pub fn is_transient(self) -> bool {
        !matches!(self, RefreshFailure::Refused)
    }
}

/// Classify an atrium OAuth error by its message.
///
/// atrium's server-agent and session-registry error types are private, so
/// their messages are what can be read. The strings below are atrium-oauth
/// 0.1.7's (`http status: 400 Bad Request, body: …`, `no refresh token
/// available`, `session does not exist`, `session store error: …`) wrapping
/// `auth::oauth_store::StoreError`'s (`database error`, `json error`,
/// `encryption error`). `atrium_oauth_is_the_version_these_strings_match`
/// fails when the dependency moves, so they are checked again.
pub fn classify(message: &str) -> RefreshFailure {
    let m = message.to_ascii_lowercase();

    // A DPoP nonce challenge is a 400 that only asks for a retry.
    if m.contains("use_dpop_nonce") {
        return RefreshFailure::Upstream;
    }

    // A session that no longer decrypts (the key that sealed it is gone) or
    // no longer parses can't be restored by retrying.
    if m.contains("session store error: encryption error")
        || m.contains("session store error: json error")
    {
        return RefreshFailure::Refused;
    }
    if m.contains("session store error") {
        return RefreshFailure::Local;
    }

    const REFUSALS: &[&str] = &[
        "invalid_grant",
        "invalid_client",
        "unauthorized_client",
        "access_denied",
        "revoked",
        "no refresh token available",
        "session does not exist",
        "http status: 400",
        "http status: 401",
    ];
    if REFUSALS.iter().any(|r| m.contains(r)) {
        RefreshFailure::Refused
    } else {
        RefreshFailure::Upstream
    }
}

/// How long a grant may go without a successful refresh, failing transiently
/// throughout, before the keep-alive gives up on it.
pub const GIVE_UP_AFTER: chrono::Duration = chrono::Duration::days(7);

/// Whether a grant last refreshed (or authorized) at `last_success` has been
/// failing for longer than [`GIVE_UP_AFTER`].
pub fn given_up(last_success: Option<&str>, now: chrono::DateTime<chrono::Utc>) -> bool {
    last_success
        .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
        .is_some_and(|t| now - t.with_timezone(&chrono::Utc) > GIVE_UP_AFTER)
}

/// How long the keep-alive waits before retrying a grant after `failures`
/// transient failures in a row: one tick, then doubling, up to `cap`.
pub fn backoff(
    failures: u32,
    tick: std::time::Duration,
    cap: std::time::Duration,
) -> std::time::Duration {
    let doublings = failures.saturating_sub(1).min(16);
    tick.saturating_mul(1u32 << doublings).min(cap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn refusals_need_reauthorization() {
        for message in [
            r#"http status: 400 Bad Request, body: Object {"error": String("invalid_grant")}"#,
            "http status: 401 Unauthorized",
            "token has been revoked",
            "no refresh token available",
            "session does not exist",
        ] {
            assert_eq!(classify(message), RefreshFailure::Refused, "{message}");
        }
    }

    #[test]
    fn a_session_that_cannot_be_read_back_needs_reauthorization() {
        for message in [
            "session store error: encryption error: decryption failed",
            "session store error: json error: expected value at line 1 column 1",
        ] {
            assert_eq!(classify(message), RefreshFailure::Refused, "{message}");
        }
    }

    #[test]
    fn a_session_store_database_error_is_local() {
        assert_eq!(
            classify("session store error: database error: database is locked"),
            RefreshFailure::Local
        );
        assert!(RefreshFailure::Local.is_transient());
    }

    #[test]
    fn a_grant_failing_for_over_a_week_is_given_up() {
        let now = chrono::Utc::now();
        let days_ago = |d: i64| (now - chrono::Duration::days(d)).to_rfc3339();
        assert!(!given_up(Some(&days_ago(6)), now));
        assert!(given_up(Some(&days_ago(8)), now));
        assert!(!given_up(None, now));
    }

    /// The strings [`classify`] matches are atrium-oauth 0.1.7's. Moving the
    /// dependency means checking them again, then updating this.
    #[test]
    fn atrium_oauth_is_the_version_these_strings_match() {
        let lock = include_str!("../../Cargo.lock");
        assert!(
            lock.contains("name = \"atrium-oauth\"\nversion = \"0.1.7\""),
            "atrium-oauth moved from 0.1.7: check classify's strings"
        );
    }

    #[test]
    fn outages_are_transient() {
        for message in [
            "http status: 502 Bad Gateway",
            "http status: 503 Service Unavailable, body: Null",
            "http status: 429 Too Many Requests",
            "http client error: error sending request: operation timed out",
            "http client error: error sending request: connection refused",
            r#"http status: 400 Bad Request, body: Object {"error": String("use_dpop_nonce")}"#,
        ] {
            assert_eq!(classify(message), RefreshFailure::Upstream, "{message}");
        }
    }

    #[test]
    fn backoff_doubles_from_one_tick_to_the_cap() {
        let tick = Duration::from_secs(30 * 60);
        let cap = Duration::from_secs(8 * 60 * 60);
        assert_eq!(backoff(1, tick, cap), tick);
        assert_eq!(backoff(2, tick, cap), tick * 2);
        assert_eq!(backoff(3, tick, cap), tick * 4);
        assert_eq!(backoff(5, tick, cap), cap);
        assert_eq!(backoff(40, tick, cap), cap);
    }
}

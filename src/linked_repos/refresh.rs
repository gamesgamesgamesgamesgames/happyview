//! Telling a refusal from an outage when a linked repo's session fails.
//!
//! A grant needs reauthorization only when the account's authorization server
//! refused it: the grant was rejected or revoked, the session is gone, or the
//! key it was made with is no longer held. Anything else, such as a 5xx, a
//! rate limit, a timeout or a dropped connection, leaves the grant active, so
//! a passing outage doesn't make an operator authorize it again.

/// What a failed session restore or refresh means for the grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshFailure {
    /// The authorization server refused the grant. It needs reauthorization.
    Refused,
    /// The server or the network failed. Retry later.
    Transient,
}

/// Classify an atrium OAuth error by its message. atrium's server-agent and
/// session-registry error types are private, so their messages are what can be
/// read: `http status: 400 Bad Request, body: …`, `no refresh token
/// available`, `session does not exist`.
pub fn classify(message: &str) -> RefreshFailure {
    let m = message.to_ascii_lowercase();

    // A DPoP nonce challenge is a 400 that only asks for a retry.
    if m.contains("use_dpop_nonce") {
        return RefreshFailure::Transient;
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
        RefreshFailure::Transient
    }
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
    fn outages_are_transient() {
        for message in [
            "http status: 502 Bad Gateway",
            "http status: 503 Service Unavailable, body: Null",
            "http status: 429 Too Many Requests",
            "http client error: error sending request: operation timed out",
            "http client error: error sending request: connection refused",
            r#"http status: 400 Bad Request, body: Object {"error": String("use_dpop_nonce")}"#,
            "session store error: database is locked",
        ] {
            assert_eq!(classify(message), RefreshFailure::Transient, "{message}");
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

//! How a fetch from a PDS, a DID document host or an archive failed:
//! classified, so a caller can tell a failure worth retrying from a final
//! one. HappyView's identity lookups and the Jetstream source's backfill
//! share it.

use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};

use crate::http::{describe_error_chain, parse_retry_after};

/// How a backfill attempt failed.
///
/// The split between retryable and not follows one principle: a definitive
/// answer from a live server is information, and a transport failure is the
/// absence of it. Information is final; absence is worth asking about again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackfillErrorKind {
    DnsFailure,
    ConnectionFailed,
    Timeout,
    PdsServerError,
    RateLimited,
    DidDocNotFound,
    DidDocForbidden,
    DidDocInvalid,
    RepoNotFound,
    RepoDeactivated,
    RepoTakendown,
    Other,
    /// An archive block or segment that failed to download or decode.
    ReplayBlock,
}

impl BackfillErrorKind {
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::DnsFailure
                | Self::ConnectionFailed
                | Self::Timeout
                | Self::PdsServerError
                | Self::RateLimited
                | Self::ReplayBlock
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::DnsFailure => "dns_failure",
            Self::ConnectionFailed => "connection_failed",
            Self::Timeout => "timeout",
            Self::PdsServerError => "pds_server_error",
            Self::RateLimited => "rate_limited",
            Self::DidDocNotFound => "did_doc_not_found",
            Self::DidDocForbidden => "did_doc_forbidden",
            Self::DidDocInvalid => "did_doc_invalid",
            Self::RepoNotFound => "repo_not_found",
            Self::RepoDeactivated => "repo_deactivated",
            Self::RepoTakendown => "repo_takendown",
            Self::Other => "other",
            Self::ReplayBlock => "replay_block",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "dns_failure" => Self::DnsFailure,
            "connection_failed" => Self::ConnectionFailed,
            "timeout" => Self::Timeout,
            "pds_server_error" => Self::PdsServerError,
            "rate_limited" => Self::RateLimited,
            "did_doc_not_found" => Self::DidDocNotFound,
            "did_doc_forbidden" => Self::DidDocForbidden,
            "did_doc_invalid" => Self::DidDocInvalid,
            "repo_not_found" => Self::RepoNotFound,
            "repo_deactivated" => Self::RepoDeactivated,
            "repo_takendown" => Self::RepoTakendown,
            "other" => Self::Other,
            "replay_block" => Self::ReplayBlock,
            _ => return None,
        })
    }

    /// Every kind, for zero-filling count maps and driving the dashboard legend.
    pub fn all() -> [Self; 13] {
        [
            Self::DnsFailure,
            Self::ConnectionFailed,
            Self::Timeout,
            Self::PdsServerError,
            Self::RateLimited,
            Self::DidDocNotFound,
            Self::DidDocForbidden,
            Self::DidDocInvalid,
            Self::RepoNotFound,
            Self::RepoDeactivated,
            Self::RepoTakendown,
            Self::Other,
            Self::ReplayBlock,
        ]
    }
}

const MAX_MESSAGE_CHARS: usize = 220;

fn truncate(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.chars().count() <= MAX_MESSAGE_CHARS {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(MAX_MESSAGE_CHARS - 1).collect();
    out.push('…');
    out
}

#[derive(Clone, Debug)]
pub struct BackfillFailure {
    pub kind: BackfillErrorKind,
    pub message: String,
    pub retry_after: Option<Duration>,
}

impl BackfillFailure {
    pub fn from_reqwest(e: &reqwest::Error) -> Self {
        let message = describe_error_chain(e);
        // Order matters: a DNS failure also reports `is_connect()`, so it must
        // be checked first or every NXDOMAIN is mislabelled as a refused
        // connection.
        let kind = if e.is_timeout() {
            BackfillErrorKind::Timeout
        } else if message.contains("dns error") || message.contains("failed to lookup address") {
            BackfillErrorKind::DnsFailure
        } else if e.is_connect() {
            BackfillErrorKind::ConnectionFailed
        } else {
            BackfillErrorKind::Other
        };
        Self {
            kind,
            message: truncate(&message),
            retry_after: None,
        }
    }

    pub fn from_did_doc_response(status: StatusCode, headers: &HeaderMap) -> Self {
        let (kind, retry_after) = match status {
            StatusCode::TOO_MANY_REQUESTS => (
                BackfillErrorKind::RateLimited,
                Some(Duration::from_secs(parse_retry_after(headers))),
            ),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                (BackfillErrorKind::DidDocForbidden, None)
            }
            StatusCode::NOT_FOUND | StatusCode::GONE => (BackfillErrorKind::DidDocNotFound, None),
            s if s.is_server_error() => (BackfillErrorKind::PdsServerError, None),
            _ => (BackfillErrorKind::Other, None),
        };
        Self {
            kind,
            message: truncate(&format!("DID document request returned {status}")),
            retry_after,
        }
    }

    pub fn from_pds_response(status: StatusCode, body: &str, headers: &HeaderMap) -> Self {
        let error_code = serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or_default();

        let (kind, retry_after) = match status {
            StatusCode::TOO_MANY_REQUESTS => (
                BackfillErrorKind::RateLimited,
                Some(Duration::from_secs(parse_retry_after(headers))),
            ),
            s if s.is_server_error() => (BackfillErrorKind::PdsServerError, None),
            _ => {
                let kind = match error_code.as_str() {
                    "RepoDeactivated" => BackfillErrorKind::RepoDeactivated,
                    "RepoTakendown" => BackfillErrorKind::RepoTakendown,
                    "RepoNotFound" => BackfillErrorKind::RepoNotFound,
                    // atproto PDSes answer a missing repo as a generic
                    // InvalidRequest whose message names the cause.
                    _ if body.contains("Could not find repo") => BackfillErrorKind::RepoNotFound,
                    _ => BackfillErrorKind::Other,
                };
                (kind, None)
            }
        };

        let message = if body.trim().is_empty() {
            format!("PDS returned {status}")
        } else {
            format!("PDS returned {status}: {}", body.trim())
        };
        Self {
            kind,
            message: truncate(&message),
            retry_after,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;
    use reqwest::header::HeaderMap;

    #[test]
    fn retryable_kinds_are_exactly_the_transport_and_server_ones() {
        use BackfillErrorKind::*;
        for k in [
            DnsFailure,
            ConnectionFailed,
            Timeout,
            PdsServerError,
            RateLimited,
            ReplayBlock,
        ] {
            assert!(k.is_retryable(), "{k:?} should be retryable");
        }
        for k in [
            DidDocNotFound,
            DidDocForbidden,
            DidDocInvalid,
            RepoNotFound,
            RepoDeactivated,
            RepoTakendown,
            Other,
        ] {
            assert!(!k.is_retryable(), "{k:?} should not be retryable");
        }
    }

    #[test]
    fn did_doc_404_is_not_found() {
        let f = BackfillFailure::from_did_doc_response(StatusCode::NOT_FOUND, &HeaderMap::new());
        assert_eq!(f.kind, BackfillErrorKind::DidDocNotFound);
        assert!(!f.kind.is_retryable());
    }

    #[test]
    fn did_doc_403_is_forbidden_not_missing() {
        // www.viruus.zip answers 403 from a WAF. Reporting that as "not found"
        // is what made the original log unreadable.
        let f = BackfillFailure::from_did_doc_response(StatusCode::FORBIDDEN, &HeaderMap::new());
        assert_eq!(f.kind, BackfillErrorKind::DidDocForbidden);
    }

    #[test]
    fn did_doc_5xx_is_retryable() {
        let f = BackfillFailure::from_did_doc_response(StatusCode::BAD_GATEWAY, &HeaderMap::new());
        assert_eq!(f.kind, BackfillErrorKind::PdsServerError);
        assert!(f.kind.is_retryable());
    }

    #[test]
    fn pds_400_could_not_find_repo_is_repo_not_found() {
        // Verbatim from pioppino.us-west.host.bsky.network.
        let body = r#"{"error":"InvalidRequest","message":"Could not find repo: did:plc:izvzfrxkzrjqhyere4ch4qok"}"#;
        let f =
            BackfillFailure::from_pds_response(StatusCode::BAD_REQUEST, body, &HeaderMap::new());
        assert_eq!(f.kind, BackfillErrorKind::RepoNotFound);
        assert!(!f.kind.is_retryable());
    }

    #[test]
    fn pds_deactivated_and_takendown_are_distinguished() {
        let d = BackfillFailure::from_pds_response(
            StatusCode::BAD_REQUEST,
            r#"{"error":"RepoDeactivated"}"#,
            &HeaderMap::new(),
        );
        assert_eq!(d.kind, BackfillErrorKind::RepoDeactivated);

        let t = BackfillFailure::from_pds_response(
            StatusCode::BAD_REQUEST,
            r#"{"error":"RepoTakendown"}"#,
            &HeaderMap::new(),
        );
        assert_eq!(t.kind, BackfillErrorKind::RepoTakendown);
    }

    #[test]
    fn pds_429_carries_retry_after() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "30".parse().unwrap());
        let f = BackfillFailure::from_pds_response(StatusCode::TOO_MANY_REQUESTS, "", &headers);
        assert_eq!(f.kind, BackfillErrorKind::RateLimited);
        assert_eq!(f.retry_after, Some(Duration::from_secs(30)));
    }

    #[test]
    fn pds_5xx_is_server_error() {
        let f = BackfillFailure::from_pds_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "",
            &HeaderMap::new(),
        );
        assert_eq!(f.kind, BackfillErrorKind::PdsServerError);
    }

    #[test]
    fn message_is_truncated_but_marked() {
        let body = "x".repeat(500);
        let f =
            BackfillFailure::from_pds_response(StatusCode::BAD_REQUEST, &body, &HeaderMap::new());
        assert!(f.message.chars().count() <= 220, "message not truncated");
        assert!(f.message.ends_with('…'));
    }

    #[test]
    fn kind_str_round_trips_for_storage() {
        use BackfillErrorKind::*;
        for k in [
            DnsFailure,
            ConnectionFailed,
            Timeout,
            PdsServerError,
            RateLimited,
            DidDocNotFound,
            DidDocForbidden,
            DidDocInvalid,
            RepoNotFound,
            RepoDeactivated,
            RepoTakendown,
            Other,
        ] {
            assert_eq!(BackfillErrorKind::parse(k.as_str()), Some(k));
        }
    }
}

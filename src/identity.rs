//! Resolution of AT Protocol account identifiers (handle or DID) to a DID.
//!
//! Every surface that accepts "an account" from an operator accepts either
//! form, so the normalization has to live in one place. Storing an
//! unresolved handle where a DID is expected is silently broken rather than
//! loudly broken: authorization checks compare against the DID in an OAuth
//! session, so the row simply never matches and the account cannot sign in
//! (issue #85). Resolving at the point of entry keeps that failure at the
//! point of the mistake.

use std::sync::Arc;
use std::time::Duration;

use atrium_identity::handle::{AtprotoHandleResolver, AtprotoHandleResolverConfig};

use crate::admin::backfill_errors::{BackfillErrorKind, BackfillFailure};
use crate::dns::NativeDnsResolver;
use crate::error::AppError;

/// The shared handle resolver, wired to HappyView's own DNS resolver and
/// retrying HTTP client.
pub fn handle_resolver()
-> AtprotoHandleResolver<NativeDnsResolver, crate::http_retry::HappyViewHttpClient> {
    AtprotoHandleResolver::new(AtprotoHandleResolverConfig {
        dns_txt_resolver: NativeDnsResolver::new(),
        http_client: Arc::new(crate::http_retry::HappyViewHttpClient::new(
            crate::http_retry::shared_client().clone(),
        )),
    })
}

pub struct ResolvedIdentifier {
    pub did: String,
    /// Set only when the caller supplied a handle — a bare DID tells us
    /// nothing about which handle currently points at it.
    pub handle: Option<String>,
}

/// Resolve an account identifier to a DID.
///
/// A `did:` prefix is validated for shape and passed through. Anything else is
/// treated as a handle: validated, then resolved via DNS TXT `_atproto.<handle>`
/// with an HTTPS `.well-known/atproto-did` fallback. A leading `@` is accepted
/// because operators paste handles that way.
///
/// Both failure modes are `BadRequest` — the input is at fault, not the server.
pub async fn resolve_identifier(input: &str) -> Result<ResolvedIdentifier, AppError> {
    let input = input.trim();

    if input.is_empty() {
        return Err(AppError::BadRequest(
            "expected a handle or DID, got an empty value".into(),
        ));
    }

    if input.starts_with("did:") {
        let did = atrium_api::types::string::Did::new(input.to_string())
            .map_err(|e| AppError::BadRequest(format!("invalid DID {input}: {e}")))?;
        return Ok(ResolvedIdentifier {
            did: did.as_str().to_string(),
            handle: None,
        });
    }

    let handle_str = input.trim_start_matches('@').to_string();
    let did = resolve_handle(&handle_str).await?;

    Ok(ResolvedIdentifier {
        did,
        handle: Some(handle_str),
    })
}

/// Resolve a handle (without a leading `@`) to its DID via DNS TXT
/// `_atproto.<handle>` with an HTTPS `.well-known/atproto-did` fallback.
pub async fn resolve_handle(handle: &str) -> Result<String, AppError> {
    use atrium_common::resolver::Resolver;

    let parsed = atrium_api::types::string::Handle::new(handle.to_string())
        .map_err(|_| AppError::BadRequest(format!("invalid handle: {handle}")))?;
    // The resolver's error text can include the `.well-known` URL and HTTP
    // client detail, so the caller gets a fixed reason and the detail is
    // logged.
    let did = handle_resolver().resolve(&parsed).await.map_err(|e| {
        tracing::debug!(handle, error = %e, "handle resolution failed");
        AppError::BadRequest(format!("could not resolve handle {handle}"))
    })?;
    Ok(did.as_ref().to_string())
}

pub struct VerifiedIdentity {
    pub did: String,
    /// Present only when the DID document claims the handle and the handle
    /// resolves to the same DID. A one-directional match is how a handle
    /// gets impersonated, so it is never shown.
    pub handle: Option<String>,
}

/// Resolve a handle or DID and confirm the handle in both directions.
///
/// Resolution is injected so the verification rules can be tested without
/// network access; [`resolve_verified`] wires in the real resolvers.
pub async fn resolve_verified_with<RH, RHF, AKA, AKAF>(
    input: &str,
    resolve_handle: RH,
    also_known_as: AKA,
) -> Result<VerifiedIdentity, AppError>
where
    RH: Fn(String) -> RHF,
    RHF: std::future::Future<Output = Result<String, AppError>>,
    AKA: Fn(String) -> AKAF,
    AKAF: std::future::Future<Output = Result<Vec<String>, AppError>>,
{
    let input = input.trim();

    if input.is_empty() {
        return Err(AppError::BadRequest(
            "expected a handle or DID, got an empty value".into(),
        ));
    }

    if input.starts_with("did:") {
        let did = atrium_api::types::string::Did::new(input.to_string())
            .map_err(|e| AppError::BadRequest(format!("invalid DID {input}: {e}")))?
            .as_str()
            .to_string();
        let aka = also_known_as(did.clone()).await?;
        let handle = match claimed_handle(&aka) {
            Some(candidate) => match resolve_handle(candidate.clone()).await {
                Ok(resolved) if resolved == did => Some(candidate),
                _ => None,
            },
            None => None,
        };
        return Ok(VerifiedIdentity { did, handle });
    }

    let handle = input.trim_start_matches('@').to_ascii_lowercase();
    atrium_api::types::string::Handle::new(handle.clone())
        .map_err(|_| AppError::BadRequest(format!("invalid handle: {input}")))?;
    let did = resolve_handle(handle.clone()).await?;
    let aka = also_known_as(did.clone()).await?;
    let claimed = aka.iter().any(|uri| {
        uri.strip_prefix("at://")
            .is_some_and(|h| h.eq_ignore_ascii_case(&handle))
    });

    Ok(VerifiedIdentity {
        did,
        handle: claimed.then_some(handle),
    })
}

fn claimed_handle(also_known_as: &[String]) -> Option<String> {
    also_known_as
        .iter()
        .find_map(|uri| uri.strip_prefix("at://"))
        .map(str::to_ascii_lowercase)
}

/// Upper bound on a whole verified resolution. A `did:web` host is chosen by
/// the caller, so without a bound a slow or hostile host holds the request
/// open.
const RESOLUTION_TIMEOUT: Duration = Duration::from_secs(10);

/// [`resolve_verified_with`] using DNS/HTTPS handle resolution and the PLC
/// directory or `did:web` for DID documents, bounded by
/// [`RESOLUTION_TIMEOUT`].
///
/// The DID document is fetched once: retrying a 429 would spend the bound
/// sleeping on whatever `Retry-After` the host chose.
pub async fn resolve_verified(
    http: &reqwest::Client,
    plc_url: &str,
    input: &str,
) -> Result<VerifiedIdentity, AppError> {
    let work = resolve_verified_with(
        input,
        |handle| async move { resolve_handle(&handle).await },
        |did| async move {
            crate::profile::resolve_did_document_once(http, plc_url, &did)
                .await
                .map(|doc| doc.also_known_as)
                .map_err(|failure| did_document_failure(&did, &failure))
        },
    );
    bounded_resolution(input, RESOLUTION_TIMEOUT, work).await
}

async fn bounded_resolution<T>(
    input: &str,
    limit: Duration,
    work: impl std::future::Future<Output = Result<T, AppError>>,
) -> Result<T, AppError> {
    tokio::time::timeout(limit, work)
        .await
        .map_err(|_| AppError::BadRequest(format!("timed out resolving {}", input.trim())))?
}

/// A fixed reason for a failed DID document fetch.
///
/// The failure's own message carries URLs and connection detail. Returning it
/// would let any signed-in caller learn which hosts the server can reach by
/// probing `did:web:<host>`, so only the DID and a coarse category are
/// reported; the detail goes to the log.
fn did_document_failure(did: &str, failure: &BackfillFailure) -> AppError {
    tracing::debug!(
        did,
        kind = failure.kind.as_str(),
        detail = %failure.message,
        "DID document fetch failed"
    );
    let reason = match failure.kind {
        BackfillErrorKind::DidDocNotFound => format!("DID document not found for {did}"),
        BackfillErrorKind::DidDocForbidden => {
            format!("access to the DID document for {did} was denied")
        }
        BackfillErrorKind::DidDocInvalid => format!("DID document for {did} is invalid"),
        BackfillErrorKind::RateLimited => {
            format!("DID document lookup for {did} was rate limited")
        }
        BackfillErrorKind::DnsFailure
        | BackfillErrorKind::ConnectionFailed
        | BackfillErrorKind::Timeout
        | BackfillErrorKind::PdsServerError => {
            format!("could not reach the DID document host for {did}")
        }
        BackfillErrorKind::RepoNotFound
        | BackfillErrorKind::RepoDeactivated
        | BackfillErrorKind::RepoTakendown
        | BackfillErrorKind::Other => format!("could not fetch DID document for {did}"),
    };
    AppError::BadRequest(reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_bare_did_passes_through_with_no_handle() {
        let resolved = resolve_identifier("did:plc:abc123").await.unwrap();
        assert_eq!(resolved.did, "did:plc:abc123");
        assert!(resolved.handle.is_none());
    }

    #[tokio::test]
    async fn surrounding_whitespace_is_ignored() {
        let resolved = resolve_identifier("  did:plc:abc123  ").await.unwrap();
        assert_eq!(resolved.did, "did:plc:abc123");
    }

    #[tokio::test]
    async fn an_empty_identifier_is_refused() {
        assert!(matches!(
            resolve_identifier("   ").await,
            Err(AppError::BadRequest(_))
        ));
    }

    /// Refused on shape, before any DNS or HTTP work is attempted.
    #[tokio::test]
    async fn a_malformed_handle_is_refused_without_network_access() {
        for bad in ["not a handle", "@", "no-dot", "trailing-.", "-leading.com"] {
            assert!(
                matches!(resolve_identifier(bad).await, Err(AppError::BadRequest(_))),
                "expected {bad:?} to be refused"
            );
        }
    }

    #[tokio::test]
    async fn a_malformed_did_is_refused() {
        for bad in ["did:", "did:plc:"] {
            assert!(
                matches!(resolve_identifier(bad).await, Err(AppError::BadRequest(_))),
                "expected {bad:?} to be refused"
            );
        }
    }

    use std::collections::HashMap;

    /// Resolvers backed by fixed maps, so verification logic is tested
    /// without DNS or HTTP.
    #[allow(clippy::type_complexity)]
    fn fixtures(
        handles: &[(&str, &str)],
        docs: &[(&str, &[&str])],
    ) -> (
        impl Fn(String) -> std::future::Ready<Result<String, AppError>>,
        impl Fn(String) -> std::future::Ready<Result<Vec<String>, AppError>>,
    ) {
        let handles: HashMap<String, String> = handles
            .iter()
            .map(|(h, d)| (h.to_string(), d.to_string()))
            .collect();
        let docs: HashMap<String, Vec<String>> = docs
            .iter()
            .map(|(d, aka)| (d.to_string(), aka.iter().map(|s| s.to_string()).collect()))
            .collect();
        let resolve = move |h: String| {
            std::future::ready(
                handles
                    .get(&h)
                    .cloned()
                    .ok_or_else(|| AppError::BadRequest(format!("could not resolve handle {h}"))),
            )
        };
        let aka = move |d: String| {
            std::future::ready(
                docs.get(&d)
                    .cloned()
                    .ok_or_else(|| AppError::BadRequest(format!("no DID document for {d}"))),
            )
        };
        (resolve, aka)
    }

    #[tokio::test]
    async fn a_handle_claimed_by_its_did_document_is_verified() {
        let (resolve, aka) = fixtures(
            &[("alice.test", "did:plc:alice")],
            &[("did:plc:alice", &["at://alice.test"])],
        );
        let v = resolve_verified_with("@Alice.Test", resolve, aka)
            .await
            .unwrap();
        assert_eq!(v.did, "did:plc:alice");
        assert_eq!(v.handle.as_deref(), Some("alice.test"));
    }

    #[tokio::test]
    async fn a_handle_its_did_document_does_not_claim_is_dropped() {
        let (resolve, aka) = fixtures(
            &[("alice.test", "did:plc:alice")],
            &[("did:plc:alice", &["at://someone-else.test"])],
        );
        let v = resolve_verified_with("alice.test", resolve, aka)
            .await
            .unwrap();
        assert_eq!(v.did, "did:plc:alice");
        assert!(v.handle.is_none());
    }

    #[tokio::test]
    async fn a_did_gains_its_handle_when_the_handle_resolves_back() {
        let (resolve, aka) = fixtures(
            &[("alice.test", "did:plc:alice")],
            &[("did:plc:alice", &["at://alice.test"])],
        );
        let v = resolve_verified_with("did:plc:alice", resolve, aka)
            .await
            .unwrap();
        assert_eq!(v.handle.as_deref(), Some("alice.test"));
    }

    #[tokio::test]
    async fn a_did_whose_claimed_handle_points_elsewhere_has_no_handle() {
        let (resolve, aka) = fixtures(
            &[("alice.test", "did:plc:mallory")],
            &[("did:plc:alice", &["at://alice.test"])],
        );
        let v = resolve_verified_with("did:plc:alice", resolve, aka)
            .await
            .unwrap();
        assert_eq!(v.did, "did:plc:alice");
        assert!(v.handle.is_none());
    }

    #[tokio::test]
    async fn a_did_without_a_document_is_refused() {
        let (resolve, aka) = fixtures(&[], &[]);
        assert!(matches!(
            resolve_verified_with("did:plc:ghost", resolve, aka).await,
            Err(AppError::BadRequest(_))
        ));
    }

    #[tokio::test]
    async fn an_unresolvable_handle_is_refused() {
        let (resolve, aka) = fixtures(&[], &[]);
        assert!(matches!(
            resolve_verified_with("nobody.test", resolve, aka).await,
            Err(AppError::BadRequest(_))
        ));
    }

    #[test]
    fn did_document_failures_name_the_did_but_not_the_failure_detail() {
        use crate::admin::backfill_errors::{BackfillErrorKind, BackfillFailure};

        for kind in BackfillErrorKind::all() {
            let failure = BackfillFailure {
                kind,
                message: "error sending request for url (https://internal.example/did.json): \
                          connection refused"
                    .into(),
                retry_after: None,
            };
            let AppError::BadRequest(msg) = did_document_failure("did:web:alice.test", &failure)
            else {
                panic!("expected BadRequest for {kind:?}");
            };
            assert!(msg.contains("did:web:alice.test"), "{kind:?}: {msg}");
            assert!(!msg.contains("internal.example"), "{kind:?}: {msg}");
            assert!(!msg.contains("refused"), "{kind:?}: {msg}");
        }
    }

    #[tokio::test]
    async fn resolution_that_outlasts_its_bound_is_refused() {
        let result: Result<(), AppError> = bounded_resolution(
            " slow.test ",
            std::time::Duration::from_millis(10),
            std::future::pending(),
        )
        .await;
        let Err(AppError::BadRequest(msg)) = result else {
            panic!("expected BadRequest, got {result:?}");
        };
        assert_eq!(msg, "timed out resolving slow.test");
    }

    #[tokio::test]
    async fn verified_resolution_refuses_empty_and_malformed_input() {
        for bad in ["  ", "did:", "not a handle"] {
            let (resolve, aka) = fixtures(&[], &[]);
            assert!(
                matches!(
                    resolve_verified_with(bad, resolve, aka).await,
                    Err(AppError::BadRequest(_))
                ),
                "expected {bad:?} to be refused"
            );
        }
    }
}

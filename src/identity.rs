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

use atrium_identity::handle::{AtprotoHandleResolver, AtprotoHandleResolverConfig};

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
    let handle = atrium_api::types::string::Handle::new(handle_str.clone())
        .map_err(|_| AppError::BadRequest(format!("invalid handle: {input}")))?;

    use atrium_common::resolver::Resolver;
    let did = handle_resolver()
        .resolve(&handle)
        .await
        .map_err(|e| AppError::BadRequest(format!("could not resolve handle {handle_str}: {e}")))?;

    Ok(ResolvedIdentifier {
        did: did.as_ref().to_string(),
        handle: Some(handle_str),
    })
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
}

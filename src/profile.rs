use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::http_retry::parse_retry_after;

#[derive(Serialize)]
pub struct Profile {
    pub did: String,
    pub handle: String,
    #[serde(rename = "displayName", skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "avatarURL", skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<AvatarBlob>,
}

#[derive(Serialize)]
pub struct AvatarBlob {
    #[serde(rename = "$link")]
    pub link: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    pub size: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DidDocument {
    #[serde(default)]
    pub also_known_as: Vec<String>,
    #[serde(default)]
    pub verification_method: Vec<DidVerificationMethod>,
    #[serde(default)]
    pub service: Vec<DidService>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DidVerificationMethod {
    pub id: String,
    #[serde(rename = "type")]
    pub method_type: String,
    #[serde(default)]
    pub public_key_multibase: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DidService {
    pub id: String,
    pub service_endpoint: String,
}

#[derive(Deserialize)]
struct GetRecordResponse {
    value: serde_json::Value,
}

/// Find a service endpoint in a DID document by its fragment.
///
/// A service `id` is a DID URL, so both the relative form (`#atproto_pds`) and
/// the fully-qualified form (`did:web:example.com#atproto_pds`) are legal, and
/// both appear in the wild — `did:web` documents in particular tend to use the
/// long form. Matching the literal string `"#atproto_pds"` silently skips every
/// account that writes it the other way, so compare fragments instead.
pub fn find_service_endpoint(doc: &DidDocument, fragment: &str) -> Option<String> {
    doc.service
        .iter()
        .find(|s| s.id.rsplit_once('#').is_some_and(|(_, f)| f == fragment))
        .map(|s| s.service_endpoint.clone())
}

/// Resolve a full profile for the given DID: DID document -> handle + PDS -> profile record.
pub async fn resolve_profile(
    http: &reqwest::Client,
    plc_url: &str,
    did: &str,
) -> Result<Profile, AppError> {
    let did_doc = resolve_did_document(http, plc_url, did).await?;

    let handle = did_doc
        .also_known_as
        .iter()
        .find_map(|uri| uri.strip_prefix("at://"))
        .map(|h| h.to_string());

    let pds_endpoint = find_service_endpoint(&did_doc, "atproto_pds")
        .ok_or_else(|| AppError::NotFound("no PDS endpoint in DID document".into()))?;

    let (display_name, description, avatar_url, avatar) =
        fetch_profile_from_pds(http, &pds_endpoint, did)
            .await
            .unwrap_or((None, None, None, None));

    Ok(Profile {
        did: did.to_string(),
        handle: handle.unwrap_or_else(|| did.to_string()),
        display_name,
        description,
        avatar_url,
        avatar,
    })
}

/// Resolve the PDS endpoint for a DID by fetching its DID document.
pub async fn resolve_pds_endpoint(
    http: &reqwest::Client,
    plc_url: &str,
    did: &str,
) -> Result<String, AppError> {
    let did_doc = resolve_did_document(http, plc_url, did).await?;

    find_service_endpoint(&did_doc, "atproto_pds")
        .ok_or_else(|| AppError::NotFound("no PDS endpoint in DID document".into()))
}

/// Resolve the labeler service endpoint for a DID.
/// Tries `#atproto_labeler` first, falls back to `#atproto_pds`.
pub async fn resolve_labeler_endpoint(
    http: &reqwest::Client,
    plc_url: &str,
    did: &str,
) -> Result<String, AppError> {
    let did_doc = resolve_did_document(http, plc_url, did).await?;

    find_service_endpoint(&did_doc, "atproto_labeler")
        .or_else(|| find_service_endpoint(&did_doc, "atproto_pds"))
        .ok_or_else(|| AppError::NotFound("no labeler or PDS endpoint in DID document".into()))
}

/// Host portion of an origin URL, for cooldown keying.
pub fn host_of(url: &str) -> String {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .split('/')
        .next()
        .unwrap_or(url)
        .to_string()
}

/// The host a DID-document resolution will contact.
///
/// Used to key retry cooldowns. Every `did:plc` shares one host, so a rate
/// limit learned from one applies to all of them.
pub fn did_doc_host(plc_url: &str, did: &str) -> String {
    if let Some(domain) = did.strip_prefix("did:web:") {
        return domain.to_string();
    }
    host_of(plc_url)
}

/// One attempt at fetching a DID document. Never sleeps and never retries —
/// the caller decides what a failure means. Backfill defers; a user-facing
/// lookup uses the retrying wrapper below.
pub async fn resolve_did_document_once(
    http: &reqwest::Client,
    plc_url: &str,
    did: &str,
) -> Result<DidDocument, crate::admin::backfill_errors::BackfillFailure> {
    use crate::admin::backfill_errors::{BackfillErrorKind, BackfillFailure};

    let url = if let Some(domain) = did.strip_prefix("did:web:") {
        format!("https://{}/.well-known/did.json", domain)
    } else {
        format!("{}/{did}", plc_url.trim_end_matches('/'))
    };

    let resp = http
        .get(&url)
        .send()
        .await
        .map_err(|e| BackfillFailure::from_reqwest(&e))?;

    if !resp.status().is_success() {
        return Err(BackfillFailure::from_did_doc_response(
            resp.status(),
            resp.headers(),
        ));
    }

    resp.json().await.map_err(|e| BackfillFailure {
        kind: BackfillErrorKind::DidDocInvalid,
        message: format!("invalid DID document: {e}"),
        retry_after: None,
    })
}

/// One attempt at resolving a DID's PDS endpoint. Never sleeps and never
/// retries — see `resolve_did_document_once`.
pub async fn resolve_pds_endpoint_once(
    http: &reqwest::Client,
    plc_url: &str,
    did: &str,
) -> Result<String, crate::admin::backfill_errors::BackfillFailure> {
    use crate::admin::backfill_errors::{BackfillErrorKind, BackfillFailure};

    let doc = resolve_did_document_once(http, plc_url, did).await?;
    find_service_endpoint(&doc, "atproto_pds").ok_or(BackfillFailure {
        kind: BackfillErrorKind::DidDocInvalid,
        message: "no PDS endpoint in DID document".to_string(),
        retry_after: None,
    })
}

/// Fetch a DID document from the PLC directory or via `did:web` resolution.
pub async fn resolve_did_document(
    http: &reqwest::Client,
    plc_url: &str,
    did: &str,
) -> Result<DidDocument, AppError> {
    let url = if let Some(domain) = did.strip_prefix("did:web:") {
        format!("https://{}/.well-known/did.json", domain)
    } else {
        format!("{}/{did}", plc_url.trim_end_matches('/'))
    };

    let resp = {
        let max_retries = 5;
        let mut attempts = 0;
        loop {
            let r = http.get(&url).send().await.map_err(|e| {
                AppError::Internal(format!(
                    "DID resolution failed: {}",
                    crate::error::describe_error_chain(&e)
                ))
            })?;

            if r.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                attempts += 1;
                if attempts >= max_retries {
                    return Err(AppError::Internal(format!(
                        "DID resolution for {did} rate-limited after {max_retries} retries"
                    )));
                }
                let wait = parse_retry_after(r.headers());
                tracing::warn!(
                    did,
                    wait,
                    attempts,
                    max_retries,
                    "rate limited during DID resolution, sleeping"
                );
                tokio::time::sleep(tokio::time::Duration::from_secs(wait)).await;
                continue;
            }

            break r;
        }
    };

    if !resp.status().is_success() {
        // The status matters: a 404 means the document is genuinely gone, while
        // a 403 is usually a WAF blocking us and a 5xx is transient. Collapsing
        // them all into a bare "not found" makes a whole backfill log look like
        // one failure mode when it is several.
        return Err(AppError::NotFound(format!(
            "DID document not found for {did} ({} from {url})",
            resp.status()
        )));
    }

    resp.json()
        .await
        .map_err(|e| AppError::Internal(format!("invalid DID document: {e}")))
}

/// Fetch the `app.bsky.actor.profile` record from the user's PDS and extract
/// displayName, description, and avatar URL.
async fn fetch_profile_from_pds(
    http: &reqwest::Client,
    pds_endpoint: &str,
    did: &str,
) -> Result<
    (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<AvatarBlob>,
    ),
    AppError,
> {
    let url = format!(
        "{}/xrpc/com.atproto.repo.getRecord?repo={}&collection=app.bsky.actor.profile&rkey=self",
        pds_endpoint.trim_end_matches('/'),
        did,
    );

    let resp = http
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("PDS request failed: {e}")))?;

    if !resp.status().is_success() {
        return Ok((None, None, None, None));
    }

    let record: GetRecordResponse = resp
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("invalid PDS response: {e}")))?;

    let value = &record.value;

    let display_name = value
        .get("displayName")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let description = value
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let avatar_value = value.get("avatar");

    let avatar_url = avatar_value
        .and_then(|avatar| avatar.get("ref"))
        .and_then(|r| r.get("$link"))
        .and_then(|link| link.as_str())
        .map(|cid| {
            format!(
                "{}/xrpc/com.atproto.sync.getBlob?did={}&cid={}",
                pds_endpoint.trim_end_matches('/'),
                did,
                cid,
            )
        });

    let avatar = avatar_value.and_then(|av| {
        let link = av.get("ref")?.get("$link")?.as_str()?.to_string();
        let mime_type = av.get("mimeType")?.as_str()?.to_string();
        let size = av.get("size").and_then(|s| s.as_u64());
        Some(AvatarBlob {
            link,
            mime_type,
            size,
        })
    });

    Ok((display_name, description, avatar_url, avatar))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(json: serde_json::Value) -> DidDocument {
        serde_json::from_value(json).expect("valid DID document")
    }

    #[test]
    fn finds_pds_with_relative_service_id() {
        let d = doc(serde_json::json!({
            "id": "did:plc:abc123",
            "service": [{
                "id": "#atproto_pds",
                "type": "AtprotoPersonalDataServer",
                "serviceEndpoint": "https://pds.example.com"
            }]
        }));
        assert_eq!(
            find_service_endpoint(&d, "atproto_pds").as_deref(),
            Some("https://pds.example.com")
        );
    }

    #[test]
    fn finds_pds_with_fully_qualified_service_id() {
        // The real did:web:malpercio.dev document, which backfill was skipping
        // with "no PDS endpoint in DID document" despite the host being up and
        // serving a perfectly valid document.
        let d = doc(serde_json::json!({
            "@context": ["https://www.w3.org/ns/did/v1"],
            "alsoKnownAs": ["at://malpercio.dev"],
            "id": "did:web:malpercio.dev",
            "service": [{
                "id": "did:web:malpercio.dev#atproto_pds",
                "type": "AtprotoPersonalDataServer",
                "serviceEndpoint": "https://pds.obsign.org"
            }],
            "verificationMethod": [{
                "controller": "did:web:malpercio.dev",
                "id": "did:web:malpercio.dev#atproto",
                "publicKeyMultibase": "zDnaexXYs4UahAumuKoagoLa8pE7qUuRtD3Lp6GLsaXcZL73G",
                "type": "Multikey"
            }]
        }));
        assert_eq!(
            find_service_endpoint(&d, "atproto_pds").as_deref(),
            Some("https://pds.obsign.org")
        );
    }

    #[test]
    fn does_not_match_a_different_fragment() {
        let d = doc(serde_json::json!({
            "id": "did:web:example.com",
            "service": [{
                "id": "did:web:example.com#atproto_labeler",
                "type": "AtprotoLabeler",
                "serviceEndpoint": "https://labeler.example.com"
            }]
        }));
        assert_eq!(find_service_endpoint(&d, "atproto_pds"), None);
        assert_eq!(
            find_service_endpoint(&d, "atproto_labeler").as_deref(),
            Some("https://labeler.example.com")
        );
    }

    #[test]
    fn does_not_match_a_fragment_suffix() {
        // `ends_with("#atproto_pds")` would be enough for the real-world cases,
        // but a fragment is the whole thing after the last `#` — don't let a
        // longer fragment that merely ends the same way match.
        let d = doc(serde_json::json!({
            "id": "did:web:example.com",
            "service": [{
                "id": "did:web:example.com#not_atproto_pds",
                "type": "Other",
                "serviceEndpoint": "https://wrong.example.com"
            }]
        }));
        assert_eq!(find_service_endpoint(&d, "atproto_pds"), None);
    }

    #[test]
    fn ignores_a_service_id_with_no_fragment() {
        let d = doc(serde_json::json!({
            "id": "did:web:example.com",
            "service": [{
                "id": "atproto_pds",
                "type": "AtprotoPersonalDataServer",
                "serviceEndpoint": "https://wrong.example.com"
            }]
        }));
        assert_eq!(find_service_endpoint(&d, "atproto_pds"), None);
    }

    #[test]
    fn missing_service_array_yields_none() {
        let d = doc(serde_json::json!({ "id": "did:plc:abc123" }));
        assert_eq!(find_service_endpoint(&d, "atproto_pds"), None);
    }

    #[test]
    fn did_web_host_is_the_domain_itself() {
        assert_eq!(
            did_doc_host("https://plc.directory", "did:web:example.com"),
            "example.com"
        );
    }

    #[test]
    fn did_plc_host_is_the_plc_directory() {
        // Every did:plc resolution shares one host, which is exactly why the
        // cooldown must be keyed this way.
        assert_eq!(
            did_doc_host("https://plc.directory", "did:plc:abc123"),
            "plc.directory"
        );
    }

    #[test]
    fn did_web_host_strips_a_port_free_url_safely() {
        assert_eq!(
            did_doc_host("https://plc.directory", "did:web:sub.example.com"),
            "sub.example.com"
        );
    }
}

use hickory_resolver::Resolver;
use hickory_resolver::proto::rr::RData;

use crate::error::AppError;
use crate::profile::resolve_pds_endpoint;

/// Resolve the authority DID and PDS endpoint for the given NSID.
///
/// 1. Extract the authority from the NSID (all segments except the last).
/// 2. Reverse the authority segments to form a domain name.
/// 3. Look up `_lexicon.{domain}` TXT record for a `did=<DID>` value.
/// 4. Resolve the DID → PDS endpoint via the PLC directory.
pub async fn resolve_nsid_authority(
    http: &reqwest::Client,
    plc_url: &str,
    nsid: &str,
) -> Result<(String, String), AppError> {
    let domain =
        happyview_nsid::nsid_authority(nsid).map_err(|e| AppError::BadRequest(e.to_string()))?;

    let lookup_name = format!("_lexicon.{domain}.");

    let resolver = Resolver::builder_tokio()
        .map_err(|e| AppError::Internal(format!("failed to create DNS resolver: {e}")))?
        .build()
        .map_err(|e| AppError::Internal(format!("failed to build DNS resolver: {e}")))?;

    let txt_lookup = resolver.txt_lookup(&lookup_name).await.map_err(|e| {
        AppError::BadRequest(format!("DNS TXT lookup failed for {lookup_name}: {e}"))
    })?;

    let did = txt_lookup
        .answers()
        .iter()
        .filter_map(|r| match &r.data {
            RData::TXT(txt) => Some(txt),
            _ => None,
        })
        .flat_map(|txt| txt.txt_data.iter())
        .filter_map(|data| {
            let s = std::str::from_utf8(data).ok()?;
            s.strip_prefix("did=")
        })
        .next()
        .ok_or_else(|| AppError::BadRequest(format!("no did= TXT record found at {lookup_name}")))?
        .to_string();

    let pds_endpoint = resolve_pds_endpoint(http, plc_url, &did).await?;

    Ok((did, pds_endpoint))
}

/// Fetch a lexicon record from a PDS.
///
/// Calls `com.atproto.repo.getRecord` with collection `com.atproto.lexicon.schema`
/// and the NSID as the rkey. Returns the `value` field (the raw lexicon JSON).
pub async fn fetch_lexicon_from_pds(
    http: &reqwest::Client,
    pds_endpoint: &str,
    did: &str,
    nsid: &str,
) -> Result<serde_json::Value, AppError> {
    let url = format!(
        "{}/xrpc/com.atproto.repo.getRecord?repo={}&collection=com.atproto.lexicon.schema&rkey={}",
        pds_endpoint.trim_end_matches('/'),
        did,
        nsid,
    );

    let resp = http
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("PDS request failed: {e}")))?;

    if !resp.status().is_success() {
        return Err(AppError::NotFound(format!(
            "lexicon record not found for {nsid} in {did}'s repo"
        )));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("invalid PDS response: {e}")))?;

    body.get("value")
        .cloned()
        .ok_or_else(|| AppError::Internal("PDS response missing 'value' field".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nsid_too_few_segments_is_error() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let http = reqwest::Client::new();
        let result = rt.block_on(resolve_nsid_authority(&http, "http://localhost", "single"));
        assert!(result.is_err());
    }

    #[test]
    fn nsid_with_digit_leading_authority_resolves_domain() {
        // Regression guard: this NSID must reach DNS lookup rather than being
        // rejected as malformed.
        assert_eq!(
            happyview_nsid::nsid_authority("pics.2bit.feed.getPhotos").unwrap(),
            "feed.2bit.pics"
        );
    }
}

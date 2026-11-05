//! HTTP helpers shared by HappyView and its sources.

/// Parse rate-limit sleep duration from response headers.
/// Checks `RateLimit-Reset` (Unix timestamp, used by XRPC servers) first,
/// then `retry-after` (seconds), defaulting to 5s.
pub fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> u64 {
    if let Some(reset) = headers
        .get("ratelimit-reset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let wait = (reset - now).max(1) as u64;
        return wait.min(120);
    }

    headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(5)
        .min(120)
}

/// `response`'s body, refused as soon as it is known to exceed `cap` bytes:
/// up front from its `Content-Length`, or as its chunks arrive.
pub async fn read_capped(mut response: reqwest::Response, cap: usize) -> Result<Vec<u8>, String> {
    let too_large = |len: u64| format!("the response is over {cap} bytes ({len} or more)");
    if let Some(len) = response.content_length()
        && len > cap as u64
    {
        return Err(too_large(len));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if bytes.len() + chunk.len() > cap {
            return Err(too_large((bytes.len() + chunk.len()) as u64));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Render an error together with its full `source()` chain.
///
/// `reqwest::Error`'s `Display` deliberately omits its source, so a transport
/// failure formats as nothing more than `error sending request for url (…)` —
/// leaving NXDOMAIN, TLS failure, connection refused and timeout completely
/// indistinguishable in the logs. That is exactly the information needed to
/// tell "this host is gone" from "we are misconfigured", so walk the chain and
/// append each cause.
pub fn describe_error_chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        // Chains frequently restate the outer message; don't repeat it.
        if !out.ends_with(&text) {
            out.push_str(": ");
            out.push_str(&text);
        }
        source = cause.source();
    }
    out
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

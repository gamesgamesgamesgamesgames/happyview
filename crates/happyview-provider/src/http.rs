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

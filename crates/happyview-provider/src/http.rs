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

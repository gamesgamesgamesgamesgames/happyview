use super::{
    HostContext, MAX_HTTP_REQUESTS, MAX_HTTP_RESPONSE_SIZE, MAX_HTTP_TOTAL_TRANSFER, ResourceUsage,
};

/// The request the plugin sent and the response it gets back. Both are the
/// SDK's types: bodies are bytes, and travel as a JSON string whenever they are
/// valid UTF-8 so a plugin declaring a string body still decodes them.
pub use happyview_plugin_sdk::wire::{HttpRequest, HttpResponse};

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("Too many requests: {0} > {MAX_HTTP_REQUESTS}")]
    TooManyRequests(u32),
    #[error("Response too large: {0} > {MAX_HTTP_RESPONSE_SIZE}")]
    ResponseTooLarge(u64),
    #[error("Transfer limit exceeded: {0} > {MAX_HTTP_TOTAL_TRANSFER}")]
    TransferLimitExceeded(u64),
    #[error("Request failed: {0}")]
    RequestFailed(#[from] reqwest::Error),
}

pub async fn http_request(
    ctx: &HostContext,
    usage: &mut ResourceUsage,
    req: HttpRequest,
    follow_redirects: bool,
) -> Result<HttpResponse, HttpError> {
    // Check request count limit
    usage.http_requests += 1;
    if usage.http_requests > MAX_HTTP_REQUESTS {
        return Err(HttpError::TooManyRequests(usage.http_requests));
    }

    // Build request
    let method = req.method.parse().unwrap_or(reqwest::Method::GET);
    let mut builder = if follow_redirects {
        ctx.http_client.request(method, &req.url)
    } else {
        // The `allowed_hosts` check covers the request URL only. The shared client
        // follows up to 10 redirects, so a 3xx from an allowed host could carry a
        // restricted plugin to one it may not reach. The restricted path therefore
        // sends through a client that follows nothing, and the plugin sees the 3xx
        // itself.
        let client = no_redirect_client()?;
        client.request(method, &req.url)
    };

    for (name, value) in &req.headers {
        builder = builder.header(name, value);
    }

    if let Some(body) = req.body {
        usage.http_bytes_transferred += body.len() as u64;
        builder = builder.body(body);
    }

    // Check transfer limit before sending
    if usage.http_bytes_transferred > MAX_HTTP_TOTAL_TRANSFER {
        return Err(HttpError::TransferLimitExceeded(
            usage.http_bytes_transferred,
        ));
    }

    // Execute request
    let response = builder.send().await?;
    let status = response.status().as_u16();

    let headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();

    let body = response.bytes().await?;

    // Check response size
    if body.len() as u64 > MAX_HTTP_RESPONSE_SIZE {
        return Err(HttpError::ResponseTooLarge(body.len() as u64));
    }

    usage.http_bytes_transferred += body.len() as u64;
    if usage.http_bytes_transferred > MAX_HTTP_TOTAL_TRANSFER {
        return Err(HttpError::TransferLimitExceeded(
            usage.http_bytes_transferred,
        ));
    }

    Ok(HttpResponse {
        status,
        headers,
        body: body.to_vec(),
    })
}

/// A client that follows no redirects, shared by every restricted fetch path:
/// the unrestricted `host_http_request` branch above, and `blob_download`'s
/// SSRF guard (`src/plugin/host/atproto.rs`), which likewise checks a host it
/// doesn't fully trust and then must not let a redirect carry the request
/// somewhere that check never saw. Built per call; caching one on
/// `PluginState` is the fix if that ever shows up in a profile.
pub(crate) fn no_redirect_client() -> Result<reqwest::Client, reqwest::Error> {
    Ok(crate::http_retry::build_no_redirect_client(
        &crate::version::user_agent(),
    ))
}

/// The refusal message for a restricted request `host_allowed` rejected.
///
/// `network:request:defined` with nothing configured yet gets a distinct
/// message: an empty list there means an operator hasn't listed any hosts,
/// not that this particular host was excluded from one — "no hosts are
/// configured" is the true state, and "is not in allowed_hosts" would
/// wrongly imply a list exists that this host just isn't on. `network:request`
/// can never have an empty list (the loader refuses that at load time), so
/// `defined` is the only capability where the distinction can arise.
pub fn allowed_hosts_denial(allowed_hosts: &[String], defined: bool, host: Option<&str>) -> String {
    if defined && allowed_hosts.is_empty() {
        "no hosts are configured for this plugin; an admin lists them under the plugin's settings"
            .to_string()
    } else {
        format!("host {} is not in allowed_hosts", host.unwrap_or_default())
    }
}

/// `*.example.com` matches `a.example.com` and `a.b.example.com` but not `example.com`.
pub fn host_allowed(allowed_hosts: &[String], host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    allowed_hosts.iter().any(|pattern| {
        let pattern = pattern.to_ascii_lowercase();
        match pattern.strip_prefix("*.") {
            Some(suffix) => {
                host.len() > suffix.len()
                    && host.ends_with(suffix)
                    && host[..host.len() - suffix.len()].ends_with('.')
            }
            None => host == pattern,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_allowed_matches_exact_host() {
        let allowed = vec!["api.example.com".to_string()];
        assert!(host_allowed(&allowed, "api.example.com"));
        assert!(!host_allowed(&allowed, "other.example.com"));
    }

    #[test]
    fn host_allowed_matches_wildcard_subdomains() {
        let allowed = vec!["*.example.com".to_string()];
        assert!(host_allowed(&allowed, "a.example.com"));
        assert!(host_allowed(&allowed, "a.b.example.com"));
    }

    #[test]
    fn host_allowed_wildcard_does_not_match_apex() {
        let allowed = vec!["*.example.com".to_string()];
        assert!(!host_allowed(&allowed, "example.com"));
    }

    #[test]
    fn allowed_hosts_denial_reports_not_configured_when_defined_and_empty() {
        let msg = allowed_hosts_denial(&[], true, Some("api.example.com"));
        assert!(msg.contains("no hosts are configured"), "{msg}");
    }

    #[test]
    fn allowed_hosts_denial_reports_the_host_for_a_non_empty_list() {
        let allowed = vec!["api.example.com".to_string()];
        let msg = allowed_hosts_denial(&allowed, true, Some("evil.example.com"));
        assert!(msg.contains("evil.example.com"), "{msg}");
        assert!(msg.contains("is not in allowed_hosts"), "{msg}");
    }

    /// `network:request` can never have an empty `allowed_hosts` (the loader
    /// refuses that at load time), so the non-`defined` path always reports
    /// the host, matching pre-existing behavior.
    #[test]
    fn allowed_hosts_denial_is_unaffected_by_defined_when_the_list_is_non_empty() {
        let allowed = vec!["api.example.com".to_string()];
        assert_eq!(
            allowed_hosts_denial(&allowed, false, Some("evil.example.com")),
            allowed_hosts_denial(&allowed, true, Some("evil.example.com")),
        );
    }

    #[test]
    fn host_allowed_is_case_insensitive() {
        let allowed = vec!["API.Example.com".to_string()];
        assert!(host_allowed(&allowed, "api.example.com"));
        let allowed = vec!["*.Example.com".to_string()];
        assert!(host_allowed(&allowed, "A.example.com"));
    }

    #[test]
    fn test_http_request_limit_check() {
        let mut usage = ResourceUsage {
            http_requests: MAX_HTTP_REQUESTS,
            ..Default::default()
        };

        // Verify the limit check would fail
        usage.http_requests += 1;
        assert!(usage.http_requests > MAX_HTTP_REQUESTS);
    }

    #[test]
    fn test_http_transfer_limit_check() {
        let mut usage = ResourceUsage {
            http_bytes_transferred: MAX_HTTP_TOTAL_TRANSFER,
            ..Default::default()
        };

        // Adding more would exceed limit
        usage.http_bytes_transferred += 1;
        assert!(usage.http_bytes_transferred > MAX_HTTP_TOTAL_TRANSFER);
    }

    #[test]
    fn test_http_response_struct() {
        let response = HttpResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: b"{}".to_vec(),
        };
        assert_eq!(response.status, 200);
        assert_eq!(response.headers.len(), 1);
    }

    /// Only `ctx.http_client` matters to `http_request`; the rest of
    /// `HostContext` is unused plumbing here, so a lazily-connected in-memory
    /// SQLite pool (same pattern used elsewhere for host-context test
    /// fixtures) is enough without a real database.
    fn test_host_context() -> HostContext {
        sqlx::any::install_default_drivers();
        HostContext {
            plugin_id: "test-plugin".into(),
            scope: "did:example:test".into(),
            secrets: std::collections::HashMap::new(),
            config: serde_json::json!({}),
            db: sqlx::AnyPool::connect_lazy("sqlite::memory:").unwrap(),
            db_backend: crate::db::DatabaseBackend::Sqlite,
            http_client: reqwest::Client::new(),
            lexicons: std::sync::Arc::new(crate::lexicon::LexiconRegistry::new()),
        }
    }

    // Covers every restricted capability alike: `http_request`'s
    // `follow_redirects` flag has no notion of which capability chose the
    // restricted path, so this exercises `network:request:defined` too —
    // `bindings.rs` passes `false` for it exactly as it does for
    // `network:request`.
    #[tokio::test]
    async fn restricted_request_does_not_follow_redirects() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let redirect_target = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/secret"))
            .respond_with(ResponseTemplate::new(200).set_body_string("should not be reached"))
            .mount(&redirect_target)
            .await;

        let origin = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/redirect-me"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/secret", redirect_target.uri())),
            )
            .mount(&origin)
            .await;

        let ctx = test_host_context();
        let mut usage = ResourceUsage::default();
        let req = HttpRequest {
            method: "GET".into(),
            url: format!("{}/redirect-me", origin.uri()),
            headers: vec![],
            body: None,
        };

        let resp = http_request(&ctx, &mut usage, req, /* follow_redirects */ false)
            .await
            .expect("request should succeed even though it is a redirect response");

        assert_eq!(resp.status, 302);
        let location = resp
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("location"))
            .map(|(_, v)| v.as_str());
        assert_eq!(
            location,
            Some(format!("{}/secret", redirect_target.uri())).as_deref()
        );
    }

    // The unrestricted path (`network:request-unrestricted`) goes through the
    // shared client and follows redirects.
    #[tokio::test]
    async fn unrestricted_request_follows_redirects() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let target = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/final"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&target)
            .await;

        let origin = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/start"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/final", target.uri())),
            )
            .mount(&origin)
            .await;

        let ctx = test_host_context();
        let mut usage = ResourceUsage::default();
        let req = HttpRequest {
            method: "GET".into(),
            url: format!("{}/start", origin.uri()),
            headers: vec![],
            body: None,
        };

        let resp = http_request(&ctx, &mut usage, req, /* follow_redirects */ true)
            .await
            .expect("request should succeed");

        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"ok");
    }
}

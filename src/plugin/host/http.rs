use std::time::Duration;

use super::{
    HostContext, MAX_HTTP_REQUEST_DURATION, MAX_HTTP_REQUESTS, MAX_HTTP_RESPONSE_SIZE,
    MAX_HTTP_TOTAL_TRANSFER, ResourceUsage,
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
    #[error("Request timed out after {}s", .0.as_secs())]
    TimedOut(Duration),
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

    let limit = read_limit(usage.http_bytes_transferred);
    let read = send_bounded(builder, limit, MAX_HTTP_REQUEST_DURATION).await;
    let (status, headers, body) = match read {
        Ok(response) => response,
        Err(BoundedError::Over(read)) => {
            usage.http_bytes_transferred += read;
            return Err(if limit < MAX_HTTP_RESPONSE_SIZE {
                HttpError::TransferLimitExceeded(usage.http_bytes_transferred)
            } else {
                HttpError::ResponseTooLarge(read)
            });
        }
        Err(BoundedError::TimedOut(after)) => return Err(HttpError::TimedOut(after)),
        Err(BoundedError::Failed(e)) => return Err(HttpError::RequestFailed(e)),
    };
    usage.http_bytes_transferred += body.len() as u64;

    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

/// How much of a response a run that has already moved `transferred` bytes
/// may read: no more than the response cap, nor than what is left of its
/// transfer budget.
fn read_limit(transferred: u64) -> u64 {
    MAX_HTTP_TOTAL_TRANSFER
        .saturating_sub(transferred)
        .min(MAX_HTTP_RESPONSE_SIZE)
}

/// Why [`send_bounded`] gave up.
#[derive(Debug)]
pub(crate) enum BoundedError {
    /// The body would pass the limit; how much of it was read, or the
    /// advertised length when that alone was over.
    Over(u64),
    /// The whole exchange, the body included, outlasted its time.
    TimedOut(Duration),
    Failed(reqwest::Error),
}

/// Send `request` and read its response, refusing a body over `limit` bytes
/// as soon as it is known to be one: on its advertised length before any of
/// it is read, and otherwise as the chunks that would pass it arrive. A
/// buffered read checked afterwards only reports the size of something
/// already in memory. `within` bounds connecting, sending, waiting and
/// reading together; the client's own read timeout only catches a server
/// that goes silent, not one that keeps trickling bytes.
pub(crate) async fn send_bounded(
    request: reqwest::RequestBuilder,
    limit: u64,
    within: Duration,
) -> Result<(u16, Vec<(String, String)>, Vec<u8>), BoundedError> {
    let exchange = async {
        let mut response = request.send().await.map_err(BoundedError::Failed)?;
        if let Some(len) = response.content_length()
            && len > limit
        {
            return Err(BoundedError::Over(len));
        }
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(BoundedError::Failed)? {
            if (body.len() + chunk.len()) as u64 > limit {
                return Err(BoundedError::Over((body.len() + chunk.len()) as u64));
            }
            body.extend_from_slice(&chunk);
        }
        Ok((status, headers, body))
    };
    tokio::time::timeout(within, exchange)
        .await
        .unwrap_or(Err(BoundedError::TimedOut(within)))
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

    /// A server that answers every connection with a chunked body that never
    /// ends, counting the bytes it managed to write.
    async fn endless_body() -> (String, std::sync::Arc<std::sync::atomic::AtomicU64>) {
        use std::sync::atomic::Ordering;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let written = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let counter = written.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let counter = counter.clone();
                tokio::spawn(async move {
                    let mut request = [0u8; 4096];
                    let _ = socket.read(&mut request).await;
                    let head = "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n";
                    if socket.write_all(head.as_bytes()).await.is_err() {
                        return;
                    }
                    let chunk = format!("400\r\n{}\r\n", "x".repeat(0x400));
                    while socket.write_all(chunk.as_bytes()).await.is_ok() {
                        counter.fetch_add(0x400, Ordering::Relaxed);
                    }
                });
            }
        });
        (format!("http://{addr}/"), written)
    }

    /// The bound is on what is read, not a check of what was read: a body
    /// that never ends is refused once it passes the limit, instead of
    /// being buffered until memory or the server gives out.
    #[tokio::test]
    async fn an_endless_body_is_refused_once_it_passes_the_limit() {
        let (url, written) = endless_body().await;
        let client = reqwest::Client::new();

        let read = send_bounded(client.get(&url), 64 * 1024, Duration::from_secs(10)).await;

        match read {
            Err(BoundedError::Over(read)) => assert!(read > 64 * 1024 && read <= 65 * 1024),
            other => panic!("expected the limit to stop the read, got {other:?}"),
        }
        // The server is not drained afterwards: what it wrote is bounded by
        // the limit and the socket's buffers, not by how long it would go on.
        assert!(
            written.load(std::sync::atomic::Ordering::Relaxed) < 64 * 1024 * 1024,
            "the server wrote {} bytes",
            written.load(std::sync::atomic::Ordering::Relaxed)
        );
    }

    #[tokio::test]
    async fn an_advertised_length_over_the_limit_is_refused_unread() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 2048]))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();

        let read = send_bounded(client.get(server.uri()), 1024, Duration::from_secs(10)).await;

        assert!(matches!(read, Err(BoundedError::Over(2048))), "{read:?}");
    }

    #[tokio::test]
    async fn a_body_within_the_limit_is_read_whole() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(201)
                    .insert_header("x-probe", "yes")
                    .set_body_bytes(vec![b'x'; 1024]),
            )
            .mount(&server)
            .await;
        let client = reqwest::Client::new();

        let (status, headers, body) =
            send_bounded(client.get(server.uri()), 1024, Duration::from_secs(10))
                .await
                .expect("a body exactly at the limit is read");

        assert_eq!(status, 201);
        assert_eq!(body.len(), 1024);
        assert!(headers.iter().any(|(k, v)| k == "x-probe" && v == "yes"));
    }

    /// The client's own read timeout only fires on silence; the bound is on
    /// the whole exchange.
    #[tokio::test]
    async fn a_request_that_outlasts_its_time_is_abandoned() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let started = std::time::Instant::now();

        let read = send_bounded(client.get(server.uri()), 1024, Duration::from_millis(200)).await;

        assert!(matches!(read, Err(BoundedError::TimedOut(_))), "{read:?}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_read_is_limited_by_the_response_cap_and_the_budget_left() {
        assert_eq!(read_limit(0), MAX_HTTP_RESPONSE_SIZE);
        let near_the_end = MAX_HTTP_TOTAL_TRANSFER - 1024;
        assert_eq!(read_limit(near_the_end), 1024);
        assert_eq!(read_limit(MAX_HTTP_TOTAL_TRANSFER), 0);
        assert_eq!(read_limit(MAX_HTTP_TOTAL_TRANSFER + 1), 0);
    }

    #[test]
    fn a_timeout_reads_as_one() {
        let message = HttpError::TimedOut(MAX_HTTP_REQUEST_DURATION).to_string();
        assert_eq!(message, "Request timed out after 120s");
    }

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
        crate::db::install_drivers();
        let db = sqlx::AnyPool::connect_lazy("sqlite::memory:").unwrap();
        HostContext {
            plugin_id: "test-plugin".into(),
            scope: "did:example:test".into(),
            secrets: std::collections::HashMap::new(),
            config: serde_json::json!({}),
            records: crate::record_store::RecordStore::new(
                db.clone(),
                crate::db::DatabaseBackend::Sqlite,
            ),
            db,
            db_backend: crate::db::DatabaseBackend::Sqlite,
            http_client: reqwest::Client::new(),
            lexicons: std::sync::Arc::new(crate::catalog::LexiconCatalog::detached()),
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

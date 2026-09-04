use std::sync::OnceLock;
use std::time::Duration;

use atrium_xrpc::HttpClient;

static SHARED_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// How long to wait for TCP + TLS to a stranger's PDS before giving up.
/// A host that hasn't completed a handshake by now is not about to serve us.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Inactivity timeout applied to each read, deliberately *not* a total-request
/// timeout: the shared client also streams blobs, which can legitimately take
/// far longer than any fixed budget. This kills a connection that stops
/// producing bytes without capping how long an honest transfer may run.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Build the shared outbound HTTP client.
///
/// `reqwest::Client::new()` has *no* connect timeout and *no* read timeout, so a
/// host that black-holes packets (rather than refusing them) holds its caller
/// until the OS TCP timeout — roughly two minutes on Linux. Backfill resolves
/// up to 100 DIDs concurrently against domains that have often expired, so
/// without these the pipeline stalls on hosts that will never answer.
pub fn build_http_client(user_agent: &str) -> reqwest::Client {
    client_with_timeouts(CONNECT_TIMEOUT, READ_TIMEOUT, user_agent)
}

/// Initialise the process-wide outbound client. Call once, from `main`.
///
/// Returns the client so the caller can use it directly rather than going
/// back through `shared_client()`.
pub fn init_shared_client(user_agent: &str) -> reqwest::Client {
    let client = build_http_client(user_agent);
    let _ = SHARED_CLIENT.set(client.clone());
    client
}

/// The process-wide outbound client.
///
/// Falls back to a default-User-Agent client when `init_shared_client` was
/// never called — which happens in tests. The fallback exists so that no call
/// site can ever silently obtain a client with no timeouts: getting the
/// configured one must be the path of least resistance.
pub fn shared_client() -> &'static reqwest::Client {
    SHARED_CLIENT.get_or_init(|| build_http_client(&crate::version::user_agent()))
}

fn client_with_timeouts(connect: Duration, read: Duration, user_agent: &str) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(connect)
        .read_timeout(read)
        .user_agent(user_agent)
        .build()
        // Only fails if the TLS backend can't initialise, in which case no
        // outbound request would work anyway.
        .expect("failed to build HTTP client")
}

/// Wraps our shared, UA-and-timeout-configured `reqwest::Client` so it can
/// serve as the `atrium_xrpc::HttpClient` atrium-oauth needs. We no longer
/// enable atrium-oauth's `default-client` feature (see `Cargo.toml`), whose
/// own client type always built an unconfigured `reqwest::Client::new()` with
/// no seam to set headers or timeouts — this is what every `OAuthClient` and
/// resolver in the crate uses instead.
pub struct HappyViewHttpClient(reqwest::Client);

impl HappyViewHttpClient {
    pub fn new(client: reqwest::Client) -> Self {
        Self(client)
    }
}

impl Default for HappyViewHttpClient {
    fn default() -> Self {
        Self::new(shared_client().clone())
    }
}

impl HttpClient for HappyViewHttpClient {
    async fn send_http(
        &self,
        request: atrium_xrpc::http::Request<Vec<u8>>,
    ) -> core::result::Result<
        atrium_xrpc::http::Response<Vec<u8>>,
        Box<dyn std::error::Error + Send + Sync + 'static>,
    > {
        let response = self.0.execute(request.try_into()?).await?;
        let mut builder = atrium_xrpc::http::Response::builder().status(response.status());
        for (k, v) in response.headers() {
            builder = builder.header(k, v);
        }
        builder
            .body(response.bytes().await?.to_vec())
            .map_err(Into::into)
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_http_client_succeeds() {
        let _ = build_http_client("HappyView/test");
    }

    #[tokio::test]
    async fn shared_client_falls_back_to_default_ua_when_uninitialised() {
        // `init_shared_client` is only ever called from `main`, never from a
        // test, so within this test binary `shared_client()` always hits its
        // `get_or_init` fallback on first use — regardless of which test
        // happens to call it first, every caller converges on the same
        // fallback client. That makes this assertion safe despite
        // `SHARED_CLIENT` being a process-wide `OnceLock`.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let received = std::sync::Arc::new(tokio::sync::Mutex::new(String::new()));
        let sink = std::sync::Arc::clone(&received);
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 2048];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                *sink.lock().await = String::from_utf8_lossy(&buf[..n]).to_string();
                let _ = sock
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                    .await;
            }
        });

        let _ = shared_client().get(format!("http://{addr}/")).send().await;

        let expected_ua = crate::version::user_agent();
        let request = received.lock().await.clone();
        assert!(
            request
                .to_lowercase()
                .contains(&format!("user-agent: {}", expected_ua.to_lowercase())),
            "expected fallback UA {expected_ua:?} in request:\n{request}"
        );
    }

    #[tokio::test]
    async fn read_timeout_aborts_a_host_that_never_responds() {
        // A host that completes the TCP handshake and then goes silent is the
        // case the default client cannot escape: no read timeout means the
        // request hangs until the OS gives up.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            // Accept and hold the connection open, writing nothing.
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                held.push(sock);
            }
        });

        let client = client_with_timeouts(
            Duration::from_secs(5),
            Duration::from_millis(300),
            "HappyView/test",
        );
        let started = std::time::Instant::now();
        let result = client.get(format!("http://{addr}/")).send().await;

        assert!(result.is_err(), "expected the silent host to time out");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "read timeout did not fire; waited {:?}",
            started.elapsed()
        );
    }

    /// Drive one request from `client_with_timeouts` at a throwaway socket and
    /// hand back the raw request bytes, so header-level assertions are made
    /// against the wire and not against the builder that produced it.
    async fn capture_request(user_agent: &str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let received = std::sync::Arc::new(tokio::sync::Mutex::new(String::new()));
        let sink = std::sync::Arc::clone(&received);
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 2048];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                *sink.lock().await = String::from_utf8_lossy(&buf[..n]).to_string();
                let _ = sock
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                    .await;
            }
        });

        let client =
            client_with_timeouts(Duration::from_secs(5), Duration::from_secs(5), user_agent);
        let _ = client.get(format!("http://{addr}/")).send().await;

        received.lock().await.clone()
    }

    #[tokio::test]
    async fn client_sends_the_configured_user_agent() {
        let request = capture_request("HappyView/9.9.9 (+https://example.test)").await;
        assert!(
            request.contains("user-agent: HappyView/9.9.9 (+https://example.test)")
                || request.contains("User-Agent: HappyView/9.9.9 (+https://example.test)"),
            "UA header missing from request:\n{request}"
        );
    }

    #[tokio::test]
    async fn client_negotiates_gzip() {
        // `reqwest`'s gzip feature is what puts this header on the wire, and it
        // has already been lost once to a transitive feature-unification change
        // in an unrelated dependency — with no compile error and no log line.
        // `listRecords` is the highest-volume path in the product; this asserts
        // against the raw bytes so the next time it disappears, it is a red
        // test rather than a silent bandwidth regression.
        let request = capture_request("HappyView/test").await.to_lowercase();
        let line = request
            .lines()
            .find(|l| l.starts_with("accept-encoding:"))
            .unwrap_or_else(|| panic!("no accept-encoding header in request:\n{request}"));
        assert!(
            line.contains("gzip"),
            "accept-encoding does not offer gzip: {line}"
        );
    }
}

//! HTTP client for the Jetstream v2 archive: `planSnapshot`, `getSegment`,
//! `getBlock`. Only these requests carry `JETSTREAM_API_KEY`. Every body byte
//! received is counted, so a job can report what it downloaded (Bluesky
//! meters Replay by key; spec, "Bluesky's answer on replay metering"), and
//! charged to the shared `BytePacer`, so requests keep within the archive's
//! byte quota instead of running into its 429s.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{CONTENT_RANGE, ETAG, IF_RANGE, RANGE};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use super::ArchiveConfig;
use super::pacer::BytePacer;
use crate::jetstream::config::Endpoint;

pub const PLAN_PATH: &str = "/xrpc/network.bsky.jetstream.planSnapshot";
pub const SEGMENT_PATH: &str = "/xrpc/network.bsky.jetstream.getSegment";
pub const BLOCK_PATH: &str = "/xrpc/network.bsky.jetstream.getBlock";
pub const SEGMENT_PART: u64 = 16 << 20;
pub const MAX_SEGMENT_BYTES: u64 = 1 << 30;
pub const MAX_BLOCK_BYTES: usize = 64 << 20;
pub const MAX_GENERATION_RESTARTS: u32 = 2;
const MAX_PLAN_BYTES: usize = 64 << 20;
/// Attempts at a request the archive answers 503.
const UNAVAILABLE_ATTEMPTS: u32 = 5;
/// Attempts at a request that fails in transport or with another 5xx (but
/// 501). Backing off from one second, they span 31 s of waiting: enough for
/// an upstream blip such as a `502 upstream unavailable`.
const FAILED_ATTEMPTS: u32 = 6;
/// The longest one backoff step waits.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// The longest a 429 or 503's `Retry-After` is waited out at a time.
const MAX_THROTTLE_WAIT: u64 = 30;
/// The whole-request limit for `planSnapshot` and `getBlock`, and how long
/// any request may wait for its response headers.
const REQUEST_TIMEOUT: Duration = super::ARCHIVE_READ_TIMEOUT;
/// How long a `getSegment` body may go without delivering a chunk. A segment
/// has no whole-request limit: 1 GiB in 120 s would need 8.5 MB/s.
const SEGMENT_IDLE_TIMEOUT: Duration = super::ARCHIVE_READ_TIMEOUT;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRequest {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub collections: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dids: Vec<String>,
    pub after_seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_seq: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanPage {
    pub planned_through_seq: u64,
    pub sealed_tip_seq: u64,
    #[serde(default)]
    pub segments: Vec<PlanEntry>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanEntry {
    pub name: String,
    pub index: u64,
    pub checksum: String,
    pub min_seq: u64,
    pub max_seq: u64,
    pub mode: PlanMode,
    #[serde(default)]
    pub blocks: Vec<BlockRange>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanMode {
    Segment,
    Blocks,
}

/// Inclusive block indexes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub struct BlockRange {
    pub first: u32,
    pub last: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArchiveError {
    /// `planSnapshot` answered 404 or 501: this Jetstream has no archive.
    NoArchive(u16),
    /// `planSnapshot` answered 401 or 403: the archive wants a key (or
    /// refused the one sent).
    Unauthorized(u16),
    /// Nothing answered at all, after retries.
    Unreachable(String),
    Status {
        status: u16,
        message: String,
    },
    Transport(String),
    TooLarge(String),
    GenerationChurn(String),
    Io(String),
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoArchive(status) => write!(f, "no replay archive (HTTP {status})"),
            Self::Unauthorized(status) => {
                write!(f, "the archive refused the request (HTTP {status})")
            }
            Self::Unreachable(e) => write!(f, "the archive is unreachable: {e}"),
            Self::Status { status, message } if message.is_empty() => write!(f, "HTTP {status}"),
            Self::Status { status, message } => write!(f, "HTTP {status}: {message}"),
            Self::Transport(e) => write!(f, "{e}"),
            Self::TooLarge(e) | Self::GenerationChurn(e) | Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ArchiveError {}

fn io(e: std::io::Error) -> ArchiveError {
    ArchiveError::Io(e.to_string())
}

/// Cheap to clone; clones share the byte counter, so a job's total includes
/// what every clone downloaded.
#[derive(Clone)]
pub struct ArchiveClient {
    http: reqwest::Client,
    base: String,
    host: String,
    api_key: Option<String>,
    retry_unit: Duration,
    max_throttled_for: Duration,
    part_size: u64,
    bytes: Arc<AtomicU64>,
    pacer: Arc<BytePacer>,
}

impl ArchiveClient {
    /// Requests go through `config.http`, the archive's own client, never
    /// the shared one: its read timeout is too short for the archive.
    pub fn new(endpoint: &Endpoint, config: &ArchiveConfig) -> Self {
        Self {
            http: config.http.clone(),
            base: endpoint.http_base.clone(),
            host: endpoint.host.clone(),
            api_key: config.api_key.clone(),
            retry_unit: config.retry_unit,
            max_throttled_for: config.max_throttled_for,
            part_size: SEGMENT_PART,
            bytes: Arc::new(AtomicU64::new(0)),
            pacer: config.pacer.clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_part_size(mut self, bytes: u64) -> Self {
        self.part_size = bytes;
        self
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn bytes_downloaded(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    /// Count body bytes received, and charge them to the quota.
    fn count(&self, n: usize) {
        self.bytes.fetch_add(n as u64, Ordering::Relaxed);
        self.pacer.charge(n as u64);
    }

    fn backoff(&self, attempt: u32) -> Duration {
        backoff(self.retry_unit, attempt)
    }

    /// Send with the archive's retry policy. Every attempt first waits for
    /// the byte quota (`BytePacer`), and every answer teaches it the quota
    /// the archive advertises.
    ///
    /// - 429 is backpressure: it empties the shared bucket, so every request
    ///   waits out `Retry-After` (at most 30 s), and is retried for as long as
    ///   it takes, unless one request is throttled for longer than
    ///   `max_throttled_for` (15 minutes): then the 429 is returned.
    /// - 503 waits out `Retry-After` (at most 30 s), 5 attempts in all.
    /// - Other 5xx (but 501) and transport errors back off `retry_unit`
    ///   doubling, each step at most 30 s, 6 attempts in all.
    ///
    /// Any other answer is returned to the caller.
    ///
    /// Response headers must arrive within `REQUEST_TIMEOUT`. With
    /// `whole_request`, the body must finish within it too; `getSegment`
    /// passes `false` and bounds each body chunk instead (`stream`).
    async fn send(
        &self,
        whole_request: bool,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ArchiveError> {
        let (mut unavailable, mut failed) = (0u32, 0u32);
        let mut throttled_since: Option<tokio::time::Instant> = None;
        loop {
            self.pacer.acquire().await;
            let mut request = build();
            if whole_request {
                request = request.timeout(REQUEST_TIMEOUT);
            }
            if let Some(key) = &self.api_key {
                request = request.bearer_auth(key);
            }
            let sent = tokio::time::timeout(REQUEST_TIMEOUT, request.send()).await;
            if let Ok(Ok(response)) = &sent {
                self.pacer.learn(response.headers());
            }
            let error = match sent {
                Ok(Ok(response)) if response.status() == StatusCode::TOO_MANY_REQUESTS => {
                    let since = *throttled_since.get_or_insert_with(tokio::time::Instant::now);
                    if since.elapsed() >= self.max_throttled_for {
                        return Err(status_error(response).await);
                    }
                    // At least `retry_unit`, so `Retry-After: 0` can't spin.
                    let wait = Duration::from_secs(
                        crate::http_retry::parse_retry_after(response.headers())
                            .min(MAX_THROTTLE_WAIT),
                    )
                    .max(self.retry_unit);
                    // With a quota, the emptied bucket holds back this
                    // request and every other one; without, wait here.
                    if !self.pacer.throttled(wait) {
                        tokio::time::sleep(wait).await;
                    }
                    continue;
                }
                Ok(Ok(response)) if response.status() == StatusCode::SERVICE_UNAVAILABLE => {
                    throttled_since = None;
                    unavailable += 1;
                    if unavailable >= UNAVAILABLE_ATTEMPTS {
                        return Err(status_error(response).await);
                    }
                    let wait = crate::http_retry::parse_retry_after(response.headers())
                        .min(MAX_THROTTLE_WAIT);
                    tokio::time::sleep(Duration::from_secs(wait)).await;
                    continue;
                }
                Ok(Ok(response))
                    if response.status().is_server_error()
                        && response.status() != StatusCode::NOT_IMPLEMENTED =>
                {
                    throttled_since = None;
                    failed += 1;
                    if failed >= FAILED_ATTEMPTS {
                        return Err(status_error(response).await);
                    }
                    tokio::time::sleep(self.backoff(failed)).await;
                    continue;
                }
                Ok(Ok(response)) => return Ok(response),
                Ok(Err(e)) if e.is_connect() => ArchiveError::Unreachable(e.to_string()),
                Ok(Err(e)) => ArchiveError::Transport(e.to_string()),
                Err(_) => ArchiveError::Transport(format!(
                    "no response within {} s",
                    REQUEST_TIMEOUT.as_secs()
                )),
            };
            // A transport failure: no response, or none in time.
            throttled_since = None;
            failed += 1;
            if failed >= FAILED_ATTEMPTS {
                return Err(error);
            }
            tokio::time::sleep(self.backoff(failed)).await;
        }
    }

    /// A whole small response: send it, check its status, read its body, and
    /// retry the lot under the transport policy when the body fails to
    /// arrive. `probe` makes a 404 or 501 mean `NoArchive`, and a 401 or 403
    /// `Unauthorized`.
    async fn fetch(
        &self,
        probe: bool,
        cap: usize,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<Vec<u8>, ArchiveError> {
        let mut failed = 0u32;
        loop {
            let response = self.send(true, &build).await?;
            let status = response.status();
            if probe && matches!(status, StatusCode::NOT_FOUND | StatusCode::NOT_IMPLEMENTED) {
                return Err(ArchiveError::NoArchive(status.as_u16()));
            }
            if probe && matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
                return Err(ArchiveError::Unauthorized(status.as_u16()));
            }
            if !status.is_success() {
                return Err(status_error(response).await);
            }
            match read_body(response, cap).await {
                Ok(body) => {
                    self.count(body.len());
                    return Ok(body);
                }
                Err(e @ ArchiveError::Transport(_)) => {
                    failed += 1;
                    if failed >= FAILED_ATTEMPTS {
                        return Err(e);
                    }
                    tokio::time::sleep(self.backoff(failed)).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub async fn plan(&self, request: &PlanRequest) -> Result<PlanPage, ArchiveError> {
        let url = format!("{}{PLAN_PATH}", self.base);
        let body = self
            .fetch(true, MAX_PLAN_BYTES, || self.http.post(&url).json(request))
            .await?;
        serde_json::from_slice(&body).map_err(|e| {
            ArchiveError::Transport(format!("planSnapshot answered unreadable JSON: {e}"))
        })
    }

    /// One block's zstd frame.
    pub async fn block(&self, segment: &str, index: u32) -> Result<Vec<u8>, ArchiveError> {
        let url = format!("{}{BLOCK_PATH}", self.base);
        let index = index.to_string();
        self.fetch(false, MAX_BLOCK_BYTES, || {
            self.http
                .get(&url)
                .query(&[("segment", segment), ("blockIndex", index.as_str())])
        })
        .await
    }

    /// Download segment `name` into `dest` (created or truncated) in
    /// `SEGMENT_PART` ranges pinned to the first ETag with `If-Range`. A 200
    /// to a ranged request means the server ignores ranges or the segment was
    /// compacted to a new generation: either way the whole body follows, and
    /// replaces what was written. More than `MAX_GENERATION_RESTARTS` of those
    /// mid-download fails. A first part with no ETag can't be pinned, so the
    /// segment is fetched whole instead. A body that breaks mid-part is
    /// re-requested from the last byte written, within the transport retry
    /// budget. Returns the segment's size.
    pub async fn segment(&self, name: &str, dest: &Path) -> Result<u64, ArchiveError> {
        let url = format!("{}{SEGMENT_PATH}", self.base);
        let mut file = tokio::fs::File::create(dest).await.map_err(io)?;
        let mut restarts = 0u32;
        let mut body_failures = 0u32;
        let mut etag: Option<String> = None;
        let mut written = 0u64;
        let mut unranged = false;
        loop {
            let range = format!("bytes={written}-{}", written + self.part_size - 1);
            let pinned = etag.clone();
            let response = self
                .send(false, || {
                    let request = self.http.get(&url).query(&[("name", name)]);
                    if unranged {
                        return request;
                    }
                    let request = request.header(RANGE, &range);
                    match &pinned {
                        Some(tag) => request.header(IF_RANGE, tag),
                        None => request,
                    }
                })
                .await?;
            let tag = response
                .headers()
                .get(ETAG)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let mut progress = 0u64;
            let outcome = match response.status() {
                StatusCode::OK => {
                    if written > 0 {
                        restarts += 1;
                        if restarts > MAX_GENERATION_RESTARTS {
                            return Err(churn(name));
                        }
                    }
                    if let Some(len) = response.content_length()
                        && len > MAX_SEGMENT_BYTES
                    {
                        return Err(too_large(name, len));
                    }
                    // `File::create` truncates what the earlier parts wrote.
                    file = tokio::fs::File::create(dest).await.map_err(io)?;
                    written = 0;
                    etag = None;
                    match self
                        .stream(response, &mut file, MAX_SEGMENT_BYTES, name, &mut progress)
                        .await
                    {
                        Ok(()) => {
                            file.flush().await.map_err(io)?;
                            return Ok(progress);
                        }
                        Err(e) => e,
                    }
                }
                StatusCode::PARTIAL_CONTENT if unranged => {
                    return Err(ArchiveError::Status {
                        status: 206,
                        message: format!("getSegment {name} answered 206 to an unranged request"),
                    });
                }
                StatusCode::PARTIAL_CONTENT => {
                    if etag.is_none() && tag.is_none() {
                        // Nothing to pin the parts to: a compaction mid-way
                        // would splice generations. Take it whole.
                        unranged = true;
                        continue;
                    }
                    if etag.is_some() && tag != etag {
                        restarts += 1;
                        if restarts > MAX_GENERATION_RESTARTS {
                            return Err(churn(name));
                        }
                        file = tokio::fs::File::create(dest).await.map_err(io)?;
                        written = 0;
                        etag = None;
                        continue;
                    }
                    let (start, end, total) =
                        content_range(&response).ok_or_else(|| ArchiveError::Status {
                            status: 206,
                            message: format!("getSegment {name} sent no usable Content-Range"),
                        })?;
                    if total > MAX_SEGMENT_BYTES {
                        return Err(too_large(name, total));
                    }
                    if start != written || end < start || end >= total {
                        return Err(ArchiveError::Status {
                            status: 206,
                            message: format!(
                                "getSegment {name} answered bytes {start}-{end}/{total}, wanted from {written}"
                            ),
                        });
                    }
                    etag = tag;
                    let result = self
                        .stream(
                            response,
                            &mut file,
                            MAX_SEGMENT_BYTES - written,
                            name,
                            &mut progress,
                        )
                        .await;
                    written += progress;
                    match result {
                        Ok(()) if progress == end - start + 1 => {
                            if written >= total {
                                file.flush().await.map_err(io)?;
                                return Ok(written);
                            }
                            // The retry budget is per part: a long download
                            // over a flaky link must not spend it all.
                            body_failures = 0;
                            continue;
                        }
                        // A clean end that delivered other than the promised
                        // span would otherwise re-request forever.
                        Ok(()) => {
                            return Err(ArchiveError::Status {
                                status: 206,
                                message: format!(
                                    "getSegment {name} sent {progress} bytes for bytes {start}-{end}"
                                ),
                            });
                        }
                        Err(e) => e,
                    }
                }
                _ => return Err(status_error(response).await),
            };
            // The body broke. Only transport failures are worth another try.
            if !matches!(outcome, ArchiveError::Transport(_)) {
                return Err(outcome);
            }
            body_failures += 1;
            if body_failures >= FAILED_ATTEMPTS {
                return Err(outcome);
            }
            tokio::time::sleep(self.backoff(body_failures)).await;
        }
    }

    /// Append `response`'s body to `file`, counting bytes in `progress` as
    /// they land so a failure still says how far it got.
    async fn stream(
        &self,
        mut response: reqwest::Response,
        file: &mut tokio::fs::File,
        cap: u64,
        name: &str,
        progress: &mut u64,
    ) -> Result<(), ArchiveError> {
        loop {
            let chunk = match tokio::time::timeout(SEGMENT_IDLE_TIMEOUT, response.chunk()).await {
                Ok(chunk) => chunk.map_err(|e| ArchiveError::Transport(e.to_string()))?,
                Err(_) => {
                    return Err(ArchiveError::Transport(format!(
                        "getSegment {name} sent nothing for {} s",
                        SEGMENT_IDLE_TIMEOUT.as_secs()
                    )));
                }
            };
            let Some(chunk) = chunk else { break };
            if *progress + chunk.len() as u64 > cap {
                return Err(too_large(name, *progress + chunk.len() as u64));
            }
            self.count(chunk.len());
            file.write_all(&chunk).await.map_err(io)?;
            *progress += chunk.len() as u64;
        }
        Ok(())
    }
}

/// `response`'s body, refused once it passes `cap` bytes. A broken body is
/// `Transport` (retryable); only an over-cap one is `TooLarge`.
async fn read_body(mut response: reqwest::Response, cap: usize) -> Result<Vec<u8>, ArchiveError> {
    let too_large = |len: u64| {
        ArchiveError::TooLarge(format!("the response is over {cap} bytes ({len} or more)"))
    };
    if let Some(len) = response.content_length()
        && len > cap as u64
    {
        return Err(too_large(len));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| ArchiveError::Transport(e.to_string()))?
    {
        if bytes.len() + chunk.len() > cap {
            return Err(too_large((bytes.len() + chunk.len()) as u64));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn churn(name: &str) -> ArchiveError {
    ArchiveError::GenerationChurn(format!(
        "segment {name} changed more than {MAX_GENERATION_RESTARTS} times while downloading"
    ))
}

fn too_large(name: &str, len: u64) -> ArchiveError {
    ArchiveError::TooLarge(format!(
        "segment {name} is {len} bytes, over the {MAX_SEGMENT_BYTES}-byte cap"
    ))
}

/// The wait before retry `attempt` (from 1): `unit` doubling, at most
/// `MAX_BACKOFF`.
fn backoff(unit: Duration, attempt: u32) -> Duration {
    unit.saturating_mul(2u32.saturating_pow(attempt - 1))
        .min(MAX_BACKOFF)
}

/// `bytes <start>-<end>/<total>`.
fn content_range(response: &reqwest::Response) -> Option<(u64, u64, u64)> {
    let value = response.headers().get(CONTENT_RANGE)?.to_str().ok()?;
    let (span, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = span.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}

async fn status_error(response: reqwest::Response) -> ArchiveError {
    let status = response.status().as_u16();
    let body = crate::jetstream::compression::read_capped(response, 64 << 10)
        .await
        .unwrap_or_default();
    ArchiveError::Status {
        status,
        message: String::from_utf8_lossy(&body).trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    fn client(server: &MockServer, key: Option<&str>) -> ArchiveClient {
        let endpoint = crate::jetstream::config::parse_endpoint(&server.uri());
        let config = ArchiveConfig {
            api_key: key.map(str::to_string),
            retry_unit: Duration::from_millis(5),
            ..ArchiveConfig::default()
        };
        ArchiveClient::new(&endpoint, &config)
    }

    fn page() -> serde_json::Value {
        serde_json::json!({
            "plannedThroughSeq": 300,
            "sealedTipSeq": 900,
            "segments": [
                { "name": "seg_0000000001.jss", "index": 1, "checksum": "00112233aabbccdd",
                  "minSeq": 1, "maxSeq": 200, "mode": "segment" },
                { "name": "seg_0000000002.jss", "index": 2, "checksum": "0011223344556677",
                  "minSeq": 201, "maxSeq": 300, "mode": "blocks",
                  "blocks": [{ "first": 0, "last": 1 }, { "first": 4, "last": 4 }] }
            ],
            "stats": { "segmentsExamined": 2, "segmentsMatched": 2, "blocksMatched": 5, "entries": 2 }
        })
    }

    #[tokio::test]
    async fn plan_posts_its_filters_and_parses_the_page() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(PLAN_PATH))
            .and(body_json(serde_json::json!({
                "kinds": ["commit"], "collections": ["app.test.post"], "afterSeq": 0
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(page()))
            .expect(1)
            .mount(&server)
            .await;
        let plan = client(&server, None)
            .plan(&PlanRequest {
                kinds: vec!["commit".into()],
                collections: vec!["app.test.post".into()],
                ..PlanRequest::default()
            })
            .await
            .unwrap();
        assert_eq!((plan.planned_through_seq, plan.sealed_tip_seq), (300, 900));
        assert_eq!(plan.segments[0].mode, PlanMode::Segment);
        assert!(plan.segments[0].blocks.is_empty());
        assert_eq!(
            plan.segments[1].blocks,
            vec![
                BlockRange { first: 0, last: 1 },
                BlockRange { first: 4, last: 4 },
            ]
        );
    }

    #[tokio::test]
    async fn a_pinned_tip_and_dids_are_sent() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(PLAN_PATH))
            .and(body_json(serde_json::json!({
                "kinds": ["account", "sync"], "dids": ["did:plc:a"], "afterSeq": 300, "beforeSeq": 900
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(page()))
            .expect(1)
            .mount(&server)
            .await;
        client(&server, None)
            .plan(&PlanRequest {
                kinds: vec!["account".into(), "sync".into()],
                dids: vec!["did:plc:a".into()],
                after_seq: 300,
                before_seq: Some(900),
                ..PlanRequest::default()
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_404_or_501_plan_means_no_archive() {
        for status in [404, 501] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path(PLAN_PATH))
                .respond_with(ResponseTemplate::new(status))
                .expect(1)
                .mount(&server)
                .await;
            assert_eq!(
                client(&server, None).plan(&PlanRequest::default()).await,
                Err(ArchiveError::NoArchive(status))
            );
        }
    }

    #[tokio::test]
    async fn a_401_or_403_plan_is_unauthorized() {
        for status in [401, 403] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path(PLAN_PATH))
                .respond_with(ResponseTemplate::new(status))
                .expect(1)
                .mount(&server)
                .await;
            assert_eq!(
                client(&server, None).plan(&PlanRequest::default()).await,
                Err(ArchiveError::Unauthorized(status))
            );
        }
    }

    #[tokio::test]
    async fn nothing_listening_is_unreachable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let endpoint = crate::jetstream::config::parse_endpoint(&format!("http://{addr}"));
        let config = ArchiveConfig {
            retry_unit: Duration::from_millis(1),
            ..ArchiveConfig::default()
        };
        let client = ArchiveClient::new(&endpoint, &config);
        assert!(matches!(
            client.plan(&PlanRequest::default()).await,
            Err(ArchiveError::Unreachable(_))
        ));
    }

    #[tokio::test]
    async fn throttling_waits_out_retry_after_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(PLAN_PATH))
            .respond_with(ResponseTemplate::new(503).insert_header("Retry-After", "0"))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(PLAN_PATH))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(PLAN_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(page()))
            .mount(&server)
            .await;
        assert!(
            client(&server, None)
                .plan(&PlanRequest::default())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn throttling_is_waited_out_however_many_times_it_takes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(PLAN_PATH))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
            .up_to_n_times(8)
            .expect(8)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(PLAN_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(page()))
            .expect(1)
            .mount(&server)
            .await;
        assert!(
            client(&server, None)
                .plan(&PlanRequest::default())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn throttling_that_outlasts_the_bound_fails_with_the_429() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(BLOCK_PATH))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("Retry-After", "0")
                    .set_body_json(serde_json::json!({ "error": "byte limit exceeded" })),
            )
            .mount(&server)
            .await;
        let endpoint = crate::jetstream::config::parse_endpoint(&server.uri());
        let config = ArchiveConfig {
            retry_unit: Duration::from_millis(5),
            max_throttled_for: Duration::from_millis(50),
            ..ArchiveConfig::default()
        };
        let Err(ArchiveError::Status {
            status: 429,
            message,
        }) = ArchiveClient::new(&endpoint, &config).block("seg", 0).await
        else {
            panic!("a 429");
        };
        assert!(message.contains("byte limit exceeded"), "{message}");
    }

    #[tokio::test]
    async fn unavailability_gives_up_after_five_attempts() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(PLAN_PATH))
            .respond_with(ResponseTemplate::new(503).insert_header("Retry-After", "0"))
            .expect(5)
            .mount(&server)
            .await;
        assert!(matches!(
            client(&server, None).plan(&PlanRequest::default()).await,
            Err(ArchiveError::Status { status: 503, .. })
        ));
    }

    #[tokio::test]
    async fn a_plan_survives_an_upstream_blip() {
        for failures in [2, 4] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path(PLAN_PATH))
                .respond_with(
                    ResponseTemplate::new(502)
                        .set_body_json(serde_json::json!({ "error": "upstream unavailable" })),
                )
                .up_to_n_times(failures)
                .expect(failures)
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path(PLAN_PATH))
                .respond_with(ResponseTemplate::new(200).set_body_json(page()))
                .expect(1)
                .mount(&server)
                .await;
            assert!(
                client(&server, None)
                    .plan(&PlanRequest::default())
                    .await
                    .is_ok(),
                "{failures} 502s"
            );
        }
    }

    #[test]
    fn backoff_doubles_up_to_thirty_seconds() {
        let unit = Duration::from_secs(1);
        let steps: Vec<_> = (1..=7).map(|n| backoff(unit, n).as_secs()).collect();
        assert_eq!(steps, vec![1, 2, 4, 8, 16, 30, 30]);
    }

    /// The headers Bluesky's archive sends on every response.
    fn quota(response: ResponseTemplate, burst: &str, refill: &str) -> ResponseTemplate {
        response
            .insert_header("headwind-quota-burst-bytes", burst)
            .insert_header("headwind-quota-refill-bytes", refill)
            .insert_header("headwind-quota-refill-period-seconds", "1")
    }

    #[tokio::test]
    async fn the_archive_quota_is_learned_and_charged() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(PLAN_PATH))
            .respond_with(quota(
                ResponseTemplate::new(200).set_body_json(page()),
                "1000000000",
                "2000000",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(BLOCK_PATH))
            .respond_with(quota(
                ResponseTemplate::new(200).set_body_bytes(vec![7u8; 1500]),
                "1000",
                "100000",
            ))
            .mount(&server)
            .await;
        let endpoint = crate::jetstream::config::parse_endpoint(&server.uri());
        let config = ArchiveConfig::default();
        let client = ArchiveClient::new(&endpoint, &config);
        client.plan(&PlanRequest::default()).await.unwrap();
        assert_eq!(
            config.pacer.quota(),
            Some(super::super::pacer::Quota {
                burst_bytes: 1e9,
                refill_per_sec: 2e6,
            })
        );
        // A second client of the same config shares the bucket: its block
        // overspends a 1000-byte burst by 500, which at 100 kB/s takes 5 ms
        // to repay.
        let other = ArchiveClient::new(&endpoint, &config);
        other.block("seg", 0).await.unwrap();
        assert_eq!(config.pacer.quota().map(|q| q.burst_bytes), Some(1000.0));
        let start = std::time::Instant::now();
        config.pacer.acquire().await;
        assert!(
            start.elapsed() >= Duration::from_millis(4),
            "{:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn server_errors_retry_and_client_errors_do_not() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(BLOCK_PATH))
            .and(query_param("blockIndex", "1"))
            .respond_with(ResponseTemplate::new(500))
            .expect(u64::from(FAILED_ATTEMPTS))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(BLOCK_PATH))
            .and(query_param("blockIndex", "2"))
            .respond_with(ResponseTemplate::new(400).set_body_json(
                serde_json::json!({ "error": "BlockNotFound", "message": "no block 2" }),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let client = client(&server, None);
        assert!(matches!(
            client.block("seg", 1).await,
            Err(ArchiveError::Status { status: 500, .. })
        ));
        let Err(ArchiveError::Status {
            status: 400,
            message,
        }) = client.block("seg", 2).await
        else {
            panic!("a 400");
        };
        assert!(message.contains("BlockNotFound"), "{message}");
    }

    #[tokio::test]
    async fn a_block_is_its_body_and_is_counted() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(BLOCK_PATH))
            .and(query_param("segment", "seg_0000000002.jss"))
            .and(query_param("blockIndex", "4"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![7u8; 1000]))
            .mount(&server)
            .await;
        let client = client(&server, None);
        assert_eq!(
            client.block("seg_0000000002.jss", 4).await.unwrap(),
            vec![7u8; 1000]
        );
        assert_eq!(client.bytes_downloaded(), 1000);
    }

    /// Serves `generations[n]` by Range, honouring If-Range like Go's
    /// `http.ServeContent`; moves to the next generation after `swap_after`
    /// ranged requests.
    struct Ranged {
        generations: Vec<(String, Vec<u8>)>,
        swap_after: Option<usize>,
        honour_range: bool,
        seen: Mutex<Vec<(Option<String>, Option<String>)>>,
    }

    impl Respond for Ranged {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let header = |name: &str| {
                request
                    .headers
                    .get(name)
                    .map(|v| v.to_str().unwrap().to_string())
            };
            let (range, if_range) = (header("range"), header("if-range"));
            let mut seen = self.seen.lock().unwrap();
            seen.push((range.clone(), if_range.clone()));
            // The first `swap_after` requests see generation 0; every later one
            // sees generation 1.
            let generation = if self.swap_after.is_some_and(|n| seen.len() > n) {
                1
            } else {
                0
            };
            let (etag, body) = &self.generations[generation];
            let full = || {
                ResponseTemplate::new(200)
                    .insert_header("ETag", etag.as_str())
                    .set_body_bytes(body.clone())
            };
            let Some(range) = range.filter(|_| self.honour_range) else {
                return full();
            };
            if if_range.is_some_and(|tag| &tag != etag) {
                return full();
            }
            let (start, end) = range.trim_start_matches("bytes=").split_once('-').unwrap();
            let start: usize = start.parse().unwrap();
            let end = end.parse::<usize>().unwrap().min(body.len() - 1);
            ResponseTemplate::new(206)
                .insert_header("ETag", etag.as_str())
                .insert_header(
                    "Content-Range",
                    format!("bytes {start}-{end}/{}", body.len()),
                )
                .set_body_bytes(body[start..=end].to_vec())
        }
    }

    fn body(len: usize, salt: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(salt))
            .collect()
    }

    async fn serve(ranged: Ranged) -> (MockServer, std::sync::Arc<Ranged>) {
        let server = MockServer::start().await;
        let ranged = std::sync::Arc::new(ranged);
        struct Shared(std::sync::Arc<Ranged>);
        impl Respond for Shared {
            fn respond(&self, request: &Request) -> ResponseTemplate {
                self.0.respond(request)
            }
        }
        Mock::given(method("GET"))
            .and(path(SEGMENT_PATH))
            .and(query_param("name", "seg_0000000001.jss"))
            .respond_with(Shared(ranged.clone()))
            .mount(&server)
            .await;
        (server, ranged)
    }

    fn temp_file(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("hv-archive-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[tokio::test]
    async fn a_segment_downloads_in_ranged_parts_with_if_range() {
        let data = body(3000, 1);
        let (server, ranged) = serve(Ranged {
            generations: vec![("\"aaaa\"".into(), data.clone())],
            swap_after: None,
            honour_range: true,
            seen: Mutex::new(vec![]),
        })
        .await;
        let dest = temp_file("seg");
        let client = client(&server, None).with_part_size(1024);
        assert_eq!(
            client.segment("seg_0000000001.jss", &dest).await.unwrap(),
            3000
        );
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert_eq!(client.bytes_downloaded(), 3000);
        let seen = ranged.seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            vec![
                (Some("bytes=0-1023".into()), None),
                (Some("bytes=1024-2047".into()), Some("\"aaaa\"".into())),
                (Some("bytes=2048-3071".into()), Some("\"aaaa\"".into())),
            ]
        );
    }

    #[tokio::test]
    async fn a_changed_generation_restarts_the_segment() {
        let new = body(2500, 9);
        let (server, _) = serve(Ranged {
            generations: vec![
                ("\"aaaa\"".into(), body(3000, 1)),
                ("\"bbbb\"".into(), new.clone()),
            ],
            swap_after: Some(1),
            honour_range: true,
            seen: Mutex::new(vec![]),
        })
        .await;
        let dest = temp_file("seg");
        let client = client(&server, None).with_part_size(1024);
        assert_eq!(
            client.segment("seg_0000000001.jss", &dest).await.unwrap(),
            2500
        );
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            new,
            "only the new generation, no mix"
        );
    }

    #[tokio::test]
    async fn a_server_that_ignores_range_streams_the_whole_body() {
        let data = body(3000, 4);
        let (server, ranged) = serve(Ranged {
            generations: vec![("\"aaaa\"".into(), data.clone())],
            swap_after: None,
            honour_range: false,
            seen: Mutex::new(vec![]),
        })
        .await;
        let dest = temp_file("seg");
        let client = client(&server, None).with_part_size(1024);
        assert_eq!(
            client.segment("seg_0000000001.jss", &dest).await.unwrap(),
            3000
        );
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert_eq!(ranged.seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_segment_that_keeps_changing_fails() {
        let server = MockServer::start().await;
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        struct Churn(std::sync::Arc<std::sync::atomic::AtomicUsize>);
        impl Respond for Churn {
            fn respond(&self, request: &Request) -> ResponseTemplate {
                let n = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let etag = format!("\"gen{n}\"");
                let ranged_from_zero = request
                    .headers
                    .get("range")
                    .is_some_and(|r| r == "bytes=0-1023");
                if ranged_from_zero {
                    ResponseTemplate::new(206)
                        .insert_header("ETag", etag.as_str())
                        .insert_header("Content-Range", "bytes 0-1023/3000")
                        .set_body_bytes(vec![0u8; 1024])
                } else {
                    // Every follow-up part finds a new generation.
                    ResponseTemplate::new(206)
                        .insert_header("ETag", etag.as_str())
                        .insert_header("Content-Range", "bytes 1024-2047/3000")
                        .set_body_bytes(vec![0u8; 1024])
                }
            }
        }
        Mock::given(method("GET"))
            .and(path(SEGMENT_PATH))
            .respond_with(Churn(calls.clone()))
            .mount(&server)
            .await;
        let dest = temp_file("seg");
        let result = client(&server, None)
            .with_part_size(1024)
            .segment("seg_0000000001.jss", &dest)
            .await;
        assert!(
            matches!(result, Err(ArchiveError::GenerationChurn(_))),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn an_oversized_segment_is_refused_before_it_is_read() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(SEGMENT_PATH))
            .respond_with(
                ResponseTemplate::new(206)
                    .insert_header("ETag", "\"big\"")
                    .insert_header(
                        "Content-Range",
                        format!("bytes 0-1023/{}", MAX_SEGMENT_BYTES + 1),
                    )
                    .set_body_bytes(vec![0u8; 1024]),
            )
            .mount(&server)
            .await;
        let dest = temp_file("seg");
        let result = client(&server, None)
            .with_part_size(1024)
            .segment("seg_0000000001.jss", &dest)
            .await;
        assert!(
            matches!(result, Err(ArchiveError::TooLarge(_))),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn an_empty_part_fails_instead_of_looping() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(SEGMENT_PATH))
            .respond_with(
                ResponseTemplate::new(206)
                    .insert_header("ETag", "\"aaaa\"")
                    .insert_header("Content-Range", "bytes 0-1023/3000")
                    .set_body_bytes(Vec::<u8>::new()),
            )
            .expect(1)
            .mount(&server)
            .await;
        let dest = temp_file("seg");
        let result = client(&server, None)
            .with_part_size(1024)
            .segment("seg_0000000001.jss", &dest)
            .await;
        assert!(
            matches!(result, Err(ArchiveError::Status { status: 206, .. })),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_first_part_without_an_etag_is_fetched_whole() {
        let data = body(3000, 6);
        let server = MockServer::start().await;
        let seen = std::sync::Arc::new(Mutex::new(Vec::<Option<String>>::new()));
        let log = seen.clone();
        let whole = data.clone();
        Mock::given(method("GET"))
            .and(path(SEGMENT_PATH))
            .respond_with(move |request: &Request| {
                let range = request
                    .headers
                    .get("range")
                    .map(|v| v.to_str().unwrap().to_string());
                log.lock().unwrap().push(range.clone());
                match range {
                    Some(_) => ResponseTemplate::new(206)
                        .insert_header("Content-Range", "bytes 0-1023/3000")
                        .set_body_bytes(whole[..1024].to_vec()),
                    None => ResponseTemplate::new(200).set_body_bytes(whole.clone()),
                }
            })
            .mount(&server)
            .await;
        let dest = temp_file("seg");
        let n = client(&server, None)
            .with_part_size(1024)
            .segment("seg_0000000001.jss", &dest)
            .await
            .unwrap();
        assert_eq!(n, 3000);
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![Some("bytes=0-1023".to_string()), None]
        );
    }

    /// A raw HTTP server: the first `drops` connections promise 1000 body
    /// bytes, send 10 and hang up; later ones send all 1000.
    async fn flaky_block_server(drops: usize) -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut connections = 0;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = vec![0u8; 4096];
                let mut seen = Vec::new();
                while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => seen.extend_from_slice(&buf[..n]),
                    }
                }
                let head = "HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n";
                let _ = socket.write_all(head.as_bytes()).await;
                let sent = if connections < drops { 10 } else { 1000 };
                let _ = socket.write_all(&vec![7u8; sent]).await;
                let _ = socket.shutdown().await;
                connections += 1;
            }
        });
        addr
    }

    fn client_at(addr: std::net::SocketAddr) -> ArchiveClient {
        let endpoint = crate::jetstream::config::parse_endpoint(&format!("http://{addr}"));
        let config = ArchiveConfig {
            retry_unit: Duration::from_millis(1),
            ..ArchiveConfig::default()
        };
        ArchiveClient::new(&endpoint, &config)
    }

    #[tokio::test]
    async fn a_block_whose_body_drops_is_retried() {
        let client = client_at(flaky_block_server(1).await);
        assert_eq!(client.block("seg", 0).await.unwrap(), vec![7u8; 1000]);
    }

    #[tokio::test]
    async fn a_body_failure_is_a_transport_error_not_too_large() {
        let client = client_at(flaky_block_server(usize::MAX).await);
        let result = client.block("seg", 0).await;
        assert!(
            matches!(result, Err(ArchiveError::Transport(_))),
            "{result:?}"
        );
    }

    /// A raw HTTP server for a `total`-byte segment: every other request
    /// (the first, third, ...) promises its range but hangs up halfway.
    async fn flaky_segment_server(total: usize) -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut requests = 0;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = vec![0u8; 4096];
                let mut seen = Vec::new();
                while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => seen.extend_from_slice(&buf[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&seen).to_ascii_lowercase();
                let (start, end) = head
                    .lines()
                    .find_map(|l| l.strip_prefix("range: bytes="))
                    .and_then(|r| r.trim().split_once('-'))
                    .map(|(a, b)| {
                        let start: usize = a.parse().unwrap();
                        (start, b.parse::<usize>().unwrap().min(total - 1))
                    })
                    .unwrap();
                let len = end - start + 1;
                let response = format!(
                    "HTTP/1.1 206 Partial Content\r\nETag: \"aaaa\"\r\n\
                     Content-Range: bytes {start}-{end}/{total}\r\n\
                     Content-Length: {len}\r\nConnection: close\r\n\r\n"
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let sent = if requests % 2 == 0 { len / 2 } else { len };
                let _ = socket.write_all(&vec![7u8; sent]).await;
                let _ = socket.shutdown().await;
                requests += 1;
            }
        });
        addr
    }

    #[tokio::test]
    async fn each_part_of_a_segment_gets_its_own_retry_budget() {
        // Three parts each break once: more breaks than one budget allows,
        // never two in a row.
        let client = client_at(flaky_segment_server(5000).await).with_part_size(1024);
        let dest = temp_file("seg");
        assert_eq!(
            client.segment("seg_0000000001.jss", &dest).await.unwrap(),
            5000
        );
        assert_eq!(std::fs::read(&dest).unwrap(), vec![7u8; 5000]);
    }

    #[tokio::test]
    async fn the_api_key_goes_to_archive_endpoints_only() {
        let server = MockServer::start().await;
        let bearer = || header("authorization", "Bearer s3cret");
        Mock::given(method("POST"))
            .and(path(PLAN_PATH))
            .and(bearer())
            .respond_with(ResponseTemplate::new(200).set_body_json(page()))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(BLOCK_PATH))
            .and(bearer())
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![1u8]))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(SEGMENT_PATH))
            .and(bearer())
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![1u8]))
            .expect(1)
            .mount(&server)
            .await;
        let keyed = client(&server, Some("s3cret"));
        keyed.plan(&PlanRequest::default()).await.unwrap();
        keyed.block("seg", 0).await.unwrap();
        keyed
            .segment("seg_0000000001.jss", &temp_file("seg"))
            .await
            .unwrap();

        // The dictionary fetch never carries the key. The responder records
        // what it saw and the test asserts on that: a panic inside a
        // responder only fails the request, which nothing here would notice.
        let dictionary_auth = authorization_log();
        Mock::given(method("GET"))
            .and(path(crate::jetstream::config::DICTIONARY_PATH))
            .respond_with(recording(&dictionary_auth, ResponseTemplate::new(404)))
            .mount(&server)
            .await;
        let endpoint = crate::jetstream::config::parse_endpoint(&server.uri());
        // A 404 makes this an error; only the request it sent matters.
        let _ = crate::jetstream::compression::fetch_dictionary(
            &reqwest::Client::new(),
            &crate::jetstream::config::dictionary_url(&endpoint),
        )
        .await;
        assert_eq!(
            *dictionary_auth.lock().unwrap(),
            vec![None],
            "no key on the dictionary"
        );

        // Without a key nothing is sent at all.
        let server = MockServer::start().await;
        let plan_auth = authorization_log();
        Mock::given(method("POST"))
            .and(path(PLAN_PATH))
            .respond_with(recording(
                &plan_auth,
                ResponseTemplate::new(200).set_body_json(page()),
            ))
            .mount(&server)
            .await;
        client(&server, None)
            .plan(&PlanRequest::default())
            .await
            .unwrap();
        assert_eq!(
            *plan_auth.lock().unwrap(),
            vec![None],
            "no key without one configured"
        );
    }

    #[tokio::test]
    async fn a_redirect_to_another_host_drops_the_key() {
        // Bluesky answers getBlock with a 307 to its CDN. The block arrives,
        // and the key stays with the archive.
        let cdn = MockServer::start().await;
        let cdn_auth = authorization_log();
        Mock::given(method("GET"))
            .and(path("/blocks/seg/0"))
            .respond_with(recording(
                &cdn_auth,
                ResponseTemplate::new(200).set_body_bytes(vec![9u8; 10]),
            ))
            .mount(&cdn)
            .await;
        let archive = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(BLOCK_PATH))
            .and(header("authorization", "Bearer s3cret"))
            .respond_with(
                ResponseTemplate::new(307)
                    .insert_header("Location", format!("{}/blocks/seg/0", cdn.uri())),
            )
            .expect(1)
            .mount(&archive)
            .await;
        let client = client(&archive, Some("s3cret"));
        assert_eq!(client.block("seg", 0).await.unwrap(), vec![9u8; 10]);
        assert_eq!(*cdn_auth.lock().unwrap(), vec![None], "no key at the CDN");
    }

    type AuthorizationLog = std::sync::Arc<Mutex<Vec<Option<String>>>>;

    fn authorization_log() -> AuthorizationLog {
        std::sync::Arc::new(Mutex::new(Vec::new()))
    }

    /// A responder that records each request's `Authorization` header, then
    /// answers `response`.
    fn recording(
        log: &AuthorizationLog,
        response: ResponseTemplate,
    ) -> impl Fn(&Request) -> ResponseTemplate + Send + Sync + 'static {
        let log = log.clone();
        move |request: &Request| {
            log.lock().unwrap().push(
                request
                    .headers
                    .get("authorization")
                    .map(|v| v.to_str().unwrap().to_string()),
            );
            response.clone()
        }
    }
}

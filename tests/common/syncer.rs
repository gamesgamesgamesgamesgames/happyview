//! A stand-in syncer that records the write notifications it receives.

use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

pub const NOTIFY_WRITE_PATH: &str = "/xrpc/com.atproto.space.notifyWrite";

pub async fn start() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(NOTIFY_WRITE_PATH))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    server
}

/// Notifications are delivered in the background, so wait for them to land.
pub async fn received(server: &MockServer, at_least: usize) -> Vec<Request> {
    for _ in 0..50 {
        let requests = server.received_requests().await.unwrap_or_default();
        if requests.len() >= at_least {
            return requests;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    server.received_requests().await.unwrap_or_default()
}

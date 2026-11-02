//! The clients `http_retry` builds read the installed policy. A target of its
//! own, because installing one applies to the whole process.

use happyview::outbound::{Policy, install};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn every_shared_client_screens_once_a_policy_is_installed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    // Built before the install, as a client made early in boot would be.
    let early = happyview::http_retry::build_http_client("test");
    install(Policy::default());

    for (name, client) in [
        ("early", early),
        ("shared", happyview::http_retry::shared_client().clone()),
        (
            "no-redirect",
            happyview::http_retry::build_no_redirect_client("test"),
        ),
    ] {
        let err = client
            .get(server.uri())
            .send()
            .await
            .expect_err("a loopback address must be refused");
        let mut chain = err.to_string();
        let mut source = std::error::Error::source(&err);
        while let Some(s) = source {
            chain.push_str(": ");
            chain.push_str(&s.to_string());
            source = s.source();
        }
        assert!(chain.contains("not a public address"), "{name}: {chain}");
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

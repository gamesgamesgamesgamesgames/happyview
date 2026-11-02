//! The address screening every shared outbound client does, exercised through
//! real connections. Each test builds its own client with a fixed policy, so
//! nothing is installed for the binary.

use std::sync::Arc;

use happyview::outbound::{Policy, Source, connect_tcp, screened};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(policy: Policy) -> reqwest::Client {
    screened(
        reqwest::Client::builder(),
        Source::Fixed(Arc::new(policy)),
        Some(10),
    )
    .build()
    .unwrap()
}

fn error_chain(e: &reqwest::Error) -> String {
    let mut out = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        out.push_str(": ");
        out.push_str(&s.to_string());
        source = s.source();
    }
    out
}

#[tokio::test]
async fn a_private_ip_literal_is_refused_once_connected_and_sent_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let err = client(Policy::default())
        .get(server.uri())
        .send()
        .await
        .expect_err("a loopback literal must be refused");
    let chain = error_chain(&err);
    assert!(
        chain.contains("127.0.0.1 is not a public address"),
        "{chain}"
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_trusted_address_is_reached() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let response = client(Policy::trusting([server.uri().as_str()]))
        .get(server.uri())
        .send()
        .await
        .expect("an operator-configured address is exempt");
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn a_name_resolving_only_to_private_addresses_is_refused_before_connecting() {
    let err = client(Policy::default())
        .get("http://localhost:9/")
        .send()
        .await
        .expect_err("localhost must be refused");
    let chain = error_chain(&err);
    assert!(
        chain.contains("localhost resolves to no public address"),
        "{chain}"
    );
}

async fn redirecting_to(location: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", location))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn a_redirect_from_an_admitted_host_to_a_private_one_is_refused() {
    for location in [
        "http://169.254.169.254/latest/meta-data/",
        "http://10.0.0.1/",
        "http://[fd00::1]/",
    ] {
        let server = redirecting_to(location).await;
        let err = client(Policy::trusting([server.uri().as_str()]))
            .get(server.uri())
            .send()
            .await
            .expect_err("the redirect must be refused");
        let chain = error_chain(&err);
        assert!(chain.contains("refusing redirect"), "{location}: {chain}");
    }
}

#[tokio::test]
async fn a_redirect_to_a_v4_mapped_private_address_is_refused() {
    let server = redirecting_to("http://[::ffff:169.254.169.254]/").await;
    let err = client(Policy::trusting([server.uri().as_str()]))
        .get(server.uri())
        .send()
        .await
        .expect_err("a v4-mapped private address must be refused");
    assert!(error_chain(&err).contains("refusing redirect"));
}

#[tokio::test]
async fn a_v4_mapped_private_literal_is_refused_once_connected() {
    let server = MockServer::start().await;
    let port = server.address().port();
    let err = client(Policy::default())
        .get(format!("http://[::ffff:127.0.0.1]:{port}/"))
        .send()
        .await
        .expect_err("a v4-mapped loopback literal must be refused");
    let chain = error_chain(&err);
    // Whichever way the platform reports the peer, it is judged as 127.0.0.1.
    assert!(chain.contains("is not a public address"), "{chain}");
}

/// A TLS listener on 127.0.0.1 answering every request with 200, and the DER
/// of its self-signed certificate for `localhost` and `127.0.0.1`.
async fn tls_server() -> (u16, Vec<u8>) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cert =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    let cert_der = cert.cert.der().to_vec();
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(cert_der.clone())],
            rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into()),
        )
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(stream).await else {
                    return;
                };
                let mut buf = vec![0u8; 4096];
                let _ = tls.read(&mut buf).await;
                let _ = tls
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await;
                let _ = tls.shutdown().await;
            });
        }
    });
    (port, cert_der)
}

#[tokio::test]
async fn the_peer_is_read_through_tls() {
    let (port, cert_der) = tls_server().await;
    let https_client = |policy: Policy| {
        screened(
            reqwest::Client::builder()
                .add_root_certificate(reqwest::Certificate::from_der(&cert_der).unwrap()),
            Source::Fixed(Arc::new(policy)),
            Some(10),
        )
        .build()
        .unwrap()
    };

    // A connection whose peer could not be read is refused, so reaching the
    // server at all shows the peer was read through the TLS stream.
    let response = https_client(Policy::trusting(["http://127.0.0.1"]))
        .get(format!("https://localhost:{port}/"))
        .send()
        .await
        .expect("an exempt address is reached over TLS");
    assert_eq!(response.text().await.unwrap(), "ok");

    let err = https_client(Policy::default())
        .get(format!("https://127.0.0.1:{port}/"))
        .send()
        .await
        .expect_err("a loopback literal must be refused over TLS too");
    let chain = error_chain(&err);
    assert!(
        chain.contains("127.0.0.1 is not a public address"),
        "{chain}"
    );
}

#[tokio::test]
async fn a_raw_socket_is_screened_the_same_way() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let refused = Source::Fixed(Arc::new(Policy::default()));
    for host in ["127.0.0.1", "localhost", "[::ffff:127.0.0.1]"] {
        let err = connect_tcp(&refused, host, port)
            .await
            .expect_err("a private destination must be refused");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{host}");
    }

    let trusted = Source::Fixed(Arc::new(Policy::trusting(["http://127.0.0.1"])));
    connect_tcp(&trusted, "127.0.0.1", port)
        .await
        .expect("an exempt address is reached");
}

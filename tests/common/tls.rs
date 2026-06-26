use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

pub struct DidWebServer {
    pub port: u16,
    pub did: String,
    _handle: JoinHandle<()>,
}

impl DidWebServer {
    pub fn issuer_did(&self) -> &str {
        &self.did
    }
}

/// Start a TLS server that serves a DID document at `/.well-known/did.json`.
///
/// `build_doc` receives the computed `did:web:localhost%3A{port}` DID and
/// returns the DID document to serve. This solves the chicken-and-egg problem
/// where the DID depends on the port.
pub async fn start_did_web_server(
    build_doc: impl FnOnce(&str) -> serde_json::Value,
) -> DidWebServer {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])
        .expect("failed to generate self-signed cert");
    let cert_der = cert.cert.der().to_vec();
    let key_der = cert.key_pair.serialize_der();

    let tls_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(cert_der)],
            rustls::pki_types::PrivateKeyDer::Pkcs8(key_der.into()),
        )
        .expect("failed to build TLS config");

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind TLS listener");
    let port = listener.local_addr().unwrap().port();
    let did = format!("did:web:localhost%3A{port}");

    let did_doc = build_doc(&did);
    let did_doc_bytes = serde_json::to_vec(&did_doc).unwrap();

    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls_config));

    let handle = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let acceptor = acceptor.clone();
            let body = did_doc_bytes.clone();

            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(stream).await else {
                    return;
                };

                let mut buf = vec![0u8; 4096];
                let _ = tls.read(&mut buf).await;

                let request = String::from_utf8_lossy(&buf);
                let response = if request.contains("/.well-known/did.json") {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len(),
                    )
                } else {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string()
                };

                let _ = tls.write_all(response.as_bytes()).await;
                if request.contains("/.well-known/did.json") {
                    let _ = tls.write_all(&body).await;
                }
                let _ = tls.shutdown().await;
            });
        }
    });

    DidWebServer {
        port,
        did,
        _handle: handle,
    }
}

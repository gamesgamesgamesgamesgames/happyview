//! Resolution with the test run's switch on. A binary of its own so no other
//! test shares a process with the switch.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// An HTTP client whose every request goes through a local listener that
/// counts the connection and drops it, so a resolver that reaches for the
/// directory or the forged PDS is seen doing so even where it swallows the
/// failure.
async fn counting_client() -> (reqwest::Client, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = seen.clone();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            drop(socket);
        }
    });
    let http = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(proxy).unwrap())
        .build()
        .unwrap();
    (http, seen)
}

#[tokio::test]
async fn resolution_answers_from_the_rule_and_never_the_network() {
    happyview::testing::identity::forge_identities();
    let (http, seen) = counting_client().await;
    // An unroutable directory: any request at all would fail the test.
    let directory = "http://127.0.0.1:1";

    let doc = happyview::profile::resolve_did_document(&http, directory, "did:plc:alice")
        .await
        .unwrap();
    assert_eq!(doc.also_known_as, vec!["at://alice.test".to_string()]);

    let once = happyview::profile::resolve_did_document_once(&http, directory, "did:plc:alice")
        .await
        .unwrap_or_else(|failure| panic!("{}", failure.message));
    assert_eq!(once.also_known_as, vec!["at://alice.test".to_string()]);

    assert_eq!(
        happyview::identity::resolve_handle("alice.test")
            .await
            .unwrap(),
        "did:plc:alice"
    );
    assert_eq!(
        happyview::identity::resolve_handle("Alice.test")
            .await
            .unwrap(),
        "did:plc:alice"
    );
    let refused = happyview::identity::resolve_handle("bob.example")
        .await
        .unwrap_err();
    assert!(refused.to_string().contains(".test"), "{refused}");

    let verified = happyview::identity::resolve_verified(&http, directory, "alice.test")
        .await
        .unwrap();
    assert_eq!(verified.did, "did:plc:alice");
    assert_eq!(verified.handle.as_deref(), Some("alice.test"));

    let profile = happyview::profile::resolve_profile(&http, directory, "did:plc:alice")
        .await
        .unwrap();
    assert_eq!(profile.handle, "alice.test");

    let blob = happyview::plugin::host::blob_download(
        &http,
        directory,
        happyview_plugin_sdk::wire::AtprotoBlobDownload {
            did: "did:plc:alice".into(),
            cid: "bafkreiblob".into(),
        },
    )
    .await
    .unwrap_err();
    assert!(
        blob.to_string().contains("not served in `happyview test`"),
        "{blob}"
    );

    let mut state = happyview::test_support::test_state_with_pool(
        happyview::test_support::migrated_memory_pool().await,
    );
    state.http = http.clone();
    let space_blob =
        happyview::spaces::service::fetch_space_blob(&state, "did:plc:alice", "bafkreiblob")
            .await
            .unwrap_err();
    assert!(
        space_blob
            .to_string()
            .contains("not served in `happyview test`"),
        "{space_blob}"
    );

    assert_eq!(
        seen.load(Ordering::SeqCst),
        0,
        "a resolver reached the network"
    );
}

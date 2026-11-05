//! End-to-end coverage of the five atproto/attestation host imports, through
//! the executor and the `sdk_atproto` fixture rather than the bare Rust
//! functions in `src/plugin/host/atproto.rs` (already unit-tested there).
//! None of these need a `CallerSession` — every import here reads the public
//! network, the label/record tables, or this instance's own attestation key,
//! never the caller's own repo.

mod common;

use std::sync::Arc;

use serde_json::json;

use common::app::TestApp;
use common::plc;

use happyview::db::{adapt_sql, now_rfc3339};
use happyview::plugin::attestation::AttestationSigner;
use happyview::plugin::library::LibraryCallContext;
use happyview::plugin::{LoadedPlugin, PluginInfo, PluginManifest, PluginSource};

const FIXTURE: &str =
    "tests/fixtures/sdk_atproto/target/wasm32-unknown-unknown/release/sdk_atproto.wasm";

fn fixture_bytes() -> Vec<u8> {
    std::fs::read(FIXTURE).expect(
        "atproto fixture not built. Run: cargo build --manifest-path tests/fixtures/sdk_atproto/Cargo.toml --target wasm32-unknown-unknown --release",
    )
}

/// The fixture registered as a library under `id`, with `capabilities`.
/// `plugin_registry.register` never runs the loader's import check, so this
/// also registers manifests missing a capability the fixture still imports,
/// to pin that the host function itself refuses the call rather than
/// trusting whatever the module happened to import.
fn atproto_plugin(id: &str, capabilities: &[&str]) -> LoadedPlugin {
    let manifest: PluginManifest = serde_json::from_value(json!({
        "id": id, "name": id, "version": "1.0.0", "api_version": "2",
        "plugin_type": "library", "namespace": id,
        "capabilities": capabilities,
    }))
    .unwrap();
    LoadedPlugin {
        info: PluginInfo {
            id: id.into(),
            name: id.into(),
            version: "1.0.0".into(),
            api_version: "2".into(),
            icon_url: None,
            required_secrets: vec![],
            auth_type: "oauth2".into(),
            config_schema: None,
        },
        source: PluginSource::File {
            path: "tests/fixtures/sdk_atproto".into(),
        },
        wasm_bytes: fixture_bytes(),
        manifest: Some(manifest),
    }
}

/// A DID document advertising `endpoint` as the DID's PDS, in the shape the
/// mock PLC directory in `tests/common/plc.rs` serves back verbatim.
fn did_doc_with_pds(did: &str, endpoint: &str) -> serde_json::Value {
    json!({
        "id": did,
        "service": [{
            "id": "#atproto_pds",
            "type": "AtprotoPersonalDataServer",
            "serviceEndpoint": endpoint,
        }],
    })
}

#[tokio::test]
async fn resolve_service_returns_the_mock_endpoint_and_none_for_unknown_did() {
    common::require_db!();
    let app = TestApp::new().await;
    let plc_store = plc::setup_mock_plc(&app.mock_server).await;
    plc_store.write().await.insert(
        "did:plc:known".to_string(),
        did_doc_with_pds("did:plc:known", &app.mock_server.uri()),
    );

    app.state
        .plugin_registry
        .register(atproto_plugin(
            "sdk_atproto",
            &["atproto:read", "attest:sign"],
        ))
        .await;
    let executor = app.state.plugin_executor();

    let known = executor
        .call_library(
            "sdk_atproto",
            "resolve_service",
            &[json!("did:plc:known")],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap();
    assert_eq!(known, json!(app.mock_server.uri()));

    let unknown = executor
        .call_library(
            "sdk_atproto",
            "resolve_service",
            &[json!("did:plc:unknown")],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap();
    assert_eq!(unknown, serde_json::Value::Null);
}

// `blob_download` fetches whatever `serviceEndpoint` a DID document names,
// and a DID document is attacker-controlled input, so the host refuses an
// endpoint that isn't `https`, or that resolves to a loopback/private
// address, before ever sending a request — see
// `refuse_unsafe_endpoint` in `src/plugin/host/atproto.rs`. That refusal
// happens before `wiremock`'s own mock could ever answer, which is why these
// two both stop at `RESOLVE_ERROR` rather than reaching a mocked PDS
// response; success and PDS-status relaying are covered at the Rust-function
// level in `src/plugin/host/atproto.rs`'s own tests, via the `allow_local`
// override `blob_download_with_policy` documents for exactly that purpose.

#[tokio::test]
async fn blob_download_refuses_a_resolved_endpoint_that_is_not_https() {
    common::require_db!();
    let app = TestApp::new().await;
    let plc_store = plc::setup_mock_plc(&app.mock_server).await;
    // `app.mock_server.uri()` is always `http://127.0.0.1:<port>` — non-https
    // on its own is enough to refuse, before the loopback address is even
    // considered.
    plc_store.write().await.insert(
        "did:plc:blobowner".to_string(),
        did_doc_with_pds("did:plc:blobowner", &app.mock_server.uri()),
    );

    app.state
        .plugin_registry
        .register(atproto_plugin(
            "sdk_atproto",
            &["atproto:read", "attest:sign"],
        ))
        .await;
    let executor = app.state.plugin_executor();

    let err = executor
        .call_library(
            "sdk_atproto",
            "blob_download",
            &[json!({"did": "did:plc:blobowner", "cid": "bafytest123"})],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap_err();

    assert!(err.to_string().contains("RESOLVE_ERROR"), "{err}");
    assert!(err.to_string().contains("non-https"), "{err}");
}

#[tokio::test]
async fn blob_download_refuses_a_resolved_loopback_https_endpoint() {
    common::require_db!();
    let app = TestApp::new().await;
    let plc_store = plc::setup_mock_plc(&app.mock_server).await;
    // A syntactically valid `https` endpoint that still names a loopback
    // address — distinct from the non-https case above, and refused for a
    // different reason. No response is mocked for it: the refusal happens
    // before any request would be sent.
    plc_store.write().await.insert(
        "did:plc:blobowner".to_string(),
        did_doc_with_pds("did:plc:blobowner", "https://127.0.0.1:1"),
    );

    app.state
        .plugin_registry
        .register(atproto_plugin(
            "sdk_atproto",
            &["atproto:read", "attest:sign"],
        ))
        .await;
    let executor = app.state.plugin_executor();

    let err = executor
        .call_library(
            "sdk_atproto",
            "blob_download",
            &[json!({"did": "did:plc:blobowner", "cid": "bafymissing"})],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap_err();

    assert!(err.to_string().contains("RESOLVE_ERROR"), "{err}");
    assert!(err.to_string().contains("loopback"), "{err}");
}

#[tokio::test]
async fn labels_get_returns_seeded_labels() {
    common::require_db!();
    let app = TestApp::new().await;

    let insert_label = adapt_sql(
        "INSERT INTO happyview_labels (src, uri, val, cts, exp) VALUES (?, ?, ?, ?, NULL)",
        app.state.db_backend,
    );
    happyview::db::query(&insert_label)
        .bind("did:plc:labeler")
        .bind("at://did:plc:owner/app.test.thing/1")
        .bind("spam")
        .bind(now_rfc3339())
        .execute(&app.state.db)
        .await
        .expect("failed to seed label");

    app.state
        .plugin_registry
        .register(atproto_plugin(
            "sdk_atproto",
            &["atproto:read", "attest:sign"],
        ))
        .await;
    let executor = app.state.plugin_executor();

    let out = executor
        .call_library(
            "sdk_atproto",
            "labels_get",
            &[json!({"uris": ["at://did:plc:owner/app.test.thing/1"]})],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap();

    let labels = out["at://did:plc:owner/app.test.thing/1"]
        .as_array()
        .expect("labels array present");
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0]["src"], "did:plc:labeler");
    assert_eq!(labels[0]["val"], "spam");
}

#[tokio::test]
async fn attest_sign_then_verify_round_trips_and_rejects_tampering() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.state.attestation_signer = Some(Arc::new(AttestationSigner::for_testing(
        "did:web:test.example#signing".to_string(),
        "test.signature".to_string(),
    )));

    app.state
        .plugin_registry
        .register(atproto_plugin(
            "sdk_atproto",
            &["atproto:read", "attest:sign"],
        ))
        .await;
    let executor = app.state.plugin_executor();

    // `attest_sign` signs as whoever the library call context names as
    // `caller_did` — it has no session of its own — and that DID is part of
    // the signed content, so `attest_verify`'s `repo_did` below must match.
    let signer_ctx = LibraryCallContext {
        caller_did: Some("did:plc:caller".into()),
        ..LibraryCallContext::default()
    };
    let record = json!({"contributionType": "correction", "changes": {"name": "Test"}});
    let signature = executor
        .call_library(
            "sdk_atproto",
            "attest_sign",
            &[json!({"record": record})],
            &signer_ctx,
            0,
        )
        .await
        .unwrap();

    let verified = executor
        .call_library(
            "sdk_atproto",
            "attest_verify",
            &[json!({
                "record": record,
                "signature": signature,
                "repo_did": "did:plc:caller",
            })],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap();
    assert_eq!(verified, json!(true));

    let tampered = json!({"contributionType": "correction", "changes": {"name": "Tampered"}});
    let verified_tampered = executor
        .call_library(
            "sdk_atproto",
            "attest_verify",
            &[json!({
                "record": tampered,
                "signature": signature,
                "repo_did": "did:plc:caller",
            })],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap();
    assert_eq!(
        verified_tampered,
        json!(false),
        "a signature over different content is invalid, not unverifiable"
    );
}

#[tokio::test]
async fn attest_sign_without_a_configured_signer_is_no_signer() {
    common::require_db!();
    let app = TestApp::new().await;
    assert!(
        app.state.attestation_signer.is_none(),
        "no signer configured by default"
    );

    app.state
        .plugin_registry
        .register(atproto_plugin(
            "sdk_atproto",
            &["atproto:read", "attest:sign"],
        ))
        .await;
    let executor = app.state.plugin_executor();

    let err = executor
        .call_library(
            "sdk_atproto",
            "attest_sign",
            &[json!({"record": {}})],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("NO_SIGNER"), "{err}");
}

/// Pins that `attest:sign` is checked at `host_attest_sign` itself, not only
/// by the loader: a manifest that omits it still gets registered here
/// (`plugin_registry.register` bypasses the loader's import check entirely),
/// and the fixture module still imports `host_attest_sign` (it was built
/// against the full capability set), so a passing call here would mean the
/// host function trusted whatever the module happened to import.
#[tokio::test]
async fn attest_sign_without_the_capability_is_forbidden() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.state.attestation_signer = Some(Arc::new(AttestationSigner::for_testing(
        "did:web:test.example#signing".to_string(),
        "test.signature".to_string(),
    )));

    app.state
        .plugin_registry
        .register(atproto_plugin("sdk_atproto_readonly", &["atproto:read"]))
        .await;
    let executor = app.state.plugin_executor();

    let err = executor
        .call_library(
            "sdk_atproto_readonly",
            "attest_sign",
            &[json!({"record": {}})],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("FORBIDDEN"), "{err}");
}

/// The four `atproto:read` imports share one capability check, and only
/// `attest:sign` had a call-site FORBIDDEN test before this — a deleted
/// `require_capability` block in `host_atproto_blob_download` would have
/// left the whole suite green. Registering the fixture with no capabilities
/// at all pins that this one is enforced too.
#[tokio::test]
async fn blob_download_without_the_capability_is_forbidden() {
    common::require_db!();
    let app = TestApp::new().await;

    app.state
        .plugin_registry
        .register(atproto_plugin("sdk_atproto_noauth", &[]))
        .await;
    let executor = app.state.plugin_executor();

    let err = executor
        .call_library(
            "sdk_atproto_noauth",
            "blob_download",
            &[json!({"did": "did:plc:blobowner", "cid": "bafytest123"})],
            &LibraryCallContext::default(),
            0,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("FORBIDDEN"), "{err}");
}

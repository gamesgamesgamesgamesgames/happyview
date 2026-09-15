//! AT Protocol network reads and attestation signing for plugins. Shared with
//! the Lua `atproto` global (`src/lua/atproto_api.rs`), which wraps these same
//! functions for scripts rather than re-implementing them.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::plugin::attestation::AttestationSigner;
use crate::profile;

use super::{MAX_HTTP_RESPONSE_SIZE, no_redirect_client};

use happyview_plugin_sdk::wire::{AtprotoBlobDownload, AttestVerify, BlobData, Label};

/// `labels_get` runs two sequential queries per URI with no batching, so an
/// unbounded array holds a pool connection for as long as the caller likes.
/// 100 matches the page size the records imports already cap at
/// (`records::MAX_LIMIT`).
pub const MAX_LABEL_URIS: usize = 100;

#[derive(Debug, thiserror::Error)]
pub enum AtprotoError {
    #[error("{0}")]
    Resolve(String),
    #[error("PDS returned {status}: {body}")]
    Blob { status: u16, body: String },
    #[error("no attestation signer is configured on this instance")]
    NoSigner,
    #[error("{0}")]
    Unverifiable(String),
    #[error("{0}")]
    Database(#[from] sqlx::Error),
    #[error("{0}")]
    Other(String),
}

/// Resolve the AT Protocol service a DID's document advertises (its PDS,
/// typically). A resolution failure is not distinguished from "no such
/// service" here — the caller cannot act on the difference — so it collapses
/// to `Ok(None)` rather than an error, exactly as the Lua global has always
/// done.
pub async fn resolve_service(
    http: &reqwest::Client,
    plc_url: &str,
    did: &str,
) -> Result<Option<String>, AtprotoError> {
    match profile::resolve_pds_endpoint(http, plc_url, did).await {
        Ok(endpoint) => Ok(Some(endpoint)),
        Err(_) => Ok(None),
    }
}

/// Refuse a resolved service endpoint that `blob_download` should not fetch
/// from. The endpoint comes out of DID resolution, not from anything the
/// plugin named directly, but a DID document is attacker-controlled input —
/// nothing stops one from advertising `http://169.254.169.254/...` or a
/// private-network address, and unlike `host_http_request` there is no
/// `allowed_hosts` list checking it, since the plugin never chose this URL
/// itself. This checks the URL's literal host, not a DNS lookup: an IP
/// literal is checked against the loopback/link-local/private ranges
/// directly, and a hostname is refused only if it is literally `localhost`.
fn refuse_unsafe_endpoint(endpoint: &str) -> Result<(), AtprotoError> {
    let url = reqwest::Url::parse(endpoint)
        .map_err(|e| AtprotoError::Resolve(format!("invalid service endpoint {endpoint}: {e}")))?;

    if url.scheme() != "https" {
        return Err(AtprotoError::Resolve(format!(
            "refusing non-https service endpoint: {endpoint}"
        )));
    }

    let Some(host) = url.host_str() else {
        return Err(AtprotoError::Resolve(format!(
            "service endpoint has no host: {endpoint}"
        )));
    };

    if host.eq_ignore_ascii_case("localhost") {
        return Err(AtprotoError::Resolve(format!(
            "refusing loopback service endpoint: {endpoint}"
        )));
    }

    // `Url::host_str` keeps an IPv6 host's brackets (they're part of its
    // serialization per the URL spec), which `IpAddr::from_str` rejects —
    // strip them before parsing so `[::1]` is recognized the same as `::1`.
    let host_for_ip_parse = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = host_for_ip_parse.parse::<std::net::IpAddr>() {
        fn unsafe_v4(v4: std::net::Ipv4Addr) -> bool {
            v4.is_loopback()
                || v4.is_link_local()
                || v4.is_private()
                || v4.is_unspecified()
                || v4.octets()[0] == 0 // 0.0.0.0/8 routes to the local host on Linux
        }
        let unsafe_address = match ip {
            std::net::IpAddr::V4(v4) => unsafe_v4(v4),
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    || (v6.segments()[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                    || (v6.segments()[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
                    // `::ffff:a.b.c.d` is routed to the IPv4 destination, so it
                    // is judged as that address rather than as a foreign IPv6 one.
                    || v6.to_ipv4_mapped().is_some_and(unsafe_v4)
            }
        };
        if unsafe_address {
            return Err(AtprotoError::Resolve(format!(
                "refusing loopback, link-local, or private service endpoint: {endpoint}"
            )));
        }
    }

    Ok(())
}

/// Download a blob from a repo, by the DID that owns it and the blob's CID.
/// Enforces the same [`MAX_HTTP_RESPONSE_SIZE`] cap `host_http_request` does;
/// request-count and total-transfer accounting live in the bindings, where
/// `ResourceUsage` is reachable.
pub async fn blob_download(
    http: &reqwest::Client,
    plc_url: &str,
    spec: AtprotoBlobDownload,
) -> Result<BlobData, AtprotoError> {
    blob_download_with_policy(http, plc_url, spec, false).await
}

/// `blob_download` with the endpoint-safety check toggleable. The plugin
/// import goes through [`blob_download`], which pins `allow_local` to
/// `false`; the Lua global passes `true` for the reason given on
/// `register_atproto_api`; tests that need a real fetch against `wiremock`
/// (always a loopback address) pass `true` as well.
pub(crate) async fn blob_download_with_policy(
    http: &reqwest::Client,
    plc_url: &str,
    spec: AtprotoBlobDownload,
    allow_local: bool,
) -> Result<BlobData, AtprotoError> {
    blob_download_capped(http, plc_url, spec, MAX_HTTP_RESPONSE_SIZE, allow_local).await
}

/// The cap is a parameter only so a test can shrink it without sending a
/// genuinely oversized body — production always goes through
/// [`blob_download`], which pins it to [`MAX_HTTP_RESPONSE_SIZE`].
async fn blob_download_capped(
    http: &reqwest::Client,
    plc_url: &str,
    spec: AtprotoBlobDownload,
    cap: u64,
    allow_local: bool,
) -> Result<BlobData, AtprotoError> {
    let did = &spec.did;
    let cid = &spec.cid;

    let pds_endpoint = profile::resolve_pds_endpoint(http, plc_url, did)
        .await
        .map_err(|e| AtprotoError::Resolve(format!("failed to resolve PDS for {did}: {e}")))?;

    if !allow_local {
        refuse_unsafe_endpoint(&pds_endpoint)?;
    }

    let url = format!(
        "{}/xrpc/com.atproto.sync.getBlob?did={}&cid={}",
        pds_endpoint,
        urlencoding::encode(did),
        urlencoding::encode(cid),
    );

    // The PDS host came out of DID resolution above, not from anything the
    // plugin named directly — there is no `allowed_hosts` list to check
    // against, unlike `host_http_request`, where the plugin picks the URL.
    // Redirects are refused for the same reason the restricted HTTP path
    // refuses them: a 3xx here could carry the fetch to a host
    // `refuse_unsafe_endpoint` never saw.
    let client = no_redirect_client()
        .map_err(|e| AtprotoError::Other(format!("failed to build client: {e}")))?;
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| AtprotoError::Other(format!("request failed: {e}")))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(AtprotoError::Blob {
            status: status.as_u16(),
            body,
        });
    }

    // Refuse on the advertised size before buffering the body, so an
    // oversized blob is rejected without paying for the transfer.
    if let Some(len) = response.content_length()
        && len > cap
    {
        return Err(too_large(len, cap));
    }

    let mime_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    let bytes = response
        .bytes()
        .await
        .map_err(|e| AtprotoError::Other(format!("failed to read body: {e}")))?;

    // A dishonest or absent `content-length` doesn't get a pass: check the
    // body actually read too.
    if bytes.len() as u64 > cap {
        return Err(too_large(bytes.len() as u64, cap));
    }

    Ok(BlobData {
        size: bytes.len() as u64,
        bytes: bytes.to_vec(),
        mime_type,
    })
}

/// A blob over the response size cap, reported the same way a bad PDS status
/// is: `413` is the accurate HTTP status for the reason, and it keeps this
/// failure inside the existing `Blob` variant rather than adding a dedicated
/// one just for size.
fn too_large(size: u64, cap: u64) -> AtprotoError {
    AtprotoError::Blob {
        status: 413,
        body: format!("blob exceeds the maximum response size ({size} > {cap})"),
    }
}

/// Look up labels applied to a set of URIs: rows from `happyview_labels`
/// (excluding anything expired) plus self-labels embedded in the record
/// itself (`record.labels.values`, `cts` reported as empty since a
/// self-label carries no separate issue time). Every requested URI is
/// present in the result, possibly with an empty list — a URI this instance
/// has never indexed is not an error, just unlabelled.
pub async fn labels_get(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
    uris: &[String],
) -> Result<BTreeMap<String, Vec<Label>>, AtprotoError> {
    if uris.len() > MAX_LABEL_URIS {
        return Err(AtprotoError::Other(format!(
            "too many URIs: {} > {MAX_LABEL_URIS}",
            uris.len()
        )));
    }

    let mut result: BTreeMap<String, Vec<Label>> = BTreeMap::new();
    for uri in uris {
        result.entry(uri.clone()).or_default();
    }

    let now = now_rfc3339();
    let label_sql = adapt_sql(
        "SELECT src, uri, val, cts FROM happyview_labels WHERE uri = ? AND (exp IS NULL OR exp > ?)",
        backend,
    );
    for uri in uris {
        let rows: Vec<(String, String, String, String)> = crate::db::query_as(&label_sql)
            .bind(uri)
            .bind(&now)
            .fetch_all(db)
            .await?;
        let entry = result.entry(uri.clone()).or_default();
        for (src, label_uri, val, cts) in rows {
            entry.push(Label {
                src,
                uri: label_uri,
                val,
                cts,
            });
        }
    }

    let record_sql = adapt_sql(
        "SELECT did, record FROM happyview_records WHERE uri = ?",
        backend,
    );
    for uri in uris {
        let record: Option<(String, String)> = crate::db::query_as(&record_sql)
            .bind(uri)
            .fetch_optional(db)
            .await?;
        let Some((did, record_str)) = record else {
            continue;
        };
        let record_val: Value = serde_json::from_str(&record_str).unwrap_or(Value::Null);
        let Some(arr) = record_val
            .get("labels")
            .and_then(|l| l.get("values"))
            .and_then(|v| v.as_array())
        else {
            continue;
        };
        let entry = result.entry(uri.clone()).or_default();
        for item in arr {
            if let Some(val) = item.get("val").and_then(|v| v.as_str()) {
                entry.push(Label {
                    src: did.clone(),
                    uri: uri.clone(),
                    val: val.to_string(),
                    cts: String::new(),
                });
            }
        }
    }

    Ok(result)
}

/// Sign `record` as `did`, returning the inline signature object just added
/// (the last entry of `signatures` after `sign_record` runs).
pub fn attest_sign(
    signer: Option<&AttestationSigner>,
    did: &str,
    mut record: Value,
) -> Result<Value, AtprotoError> {
    let signer = signer.ok_or(AtprotoError::NoSigner)?;

    signer
        .sign_record(&mut record, did)
        .map_err(|e| AtprotoError::Other(e.to_string()))?;

    record
        .get("signatures")
        .and_then(|s| s.as_array())
        .and_then(|arr| arr.last())
        .cloned()
        .ok_or_else(|| AtprotoError::Other("no signature produced".into()))
}

/// Verify that an inline signature was produced by this instance's
/// attestation signer. `false` and an error are different facts: `false`
/// means the check ran and the signature does not match; an error means the
/// check could not run at all — malformed signature bytes, a missing field,
/// a record that will not encode. Collapsing the second into the first would
/// let any fault in this path present as "this user forged their record",
/// with nothing to say otherwise, so a verification failure is always
/// reported as `Unverifiable` rather than folded into `Ok(false)`.
pub fn attest_verify(
    signer: Option<&AttestationSigner>,
    spec: AttestVerify,
) -> Result<bool, AtprotoError> {
    let signer = signer.ok_or(AtprotoError::NoSigner)?;

    signer
        .verify_record_signature(&spec.record, &spec.signature, &spec.repo_did)
        .map_err(|e| AtprotoError::Unverifiable(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn seeded_pool() -> sqlx::AnyPool {
        let pool = crate::test_support::memory_pool().await;
        for sql in [
            "CREATE TABLE happyview_labels (src TEXT NOT NULL, uri TEXT NOT NULL, val TEXT NOT NULL, cts TEXT NOT NULL, exp TEXT)",
            "CREATE TABLE happyview_records (uri TEXT PRIMARY KEY, did TEXT NOT NULL, collection TEXT NOT NULL, rkey TEXT, record TEXT NOT NULL, cid TEXT, indexed_at TEXT, created_at TEXT)",
            "INSERT INTO happyview_labels VALUES ('did:plc:labeler', 'at://a/c/1', 'spam', '2026-01-01T00:00:00Z', NULL)",
            "INSERT INTO happyview_labels VALUES ('did:plc:labeler', 'at://a/c/1', 'expired', '2020-01-01T00:00:00Z', '2020-06-01T00:00:00Z')",
            "INSERT INTO happyview_records VALUES ('at://a/c/1', 'did:plc:a', 'c', '1', '{\"labels\":{\"values\":[{\"val\":\"self-applied\"}]}}', 'cid1', NULL, '2026-01-01T00:00:01Z')",
        ] {
            crate::db::query(sql).execute(&pool).await.unwrap();
        }
        pool
    }

    #[tokio::test]
    async fn labels_get_excludes_expired_and_includes_self_labels() {
        let pool = seeded_pool().await;
        let result = labels_get(&pool, DatabaseBackend::Sqlite, &["at://a/c/1".to_string()])
            .await
            .unwrap();

        let labels = result.get("at://a/c/1").expect("uri present");
        assert_eq!(labels.len(), 2, "{labels:?}");

        let external = labels.iter().find(|l| l.val == "spam").unwrap();
        assert_eq!(external.src, "did:plc:labeler");
        assert_ne!(external.cts, "", "an external label keeps its own cts");

        let self_label = labels.iter().find(|l| l.val == "self-applied").unwrap();
        assert_eq!(self_label.src, "did:plc:a");
        assert_eq!(
            self_label.cts, "",
            "a self-label has no separate issue time"
        );

        assert!(
            !labels.iter().any(|l| l.val == "expired"),
            "an expired label must not be returned: {labels:?}"
        );
    }

    #[tokio::test]
    async fn labels_get_yields_empty_vec_for_missing_uri() {
        let pool = seeded_pool().await;
        let result = labels_get(
            &pool,
            DatabaseBackend::Sqlite,
            &["at://nowhere/c/1".to_string()],
        )
        .await
        .unwrap();

        assert_eq!(
            result.get("at://nowhere/c/1"),
            Some(&Vec::new()),
            "a URI this instance never indexed is unlabelled, not an error"
        );
    }

    #[tokio::test]
    async fn labels_get_covers_every_requested_uri() {
        let pool = seeded_pool().await;
        let result = labels_get(
            &pool,
            DatabaseBackend::Sqlite,
            &["at://a/c/1".to_string(), "at://nowhere/c/1".to_string()],
        )
        .await
        .unwrap();

        assert_eq!(result.len(), 2);
        assert!(!result["at://a/c/1"].is_empty());
        assert!(result["at://nowhere/c/1"].is_empty());
    }

    fn test_signer() -> AttestationSigner {
        AttestationSigner::for_testing(
            "did:web:test.example#signing".to_string(),
            "test.signature".to_string(),
        )
    }

    #[test]
    fn attest_sign_without_signer_is_no_signer() {
        let err = attest_sign(None, "did:plc:caller", serde_json::json!({})).unwrap_err();
        assert!(matches!(err, AtprotoError::NoSigner), "{err}");
    }

    #[test]
    fn attest_verify_without_signer_is_no_signer() {
        let err = attest_verify(
            None,
            AttestVerify {
                record: serde_json::json!({}),
                signature: serde_json::json!({}),
                repo_did: "did:plc:caller".into(),
            },
        )
        .unwrap_err();
        assert!(matches!(err, AtprotoError::NoSigner), "{err}");
    }

    #[test]
    fn attest_sign_and_verify_round_trip() {
        let signer = test_signer();
        let record =
            serde_json::json!({"contributionType": "correction", "changes": {"name": "Test"}});

        let sig = attest_sign(Some(&signer), "did:plc:caller", record.clone()).unwrap();
        assert_eq!(sig["key"], "did:web:test.example#signing");

        let ok = attest_verify(
            Some(&signer),
            AttestVerify {
                record,
                signature: sig,
                repo_did: "did:plc:caller".into(),
            },
        )
        .unwrap();
        assert!(ok);
    }

    #[test]
    fn attest_verify_rejects_tampered_record_without_erroring() {
        let signer = test_signer();
        let record =
            serde_json::json!({"contributionType": "correction", "changes": {"name": "Original"}});
        let sig = attest_sign(Some(&signer), "did:plc:caller", record.clone()).unwrap();

        let tampered =
            serde_json::json!({"contributionType": "correction", "changes": {"name": "Tampered"}});
        let ok = attest_verify(
            Some(&signer),
            AttestVerify {
                record: tampered,
                signature: sig,
                repo_did: "did:plc:caller".into(),
            },
        )
        .unwrap();
        assert!(
            !ok,
            "a signature over different content is invalid, not unverifiable"
        );
    }

    /// A signature that cannot even be decoded is a different fact from one
    /// that decodes and simply does not match: only the second is a
    /// statement about the record, so the first must come back as an error.
    #[test]
    fn attest_verify_reports_malformed_signature_as_unverifiable() {
        let signer = test_signer();
        let record =
            serde_json::json!({"contributionType": "correction", "changes": {"name": "Original"}});
        let mut sig = attest_sign(Some(&signer), "did:plc:caller", record.clone()).unwrap();
        sig["signature"]["$bytes"] = serde_json::json!("not!valid!base64");

        let err = attest_verify(
            Some(&signer),
            AttestVerify {
                record,
                signature: sig,
                repo_did: "did:plc:caller".into(),
            },
        )
        .unwrap_err();
        assert!(matches!(err, AtprotoError::Unverifiable(_)), "{err}");
        assert!(err.to_string().contains("invalid base64"), "{err}");
    }

    #[tokio::test]
    async fn resolve_service_returns_the_advertised_endpoint() {
        let mock = wiremock::MockServer::start().await;

        let did_doc = serde_json::json!({
            "id": "did:plc:test123",
            "service": [{
                "id": "#atproto_pds",
                "type": "AtprotoPersonalDataServer",
                "serviceEndpoint": "https://pds.example.com"
            }]
        });
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/did:plc:test123"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&did_doc))
            .mount(&mock)
            .await;

        let http = reqwest::Client::new();
        let endpoint = resolve_service(&http, &mock.uri(), "did:plc:test123")
            .await
            .unwrap();
        assert_eq!(endpoint.as_deref(), Some("https://pds.example.com"));
    }

    /// A resolution failure and "no such service" are not distinguishable to
    /// a caller here, so both collapse to `Ok(None)` rather than an error.
    #[tokio::test]
    async fn resolve_service_returns_none_when_resolution_fails() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/did:plc:unknown"))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&mock)
            .await;

        let http = reqwest::Client::new();
        let endpoint = resolve_service(&http, &mock.uri(), "did:plc:unknown")
            .await
            .unwrap();
        assert_eq!(endpoint, None);
    }

    #[tokio::test]
    async fn blob_download_returns_bytes_and_mime_type() {
        let mock = wiremock::MockServer::start().await;

        let did_doc = serde_json::json!({
            "id": "did:plc:blobsource",
            "service": [{
                "id": "#atproto_pds",
                "type": "AtprotoPersonalDataServer",
                "serviceEndpoint": mock.uri()
            }]
        });
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/did:plc:blobsource"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&did_doc))
            .mount(&mock)
            .await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/xrpc/com.atproto.sync.getBlob"))
            .and(wiremock::matchers::query_param("did", "did:plc:blobsource"))
            .and(wiremock::matchers::query_param("cid", "bafytest123"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(vec![0x89, 0x50, 0x4E, 0x47]),
            )
            .mount(&mock)
            .await;

        let http = reqwest::Client::new();
        // `wiremock` only ever serves from a loopback address, which
        // `refuse_unsafe_endpoint` exists to reject — this is the test-only
        // bypass `blob_download_with_policy` documents for exactly that case.
        let blob = blob_download_with_policy(
            &http,
            &mock.uri(),
            AtprotoBlobDownload {
                did: "did:plc:blobsource".into(),
                cid: "bafytest123".into(),
            },
            true,
        )
        .await
        .unwrap();

        assert_eq!(blob.mime_type, "image/png");
        assert_eq!(blob.size, 4);
        assert_eq!(blob.bytes, vec![0x89, 0x50, 0x4E, 0x47]);
    }

    #[tokio::test]
    async fn blob_download_reports_status_on_404() {
        let mock = wiremock::MockServer::start().await;

        let did_doc = serde_json::json!({
            "id": "did:plc:blobsource",
            "service": [{
                "id": "#atproto_pds",
                "type": "AtprotoPersonalDataServer",
                "serviceEndpoint": mock.uri()
            }]
        });
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/did:plc:blobsource"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&did_doc))
            .mount(&mock)
            .await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/xrpc/com.atproto.sync.getBlob"))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&mock)
            .await;

        let http = reqwest::Client::new();
        let err = blob_download_with_policy(
            &http,
            &mock.uri(),
            AtprotoBlobDownload {
                did: "did:plc:blobsource".into(),
                cid: "bafymissing".into(),
            },
            true,
        )
        .await
        .unwrap_err();

        match err {
            AtprotoError::Blob { status, .. } => assert_eq!(status, 404),
            other => panic!("expected Blob{{404}}, got {other}"),
        }
    }

    /// A blob over the size cap is refused off the advertised
    /// `content-length` alone, before the body is even read. The real
    /// `MAX_HTTP_RESPONSE_SIZE` is 100 MB, so this drives the cap down
    /// through `blob_download_capped` instead of actually transferring an
    /// oversized body — a real server would refuse to serve a body that
    /// contradicts its own declared `content-length` anyway, so faking the
    /// header on a small body isn't a usable test double for this.
    #[tokio::test]
    async fn blob_download_refuses_a_body_over_the_size_cap() {
        let mock = wiremock::MockServer::start().await;

        let did_doc = serde_json::json!({
            "id": "did:plc:blobsource",
            "service": [{
                "id": "#atproto_pds",
                "type": "AtprotoPersonalDataServer",
                "serviceEndpoint": mock.uri()
            }]
        });
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/did:plc:blobsource"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&did_doc))
            .mount(&mock)
            .await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/xrpc/com.atproto.sync.getBlob"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(vec![0u8; 4]))
            .mount(&mock)
            .await;

        let http = reqwest::Client::new();
        let err = blob_download_capped(
            &http,
            &mock.uri(),
            AtprotoBlobDownload {
                did: "did:plc:blobsource".into(),
                cid: "bafyhuge".into(),
            },
            2,
            true,
        )
        .await
        .unwrap_err();

        match err {
            AtprotoError::Blob { status, body } => {
                assert_eq!(status, 413);
                assert!(body.contains("4 > 2"), "{body}");
            }
            other => panic!("expected Blob{{413}}, got {other}"),
        }
    }

    /// Endpoints `refuse_unsafe_endpoint` must reject before `blob_download`
    /// ever sends a request: a DID document is attacker-controlled input, and
    /// these are exactly the shapes that would otherwise reach a metadata
    /// service or an internal port.
    #[tokio::test]
    async fn blob_download_refuses_a_non_https_endpoint() {
        let mock = wiremock::MockServer::start().await;
        let did_doc = did_doc_with_endpoint("did:plc:evil", "http://pds.example.com");
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/did:plc:evil"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&did_doc))
            .mount(&mock)
            .await;

        let http = reqwest::Client::new();
        let err = blob_download(
            &http,
            &mock.uri(),
            AtprotoBlobDownload {
                did: "did:plc:evil".into(),
                cid: "bafytest".into(),
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, AtprotoError::Resolve(_)), "{err}");
        assert!(err.to_string().contains("non-https"), "{err}");
    }

    #[tokio::test]
    async fn blob_download_refuses_a_loopback_ipv4_endpoint() {
        let mock = wiremock::MockServer::start().await;
        let did_doc = did_doc_with_endpoint("did:plc:evil", "https://127.0.0.1:6379");
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/did:plc:evil"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&did_doc))
            .mount(&mock)
            .await;

        let http = reqwest::Client::new();
        let err = blob_download(
            &http,
            &mock.uri(),
            AtprotoBlobDownload {
                did: "did:plc:evil".into(),
                cid: "bafytest".into(),
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, AtprotoError::Resolve(_)), "{err}");
    }

    #[tokio::test]
    async fn blob_download_refuses_a_private_range_endpoint() {
        let mock = wiremock::MockServer::start().await;
        let did_doc = did_doc_with_endpoint("did:plc:evil", "https://10.0.0.1");
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/did:plc:evil"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&did_doc))
            .mount(&mock)
            .await;

        let http = reqwest::Client::new();
        let err = blob_download(
            &http,
            &mock.uri(),
            AtprotoBlobDownload {
                did: "did:plc:evil".into(),
                cid: "bafytest".into(),
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, AtprotoError::Resolve(_)), "{err}");
    }

    #[tokio::test]
    async fn blob_download_refuses_an_ipv6_loopback_endpoint() {
        let mock = wiremock::MockServer::start().await;
        let did_doc = did_doc_with_endpoint("did:plc:evil", "https://[::1]");
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/did:plc:evil"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&did_doc))
            .mount(&mock)
            .await;

        let http = reqwest::Client::new();
        let err = blob_download(
            &http,
            &mock.uri(),
            AtprotoBlobDownload {
                did: "did:plc:evil".into(),
                cid: "bafytest".into(),
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, AtprotoError::Resolve(_)), "{err}");
    }

    #[tokio::test]
    async fn blob_download_refuses_an_ipv4_mapped_ipv6_endpoint() {
        let mock = wiremock::MockServer::start().await;
        let did_doc = did_doc_with_endpoint("did:plc:evil", "https://[::ffff:169.254.169.254]");
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/did:plc:evil"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&did_doc))
            .mount(&mock)
            .await;

        let http = reqwest::Client::new();
        let err = blob_download(
            &http,
            &mock.uri(),
            AtprotoBlobDownload {
                did: "did:plc:evil".into(),
                cid: "bafytest".into(),
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, AtprotoError::Resolve(_)), "{err}");
    }

    fn did_doc_with_endpoint(did: &str, endpoint: &str) -> Value {
        serde_json::json!({
            "id": did,
            "service": [{
                "id": "#atproto_pds",
                "type": "AtprotoPersonalDataServer",
                "serviceEndpoint": endpoint
            }]
        })
    }

    /// The records imports cap their page size; an unbounded `uris` array
    /// would otherwise hold a pool connection open for as long as the caller
    /// likes.
    #[tokio::test]
    async fn labels_get_refuses_more_than_the_uri_cap() {
        let pool = seeded_pool().await;
        let too_many: Vec<String> = (0..=MAX_LABEL_URIS)
            .map(|i| format!("at://a/c/{i}"))
            .collect();

        let err = labels_get(&pool, DatabaseBackend::Sqlite, &too_many)
            .await
            .unwrap_err();

        assert!(matches!(err, AtprotoError::Other(_)), "{err}");
    }
}

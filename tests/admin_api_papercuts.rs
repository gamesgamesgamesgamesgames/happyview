//! Admin API fixes from the docs field-check (HAP-307): clearing an API
//! client's overrides, the API client type, deleting a verification method
//! by its bare fragment, a duplicate service entry, and bulk dead-letter
//! actions that report each item.

mod common;

use axum::body::Body;
use axum::http::Method;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

use common::app::TestApp;

async fn send(app: &TestApp, method: Method, uri: &str, body: Option<Value>) -> (u16, Value) {
    let mut req = app.authed_request().method(method).uri(uri);
    let body = match body {
        Some(body) => {
            req = req.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&body).unwrap())
        }
        None => Body::empty(),
    };
    let resp = app
        .router
        .clone()
        .oneshot(req.body(body).unwrap())
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn client_body(name: &str, client_type: &str) -> Value {
    json!({
        "name": name,
        "client_id_url": format!("https://{name}.example.com/oauth-client-metadata.json"),
        "client_uri": format!("https://{name}.example.com"),
        "redirect_uris": [format!("https://{name}.example.com/callback")],
        "client_type": client_type,
    })
}

// --- API clients ------------------------------------------------------------

/// `null` clears a rate-limit override or the allowed origins; leaving a
/// field out keeps it. Before, `null` read as "unchanged".
#[tokio::test]
#[serial]
async fn null_clears_an_api_clients_overrides_and_absence_keeps_them() {
    common::require_db!();
    let app = TestApp::new().await;
    let mut body = client_body("overrides", "public");
    body["rate_limit_capacity"] = json!(500);
    body["rate_limit_refill_rate"] = json!(5.0);
    body["allowed_origins"] = json!(["https://overrides.example.com"]);
    let (status, created) = send(&app, Method::POST, "/admin/api-clients", Some(body)).await;
    assert_eq!(status, 201, "{created}");
    let uri = format!("/admin/api-clients/{}", created["id"].as_str().unwrap());

    let (status, body) = send(&app, Method::PUT, &uri, Some(json!({ "name": "renamed" }))).await;
    assert!((200..300).contains(&status), "{body}");
    let (_, got) = send(&app, Method::GET, &uri, None).await;
    assert_eq!(got["rate_limit_capacity"], 500, "absent keeps it: {got}");
    assert_eq!(got["rate_limit_refill_rate"], 5.0);
    assert_eq!(
        got["allowed_origins"],
        json!(["https://overrides.example.com"])
    );

    let (status, body) = send(
        &app,
        Method::PUT,
        &uri,
        Some(json!({
            "rate_limit_capacity": null,
            "rate_limit_refill_rate": null,
            "allowed_origins": null,
        })),
    )
    .await;
    assert!((200..300).contains(&status), "{body}");
    let (_, got) = send(&app, Method::GET, &uri, None).await;
    assert!(
        got["rate_limit_capacity"].is_null(),
        "null clears it: {got}"
    );
    assert!(got["rate_limit_refill_rate"].is_null(), "{got}");
    assert!(got["allowed_origins"].is_null(), "{got}");
    assert_eq!(got["name"], "renamed");
}

#[tokio::test]
#[serial]
async fn an_unknown_client_type_is_refused() {
    common::require_db!();
    let app = TestApp::new().await;
    let (status, body) = send(
        &app,
        Method::POST,
        "/admin/api-clients",
        Some(client_body("typo", "confidental")),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    for client_type in ["public", "confidential"] {
        let (status, body) = send(
            &app,
            Method::POST,
            "/admin/api-clients",
            Some(client_body(&format!("ok-{client_type}"), client_type)),
        )
        .await;
        assert_eq!(status, 201, "{client_type}: {body}");
    }
}

// --- Verification methods ---------------------------------------------------

/// The stored fragment keeps its `#`, which a path cannot carry unencoded:
/// the bare name and the encoded form both name it.
#[tokio::test]
#[serial]
async fn a_verification_method_is_deleted_by_its_bare_or_encoded_fragment() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    for (fragment, path) in [("#bare", "bare"), ("#encoded", "%23encoded")] {
        let (status, body) = send(
            &app,
            Method::POST,
            "/admin/verification-methods",
            Some(json!({ "fragment_id": fragment })),
        )
        .await;
        assert_eq!(status, 201, "{body}");
        let uri = format!("/admin/verification-methods/{path}");
        let (status, body) = send(&app, Method::DELETE, &uri, None).await;
        assert_eq!(status, 204, "{uri}: {body}");
        let (status, _) = send(&app, Method::DELETE, &uri, None).await;
        assert_eq!(status, 404, "{uri} is gone");
    }
}

// --- Service entries --------------------------------------------------------

#[tokio::test]
#[serial]
async fn a_duplicate_service_entry_fragment_is_a_conflict() {
    common::require_db!();
    let app = TestApp::new().await;
    let entry = json!({ "fragment_id": "#dup", "service_type": "DupAppView" });
    let (status, body) = send(
        &app,
        Method::POST,
        "/admin/service-entries",
        Some(entry.clone()),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let (status, body) = send(&app, Method::POST, "/admin/service-entries", Some(entry)).await;
    assert_eq!(status, 409, "{body}");
}

// --- Bulk dead-letter actions -----------------------------------------------

const DID: &str = "did:plc:deadletterbulk";
const COLLECTION: &str = "com.example.bulk";

/// A dead letter from the record-hook table, reindexable once the mock PLC
/// and PDS below answer for `DID`.
async fn seed_hook_dead_letter(app: &TestApp, id: &str, rkey: &str) {
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_dead_letter_hooks \
         (id, lexicon_id, uri, did, collection, rkey, action, record, error, attempts, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, 'create', '{}', 'seeded', 1, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(id)
        .bind(COLLECTION)
        .bind(format!("at://{DID}/{COLLECTION}/{rkey}"))
        .bind(DID)
        .bind(COLLECTION)
        .bind(rkey)
        .bind(happyview::db::now_rfc3339())
        .execute(&app.state.db)
        .await
        .expect("seed a dead letter");
}

async fn answer_for_did(app: &TestApp) {
    let pds = app.mock_server.uri();
    Mock::given(method("GET"))
        .and(path(format!("/{DID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": DID,
            "service": [{
                "id": "#atproto_pds",
                "type": "AtprotoPersonalDataServer",
                "serviceEndpoint": pds,
            }],
        })))
        .mount(&app.mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/xrpc/com.atproto.repo.getRecord"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "uri": format!("at://{DID}/{COLLECTION}/ok"),
            "cid": "bafyreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy",
            "value": { "$type": COLLECTION },
        })))
        .mount(&app.mock_server)
        .await;
}

/// One dead letter that cannot be acted on does not stop the rest: every
/// one is tried, in order, and the answer says which failed and why.
#[tokio::test]
#[serial]
async fn a_bulk_reindex_carries_on_past_a_failure_and_reports_each() {
    common::require_db!();
    let app = TestApp::new().await;
    answer_for_did(&app).await;
    seed_hook_dead_letter(&app, "dl-bulk-ok", "ok").await;

    let (status, body) = send(
        &app,
        Method::POST,
        "/admin/dead-letters/bulk/reindex",
        Some(json!({ "ids": ["dl-bulk-missing", "dl-bulk-ok", "dl-bulk-also-missing"] })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["ok"], false, "{body}");
    assert_eq!(body["succeeded"], 1, "{body}");
    assert_eq!(body["failed"], 2, "{body}");
    let results = body["results"].as_array().expect("results");
    let ids: Vec<&str> = results.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        ["dl-bulk-missing", "dl-bulk-ok", "dl-bulk-also-missing"]
    );
    assert_eq!(results[1]["ok"], true, "{body}");
    for failed in [&results[0], &results[2]] {
        assert_eq!(failed["ok"], false);
        assert!(
            failed["error"]
                .as_str()
                .is_some_and(|e| e.contains("not found")),
            "{failed}"
        );
    }

    // What succeeded is resolved: a second pass finds nothing to do for it.
    let (status, body) = send(
        &app,
        Method::POST,
        "/admin/dead-letters/bulk/reindex",
        Some(json!({ "ids": ["dl-bulk-ok"] })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["failed"], 1, "already resolved: {body}");
}

#[tokio::test]
#[serial]
async fn a_bulk_retry_reports_each_failure() {
    common::require_db!();
    let app = TestApp::new().await;
    let (status, body) = send(
        &app,
        Method::POST,
        "/admin/dead-letters/bulk/retry",
        Some(json!({ "ids": ["dl-retry-a", "dl-retry-b"] })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["ok"], false, "{body}");
    assert_eq!(body["failed"], 2, "both tried: {body}");
    assert_eq!(body["results"].as_array().map(Vec::len), Some(2));
}

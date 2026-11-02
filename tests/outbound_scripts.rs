//! Script requests through the screened clients: `http.*` names its own URL,
//! so `ALLOW_PRIVATE_HOSTS` reaches it, while `atproto.blob_download` goes to
//! an endpoint from a DID document and is screened like any other request. A
//! target of its own, because the policies are installed for the whole
//! process.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::outbound::{Policy, install_with_plugins};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::app::TestApp;
use common::fixtures;

const NSID: &str = "games.gamesgamesgamesgames.listGames";

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or(Value::Null)
}

async fn admin_post(app: &TestApp, uri: &str, body: Value) -> axum::response::Response {
    let cookie = app.admin_cookie();
    app.router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(cookie.0, cookie.1)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Bind `body` to the query and call it.
async fn run_query(app: &TestApp, body: &str) -> Value {
    let resp = admin_post(
        app,
        "/admin/scripts",
        json!({ "id": format!("xrpc.query:{NSID}"), "body": body }),
    )
    .await;
    assert!(resp.status().is_success(), "{:?}", json_body(resp).await);
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/xrpc/{NSID}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    json_body(resp).await
}

#[tokio::test]
async fn scripts_reach_listed_hosts_and_never_a_did_documents_private_endpoint() {
    let mut app = TestApp::new().await;
    // The shared client a serving instance gives its state, not the
    // unscreened one `TestApp` builds.
    app.state.http = happyview::http_retry::shared_client().clone();
    app.rebuild_router();

    let sidecar = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/hello"))
        .respond_with(ResponseTemplate::new(200).set_body_string("hi"))
        .mount(&sidecar)
        .await;
    let pds = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("blob"))
        .mount(&pds)
        .await;

    // As `install_for` does for `PLC_URL` at the mock directory and
    // `ALLOW_PRIVATE_HOSTS=localhost`.
    let plc = app.mock_server.uri();
    install_with_plugins(
        Policy::trusting([plc.as_str()]),
        Some(Policy::trusting([plc.as_str(), "localhost"])),
    );

    // The PDS is on the same address as the configured directory, on
    // another port.
    let did = "did:plc:privatepds";
    Mock::given(method("GET"))
        .and(path(format!("/{did}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixtures::did_document(did, &pds.uri())),
        )
        .mount(&app.mock_server)
        .await;

    let resp = admin_post(
        &app,
        "/admin/lexicons",
        json!({ "lexicon_json": fixtures::list_games_query_lexicon() }),
    )
    .await;
    assert!(resp.status().is_success());

    let port = sidecar.address().port();
    let out = run_query(
        &app,
        &format!(
            r#"function handle()
                local listed = http.get("http://localhost:{port}/hello")
                local blob_ok, blob_err = pcall(atproto.blob_download, "{did}", "bafkreiexample")
                return {{
                    listed = listed.status,
                    blob = blob_ok and "reached" or tostring(blob_err),
                }}
            end"#
        ),
    )
    .await;

    assert_eq!(out["listed"], 200, "{out}");
    let blob = out["blob"].as_str().unwrap_or_default();
    assert!(blob.contains("blob_download: request failed"), "{out}");

    assert_eq!(
        sidecar.received_requests().await.unwrap().len(),
        1,
        "the request to the listed name arrives"
    );
    assert!(
        pds.received_requests().await.unwrap().is_empty(),
        "nothing reaches the DID document's private endpoint"
    );
}

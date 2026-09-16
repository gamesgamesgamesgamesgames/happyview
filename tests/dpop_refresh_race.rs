mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::json;
use serial_test::serial;
use std::time::Duration;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn post_json_with_headers(
    uri: &str,
    body: &serde_json::Value,
    headers: Vec<(&str, &str)>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("host", "127.0.0.1");
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

async fn response_json(resp: axum::http::Response<Body>) -> serde_json::Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or(json!(null))
}

/// Count the surviving session and key rows for a DID.
async fn row_counts(app: &common::app::TestApp, did: &str) -> (i64, i64) {
    let sessions: (i64,) = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT COUNT(*) FROM happyview_dpop_sessions WHERE user_did = ?",
        app.state.db_backend,
    ))
    .bind(did)
    .fetch_one(&app.state.db)
    .await
    .unwrap();

    let keys: (i64,) = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT COUNT(*) FROM happyview_dpop_keys",
        app.state.db_backend,
    ))
    .fetch_one(&app.state.db)
    .await
    .unwrap();

    (sessions.0, keys.0)
}

/// Several requests hitting token expiry at once must not destroy the session.
#[tokio::test]
#[serial]
async fn concurrent_refresh_does_not_destroy_session() {
    common::require_db!();
    let app = common::app::TestApp::new_with_encryption().await;
    let (client_key, client_secret, client_id) = app.create_api_client("confidential", None).await;

    let did = "did:plc:racesubject";

    let auth_server = MockServer::start().await;
    let pds = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": auth_server.uri(),
            "token_endpoint": format!("{}/oauth/token", auth_server.uri()),
        })))
        .mount(&auth_server)
        .await;

    // The winner: one success, delayed so its database write lands last.
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(300))
                .set_body_json(json!({
                    "access_token": "rotated-access-token",
                    "refresh_token": "rotated-refresh-token",
                    "expires_in": 3600,
                    "token_type": "DPoP",
                })),
        )
        .up_to_n_times(1)
        .mount(&auth_server)
        .await;

    // Every replay of the spent refresh token.
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "Invalid refresh token",
        })))
        .mount(&auth_server)
        .await;

    // The PDS rejects the expired access token, which is what sends every
    // request into the refresh path in the first place.
    Mock::given(method("GET"))
        .and(path("/xrpc/com.example.race.get"))
        .respond_with(
            ResponseTemplate::new(401)
                .insert_header("www-authenticate", "DPoP error=\"invalid_token\"")
                .set_body_json(json!({"error": "invalid_token"})),
        )
        .mount(&pds)
        .await;

    // Provision a key and register a session pointing at both mocks.
    let key_req = post_json_with_headers(
        "/oauth/dpop-keys",
        &json!({}),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let key_resp = app.router.clone().oneshot(key_req).await.unwrap();
    assert_eq!(key_resp.status(), StatusCode::CREATED);
    let key_body = response_json(key_resp).await;
    let provision_id = key_body["provision_id"].as_str().unwrap().to_string();

    app.mock_session_verification(did, did).await;

    let session_req = post_json_with_headers(
        "/oauth/sessions",
        &json!({
            "provision_id": provision_id,
            "did": did,
            "access_token": "expired-access-token",
            "refresh_token": "original-refresh-token",
            "scopes": "atproto",
            "pds_url": pds.uri(),
            "issuer": auth_server.uri(),
        }),
        vec![
            ("x-client-key", &client_key),
            ("x-client-secret", &client_secret),
        ],
    );
    let session_resp = app.router.clone().oneshot(session_req).await.unwrap();
    assert_eq!(session_resp.status(), StatusCode::CREATED);

    let dpop_key_id: (String,) = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT dpop_key_id FROM happyview_dpop_sessions WHERE user_did = ?",
        app.state.db_backend,
    ))
    .bind(did)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let dpop_key_id = dpop_key_id.0;

    assert_eq!(row_counts(&app, did).await, (1, 1), "session should exist");

    // Six requests discover the expired token at the same instant.
    let encryption_key = app.state.config.token_encryption_key.unwrap();
    let mut tasks = Vec::new();
    for _ in 0..6 {
        let http = app.state.http.clone();
        let pool = app.state.db.clone();
        let backend = app.state.db_backend;
        let registry = app.state.oauth.clone();
        let plc_url = app.state.config.plc_url.clone();
        let client_id = client_id.clone();
        let did = did.to_string();
        let key_id = dpop_key_id.clone();
        tasks.push(tokio::spawn(async move {
            happyview::oauth::pds_write::dpop_pds_get(
                &http,
                &pool,
                backend,
                &encryption_key,
                &registry,
                &plc_url,
                &client_id,
                &did,
                &key_id,
                "com.example.race.get",
                "",
                &[],
            )
            .await
            .map(|r| r.status().as_u16())
        }));
    }

    let results: Vec<_> = futures::future::join_all(tasks).await;
    let outcomes: Vec<String> = results
        .into_iter()
        .map(|r| match r.unwrap() {
            Ok(status) => format!("http {status}"),
            Err(e) => format!("err {e}"),
        })
        .collect();

    let (sessions, keys) = row_counts(&app, did).await;
    assert_eq!(
        (sessions, keys),
        (1, 1),
        "a concurrent refresh must leave the session and its DPoP key intact; outcomes were {outcomes:?}"
    );

    let refreshes = auth_server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == "/oauth/token")
        .count();
    assert_eq!(
        refreshes, 1,
        "exactly one refresh should reach the token endpoint, saw {refreshes}"
    );
}

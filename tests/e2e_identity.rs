mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

use common::app::TestApp;

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

fn resolve_uri(identifier: &str) -> String {
    format!(
        "/admin/identity/resolve?identifier={}",
        urlencoding::encode(identifier)
    )
}

#[tokio::test]
#[serial]
async fn resolve_requires_authentication() {
    common::require_db!();
    let app = TestApp::new().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(resolve_uri("did:plc:abc"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[serial]
async fn resolving_a_did_without_a_document_is_a_bad_request() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.use_permissive_http_client();

    let cookie = app.admin_cookie();
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(resolve_uri("did:plc:nodocument"))
                .header(cookie.0, cookie.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // The reason names the DID but never the host that was contacted, so the
    // endpoint cannot be used to probe which hosts the server can reach.
    let body = json_body(resp).await.to_string();
    let mock_uri = app.mock_server.uri();
    let mock_host = mock_uri.trim_start_matches("http://");
    assert!(body.contains("did:plc:nodocument"), "{body}");
    assert!(!body.contains(mock_host), "{body}");
}

#[tokio::test]
#[serial]
async fn a_did_whose_handle_does_not_resolve_back_has_no_handle() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.use_permissive_http_client();

    // `.invalid` never resolves, so the claimed handle cannot be confirmed.
    Mock::given(method("GET"))
        .and(path("/did:plc:unverified"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "did:plc:unverified",
            "alsoKnownAs": ["at://claimed.invalid"],
            "verificationMethod": [],
            "service": []
        })))
        .mount(&app.mock_server)
        .await;

    let cookie = app.admin_cookie();
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(resolve_uri("did:plc:unverified"))
                .header(cookie.0, cookie.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["did"], "did:plc:unverified");
    assert!(body["handle"].is_null());
}

#[tokio::test]
#[serial]
async fn profile_adds_display_name_and_avatar_from_the_pds() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.use_permissive_http_client();
    let pds = app.mock_server.uri();

    Mock::given(method("GET"))
        .and(path("/did:plc:withprofile"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "did:plc:withprofile",
            "alsoKnownAs": ["at://claimed.invalid"],
            "verificationMethod": [],
            "service": [{
                "id": "#atproto_pds",
                "type": "AtprotoPersonalDataServer",
                "serviceEndpoint": pds
            }]
        })))
        .mount(&app.mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/xrpc/com.atproto.repo.getRecord"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "uri": "at://did:plc:withprofile/app.bsky.actor.profile/self",
            "value": {
                "$type": "app.bsky.actor.profile",
                "displayName": "With Profile",
                "avatar": {
                    "$type": "blob",
                    "ref": { "$link": "bafkreiavatar" },
                    "mimeType": "image/jpeg",
                    "size": 1234
                }
            }
        })))
        .mount(&app.mock_server)
        .await;

    let get = |uri: String| {
        let cookie = app.admin_cookie();
        let router = app.router.clone();
        async move {
            router
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header(cookie.0, cookie.1)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
        }
    };

    let resp = get(format!(
        "{}&profile=true",
        resolve_uri("did:plc:withprofile")
    ))
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["did"], "did:plc:withprofile");
    assert_eq!(body["display_name"], "With Profile");
    let avatar = body["avatar"].as_str().unwrap();
    assert!(avatar.starts_with(&pds), "{avatar}");
    assert!(avatar.contains("cid=bafkreiavatar"), "{avatar}");

    let body = json_body(get(resolve_uri("did:plc:withprofile")).await).await;
    assert!(body.get("display_name").is_none() && body.get("avatar").is_none());
}

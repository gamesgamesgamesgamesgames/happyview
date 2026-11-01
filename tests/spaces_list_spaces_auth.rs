mod common;

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;
use uuid::Uuid;

use common::app::TestApp;

fn rand_did(label: &str) -> String {
    format!("did:plc:{label}{}", Uuid::new_v4().simple())
}

fn cookie_for(app: &TestApp, did: &str) -> (HeaderName, HeaderValue) {
    common::auth::admin_cookie_header(did, &app.state.cookie_key)
}

async fn enable_spaces(app: &TestApp) {
    let (name, value) = app.admin_cookie();
    let req = Request::builder()
        .method("PUT")
        .uri("/admin/settings/feature.spaces_enabled")
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(json!({ "value": "true" }).to_string()))
        .unwrap();
    assert!(
        app.router
            .clone()
            .oneshot(req)
            .await
            .unwrap()
            .status()
            .is_success(),
        "failed to enable spaces"
    );
}

async fn create_space(app: &TestApp, authority: &str) -> String {
    let (name, value) = cookie_for(app, authority);
    let policy = json!({ "$type": "com.atproto.simplespace.defs#memberListPolicy" });
    let req = Request::builder()
        .method("POST")
        .uri("/xrpc/com.atproto.simplespace.createSpace")
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "type": "com.example.forum",
                "skey": format!("s{}", Uuid::new_v4().simple()),
                "readPolicy": policy,
                "writePolicy": policy,
                "appAccess": { "$type": "com.atproto.simplespace.defs#open" },
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert!(
        resp.status().is_success(),
        "createSpace failed: {}",
        resp.status()
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&body).unwrap();
    body["uri"]
        .as_str()
        .or_else(|| body["space"]["uri"].as_str())
        .unwrap_or_else(|| panic!("createSpace response has no uri: {body}"))
        .to_string()
}

/// Lists spaces as `cookie`'s account, returning the status and the listed URIs.
async fn list_spaces(
    app: &TestApp,
    method: &str,
    did: Option<&str>,
    cookie: (HeaderName, HeaderValue),
) -> (StatusCode, Vec<String>) {
    let uri = match did {
        Some(did) => format!("/xrpc/{method}?did={}", urlencoding::encode(did)),
        None => format!("/xrpc/{method}"),
    };
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .header(cookie.0, cookie.1)
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&body).unwrap_or(json!(null));
    let uris = body["spaces"]
        .as_array()
        .map(|spaces| {
            spaces
                .iter()
                .filter_map(|s| s["uri"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    (status, uris)
}

/// `did` names whose spaces are listed, and only the caller's own, or a super
/// admin, may name another account.
#[tokio::test]
#[serial]
async fn list_spaces_lists_another_account_only_for_a_super_admin() {
    common::require_db!();
    let app = TestApp::new().await;
    enable_spaces(&app).await;

    let owner = rand_did("owner");
    let stranger = rand_did("stranger");
    let space = create_space(&app, &owner).await;

    for method in [
        "com.atproto.space.listSpaces",
        "dev.happyview.space.listSpaces",
    ] {
        let (status, uris) = list_spaces(&app, method, None, cookie_for(&app, &owner)).await;
        assert_eq!(status, StatusCode::OK, "{method}: owner, own list");
        assert!(uris.contains(&space), "{method}: owner sees {space}");

        let (status, uris) =
            list_spaces(&app, method, Some(&owner), cookie_for(&app, &owner)).await;
        assert_eq!(status, StatusCode::OK, "{method}: owner naming itself");
        assert!(uris.contains(&space), "{method}: owner naming itself");

        let (status, uris) =
            list_spaces(&app, method, Some(&owner), cookie_for(&app, &stranger)).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{method}: a stranger naming the owner"
        );
        assert!(uris.is_empty(), "{method}: a refusal lists nothing");

        let (status, uris) = list_spaces(&app, method, None, cookie_for(&app, &stranger)).await;
        assert_eq!(status, StatusCode::OK, "{method}: stranger, own list");
        assert!(!uris.contains(&space), "{method}: stranger is no member");

        let (status, uris) = list_spaces(&app, method, Some(&owner), app.admin_cookie()).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{method}: super admin naming the owner"
        );
        assert!(uris.contains(&space), "{method}: super admin sees {space}");
    }
}

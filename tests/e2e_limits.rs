//! The instance limits a host sets, enforced (HAP-226): API clients counted
//! on create and on turning one back on, and XRPC without an API client
//! refused when the host says so.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview_provider::{Cap, FixedLimits, Gate, Limits, Wording};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

fn admin_json(
    method: &str,
    uri: &str,
    cookie: (axum::http::HeaderName, axum::http::HeaderValue),
    body: &Value,
) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(cookie.0, cookie.1)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

fn client(n: u32) -> Value {
    json!({
        "name": format!("App {n}"),
        "client_id_url": format!("https://app{n}.example/client-metadata.json"),
        "client_uri": format!("https://app{n}.example"),
        "redirect_uris": [format!("https://app{n}.example/callback")],
        "scopes": "atproto",
        "client_type": "public",
        "allowed_origins": [format!("https://app{n}.example")],
    })
}

fn limited(limits: Limits) -> Arc<FixedLimits> {
    Arc::new(FixedLimits::new(limits))
}

#[tokio::test]
#[serial]
async fn creating_an_api_client_past_the_limit_is_refused() {
    common::require_db!();
    let mut app = TestApp::new().await;
    app.state.limits = limited(Limits {
        api_clients: Some(Cap {
            max: 1,
            wording: Wording {
                message: Some("Your plan includes 1 API client.".into()),
                ..Wording::default()
            },
        }),
        ..Limits::default()
    });
    app.rebuild_router();

    let create = |n| admin_json("POST", "/admin/api-clients", app.admin_cookie(), &client(n));
    let resp = app.router.clone().oneshot(create(1)).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "hpy's built-in client isn't counted"
    );
    let first = json_body(resp).await;

    let resp = app.router.clone().oneshot(create(2)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = json_body(resp).await;
    assert_eq!(body["error"], "LimitReached");
    assert_eq!(body["limit"], "api_clients");
    assert_eq!(
        (body["max"].clone(), body["current"].clone()),
        (json!(1), json!(1))
    );
    assert_eq!(body["message"], "Your plan includes 1 API client.");

    // Turned off, it frees its place; turning it back on takes it again.
    let id = first["id"].as_str().unwrap();
    let patch = |active: bool| {
        admin_json(
            "PUT",
            &format!("/admin/api-clients/{id}"),
            app.admin_cookie(),
            &json!({ "is_active": active }),
        )
    };
    let resp = app.router.clone().oneshot(patch(false)).await.unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
    let resp = app.router.clone().oneshot(create(2)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp = app.router.clone().oneshot(patch(true)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(json_body(resp).await["error"], "LimitReached");
}

#[tokio::test]
#[serial]
async fn api_clients_are_unlimited_without_a_limit() {
    common::require_db!();
    let app = TestApp::new().await;
    for n in 1..=3 {
        let resp = app
            .router
            .clone()
            .oneshot(admin_json(
                "POST",
                "/admin/api-clients",
                app.admin_cookie(),
                &client(n),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
    }
}

#[tokio::test]
#[serial]
async fn xrpc_without_an_api_client_is_refused_when_the_host_says_so() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let resp = app
        .router
        .clone()
        .oneshot(admin_json(
            "POST",
            "/admin/api-clients",
            app.admin_cookie(),
            &client(1),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let key = json_body(resp).await["client_key"]
        .as_str()
        .unwrap()
        .to_string();

    app.state.limits = limited(Limits {
        anonymous_xrpc: Some(Gate {
            allowed: false,
            wording: Wording::default(),
        }),
        ..Limits::default()
    });
    app.rebuild_router();

    let get = |headers: &[(&str, &str)]| {
        let mut req = Request::builder().uri("/xrpc/com.example.nothing");
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        req.body(Body::empty()).unwrap()
    };

    let resp = app.router.clone().oneshot(get(&[])).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        json_body(resp).await,
        json!({
            "error": "LimitReached",
            "limit": "anonymous_xrpc",
            "message": "This instance only answers XRPC calls made with an API client.",
        })
    );

    // A key that doesn't check out is anonymous too.
    let resp = app
        .router
        .clone()
        .oneshot(get(&[("x-client-key", "hvc_unknown")]))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // A real client, from its own origin, gets past the limit to the method.
    let resp = app
        .router
        .clone()
        .oneshot(get(&[
            ("x-client-key", &key),
            ("origin", "https://app1.example"),
        ]))
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::FORBIDDEN);
    let body = json_body(resp).await;
    assert_ne!(body["error"], "LimitReached", "{body}");
}

#[tokio::test]
#[serial]
async fn xrpc_without_an_api_client_is_answered_by_default() {
    common::require_db!();
    let app = TestApp::new().await;
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/xrpc/com.example.nothing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::FORBIDDEN);
}

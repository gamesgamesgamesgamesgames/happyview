//! An API client's secrets through `/admin/api-clients/{id}/secrets`: several
//! at once, each revocable without touching the others, a revocation holding
//! at once on both paths a secret is checked on, and no secret ever
//! readable after the response that created it.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;

async fn send(app: &TestApp, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn admin(app: &TestApp, method: &str, uri: &str, body: Option<Value>) -> Request<Body> {
    let (name, value) = app.admin_cookie();
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(name, value);
    match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

/// A confidential client made through the admin API: its id, key and the
/// secret it was created with.
async fn confidential_client(app: &TestApp, name: &str) -> (String, String, String) {
    let (status, body) = send(
        app,
        admin(
            app,
            "POST",
            "/admin/api-clients",
            Some(json!({
                "name": name,
                "client_id_url": format!("https://{name}.example.com/oauth-client-metadata.json"),
                "client_uri": format!("https://{name}.example.com"),
                "redirect_uris": [format!("https://{name}.example.com/callback")],
                "scopes": "atproto",
                "client_type": "confidential",
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    (
        body["id"].as_str().unwrap().to_string(),
        body["client_key"].as_str().unwrap().to_string(),
        body["client_secret"].as_str().unwrap().to_string(),
    )
}

/// Whether `/oauth/dpop-keys` accepts `secret` for `key`: the path that
/// reads secrets from the database.
async fn oauth_accepts(app: &TestApp, key: &str, secret: &str) -> bool {
    let req = Request::builder()
        .method("POST")
        .uri("/oauth/dpop-keys")
        .header("content-type", "application/json")
        .header("x-client-key", key)
        .header("x-client-secret", secret)
        .body(Body::from("{}"))
        .unwrap();
    let (status, body) = send(app, req).await;
    match status {
        StatusCode::CREATED => true,
        StatusCode::UNAUTHORIZED => false,
        other => panic!("unexpected {other}: {body}"),
    }
}

async fn add_secret(app: &TestApp, client_id: &str, label: &str) -> (String, String) {
    let (status, body) = send(
        app,
        admin(
            app,
            "POST",
            &format!("/admin/api-clients/{client_id}/secrets"),
            Some(json!({ "label": label })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["label"], label.trim());
    (
        body["id"].as_str().unwrap().to_string(),
        body["client_secret"].as_str().unwrap().to_string(),
    )
}

async fn list(app: &TestApp, client_id: &str) -> Vec<Value> {
    let (status, body) = send(
        app,
        admin(
            app,
            "GET",
            &format!("/admin/api-clients/{client_id}/secrets"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["secrets"].as_array().unwrap().clone()
}

#[tokio::test]
#[serial]
async fn revoking_one_secret_stops_it_at_once_and_leaves_the_others() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let (client_id, key, original) = confidential_client(&app, "rotating").await;
    let (staging_id, staging) = add_secret(&app, &client_id, "staging").await;

    // Both work, on both paths a secret is checked on.
    assert!(oauth_accepts(&app, &key, &original).await);
    assert!(oauth_accepts(&app, &key, &staging).await);
    let original_id = app
        .state
        .rate_limiter
        .match_client_secret(&key, &original)
        .expect("the original secret is live");
    assert_eq!(
        app.state.rate_limiter.match_client_secret(&key, &staging),
        Some(staging_id.clone())
    );

    let (status, body) = send(
        &app,
        admin(
            &app,
            "DELETE",
            &format!("/admin/api-clients/{client_id}/secrets/{original_id}"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "revoked": true, "active_secrets": 1 }));

    // The revoked secret is refused by the very next request, with no wait
    // for a cache, and the other keeps working.
    assert!(!oauth_accepts(&app, &key, &original).await);
    assert_eq!(
        app.state.rate_limiter.match_client_secret(&key, &original),
        None
    );
    assert!(oauth_accepts(&app, &key, &staging).await);
    assert_eq!(
        app.state.rate_limiter.match_client_secret(&key, &staging),
        Some(staging_id)
    );

    // Revoking it again changes nothing and says so.
    let (status, body) = send(
        &app,
        admin(
            &app,
            "DELETE",
            &format!("/admin/api-clients/{client_id}/secrets/{original_id}"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "revoked": false, "active_secrets": 1 }));
}

#[tokio::test]
#[serial]
async fn the_listing_shows_labels_and_use_but_never_a_secret() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let (client_id, key, original) = confidential_client(&app, "listed").await;
    let (_, unused) = add_secret(&app, &client_id, "  prod  ").await;
    assert!(oauth_accepts(&app, &key, &original).await);

    let secrets = list(&app, &client_id).await;
    assert_eq!(secrets.len(), 2);
    assert_eq!(secrets[0]["label"], "Original");
    assert!(
        secrets[0]["last_used_at"].is_string(),
        "a secret that authenticated has a last use: {}",
        secrets[0]
    );
    assert_eq!(secrets[1]["label"], "prod", "the label is trimmed");
    assert!(secrets[1]["last_used_at"].is_null());
    assert!(secrets[1]["revoked_at"].is_null());

    let listed = serde_json::to_string(&secrets).unwrap();
    for leaked in [
        original.as_str(),
        unused.as_str(),
        &hex::encode(<sha2::Sha256 as sha2::Digest>::digest(original.as_bytes())),
        "hvs_",
        "secret_hash",
    ] {
        assert!(
            !listed.contains(leaked),
            "the listing carried {leaked:?}: {listed}"
        );
    }
}

#[tokio::test]
#[serial]
async fn a_client_whose_every_secret_is_revoked_can_be_given_a_new_one() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let (client_id, key, original) = confidential_client(&app, "emptied").await;
    let original_id = list(&app, &client_id).await[0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (_, body) = send(
        &app,
        admin(
            &app,
            "DELETE",
            &format!("/admin/api-clients/{client_id}/secrets/{original_id}"),
            None,
        ),
    )
    .await;
    assert_eq!(body["active_secrets"], 0);
    assert!(!oauth_accepts(&app, &key, &original).await);

    let (_, replacement) = add_secret(&app, &client_id, "replacement").await;
    assert!(oauth_accepts(&app, &key, &replacement).await);
    assert!(
        app.state
            .rate_limiter
            .match_client_secret(&key, &replacement)
            .is_some()
    );
}

#[tokio::test]
#[serial]
async fn a_secret_added_while_inactive_works_once_the_client_is_back() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let (client_id, key, _) = confidential_client(&app, "dormant").await;
    let deactivate = |active: bool| {
        admin(
            &app,
            "PUT",
            &format!("/admin/api-clients/{client_id}"),
            Some(json!({ "is_active": active })),
        )
    };

    let (status, _) = send(&app, deactivate(false)).await;
    assert!(status.is_success());
    let (_, added) = add_secret(&app, &client_id, "while away").await;
    assert_eq!(
        app.state.rate_limiter.match_client_secret(&key, &added),
        None
    );

    let (status, _) = send(&app, deactivate(true)).await;
    assert!(status.is_success());
    assert!(
        app.state
            .rate_limiter
            .match_client_secret(&key, &added)
            .is_some()
    );
    assert!(oauth_accepts(&app, &key, &added).await);
}

#[tokio::test]
#[serial]
async fn secrets_are_refused_where_they_do_not_apply() {
    common::require_db!();
    let app = TestApp::new_with_encryption().await;
    let (client_id, _, _) = confidential_client(&app, "owner").await;
    let (other_id, _, _) = confidential_client(&app, "other").await;
    let other_secret_id = list(&app, &other_id).await[0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Another client's secret cannot be revoked through this client.
    let (status, _) = send(
        &app,
        admin(
            &app,
            "DELETE",
            &format!("/admin/api-clients/{client_id}/secrets/{other_secret_id}"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(list(&app, &other_id).await[0]["revoked_at"].is_null());

    // A label has to say something.
    let (status, _) = send(
        &app,
        admin(
            &app,
            "POST",
            &format!("/admin/api-clients/{client_id}/secrets"),
            Some(json!({ "label": "   " })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A public client has no secrets to add.
    let (status, body) = send(
        &app,
        admin(
            &app,
            "POST",
            "/admin/api-clients",
            Some(json!({
                "name": "browser",
                "client_id_url": "https://browser.example.com/oauth-client-metadata.json",
                "client_uri": "https://browser.example.com",
                "redirect_uris": ["https://browser.example.com/callback"],
                "scopes": "atproto",
                "client_type": "public",
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let public_id = body["id"].as_str().unwrap();
    assert!(body["client_secret"].is_null());
    assert!(list(&app, public_id).await.is_empty());
    let (status, _) = send(
        &app,
        admin(
            &app,
            "POST",
            &format!("/admin/api-clients/{public_id}/secrets"),
            Some(json!({ "label": "nope" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // An unknown client is a 404 rather than an empty list.
    let (status, _) = send(
        &app,
        admin(
            &app,
            "GET",
            "/admin/api-clients/no-such-client/secrets",
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use serial_test::serial;
use sha2::{Digest, Sha256};
use tower::ServiceExt;

use common::app::TestApp;

const PLATFORM_KEY: &str = "hv_0123456789abcdef0123456789abcdef";

fn platform_key() -> (String, String) {
    let hash = hex::encode(Sha256::digest(PLATFORM_KEY.as_bytes()));
    (PLATFORM_KEY.to_string(), hash)
}

async fn app_with_platform_key() -> TestApp {
    let mut app = TestApp::new().await;
    app.state.config.platform_api_key_hash = Some(platform_key().1);
    app.rebuild_router();
    app
}

async fn json_body(resp: axum::response::Response) -> Value {
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

fn bearer(method: Method, uri: &str, token: &str, body: Option<&Value>) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"));
    match body {
        Some(b) => builder
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(b).unwrap()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

async fn seed_domain(app: &TestApp, id: &str, url: &str, is_primary: bool) {
    let now = happyview::db::now_rfc3339();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_domains (id, url, is_primary, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(id)
        .bind(url)
        .bind(if is_primary { 1i32 } else { 0i32 })
        .bind(&now)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .unwrap();
}

#[tokio::test]
#[serial]
async fn platform_key_can_list_domains() {
    common::require_db!();
    let app = app_with_platform_key().await;
    seed_domain(&app, "d1", "http://127.0.0.1:0", true).await;

    let resp = app
        .router
        .clone()
        .oneshot(bearer(Method::GET, "/admin/domains", PLATFORM_KEY, None))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body.as_array().unwrap().len(), 1);
}

#[tokio::test]
#[serial]
async fn platform_key_is_refused_outside_its_permission_set() {
    common::require_db!();
    let app = app_with_platform_key().await;

    for uri in [
        "/admin/users",
        "/admin/api-keys",
        "/admin/scripts",
        "/admin/jobs",
    ] {
        let resp = app
            .router
            .clone()
            .oneshot(bearer(Method::GET, uri, PLATFORM_KEY, None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "GET {uri}");
    }
}

#[tokio::test]
#[serial]
async fn platform_key_cannot_transfer_super() {
    common::require_db!();
    let app = app_with_platform_key().await;

    let resp = app
        .router
        .clone()
        .oneshot(bearer(
            Method::POST,
            "/admin/users/transfer-super",
            PLATFORM_KEY,
            Some(&json!({ "target_user_id": "anything" })),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
#[serial]
async fn platform_key_is_rejected_when_feature_is_off() {
    common::require_db!();
    let app = TestApp::new().await; // no hash configured

    let resp = app
        .router
        .clone()
        .oneshot(bearer(Method::GET, "/admin/domains", PLATFORM_KEY, None))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[serial]
async fn unknown_key_is_still_rejected_when_feature_is_on() {
    common::require_db!();
    let app = app_with_platform_key().await;

    let resp = app
        .router
        .clone()
        .oneshot(bearer(
            Method::GET,
            "/admin/domains",
            "hv_ffffffffffffffffffffffffffffffff",
            None,
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[serial]
async fn regular_api_key_still_works_when_feature_is_on() {
    common::require_db!();
    let app = app_with_platform_key().await;

    // Create a key as the seeded super admin, with settings:manage.
    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .method(Method::POST)
                .uri("/admin/api-keys")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "name": "ci",
                        "permissions": ["settings:manage"],
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let key = json_body(resp).await["key"].as_str().unwrap().to_string();

    let resp = app
        .router
        .clone()
        .oneshot(bearer(Method::GET, "/admin/domains", &key, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
#[serial]
async fn config_reports_platform_managed() {
    common::require_db!();
    let off = TestApp::new().await;
    let resp = off
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/config")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(json_body(resp).await["platform_managed"], false);
    // Release the first app before starting another: both migrate under the
    // same advisory lock, so a live `off` would block `on` forever.
    drop(off);

    let on = app_with_platform_key().await;
    let resp = on
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/config")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(json_body(resp).await["platform_managed"], true);
}

#[tokio::test]
#[serial]
async fn platform_key_is_refused_on_auth_only_routes() {
    common::require_db!();
    let app = app_with_platform_key().await;

    let cases = [
        (Method::GET, "/api/setup/rotation-key", None),
        (Method::POST, "/api/setup/complete", Some(json!({}))),
        (
            Method::GET,
            "/admin/identity/resolve?identifier=alice.test",
            None,
        ),
    ];
    for (method, uri, body) in cases {
        let resp = app
            .router
            .clone()
            .oneshot(bearer(method.clone(), uri, PLATFORM_KEY, body.as_ref()))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{method} {uri}");
    }
}

#[tokio::test]
#[serial]
async fn platform_key_can_manage_domains() {
    common::require_db!();
    let app = app_with_platform_key().await;
    seed_domain(&app, "d1", "http://127.0.0.1:0", true).await;

    let resp = app
        .router
        .clone()
        .oneshot(bearer(
            Method::POST,
            "/admin/domains",
            PLATFORM_KEY,
            Some(&json!({ "url": "https://two.example.com" })),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let new_id = json_body(resp).await["id"].as_str().unwrap().to_string();

    let resp = app
        .router
        .clone()
        .oneshot(bearer(
            Method::POST,
            &format!("/admin/domains/{new_id}/primary"),
            PLATFORM_KEY,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .router
        .clone()
        .oneshot(bearer(
            Method::DELETE,
            "/admin/domains/d1",
            PLATFORM_KEY,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
#[serial]
async fn platform_key_can_read_stats() {
    common::require_db!();
    let app = app_with_platform_key().await;

    let resp = app
        .router
        .clone()
        .oneshot(bearer(Method::GET, "/admin/stats", PLATFORM_KEY, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

async fn counts(app: &TestApp, token: &str) -> (StatusCode, Value) {
    let resp = app
        .router
        .clone()
        .oneshot(bearer(Method::GET, "/admin/stats/counts", token, None))
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

#[tokio::test]
#[serial]
async fn platform_key_can_read_counts() {
    common::require_db!();
    let app = app_with_platform_key().await;

    let (status, before) = counts(&app, PLATFORM_KEY).await;
    assert_eq!(status, StatusCode::OK);
    let users = before["users"].as_i64().expect("users is a number");
    let clients = before["api_clients"]
        .as_i64()
        .expect("api_clients is a number");

    let now = happyview::db::now_rfc3339();
    let user_sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_users (id, did, is_super, created_at) VALUES (?, ?, 0, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&user_sql)
        .bind("counts-extra-user")
        .bind("did:plc:countsextra")
        .bind(&now)
        .execute(&app.state.db)
        .await
        .unwrap();

    let client_sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_api_clients (id, client_key, client_secret_hash, name, client_id_url, client_uri, redirect_uris, scopes, client_type, is_active, created_by, created_at, updated_at, owner_did) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        app.state.db_backend,
    );
    // One active and one inactive: both count.
    for (n, active) in [(1, 1i32), (2, 0i32)] {
        happyview::db::query(&client_sql)
            .bind(format!("counts-client-{n}"))
            .bind(format!("hvc_counts{n}"))
            .bind("hash")
            .bind(format!("Counts {n}"))
            .bind(format!("https://counts{n}.example.com/metadata.json"))
            .bind(format!("https://counts{n}.example.com"))
            .bind("[]")
            .bind("atproto")
            .bind("confidential")
            .bind(active)
            .bind("did:plc:countsextra")
            .bind(&now)
            .bind(&now)
            .bind("did:plc:countsextra")
            .execute(&app.state.db)
            .await
            .unwrap();
    }

    let (status, after) = counts(&app, PLATFORM_KEY).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        after,
        json!({ "users": users + 1, "api_clients": clients + 2 })
    );
}

#[tokio::test]
#[serial]
async fn counts_refuses_a_caller_without_stats_read() {
    common::require_db!();
    let app = app_with_platform_key().await;

    let create = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .method(Method::POST)
                .uri("/admin/api-keys")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "name": "no-stats", "permissions": ["lexicons:create"] }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::CREATED);
    let key = json_body(create).await["key"]
        .as_str()
        .expect("api key returned")
        .to_string();

    let (status, _) = counts(&app, &key).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
#[serial]
async fn counts_refuses_an_unauthenticated_caller() {
    common::require_db!();
    let app = app_with_platform_key().await;

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/admin/stats/counts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

async fn supers(app: &TestApp) -> Vec<String> {
    let sql = happyview::db::adapt_sql(
        "SELECT did FROM happyview_users WHERE is_super = 1 ORDER BY did",
        app.state.db_backend,
    );
    happyview::db::query_as::<(String,)>(&sql)
        .fetch_all(&app.state.db)
        .await
        .unwrap()
        .into_iter()
        .map(|(d,)| d)
        .collect()
}

async fn put_super(app: &TestApp, token: &str, did: &str) -> axum::response::Response {
    app.router
        .clone()
        .oneshot(bearer(
            Method::PUT,
            "/admin/platform/super-user",
            token,
            Some(&json!({ "did": did })),
        ))
        .await
        .unwrap()
}

async fn delete_all_users(app: &TestApp) {
    for sql in [
        "DELETE FROM happyview_user_permissions",
        "DELETE FROM happyview_users",
    ] {
        happyview::db::query(sql)
            .execute(&app.state.db)
            .await
            .unwrap();
    }
}

#[tokio::test]
#[serial]
async fn super_user_is_created_on_an_empty_instance() {
    common::require_db!();
    let app = app_with_platform_key().await;
    delete_all_users(&app).await;

    let resp = put_super(&app, PLATFORM_KEY, "did:plc:owner").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json_body(resp).await["did"], "did:plc:owner");
    assert_eq!(supers(&app).await, vec!["did:plc:owner".to_string()]);
}

#[tokio::test]
#[serial]
async fn super_user_blocks_first_login_bootstrap() {
    common::require_db!();
    let app = app_with_platform_key().await;
    delete_all_users(&app).await;
    put_super(&app, PLATFORM_KEY, "did:plc:owner").await;

    // An interloper signing in afterwards must not become a user, let alone super.
    let interloper = common::auth::admin_cookie_header("did:plc:interloper", &app.state.cookie_key);
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/domains")
                .header(interloper.0, interloper.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(supers(&app).await, vec!["did:plc:owner".to_string()]);
}

#[tokio::test]
#[serial]
async fn super_user_is_idempotent() {
    common::require_db!();
    let app = app_with_platform_key().await;
    delete_all_users(&app).await;

    let first = json_body(put_super(&app, PLATFORM_KEY, "did:plc:owner").await).await;
    let second_resp = put_super(&app, PLATFORM_KEY, "did:plc:owner").await;
    assert_eq!(second_resp.status(), StatusCode::OK);
    let second = json_body(second_resp).await;

    assert_eq!(first["user_id"], second["user_id"]);
    assert_eq!(supers(&app).await, vec!["did:plc:owner".to_string()]);
}

#[tokio::test]
#[serial]
async fn super_user_transfers_from_existing_super() {
    common::require_db!();
    let app = app_with_platform_key().await; // seeds did:plc:testadmin as super

    let resp = put_super(&app, PLATFORM_KEY, "did:plc:newowner").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(supers(&app).await, vec!["did:plc:newowner".to_string()]);
}

#[tokio::test]
#[serial]
async fn super_user_promotes_existing_non_super_user() {
    common::require_db!();
    let app = app_with_platform_key().await;
    let now = happyview::db::now_rfc3339();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_users (id, did, is_super, created_at) VALUES (?, ?, 0, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind("existing-user-id")
        .bind("did:plc:member")
        .bind(&now)
        .execute(&app.state.db)
        .await
        .unwrap();

    let resp = put_super(&app, PLATFORM_KEY, "did:plc:member").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json_body(resp).await["user_id"], "existing-user-id");
    assert_eq!(supers(&app).await, vec!["did:plc:member".to_string()]);
}

#[tokio::test]
#[serial]
async fn super_user_rejects_non_did() {
    common::require_db!();
    let app = app_with_platform_key().await;
    let resp = put_super(&app, PLATFORM_KEY, "alice.example.com").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[serial]
async fn super_user_requires_platform_principal() {
    common::require_db!();
    let app = app_with_platform_key().await;

    // The seeded super admin, via cookie, is not the platform.
    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .method(Method::PUT)
                .uri("/admin/platform/super-user")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({ "did": "did:plc:x" })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(supers(&app).await, vec!["did:plc:testadmin".to_string()]);
}

#[tokio::test]
#[serial]
async fn platform_key_can_read_the_jetstream_status() {
    common::require_db!();
    let app = app_with_platform_key().await;

    let resp = app
        .router
        .clone()
        .oneshot(bearer(
            Method::GET,
            "/admin/jetstream/status",
            PLATFORM_KEY,
            None,
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert!(body["worker_queue_depths"].is_array());
    assert!(body["worker_busy_since_ms"].is_array());
}

#[tokio::test]
#[serial]
async fn the_jetstream_status_needs_stats_read() {
    common::require_db!();
    let app = TestApp::new().await;
    let (name, value) = app.admin_cookie();
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/api-keys")
                .header(name, value)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "name": "no-stats", "permissions": ["spaces:read"] }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let key = json_body(resp).await["key"].as_str().unwrap().to_string();

    let resp = app
        .router
        .clone()
        .oneshot(bearer(Method::GET, "/admin/jetstream/status", &key, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

/// A `settings:manage` key, made by the seeded super admin.
async fn settings_key(app: &TestApp) -> String {
    let resp = app
        .router
        .clone()
        .oneshot(
            app.authed_request()
                .method(Method::POST)
                .uri("/admin/api-keys")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "name": "settings",
                        "permissions": ["settings:manage"],
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    json_body(resp).await["key"].as_str().unwrap().to_string()
}

/// On a platform-managed instance the platform's list of domains is the one
/// that counts, so nobody else may change it: not a `settings:manage` key and
/// not the super user. Reading is unchanged.
#[tokio::test]
#[serial]
async fn only_the_platform_changes_domains_on_a_platform_managed_instance() {
    common::require_db!();
    let app = app_with_platform_key().await;
    seed_domain(&app, "d1", "http://127.0.0.1:0", true).await;
    seed_domain(&app, "d2", "https://two.example.com", false).await;
    let key = settings_key(&app).await;

    let send = |request: Request<Body>| {
        let router = app.router.clone();
        async move { router.oneshot(request).await.unwrap().status() }
    };
    let cookie_request = |method: Method, uri: &str, body: Option<Value>| {
        let builder = app.authed_request().method(method).uri(uri);
        match body {
            Some(b) => builder
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&b).unwrap()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        }
    };

    assert_eq!(
        send(bearer(Method::GET, "/admin/domains", &key, None)).await,
        StatusCode::OK
    );
    assert_eq!(
        send(cookie_request(Method::GET, "/admin/domains", None)).await,
        StatusCode::OK
    );

    let add = json!({ "url": "https://three.example.com" });
    for (label, status) in [
        (
            "key add",
            send(bearer(Method::POST, "/admin/domains", &key, Some(&add))).await,
        ),
        (
            "key primary",
            send(bearer(
                Method::POST,
                "/admin/domains/d2/primary",
                &key,
                None,
            ))
            .await,
        ),
        (
            "key delete",
            send(bearer(Method::DELETE, "/admin/domains/d2", &key, None)).await,
        ),
        (
            "super add",
            send(cookie_request(
                Method::POST,
                "/admin/domains",
                Some(add.clone()),
            ))
            .await,
        ),
        (
            "super primary",
            send(cookie_request(
                Method::POST,
                "/admin/domains/d2/primary",
                None,
            ))
            .await,
        ),
        (
            "super delete",
            send(cookie_request(Method::DELETE, "/admin/domains/d2", None)).await,
        ),
    ] {
        assert_eq!(status, StatusCode::FORBIDDEN, "{label}");
    }

    let rows: Vec<(String, i32)> = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT id, is_primary FROM happyview_domains ORDER BY id",
        app.state.db_backend,
    ))
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    assert_eq!(
        rows,
        [("d1".to_string(), 1), ("d2".to_string(), 0)],
        "a refused write changed the domains"
    );
}

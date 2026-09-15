//! `POST /external-auth/{plugin}/refresh` against the SDK-built `sdk_auth`
//! fixture, whose `refresh` handler mints `access-<refresh_token>` with a
//! one-hour `expires_in`.

mod common;

use chrono::{DateTime, Duration, Utc};
use happyview::external_auth::tokens::{delete_tokens, get_tokens, store_tokens};
use happyview::plugin::loader;
use serde_json::json;
use serial_test::serial;

use common::app::TestApp;

async fn app_with_sdk_auth() -> TestApp {
    let app = TestApp::new_with_encryption().await;
    let plugin = loader::load_from_file(std::path::Path::new("tests/fixtures/sdk_auth"))
        .await
        .expect("sdk_auth fixture not built. Run: cargo build --manifest-path tests/fixtures/sdk_auth/Cargo.toml --target wasm32-unknown-unknown --release");
    app.state.plugin_registry.install(plugin).await.unwrap();

    // The tokens table has a foreign key to `happyview_plugins`, and the test
    // app's registry is not database-backed, so the row is written by hand.
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_plugins (id, source, url, sha256, enabled, loaded_at, api_version, manifest)
         VALUES ('sdk_auth', 'file', 'tests/fixtures/sdk_auth', NULL, TRUE, ?, '2', NULL)
         ON CONFLICT (id) DO NOTHING",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(happyview::db::now_rfc3339())
        .execute(&app.state.db)
        .await
        .unwrap();

    // The test database is shared across runs, so start from no link.
    delete_tokens(
        &app.state.db,
        app.state.db_backend,
        &app.admin_did,
        "sdk_auth",
    )
    .await
    .unwrap();
    app
}

async fn store(app: &TestApp, refresh_token: Option<&str>, expires_at: Option<&str>) {
    store_tokens(
        &app.state.db,
        app.state.db_backend,
        app.state.config.token_encryption_key.as_ref(),
        &app.admin_did,
        "sdk_auth",
        "acct-1",
        "old-access",
        refresh_token,
        Some("Bearer"),
        Some("read"),
        expires_at,
    )
    .await
    .unwrap();
}

async fn stored(app: &TestApp) -> happyview::external_auth::tokens::StoredTokens {
    get_tokens(
        &app.state.db,
        app.state.db_backend,
        app.state.config.token_encryption_key.as_ref(),
        &app.admin_did,
        "sdk_auth",
    )
    .await
    .unwrap()
}

fn rfc3339(offset: Duration) -> String {
    (Utc::now() + offset).to_rfc3339()
}

#[tokio::test]
#[serial]
async fn expired_token_with_refresh_token_is_exchanged() {
    let app = app_with_sdk_auth().await;
    store(&app, Some("r1"), Some(&rfc3339(-Duration::hours(1)))).await;

    let (status, body) = app
        .post_json_status("/external-auth/sdk_auth/refresh", json!({}))
        .await;

    assert_eq!(status, 200, "{body}");
    assert_eq!(body["refreshed"], json!(true));
    let expires_at = DateTime::parse_from_rfc3339(body["expires_at"].as_str().unwrap()).unwrap();
    assert!(expires_at > Utc::now() + Duration::minutes(30));

    let tokens = stored(&app).await;
    assert_eq!(tokens.access_token, "access-r1");
    assert_eq!(tokens.refresh_token.as_deref(), Some("r1"));
    assert_eq!(tokens.scope.as_deref(), Some("read"));
    assert_eq!(tokens.account_id, "acct-1");
    assert_eq!(tokens.expires_at.as_deref(), body["expires_at"].as_str());

    let accounts = app.get_json("/external-auth/accounts").await;
    assert_eq!(accounts[0]["plugin_id"], "sdk_auth");
    assert_eq!(accounts[0]["expires_at"], body["expires_at"]);
}

#[tokio::test]
#[serial]
async fn unexpired_token_is_left_alone() {
    let app = app_with_sdk_auth().await;
    let expires_at = rfc3339(Duration::hours(1));
    store(&app, Some("r1"), Some(&expires_at)).await;

    let (status, body) = app
        .post_json_status("/external-auth/sdk_auth/refresh", json!({}))
        .await;

    assert_eq!(status, 200, "{body}");
    assert_eq!(body["refreshed"], json!(false));
    assert_eq!(body["expires_at"].as_str(), Some(expires_at.as_str()));

    let tokens = stored(&app).await;
    assert_eq!(tokens.access_token, "old-access");
    assert_eq!(tokens.expires_at.as_deref(), Some(expires_at.as_str()));
}

#[tokio::test]
#[serial]
async fn expired_token_without_refresh_token_conflicts() {
    let app = app_with_sdk_auth().await;
    store(&app, None, Some(&rfc3339(-Duration::hours(1)))).await;

    let (status, body) = app
        .post_json_status("/external-auth/sdk_auth/refresh", json!({}))
        .await;

    assert_eq!(status, 409, "{body}");
    assert!(body["error"].as_str().unwrap().contains("relink"), "{body}");
    assert_eq!(stored(&app).await.access_token, "old-access");
}

#[tokio::test]
#[serial]
async fn unlinked_account_is_not_found() {
    let app = app_with_sdk_auth().await;

    let (status, body) = app
        .post_json_status("/external-auth/sdk_auth/refresh", json!({}))
        .await;

    assert_eq!(status, 404, "{body}");
}

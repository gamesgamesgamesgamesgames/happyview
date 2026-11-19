//! The linked-repo keep-alive leaves a grant active through an outage at its
//! authorization server, and flags it for reauthorization only on a refusal.

mod common;

use atrium_api::types::string::Did;
use atrium_common::store::Store;
use happyview::linked_repos::worker::{RefreshOutcome, refresh_grant};
use happyview::linked_repos::{db, types};
use serde_json::json;
use serial_test::serial;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

use common::app::TestApp;

const ACCOUNT: &str = "did:plc:linkedrefresh";

/// An active grant for [`ACCOUNT`] whose OAuth session names the mock server
/// as its authorization server. (A grant with no session needs
/// reauthorization: `e2e_linked_repos::refresh_marks_needs_reauth_when_no_session_exists`.)
async fn active_grant(app: &TestApp) -> types::LinkedRepo {
    active_grant_sealed_with(app, common::db::TEST_ENCRYPTION_KEY).await
}

/// [`active_grant`], its session sealed with `key`.
async fn active_grant_sealed_with(app: &TestApp, seal_key: [u8; 32]) -> types::LinkedRepo {
    let grant = db::create(&app.state, None, None, None, "atproto", "did:plc:admin")
        .await
        .unwrap();
    db::bind_did(&app.state, &grant.id, ACCOUNT, Some("linked.test"))
        .await
        .unwrap();

    let base = app.mock_server.uri();
    let key = happyview::oauth::client_keys::generate_client_key("linked-refresh").unwrap();
    let dpop_key = happyview::oauth::client_keys::to_atrium_jwk(&key)
        .unwrap()
        .key;
    let token_set: atrium_oauth::TokenSet = serde_json::from_value(json!({
        "iss": base,
        "sub": ACCOUNT,
        "aud": format!("{base}/pds"),
        "scope": "atproto",
        "refresh_token": "linked-refresh-token",
        "access_token": "linked-access-token",
        "token_type": "DPoP",
        "expires_at": null,
    }))
    .unwrap();
    happyview::auth::oauth_store::DbSessionStore::new_with_table(
        app.state.db.clone(),
        app.state.db_backend,
        happyview::linked_repos::client::LINKED_SESSIONS_TABLE,
        Some(seal_key.into()),
    )
    .set(
        Did::new(ACCOUNT.to_string()).unwrap(),
        atrium_oauth::store::session::Session {
            dpop_key,
            token_set,
        },
    )
    .await
    .unwrap();

    db::get(&app.state, &grant.id).await.unwrap().unwrap()
}

fn metadata(base: &str) -> serde_json::Value {
    json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/oauth/authorize"),
        "token_endpoint": format!("{base}/oauth/token"),
        "scopes_supported": ["atproto"],
        "response_types_supported": ["code"],
    })
}

/// A 502 from the authorization server leaves the grant active with the error
/// recorded; the next attempt succeeds and clears it.
#[tokio::test]
#[serial]
async fn a_502_then_success_keeps_the_grant_active() {
    common::require_db!();
    let app = TestApp::new().await;
    let grant = active_grant(&app).await;
    metadata_failing(&app, 1).await;

    assert_eq!(
        refresh_grant(&app.state, &grant).await.unwrap(),
        RefreshOutcome::Transient
    );
    let after_outage = db::get(&app.state, &grant.id).await.unwrap().unwrap();
    assert_eq!(after_outage.status, types::STATUS_ACTIVE);
    assert!(after_outage.last_error.is_some());

    assert_eq!(
        refresh_grant(&app.state, &after_outage).await.unwrap(),
        RefreshOutcome::Refreshed
    );
    let recovered = db::get(&app.state, &grant.id).await.unwrap().unwrap();
    assert_eq!(recovered.status, types::STATUS_ACTIVE);
    assert_eq!(recovered.last_error, None);
    assert!(recovered.last_refreshed_at.is_some());
}

/// Metadata that answers 502 `failures` times, then 200.
async fn metadata_failing(app: &TestApp, failures: u64) {
    let base = app.mock_server.uri();
    if failures > 0 {
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-authorization-server"))
            .respond_with(ResponseTemplate::new(502))
            .up_to_n_times(failures)
            .with_priority(1)
            .mount(&app.mock_server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(metadata(&base)))
        .with_priority(2)
        .mount(&app.mock_server)
        .await;
}

/// A grant that has failed transiently for over a week is given up on.
#[tokio::test]
#[serial]
async fn a_grant_failing_for_over_a_week_needs_reauthorization() {
    common::require_db!();
    let app = TestApp::new().await;
    let grant = active_grant(&app).await;
    let eight_days_ago = (chrono::Utc::now() - chrono::Duration::days(8)).to_rfc3339();
    let sql = happyview::db::adapt_sql(
        "UPDATE happyview_linked_repos SET last_refreshed_at = ? WHERE id = ?",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(&eight_days_ago)
        .bind(&grant.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    let grant = db::get(&app.state, &grant.id).await.unwrap().unwrap();
    metadata_failing(&app, 1).await;

    assert_eq!(
        refresh_grant(&app.state, &grant).await.unwrap(),
        RefreshOutcome::NeedsReauth
    );
    let flagged = db::get(&app.state, &grant.id).await.unwrap().unwrap();
    assert_eq!(flagged.status, types::STATUS_NEEDS_REAUTH);
    assert!(flagged.last_error.unwrap().contains("7 days"));
}

/// A session sealed with a key this instance no longer holds can't be
/// restored by retrying.
#[tokio::test]
#[serial]
async fn a_session_that_no_longer_decrypts_needs_reauthorization() {
    common::require_db!();
    let app = TestApp::new().await;
    let grant = active_grant_sealed_with(&app, [0x07u8; 32]).await;
    metadata_failing(&app, 0).await;

    assert_eq!(
        refresh_grant(&app.state, &grant).await.unwrap(),
        RefreshOutcome::NeedsReauth
    );
    let flagged = db::get(&app.state, &grant.id).await.unwrap().unwrap();
    assert_eq!(flagged.status, types::STATUS_NEEDS_REAUTH);
}

/// Using a grant during an outage answers 502 and leaves it active; the next
/// use that restores clears the error.
#[tokio::test]
#[serial]
async fn using_a_grant_through_an_outage_keeps_it_active() {
    common::require_db!();
    let app = TestApp::new().await;
    let grant = active_grant(&app).await;
    metadata_failing(&app, 1).await;

    let err = match happyview::linked_repos::pds::session_for(&app.state, &grant).await {
        Err(e) => e,
        Ok(_) => panic!("the first restore should fail"),
    };
    assert!(
        matches!(err, happyview::error::AppError::BadGateway(_)),
        "{err:?}"
    );
    let after_outage = db::get(&app.state, &grant.id).await.unwrap().unwrap();
    assert_eq!(after_outage.status, types::STATUS_ACTIVE);
    assert!(after_outage.last_error.is_some());

    assert!(
        happyview::linked_repos::pds::session_for(&app.state, &after_outage)
            .await
            .is_ok()
    );
    let recovered = db::get(&app.state, &grant.id).await.unwrap().unwrap();
    assert_eq!(recovered.last_error, None);
}

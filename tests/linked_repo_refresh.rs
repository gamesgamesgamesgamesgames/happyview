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
        Some(common::db::TEST_ENCRYPTION_KEY.into()),
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
    let base = app.mock_server.uri();

    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(502))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&app.mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(metadata(&base)))
        .with_priority(2)
        .mount(&app.mock_server)
        .await;

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

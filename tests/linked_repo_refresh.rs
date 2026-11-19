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
    token_endpoint(&app, rotated_tokens()).await;

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

/// A token endpoint that answers every refresh with `response`.
async fn token_endpoint(app: &TestApp, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(response)
        .mount(&app.mock_server)
        .await;
}

fn rotated_tokens() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "access_token": "linked-access-token-2",
        "refresh_token": "linked-refresh-token-2",
        "token_type": "DPoP",
        "expires_in": 3600,
        "scope": "atproto",
        "sub": ACCOUNT,
    }))
}

async fn stored_tokens(app: &TestApp) -> (String, Option<String>) {
    let session = happyview::auth::oauth_store::DbSessionStore::new_with_table(
        app.state.db.clone(),
        app.state.db_backend,
        happyview::linked_repos::client::LINKED_SESSIONS_TABLE,
        Some(common::db::TEST_ENCRYPTION_KEY.into()),
    )
    .get(&Did::new(ACCOUNT.to_string()).unwrap())
    .await
    .unwrap()
    .expect("the session is still stored");
    (
        session.token_set.access_token,
        session.token_set.refresh_token,
    )
}

/// The keep-alive refreshes at the token endpoint, and stores the rotated
/// tokens.
#[tokio::test]
#[serial]
async fn a_refresh_stores_the_rotated_tokens() {
    common::require_db!();
    let app = TestApp::new().await;
    let grant = active_grant(&app).await;
    metadata_failing(&app, 0).await;
    token_endpoint(&app, rotated_tokens()).await;

    assert_eq!(
        refresh_grant(&app.state, &grant).await.unwrap(),
        RefreshOutcome::Refreshed
    );
    assert_eq!(
        stored_tokens(&app).await,
        (
            "linked-access-token-2".to_string(),
            Some("linked-refresh-token-2".to_string())
        )
    );
    let refreshed = db::get(&app.state, &grant.id).await.unwrap().unwrap();
    assert_eq!(refreshed.status, types::STATUS_ACTIVE);
    assert_eq!(refreshed.last_error, None);

    let requests = app.mock_server.received_requests().await.unwrap();
    let refresh = requests
        .iter()
        .find(|r| r.url.path() == "/oauth/token")
        .expect("the keep-alive called the token endpoint");
    let form = String::from_utf8_lossy(&refresh.body);
    assert!(form.contains("grant_type=refresh_token"), "{form}");
    assert!(
        form.contains("refresh_token=linked-refresh-token"),
        "{form}"
    );
    assert!(refresh.headers.get("dpop").is_some());
}

/// A refresh the authorization server refuses needs reauthorization.
#[tokio::test]
#[serial]
async fn an_invalid_grant_on_refresh_needs_reauthorization() {
    common::require_db!();
    let app = TestApp::new().await;
    let grant = active_grant(&app).await;
    metadata_failing(&app, 0).await;
    token_endpoint(
        &app,
        ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "refresh token revoked",
        })),
    )
    .await;

    assert_eq!(
        refresh_grant(&app.state, &grant).await.unwrap(),
        RefreshOutcome::NeedsReauth
    );
    let flagged = db::get(&app.state, &grant.id).await.unwrap().unwrap();
    assert_eq!(flagged.status, types::STATUS_NEEDS_REAUTH);
    assert!(flagged.last_error.unwrap().contains("invalid_grant"));
}

/// A token endpoint outage leaves the grant active and the session as it was.
#[tokio::test]
#[serial]
async fn a_token_endpoint_outage_keeps_the_grant_active() {
    common::require_db!();
    let app = TestApp::new().await;
    let grant = active_grant(&app).await;
    metadata_failing(&app, 0).await;
    token_endpoint(&app, ResponseTemplate::new(503)).await;

    assert_eq!(
        refresh_grant(&app.state, &grant).await.unwrap(),
        RefreshOutcome::Transient
    );
    let kept = db::get(&app.state, &grant.id).await.unwrap().unwrap();
    assert_eq!(kept.status, types::STATUS_ACTIVE);
    assert!(kept.last_error.is_some());
    assert_eq!(
        stored_tokens(&app).await,
        (
            "linked-access-token".to_string(),
            Some("linked-refresh-token".to_string())
        )
    );
}

/// Make `app` a confidential client at a public URL, signing with a fresh
/// instance key, which it answers.
async fn confidential(app: &mut TestApp) -> happyview::oauth::client_keys::ClientKey {
    let public_url = "https://happyview.example";
    app.state.config.public_url = public_url.to_string();
    let key = happyview::oauth::client_keys::ensure_instance_key(
        &app.state.db,
        app.state.db_backend,
        app.state.config.keyring().as_ref(),
    )
    .await
    .unwrap();
    let client = happyview::linked_repos::client::build(
        &app.state.config.plc_url,
        &format!("{public_url}/oauth-client-metadata.json"),
        public_url,
        format!("{public_url}/auth/callback"),
        false,
        vec![atrium_oauth::Scope::Known(
            atrium_oauth::KnownScope::Atproto,
        )],
        app.state.oauth_state_store.clone(),
        app.state.db.clone(),
        app.state.db_backend,
        Some(vec![
            happyview::oauth::client_keys::to_atrium_jwk(&key).unwrap(),
        ]),
        app.state.config.keyring(),
    )
    .unwrap();
    app.state
        .oauth
        .set_linked_repos_primary(std::sync::Arc::new(client), Some(key.kid.clone()));
    key
}

/// A confidential client signs its refresh with the instance key.
#[tokio::test]
#[serial]
async fn a_confidential_refresh_carries_a_client_assertion() {
    common::require_db!();
    let mut app = TestApp::new().await;
    confidential(&mut app).await;
    let grant = active_grant(&app).await;
    metadata_failing(&app, 0).await;
    token_endpoint(&app, rotated_tokens()).await;

    assert_eq!(
        refresh_grant(&app.state, &grant).await.unwrap(),
        RefreshOutcome::Refreshed
    );
    let requests = app.mock_server.received_requests().await.unwrap();
    let refresh = requests
        .iter()
        .find(|r| r.url.path() == "/oauth/token")
        .expect("the keep-alive called the token endpoint");
    let form = String::from_utf8_lossy(&refresh.body);
    assert!(form.contains("client_assertion_type="), "{form}");
    assert!(form.contains("client_assertion=ey"), "{form}");
}

/// The form fields of a token request.
fn form(request: &wiremock::Request) -> std::collections::HashMap<String, String> {
    url::form_urlencoded::parse(&request.body)
        .into_owned()
        .collect()
}

/// A provider that treats a refresh token used twice as a replay, as the
/// reference provider does: the first use rotates, any later one is refused.
struct ReplayEnforcing {
    used: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl wiremock::Respond for ReplayEnforcing {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let token = form(request).remove("refresh_token").unwrap_or_default();
        let fresh = self.used.lock().unwrap().insert(token);
        let response = if fresh {
            rotated_tokens()
        } else {
            ResponseTemplate::new(400).set_body_json(json!({
                "error": "invalid_grant",
                "error_description": "refresh token replayed",
            }))
        };
        // Long enough for a second refresh to start while this one is out.
        response.set_delay(std::time::Duration::from_millis(300))
    }
}

/// Two refreshes at once use the refresh token once: the second finds the
/// session held, so the provider sees no replay and the grant survives.
#[tokio::test]
#[serial]
async fn racing_refreshes_use_the_refresh_token_once() {
    common::require_db!();
    let app = TestApp::new().await;
    let grant = active_grant(&app).await;
    metadata_failing(&app, 0).await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ReplayEnforcing {
            used: Default::default(),
        })
        .mount(&app.mock_server)
        .await;

    let (first, second) = tokio::join!(
        refresh_grant(&app.state, &grant),
        refresh_grant(&app.state, &grant),
    );
    let mut outcomes = vec![first.unwrap(), second.unwrap()];
    outcomes.sort_by_key(|o| format!("{o:?}"));
    assert_eq!(
        outcomes,
        vec![RefreshOutcome::InProgress, RefreshOutcome::Refreshed]
    );

    let token_requests = app
        .mock_server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == "/oauth/token")
        .count();
    assert_eq!(token_requests, 1);
    let kept = db::get(&app.state, &grant.id).await.unwrap().unwrap();
    assert_eq!(kept.status, types::STATUS_ACTIVE);
    assert_eq!(
        stored_tokens(&app).await,
        (
            "linked-access-token-2".to_string(),
            Some("linked-refresh-token-2".to_string())
        )
    );
}

/// The stored session's JSON.
async fn stored_session(app: &TestApp) -> serde_json::Value {
    let stored = happyview::auth::oauth_store::DbSessionStore::new_with_table(
        app.state.db.clone(),
        app.state.db_backend,
        happyview::linked_repos::client::LINKED_SESSIONS_TABLE,
        Some(common::db::TEST_ENCRYPTION_KEY.into()),
    )
    .read_json(ACCOUNT)
    .await
    .unwrap()
    .expect("the session is still stored");
    serde_json::from_str(&stored.json).unwrap()
}

/// A failed refresh gives the session back as it was, lease and all.
#[tokio::test]
#[serial]
async fn a_failed_refresh_releases_the_session() {
    common::require_db!();
    let app = TestApp::new().await;
    let grant = active_grant(&app).await;
    let before = stored_session(&app).await;
    metadata_failing(&app, 0).await;
    token_endpoint(&app, ResponseTemplate::new(503)).await;

    assert_eq!(
        refresh_grant(&app.state, &grant).await.unwrap(),
        RefreshOutcome::Transient
    );
    let after = stored_session(&app).await;
    assert_eq!(after, before);
    assert!(after.get("refresh_lease_until").is_none());
}

/// A refresh that answers for the wrong account, for no account, or with a
/// bearer token is refused.
#[tokio::test]
#[serial]
async fn a_refresh_answer_for_another_account_or_token_type_is_refused() {
    common::require_db!();
    for answer in [
        json!({ "access_token": "a", "token_type": "DPoP", "sub": "did:plc:someoneelse" }),
        json!({ "access_token": "a", "token_type": "DPoP" }),
        json!({ "access_token": "a", "token_type": "Bearer", "sub": ACCOUNT }),
    ] {
        let app = TestApp::new().await;
        let grant = active_grant(&app).await;
        metadata_failing(&app, 0).await;
        token_endpoint(
            &app,
            ResponseTemplate::new(200).set_body_json(answer.clone()),
        )
        .await;

        assert_eq!(
            refresh_grant(&app.state, &grant).await.unwrap(),
            RefreshOutcome::NeedsReauth,
            "{answer}"
        );
    }
}

fn jwt_parts(jwt: &str) -> (serde_json::Value, serde_json::Value) {
    use base64::Engine;
    let decode = |part: &str| -> serde_json::Value {
        serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(part)
                .unwrap(),
        )
        .unwrap()
    };
    let mut parts = jwt.split('.');
    (decode(parts.next().unwrap()), decode(parts.next().unwrap()))
}

/// A DPoP nonce challenge is answered with a new proof and a new client
/// assertion, each naming this client, the issuer and the signing key.
#[tokio::test]
#[serial]
async fn a_nonce_retry_signs_a_fresh_assertion() {
    common::require_db!();
    let mut app = TestApp::new().await;
    let key = confidential(&mut app).await;
    let grant = active_grant(&app).await;
    metadata_failing(&app, 0).await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(
            ResponseTemplate::new(400)
                .insert_header("DPoP-Nonce", "nonce-1")
                .set_body_json(json!({ "error": "use_dpop_nonce" })),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&app.mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(rotated_tokens())
        .with_priority(2)
        .mount(&app.mock_server)
        .await;

    assert_eq!(
        refresh_grant(&app.state, &grant).await.unwrap(),
        RefreshOutcome::Refreshed
    );

    let attempts: Vec<_> = app
        .mock_server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == "/oauth/token")
        .collect();
    assert_eq!(attempts.len(), 2);

    let (_, retried_proof) = jwt_parts(attempts[1].headers.get("dpop").unwrap().to_str().unwrap());
    assert_eq!(retried_proof["nonce"], "nonce-1");

    let issuer = app.mock_server.uri();
    let mut jtis = Vec::new();
    for attempt in &attempts {
        let fields = form(attempt);
        let client_id = &fields["client_id"];
        assert_eq!(
            client_id,
            "https://happyview.example/oauth-client-metadata.json"
        );
        let (header, claims) = jwt_parts(&fields["client_assertion"]);
        assert_eq!(header["kid"], key.kid.as_str());
        assert_eq!(claims["iss"], client_id.as_str());
        assert_eq!(claims["sub"], client_id.as_str());
        assert_eq!(claims["aud"], issuer.as_str());
        assert!(claims["exp"].as_i64().unwrap() > chrono::Utc::now().timestamp());
        jtis.push(claims["jti"].as_str().unwrap().to_string());
    }
    assert_ne!(jtis[0], jtis[1]);
}

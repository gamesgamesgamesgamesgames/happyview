//! Submitting the PLC operation for an attached account, against a PDS that
//! answers `submitPlcOperation` with a body. The lexicon declares no output, so
//! a 2xx is success whatever it carries; an error the PDS returns still fails.

mod common;

use atrium_api::types::string::Did;
use atrium_common::store::Store;
use axum::body::Body;
use axum::http::StatusCode;
use serde_json::json;
use serial_test::serial;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

use common::app::TestApp;

const ACCOUNT: &str = "did:plc:attachedplcsubmit";

/// How the mock PDS answers `submitPlcOperation`.
enum Answer {
    /// `200` with `{}` as `application/json`, as Tranquil answers.
    EmptyObject,
    /// `200` with an empty body labelled `application/json`.
    EmptyJsonBody,
    /// `200` with no body and no content type, as the reference PDS answers.
    NoBody,
    /// A `400` XRPC error.
    Error,
    /// A `401` for an expired access token.
    Expired,
}

/// An attached account whose stored OAuth session points at the mock server,
/// which serves its authorization server, its PDS and the PLC directory.
async fn attached_account(app: &TestApp, answer: Answer) {
    let mock = &app.mock_server;
    let base = mock.uri();

    happyview::service_identity::upsert_identity(
        &app.state.db,
        app.state.db_backend,
        &happyview::service_identity::IdentityMode::AttachAccount,
        None,
        None,
        None,
        Some(ACCOUNT),
    )
    .await
    .unwrap();

    let key = happyview::oauth::client_keys::generate_client_key("plc-submit-test").unwrap();
    let dpop_key = happyview::oauth::client_keys::to_atrium_jwk(&key)
        .unwrap()
        .key;
    let token_set: atrium_oauth::TokenSet = serde_json::from_value(json!({
        "iss": base,
        "sub": ACCOUNT,
        "aud": format!("{base}/pds"),
        "scope": "atproto identity:*",
        "refresh_token": null,
        "access_token": "plc-submit-access-token",
        "token_type": "DPoP",
        "expires_at": null,
    }))
    .unwrap();
    happyview::auth::oauth_store::DbSessionStore::new(
        app.state.db.clone(),
        app.state.db_backend,
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

    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": base,
            "authorization_endpoint": format!("{base}/oauth/authorize"),
            "token_endpoint": format!("{base}/oauth/token"),
            "scopes_supported": ["atproto"],
            "response_types_supported": ["code"],
        })))
        .mount(mock)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/{ACCOUNT}/log/last")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "type": "plc_operation",
            "rotationKeys": ["did:key:zQ3shRotation"],
            "alsoKnownAs": ["at://attached.example"],
            "services": {},
            "verificationMethods": {},
            "prev": null,
        })))
        .mount(mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/pds/xrpc/com.atproto.identity.signPlcOperation"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "operation": { "type": "plc_operation", "sig": "signed" },
        })))
        .mount(mock)
        .await;

    // atrium may retry an expired token once, so that answer is asked for at
    // least once; every other answer exactly once.
    let calls = if matches!(answer, Answer::Expired) {
        1..=2
    } else {
        1..=1
    };
    let submitted = match answer {
        Answer::EmptyObject => ResponseTemplate::new(200).set_body_json(json!({})),
        Answer::EmptyJsonBody => {
            ResponseTemplate::new(200).set_body_raw(Vec::<u8>::new(), "application/json")
        }
        Answer::NoBody => ResponseTemplate::new(200),
        Answer::Error => ResponseTemplate::new(400).set_body_json(json!({
            "error": "InvalidRequest",
            "message": "invalid operation",
        })),
        Answer::Expired => ResponseTemplate::new(401)
            .insert_header(
                "WWW-Authenticate",
                r#"DPoP error="invalid_token", error_description="token expired""#,
            )
            .set_body_json(json!({
                "error": "invalid_token",
                "message": "token expired",
            })),
    };
    Mock::given(method("POST"))
        .and(path("/pds/xrpc/com.atproto.identity.submitPlcOperation"))
        .respond_with(submitted)
        .expect(calls)
        .mount(mock)
        .await;
}

async fn submit(app: &TestApp, uri: &str) -> StatusCode {
    app.router
        .clone()
        .oneshot(
            app.authed_request()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({ "token": "email-token" })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

/// The dashboard's sync, after setup.
async fn sync_plc(answer: Answer) -> (StatusCode, TestApp) {
    let mut app = TestApp::new().await;
    app.state.config.token_encryption_key = Some(common::db::TEST_ENCRYPTION_KEY);
    app.rebuild_router();
    attached_account(&app, answer).await;
    happyview::service_identity::mark_setup_complete(&app.state.db, app.state.db_backend)
        .await
        .unwrap();
    let status = submit(&app, "/admin/service-entries/sync-plc/submit").await;
    (status, app)
}

/// Onboarding, before setup completes.
async fn onboarding_plc(answer: Answer) -> StatusCode {
    let mut app = TestApp::new().await;
    app.state.config.token_encryption_key = Some(common::db::TEST_ENCRYPTION_KEY);
    app.rebuild_router();
    attached_account(&app, answer).await;
    submit(&app, "/api/setup/plc/submit").await
}

async fn plc_synced_events(app: &TestApp) -> usize {
    let sql = happyview::db::adapt_sql(
        "SELECT COUNT(*) FROM happyview_event_logs WHERE event_type = 'service_entry.plc_synced'",
        app.state.db_backend,
    );
    let (count,): (i64,) = happyview::db::query_as(&sql)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    count as usize
}

#[tokio::test]
#[serial]
async fn sync_plc_accepts_an_empty_object() {
    common::require_db!();
    let (status, app) = sync_plc(Answer::EmptyObject).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(plc_synced_events(&app).await, 1);
}

#[tokio::test]
#[serial]
async fn sync_plc_accepts_an_empty_json_body() {
    common::require_db!();
    let (status, app) = sync_plc(Answer::EmptyJsonBody).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(plc_synced_events(&app).await, 1);
}

#[tokio::test]
#[serial]
async fn sync_plc_accepts_no_body() {
    common::require_db!();
    let (status, _app) = sync_plc(Answer::NoBody).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
#[serial]
async fn sync_plc_still_fails_on_a_pds_error() {
    common::require_db!();
    let (status, app) = sync_plc(Answer::Error).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(plc_synced_events(&app).await, 0);
}

#[tokio::test]
#[serial]
async fn onboarding_plc_accepts_an_empty_object() {
    common::require_db!();
    assert_eq!(
        onboarding_plc(Answer::EmptyObject).await,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
#[serial]
async fn onboarding_plc_accepts_an_empty_json_body() {
    common::require_db!();
    assert_eq!(
        onboarding_plc(Answer::EmptyJsonBody).await,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
#[serial]
async fn onboarding_plc_still_fails_on_a_pds_error() {
    common::require_db!();
    assert_eq!(
        onboarding_plc(Answer::Error).await,
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

/// An expired session still asks the operator to sign in again.
#[tokio::test]
#[serial]
async fn sync_plc_still_asks_to_reauthenticate_an_expired_session() {
    common::require_db!();
    let (status, app) = sync_plc(Answer::Expired).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(plc_synced_events(&app).await, 0);
}

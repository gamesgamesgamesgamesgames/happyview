//! How `feature.spaces_enabled` is resolved: a stored row first, then the
//! `FEATURE_SPACES_ENABLED` environment variable, unless the hosting platform
//! fixes the setting with `PLATFORM_LOCKED_SETTINGS`, when the variable wins.
//!
//! Its own target, because it sets process environment variables.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::feature_flags::{FeatureFlag, is_enabled};
use serde_json::json;
use serial_test::serial;
use tower::ServiceExt;

use common::app::TestApp;

const ENV: &str = "FEATURE_SPACES_ENABLED";
const LOCKED: &str = "PLATFORM_LOCKED_SETTINGS";

/// Sets the given variables for its lifetime, and removes them on drop, so a
/// failed assertion can't leak them into the next test.
struct EnvGuard(Vec<&'static str>);

impl EnvGuard {
    fn set(vars: &[(&'static str, &str)]) -> Self {
        for (name, value) in vars {
            // SAFETY: the tests in this target are serial, so no other thread
            // reads the environment while it changes.
            unsafe { std::env::set_var(name, value) };
        }
        Self(vars.iter().map(|(name, _)| *name).collect())
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for name in &self.0 {
            // SAFETY: as in `set`.
            unsafe { std::env::remove_var(name) };
        }
    }
}

/// Store `value` as the setting through the admin API, returning the status.
async fn store(app: &TestApp, value: &str) -> StatusCode {
    let (name, cookie) = app.admin_cookie();
    let req = Request::builder()
        .method("PUT")
        .uri("/admin/settings/feature.spaces_enabled")
        .header(name, cookie)
        .header("content-type", "application/json")
        .body(Body::from(json!({ "value": value }).to_string()))
        .unwrap();
    app.router.clone().oneshot(req).await.unwrap().status()
}

/// Whether spaces are on, by the flag check every spaces route uses, and by a
/// spaces route itself; the two must agree.
async fn spaces_on(app: &TestApp) -> bool {
    let flag = is_enabled(
        &app.state.db,
        FeatureFlag::SPACES_ENABLED,
        app.state.db_backend,
    )
    .await;
    let (name, cookie) =
        common::auth::admin_cookie_header("did:plc:flagprobe", &app.state.cookie_key);
    let req = Request::builder()
        .uri("/xrpc/com.atproto.space.listSpaces")
        .header(name, cookie)
        .body(Body::empty())
        .unwrap();
    let status = app.router.clone().oneshot(req).await.unwrap().status();
    let route = status != StatusCode::NOT_FOUND;
    assert_eq!(
        flag, route,
        "the flag says {flag}, but listSpaces answered {status}"
    );
    flag
}

/// With nothing stored and no variable, spaces are off.
#[tokio::test]
#[serial]
async fn with_neither_a_row_nor_the_variable_spaces_are_off() {
    common::require_db!();
    let _env = EnvGuard::set(&[]);
    let app = TestApp::new().await;
    assert!(!spaces_on(&app).await);
}

/// With nothing stored, the variable decides.
#[tokio::test]
#[serial]
async fn with_no_row_the_variable_decides() {
    common::require_db!();
    let app = TestApp::new().await;
    {
        let _env = EnvGuard::set(&[(ENV, "true")]);
        assert!(spaces_on(&app).await, "FEATURE_SPACES_ENABLED=true");
    }
    {
        let _env = EnvGuard::set(&[(ENV, "false")]);
        assert!(!spaces_on(&app).await, "FEATURE_SPACES_ENABLED=false");
    }
}

/// A stored row wins over the variable, in both directions.
#[tokio::test]
#[serial]
async fn a_stored_row_wins_over_the_variable() {
    common::require_db!();
    let app = TestApp::new().await;
    let _env = EnvGuard::set(&[(ENV, "true")]);
    assert!(store(&app, "false").await.is_success());
    assert!(!spaces_on(&app).await, "row false, variable true");

    drop(_env);
    let _env = EnvGuard::set(&[(ENV, "false")]);
    assert!(store(&app, "true").await.is_success());
    assert!(spaces_on(&app).await, "row true, variable false");
}

/// Fixed by the platform, the variable wins whatever is stored, and the
/// setting can't be changed through the settings routes.
#[tokio::test]
#[serial]
async fn fixed_by_the_platform_the_variable_wins_and_the_row_is_refused() {
    common::require_db!();
    let app = TestApp::new().await;
    assert!(store(&app, "false").await.is_success());

    let _env = EnvGuard::set(&[(ENV, "true"), (LOCKED, "feature.spaces_enabled")]);
    assert!(spaces_on(&app).await, "locked, variable true, row false");
    assert_eq!(store(&app, "false").await, StatusCode::CONFLICT);
    assert!(spaces_on(&app).await, "still on after the refused change");
}

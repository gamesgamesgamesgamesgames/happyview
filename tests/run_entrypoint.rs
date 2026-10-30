//! `happyview::run`'s boot with the providers it is given: the record source
//! starts once with a working host, core asks it for backfills, and its admin
//! routes answer under `/admin` behind the admin auth layer.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Json;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use happyview::AppState;
use happyview::run::Providers;
use happyview_provider::{
    AdminCaller, BackfillError, BackfillKind, BackfillRequest, Host, ProviderError, RecordSource,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use common::fake_storage::FakeStorage;

const ADMIN: &str = "did:plc:runentrypointadmin";
const JOB: &str = "double-job";

/// A source that records what HappyView asked of it.
#[derive(Default)]
struct Double {
    starts: AtomicUsize,
    /// What `start` read back through the host after writing it.
    host_roundtrip: Mutex<Option<Option<String>>>,
    backfills: Mutex<Vec<BackfillRequest>>,
}

#[async_trait]
impl RecordSource for Double {
    fn id(&self) -> &'static str {
        "double"
    }

    fn migrations(
        &self,
        _backend: happyview_provider::db::DatabaseBackend,
    ) -> Option<&'static sqlx::migrate::Migrator> {
        None
    }

    async fn start(self: Arc<Self>, host: Arc<dyn Host>) -> Result<(), ProviderError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        host.put_setting("run_entrypoint.double", "started")
            .await
            .expect("write a setting through the host");
        let read = host.setting("run_entrypoint.double").await;
        *self.host_roundtrip.lock().unwrap() = Some(read);
        Ok(())
    }

    fn admin_routes(&self) -> axum::Router {
        axum::Router::new().route(
            "/backfill",
            get(|caller: AdminCaller| async move {
                Json(json!({ "answered_by": "double", "caller": caller.did }))
            }),
        )
    }

    async fn start_backfill(&self, req: BackfillRequest) -> Result<Option<String>, BackfillError> {
        self.backfills.lock().unwrap().push(req);
        Ok(Some(JOB.to_string()))
    }

    async fn has_backfill(&self, _collection: &str) -> bool {
        false
    }

    async fn backfill_running(&self, _collection: &str) -> bool {
        false
    }

    async fn status(&self) -> Value {
        json!({})
    }

    async fn metrics(&self) -> Vec<(String, i64)> {
        Vec::new()
    }
}

/// A served instance's configuration on its own SQLite file.
fn config(dir: &std::path::Path) -> happyview::config::Config {
    let mut config = happyview::test_support::test_config(happyview::db::DatabaseBackend::Sqlite);
    config.database_url = format!("sqlite://{}?mode=rwc", dir.join("happyview.db").display());
    config.public_url = "http://127.0.0.1:0".into();
    config.plc_url = "http://127.0.0.1:1".into();
    config.session_secret = "test-session-secret-0123456789abcdef".into();
    config.plugin_dir = dir.join("plugins");
    config
}

async fn send(state: &AppState, request: Request<Body>) -> (StatusCode, Value) {
    let response = happyview::server::router(state.clone())
        .oneshot(request)
        .await
        .expect("serve the request");
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn admin(state: &AppState, request: axum::http::request::Builder) -> axum::http::request::Builder {
    let (name, value) = common::auth::admin_cookie_header(ADMIN, &state.cookie_key);
    request.header("host", "127.0.0.1").header(name, value)
}

#[tokio::test]
async fn boot_runs_the_given_source_and_routes_backfills_and_admin_requests_to_it() {
    let dir = tempfile::tempdir().unwrap();
    let double = Arc::new(Double::default());
    let state = happyview::run::boot_for_test(
        config(dir.path()),
        Providers {
            source: double.clone(),
            storage: None,
        },
    )
    .await;

    assert_eq!(double.starts.load(Ordering::SeqCst), 1, "start runs once");
    assert_eq!(
        *double.host_roundtrip.lock().unwrap(),
        Some(Some("started".to_string())),
        "the host start was given reads back what it wrote"
    );

    // The admin auth layer stands in front of the source's routes.
    let (status, _) = send(
        &state,
        Request::get("/admin/backfill")
            .header("host", "127.0.0.1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // The first signed-in account becomes the super user.
    let (status, body) = send(
        &state,
        admin(&state, Request::get("/admin/backfill"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "answered_by": "double", "caller": ADMIN }));

    let (status, body) = send(
        &state,
        admin(&state, Request::post("/admin/lexicons"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "lexicon_json": common::fixtures::game_record_lexicon(),
                    "backfill": true,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["backfill_job_id"], JOB);
    assert_eq!(
        *double.backfills.lock().unwrap(),
        [BackfillRequest {
            collection: Some("games.gamesgamesgamesgames.game".into()),
            dids: Vec::new(),
            source: None,
            actor: ADMIN.into(),
            kind: BackfillKind::Automatic,
        }]
    );
}

#[tokio::test]
async fn boot_routes_claimed_collections_to_the_given_storage() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(FakeStorage::claiming(&["com.example.claimed"]).await);
    let state = happyview::run::boot_for_test(
        config(dir.path()),
        Providers {
            source: Arc::new(Double::default()),
            storage: Some(storage),
        },
    )
    .await;
    assert!(state.records.external().is_some());
}

/// Boot installs `HAPPYVIEW_DEFAULT_PLUGINS` in the background, after the
/// plugin runtime is up, and records what it applied.
#[tokio::test]
async fn boot_installs_the_default_plugins() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/lua/manifest.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "lua", "name": "lua", "version": "1.0.0", "api_version": "2",
            "plugin_type": "library", "capabilities": [], "wasm_file": "plugin.wasm",
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/lua/plugin.wasm"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            wat::parse_str(r#"(module (memory (export "memory") 1))"#).unwrap(),
            "application/wasm",
        ))
        .mount(&server)
        .await;
    let source = format!("{}/lua/manifest.json", server.uri());

    let dir = tempfile::tempdir().unwrap();
    let mut config = config(dir.path());
    config.default_plugins = vec![source.clone()];
    let state = happyview::run::boot_for_test(
        config,
        Providers {
            source: Arc::new(Double::default()),
            storage: None,
        },
    )
    .await;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let applied = loop {
        let applied = happyview::admin::settings::get_setting(
            &state.db,
            happyview::admin::default_plugins::APPLIED_KEY,
            state.db_backend,
        )
        .await;
        if let Some(applied) = applied {
            break applied;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the default plugin was not applied"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert_eq!(
        serde_json::from_str::<Vec<String>>(&applied).unwrap(),
        [source]
    );
    assert!(state.plugin_registry.get("lua").await.is_some());
}

async fn boot_in(dir: &std::path::Path) -> AppState {
    happyview::run::boot_for_test(
        config(dir),
        Providers {
            source: Arc::new(Double::default()),
            storage: None,
        },
    )
    .await
}

/// Boot, and let the boot's promotion task finish its first pass, so a key
/// staged afterwards is left to the next boot rather than to that task,
/// which then sleeps for a minute.
async fn boot_and_settle(dir: &std::path::Path) -> AppState {
    let state = boot_in(dir).await;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    state
}

/// The kid of the instance key the boot built its clients around.
fn booted_kid(state: &AppState) -> String {
    serde_json::to_value(&state.client_jwks[0]).unwrap()["kid"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn stage_instance_key_due_at(state: &AppState, at: chrono::DateTime<chrono::Utc>) -> String {
    let (staged, _) = happyview::oauth::client_keys::stage_key(
        &state.db,
        state.db_backend,
        state.config.token_encryption_key.as_ref(),
        happyview::oauth::client_keys::INSTANCE_OWNER,
    )
    .await
    .unwrap();
    happyview::db::query(&happyview::db::adapt_sql(
        "UPDATE happyview_oauth_client_keys SET activates_at = ? WHERE kid = ?",
        state.db_backend,
    ))
    .bind(at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
    .bind(&staged.kid)
    .execute(&state.db)
    .await
    .unwrap();
    staged.kid
}

async fn instance_key_status(
    state: &AppState,
    kid: &str,
) -> Option<happyview::oauth::client_keys::KeyStatus> {
    happyview::oauth::client_keys::list_keys_for_owner(
        &state.db,
        state.db_backend,
        happyview::oauth::client_keys::INSTANCE_OWNER,
    )
    .await
    .unwrap()
    .into_iter()
    .find(|k| k.kid == kid)
    .map(|k| k.status)
}

/// A staged key that came due while the instance was down takes over before
/// the boot builds its clients, not a poll later.
#[tokio::test]
async fn boot_promotes_a_staged_key_that_came_due_while_it_was_down() {
    let dir = tempfile::tempdir().unwrap();
    let first = boot_and_settle(dir.path()).await;
    let old = booted_kid(&first);
    let staged =
        stage_instance_key_due_at(&first, chrono::Utc::now() - chrono::Duration::minutes(1)).await;

    let second = boot_in(dir.path()).await;

    assert_ne!(staged, old);
    assert_eq!(
        booted_kid(&second),
        staged,
        "the boot must build its clients around the key that is due"
    );
}

/// The boot starts the task that promotes a staged key when it comes due.
#[tokio::test]
async fn boot_starts_promoting_staged_keys_as_they_come_due() {
    let dir = tempfile::tempdir().unwrap();
    let first = boot_and_settle(dir.path()).await;
    let old = booted_kid(&first);
    let staged =
        stage_instance_key_due_at(&first, chrono::Utc::now() + chrono::Duration::seconds(2)).await;

    let second = boot_in(dir.path()).await;
    assert_eq!(booted_kid(&second), old, "not due at boot");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while instance_key_status(&second, &staged).await
        != Some(happyview::oauth::client_keys::KeyStatus::Current)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "nothing promoted the staged key once it came due"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(
        instance_key_status(&second, &old).await,
        Some(happyview::oauth::client_keys::KeyStatus::Retiring)
    );
}

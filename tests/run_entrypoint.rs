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

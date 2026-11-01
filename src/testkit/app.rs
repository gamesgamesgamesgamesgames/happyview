//! A HappyView instance on the test database (`TEST_DATABASE_URL`).
//!
//! The plumbing here (the lock that serialises tests on one database, the
//! reset between them, the signed admin cookie and the router a test drives)
//! is shared by HappyView's own `TestApp` and by [`ProviderApp`], so a
//! provider's tests run on an instance built the way HappyView's are.

use axum::http::{HeaderName, HeaderValue};
use axum::response::IntoResponse;
use axum_extra::extract::cookie::{Cookie, Key, SignedCookieJar};
use sqlx::AnyPool;

use crate::AppState;
use crate::db::{self, DatabaseBackend};

/// `TEST_DATABASE_URL`, which every test instance runs on.
pub fn test_database_url() -> String {
    std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL must be set for e2e tests")
}

/// A migrated pool on `TEST_DATABASE_URL`.
pub async fn test_pool() -> AnyPool {
    let url = test_database_url();
    let backend = DatabaseBackend::from_url(&url);
    db::connect(&url, backend).await
}

pub fn test_backend() -> DatabaseBackend {
    DatabaseBackend::from_url(&test_database_url())
}

/// Held for as long as a test uses the shared database; dropping it lets the
/// next test in. Tests in one binary run in parallel and every one of them
/// truncates and seeds the same tables, so without it they read each other's
/// rows.
pub enum TestLock {
    /// An advisory lock, held on the one connection in this pool.
    Postgres(AnyPool),
    /// An exclusive lock on a file beside the database. A file lock rather
    /// than a mutex because it also holds across processes, as the advisory
    /// lock does.
    Sqlite(std::fs::File),
}

/// Acquire the lock that serialises tests on `TEST_DATABASE_URL`, or `None`
/// when no database is configured (or SQLite runs in memory, where nothing is
/// shared).
pub async fn acquire_test_lock() -> Option<TestLock> {
    let url = std::env::var("TEST_DATABASE_URL").ok()?;
    let backend = DatabaseBackend::from_url(&url);

    if !matches!(backend, DatabaseBackend::Postgres) {
        let mut path = db::sqlite_path_from_url(&url)?.into_os_string();
        path.push(".test-lock");
        // Taken before the first connection, so the test holding it is also
        // the only one migrating a fresh file.
        let file = tokio::task::spawn_blocking(move || {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&path)
                .expect("failed to open the SQLite test lock file");
            file.lock()
                .expect("failed to lock the SQLite test lock file");
            file
        })
        .await
        .expect("the SQLite test lock task panicked");
        return Some(TestLock::Sqlite(file));
    }

    // Migrate before taking the lock. A test blocked on `pg_advisory_lock(42)`
    // holds a statement open, and a migration building an index
    // `CONCURRENTLY` waits for every open statement to finish, so a holder
    // migrating a fresh database would wait on a waiter that waits on it, a
    // cycle Postgres cannot see to break.
    test_pool().await.close().await;

    let lock_pool = sqlx::any::AnyPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("failed to create advisory lock pool");

    db::query("SELECT pg_advisory_lock(42)")
        .execute(&lock_pool)
        .await
        .expect("failed to acquire advisory lock");

    Some(TestLock::Postgres(lock_pool))
}

/// Panics unless `pool` is connected to the `TEST_DATABASE_URL` database
/// (same backend, host, port and database or file), so a helper that wipes
/// data never runs against any other.
pub fn assert_test_pool(pool: &AnyPool) {
    let url = test_database_url();
    let expected: sqlx::any::AnyConnectOptions = url
        .parse()
        .unwrap_or_else(|e| panic!("TEST_DATABASE_URL does not parse: {e}"));
    let expected = &expected.database_url;
    let options = pool.connect_options();
    let actual = &options.database_url;
    let backend_of = |scheme: &str| DatabaseBackend::from_url(&format!("{scheme}:"));
    assert!(
        backend_of(actual.scheme()) == test_backend(),
        "refusing to wipe a {} pool: TEST_DATABASE_URL is a {:?} database",
        actual.scheme(),
        test_backend()
    );
    let same = actual.scheme() == expected.scheme()
        && actual.host_str() == expected.host_str()
        && actual.port_or_known_default() == expected.port_or_known_default()
        && actual.path() == expected.path();
    assert!(
        same,
        "refusing to wipe a pool that is not on the TEST_DATABASE_URL database \
         (the pool is on {}://{}{})",
        actual.scheme(),
        actual.host_str().unwrap_or(""),
        actual.path()
    );
}

/// Empty every table a test writes, so the next test starts from nothing.
/// Test-only and destructive: refuses (panics) unless `pool` is on the
/// `TEST_DATABASE_URL` database.
pub async fn truncate_all(pool: &AnyPool) {
    assert_test_pool(pool);
    match test_backend() {
        DatabaseBackend::Postgres => {
            // The catalog's tables by their constants: only `catalog` spells them.
            db::query(&format!(
                "TRUNCATE happyview_records_v2, happyview_record_refs_v2, happyview_record_tombstones, happyview_did_tombstones, {lexicons}, happyview_backfill_jobs, happyview_users, happyview_user_permissions, happyview_api_keys, happyview_event_logs, happyview_script_variables, {scripts}, happyview_dead_letter_scripts, happyview_dead_letter_hooks, happyview_labeler_subscriptions, happyview_labels, happyview_instance_settings, happyview_domains, happyview_dpop_sessions, happyview_dpop_keys, happyview_api_clients, happyview_api_client_probes, happyview_delegated_accounts, happyview_account_delegates, happyview_service_identity, happyview_service_entries, happyview_service_entry_xrpcs, happyview_jobs, happyview_spaces, happyview_space_members, happyview_space_records, happyview_space_repo_state, happyview_space_record_oplog, happyview_space_notify_registrations, happyview_space_invites, happyview_lexicon_publish_rules, happyview_lexicon_publications, happyview_linked_repo_sessions, happyview_linked_repo_auth_state, happyview_linked_repos, happyview_oauth_sessions, happyview_auth_login_redirects, happyview_oauth_client_keys, happyview_verification_methods, happyview_delegation_key, happyview_space_pds_support, happyview_project_deployments, happyview_script_metrics RESTART IDENTITY CASCADE",
                lexicons = crate::catalog::lexicon_rows::TABLE,
                scripts = crate::catalog::script_rows::TABLE,
            ))
            .execute(pool)
            .await
            .expect("failed to truncate tables");
        }
        DatabaseBackend::Sqlite => {
            let tables = [
                // Reset between apps: a key provisioned under one encryption
                // key cannot be decrypted with the next test's, which surfaces
                // as an opaque 500 on the first space write.
                "happyview_verification_methods",
                "happyview_delegation_key",
                "happyview_space_pds_support",
                "happyview_lexicon_publish_rules",
                "happyview_lexicon_publications",
                "happyview_project_deployments",
                "happyview_script_metrics",
                "happyview_linked_repo_sessions",
                "happyview_linked_repo_auth_state",
                "happyview_linked_repos",
                "happyview_auth_login_redirects",
                // Spaces tables (children before parents — no cascade on SQLite).
                "happyview_space_credentials",
                "happyview_space_dids",
                "happyview_space_invites",
                "happyview_space_notify_registrations",
                "happyview_space_record_oplog",
                "happyview_space_repo_state",
                "happyview_space_records",
                "happyview_space_members",
                "happyview_spaces",
                "happyview_service_entry_xrpcs",
                "happyview_service_entries",
                "happyview_service_identity",
                "happyview_account_delegates",
                "happyview_delegated_accounts",
                "happyview_dpop_sessions",
                "happyview_dpop_keys",
                "happyview_api_client_probes",
                "happyview_api_clients",
                // Refs before the records they reference. The interned
                // names stay: a live store's cached ids must remain valid.
                "happyview_record_refs_v2",
                "happyview_records_v2",
                "happyview_record_tombstones",
                "happyview_did_tombstones",
                crate::catalog::lexicon_rows::TABLE,
                "happyview_backfill_completions",
                "happyview_backfill_pds_stats",
                "happyview_backfill_cursors",
                "happyview_backfill_queue",
                "happyview_backfill_replay_retries",
                "happyview_backfill_replay_sweeps",
                "happyview_backfill_jobs",
                "happyview_users",
                "happyview_user_permissions",
                "happyview_api_keys",
                "happyview_event_logs",
                "happyview_script_variables",
                crate::catalog::script_rows::TABLE,
                "happyview_dead_letter_scripts",
                "happyview_dead_letter_hooks",
                "happyview_labeler_subscriptions",
                "happyview_labels",
                "happyview_instance_settings",
                "happyview_domains",
                "happyview_jobs",
                "happyview_oauth_sessions",
                "happyview_oauth_client_keys",
            ];
            for table in tables {
                db::query(&format!("DELETE FROM {table}"))
                    .execute(pool)
                    .await
                    .unwrap_or_else(|e| panic!("failed to delete from {table}: {e}"));
            }
        }
    }
}

/// A Cookie header carrying a signed session cookie for `did`: a user who
/// has completed OAuth.
pub fn admin_cookie_header(did: &str, cookie_key: &Key) -> (HeaderName, HeaderValue) {
    session_cookie_header(did, cookie_key)
}

/// A Cookie header carrying a signed session cookie holding `value`, as the
/// OAuth callback writes it: a DID, or a DID and an API client key on two lines.
pub fn session_cookie_header(value: &str, cookie_key: &Key) -> (HeaderName, HeaderValue) {
    let jar = SignedCookieJar::new(cookie_key.clone());
    let mut cookie = Cookie::new(crate::auth::COOKIE_NAME, value.to_string());
    cookie.set_path("/");
    let jar = jar.add(cookie);

    // Build a response to extract the Set-Cookie header with the signed value,
    // then convert it to a Cookie request header.
    let response = jar.into_response();
    let set_cookie_values: Vec<String> = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| {
            let s = v.to_str().ok()?;
            // Extract just "name=value" from "name=value; Path=/; ..."
            Some(s.split(';').next()?.to_string())
        })
        .collect();
    let cookie_header = set_cookie_values.join("; ");

    (
        HeaderName::from_static("cookie"),
        HeaderValue::from_str(&cookie_header).unwrap(),
    )
}

/// HappyView's router over `state`, with a `host` header filled in for a
/// request that has none, as a test's requests usually do not.
pub fn router(state: &AppState) -> axum::Router {
    crate::server::router(state.clone()).layer(axum::middleware::from_fn(
        |mut req: axum::extract::Request, next: axum::middleware::Next| async move {
            if !req.headers().contains_key("host") {
                req.headers_mut()
                    .insert("host", axum::http::HeaderValue::from_static("127.0.0.1"));
            }
            next.run(req).await
        },
    ))
}

#[cfg(feature = "testkit")]
pub use provider_app::{ADMIN_DID, LUA_PLUGIN_DIR, ProviderApp};

#[cfg(feature = "testkit")]
mod provider_app {
    use std::path::PathBuf;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use happyview_provider::{Action, BatchOutcome, Host, SourceEvent};
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;
    use crate::Providers;
    use crate::config::Config;

    /// The DID of the super user a [`ProviderApp`] signs in as.
    pub const ADMIN_DID: &str = "did:plc:provideradmin";

    /// The directory holding the Lua interpreter plugin, which a hook needs:
    /// its `manifest.json` beside the module it names.
    pub const LUA_PLUGIN_DIR: &str = "HAPPYVIEW_LUA_PLUGIN";

    /// A HappyView booted through [`crate::run::boot_for_test`] on the test
    /// database with the providers under test, its tables emptied first.
    /// It holds the test database's lock until it is dropped, so instances
    /// in one process never see each other's rows.
    pub struct ProviderApp {
        pub state: AppState,
        pub router: axum::Router,
        lua: bool,
        _plugins: tempdir::Dir,
        _lock: Option<TestLock>,
    }

    impl ProviderApp {
        /// Boots with `providers` on `TEST_DATABASE_URL`, migrated, its
        /// tables emptied, with a super user ([`ADMIN_DID`]) and, when
        /// `HAPPYVIEW_LUA_PLUGIN` names it, the Lua interpreter installed.
        pub async fn new(providers: Providers) -> Self {
            let _lock = acquire_test_lock().await;
            let pool = test_pool().await;
            truncate_all(&pool).await;
            let backend = test_backend();
            let sql = db::adapt_sql(
                "INSERT INTO happyview_users (id, did, is_super, created_at) VALUES (?, ?, 1, ?)",
                backend,
            );
            db::query(&sql)
                .bind(uuid::Uuid::new_v4().to_string())
                .bind(ADMIN_DID)
                .bind(db::now_rfc3339())
                .execute(&pool)
                .await
                .expect("failed to seed the admin user");
            pool.close().await;

            let plugins = tempdir::Dir::new();
            let state = crate::run::boot_for_test(config(backend, plugins.path()), providers).await;
            let lua = match std::env::var_os(LUA_PLUGIN_DIR) {
                Some(dir) => {
                    let plugin = crate::plugin::loader::load_from_file(&PathBuf::from(dir))
                        .await
                        .unwrap_or_else(|e| {
                            panic!("{LUA_PLUGIN_DIR} names no loadable plugin: {e}")
                        });
                    state.plugin_registry.register(plugin).await;
                    true
                }
                None => false,
            };
            let router = router(&state);
            Self {
                state,
                router,
                lua,
                _plugins: plugins,
                _lock,
            }
        }

        /// Whether hooks can run: the Lua interpreter is installed.
        pub fn runs_hooks(&self) -> bool {
            self.lua
        }

        /// Hands `events` to the host through [`Host::ingest_batch`], as a
        /// source delivers them.
        pub async fn deliver(&self, events: Vec<SourceEvent>) -> BatchOutcome {
            crate::host::AppHost::new(self.state.clone())
                .ingest_batch(events)
                .await
        }

        /// The record at `uri` as HappyView serves it, wherever it is kept.
        pub async fn get_record(&self, uri: &str) -> Option<Value> {
            let stored = self
                .state
                .records
                .get(uri)
                .await
                .unwrap_or_else(|e| panic!("reading {uri}: {e}"))?;
            Some(serde_json::from_str(&stored.record).expect("a stored record is JSON"))
        }

        /// The URIs HappyView serves for `collection`, sorted.
        pub async fn list(&self, collection: &str) -> Vec<String> {
            let mut uris: Vec<String> = self
                .state
                .records
                .list_recent(collection, None, 10_000, 0)
                .await
                .unwrap_or_else(|e| panic!("listing {collection}: {e}"))
                .into_iter()
                .map(|record| record.uri)
                .collect();
            uris.sort();
            uris
        }

        /// Installs `lua` as the record hook for `action` on `collection`
        /// (`record.<action>:<collection>`).
        pub async fn set_hook(&self, collection: &str, action: Action, lua: &str) {
            assert!(
                self.lua,
                "a hook needs the Lua interpreter plugin: set {LUA_PLUGIN_DIR} to a directory \
                 holding it (scripts/fetch-lua-plugin.sh fetches one)"
            );
            self.state
                .scripts
                .put_manual(
                    &self.state.db,
                    self.state.db_backend,
                    crate::catalog::ScriptWrite {
                        id: format!("record.{}:{collection}", action.as_str()),
                        script_type: "lua".to_string(),
                        body: lua.to_string(),
                        description: None,
                    },
                )
                .await
                .unwrap_or_else(|e| panic!("installing the {} hook: {e}", action.as_str()));
        }

        /// Tracks `collection` by uploading a record lexicon for it through
        /// the admin API, which starts the source's automatic backfill when
        /// `backfill` is set. Returns that backfill's job id.
        pub async fn track(&self, collection: &str, backfill: bool) -> Option<String> {
            let lexicon = json!({
                "lexicon": 1,
                "id": collection,
                "defs": {"main": {"type": "record", "key": "any",
                    "record": {"type": "object", "properties": {"text": {"type": "string"}}}}}
            });
            let (name, value) = admin_cookie_header(ADMIN_DID, &self.state.cookie_key);
            let response = self
                .router
                .clone()
                .oneshot(
                    Request::post("/admin/lexicons")
                        .header(name, value)
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({ "lexicon_json": lexicon, "backfill": backfill }).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .expect("the router answers");
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            assert!(
                status == StatusCode::CREATED || status == StatusCode::OK,
                "tracking {collection}: {status} {body}"
            );
            body["backfill_job_id"].as_str().map(str::to_string)
        }

        /// Stores a query lexicon `nsid` over `collection`, which HappyView
        /// then serves as a default XRPC query: a get with `uri`, a list
        /// without.
        pub async fn track_query(&self, nsid: &str, collection: &str) {
            let lexicon = json!({
                "lexicon": 1,
                "id": nsid,
                "defs": {"main": {"type": "query",
                    "parameters": {"type": "params",
                        "properties": {"uri": {"type": "string"}, "limit": {"type": "integer"}}},
                    "output": {"encoding": "application/json"}}}
            });
            self.state
                .lexicons
                .put_manual(
                    &self.state.db,
                    self.state.db_backend,
                    crate::catalog::LexiconWrite {
                        lexicon_json: lexicon,
                        backfill: false,
                        target_collection: Some(collection.to_string()),
                        action: crate::lexicon::ProcedureAction::Upsert,
                        token_cost: None,
                        authority_did: None,
                        indexed: true,
                    },
                )
                .await
                .unwrap_or_else(|e| panic!("storing the {nsid} query: {e}"));
        }

        /// GETs `path` (such as `/xrpc/<nsid>?uri=...`) from the router,
        /// unauthenticated: its status and JSON body (`null` when it is not
        /// JSON).
        pub async fn xrpc_get(&self, path: &str) -> (StatusCode, Value) {
            let response = self
                .router
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .expect("the router answers");
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            (
                status,
                serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            )
        }

        /// Whether an external storage claims `collection`.
        pub fn claims(&self, collection: &str) -> bool {
            self.state.records.external_route(collection).is_some()
        }

        /// Rows of `collection` in the instance's own record table, whatever
        /// the routing: 0 for a collection an external storage claims.
        pub async fn local_rows(&self, collection: &str) -> i64 {
            let sql = db::adapt_sql(
                "SELECT COUNT(*) FROM happyview_records_v2 WHERE collection_id = \
                 (SELECT id FROM happyview_collections WHERE nsid = ?)",
                self.state.db_backend,
            );
            db::query_scalar::<i64>(&sql)
                .bind(collection)
                .fetch_one(&self.state.db)
                .await
                .expect("counting local rows")
        }

        /// The `script.log` events hooks wrote, oldest first: each one's
        /// message and fields.
        pub async fn script_logs(&self) -> Vec<(String, Value)> {
            let rows: Vec<(String,)> = db::query_as(
                "SELECT detail FROM happyview_event_logs WHERE event_type = 'script.log' \
                 ORDER BY created_at, id",
            )
            .fetch_all(&self.state.db)
            .await
            .expect("reading the script log");
            rows.into_iter()
                .map(|(detail,)| {
                    let detail: Value = serde_json::from_str(&detail).unwrap_or(Value::Null);
                    (
                        detail["message"].as_str().unwrap_or_default().to_string(),
                        detail["fields"].clone(),
                    )
                })
                .collect()
        }

        /// Waits, up to 30 seconds, for `done` to answer `true`.
        pub async fn wait_until<F, Fut>(&self, what: &str, mut done: F)
        where
            F: FnMut() -> Fut,
            Fut: std::future::Future<Output = bool>,
        {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
            while !done().await {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "timed out waiting for {what}"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }

    fn config(backend: DatabaseBackend, plugins: &std::path::Path) -> Config {
        let mut config = crate::test_support::test_config(backend);
        config.database_url = test_database_url();
        config.public_url = "http://127.0.0.1:0".into();
        config.plc_url = "http://127.0.0.1:1".into();
        config.relay_url = "http://127.0.0.1:1".into();
        config.session_secret = "test-session-secret-0123456789abcdef".into();
        config.plugin_dir = plugins.to_path_buf();
        config.token_encryption_key = None;
        config
    }

    /// An empty directory removed on drop: the plugin directory boot reads,
    /// so nothing on the machine running the tests is loaded.
    mod tempdir {
        use std::path::{Path, PathBuf};

        pub struct Dir(PathBuf);

        impl Dir {
            pub fn new() -> Self {
                let path = std::env::temp_dir()
                    .join(format!("happyview-provider-app-{}", uuid::Uuid::new_v4()));
                std::fs::create_dir_all(&path).expect("create a plugin directory");
                Self(path)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wiping helpers accept a pool on the test database and refuse
    /// any other, whichever backend the test database is.
    #[tokio::test]
    async fn the_wipe_guard_refuses_a_pool_off_the_test_database() {
        if std::env::var("TEST_DATABASE_URL").is_err() {
            crate::test_support::unavailable(crate::test_support::NO_DATABASE);
            return;
        }
        let on = test_pool().await;
        assert_test_pool(&on);
        let off = crate::test_support::memory_pool().await;
        let refused =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| assert_test_pool(&off)));
        assert!(
            refused.is_err(),
            "a pool off the test database was let through"
        );
    }
}

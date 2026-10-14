//! A template database migrated once per run, and an `AppState` per test file
//! on its own copy of it.
//!
//! Re-running migrations per file costs seconds; `VACUUM INTO` writes a
//! consistent copy of the open template in milliseconds, and needs no
//! checkpoint first, which a plain file copy of a WAL database would.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::AppState;
use crate::config::Config;
use crate::db::DatabaseBackend;
use crate::plugin::{PluginRegistry, WasmRuntime};

use super::harness::TestHarness;

pub struct TestEnv {
    /// The state the snapshot is built and installed on. It holds the
    /// template database and no harness.
    pub base: AppState,
    dir: PathBuf,
    next: AtomicU64,
}

pub struct FileState {
    pub state: AppState,
    pub harness: Arc<TestHarness>,
    path: PathBuf,
}

/// Every run's directory starts with this, so a later run can find the ones
/// an interrupted run left behind.
const DIR_PREFIX: &str = "happyview-test-";

/// A directory older than this belongs to a run that never reached `Drop`.
/// Long enough that no live run is that old.
const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Remove run directories left in `temp` by interrupted runs. Best-effort:
/// anything unreadable is left where it is.
fn sweep_stale(temp: &Path) {
    let Ok(entries) = std::fs::read_dir(temp) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with(DIR_PREFIX) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > STALE_AFTER);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// A project's configuration as a test run uses it.
fn run_config(config: Config, fixture: &Config) -> Config {
    Config {
        // Space writes sign with a key stored under the encryption key, so a
        // project that sets none still gets the fixture's.
        token_encryption_key: config.token_encryption_key.or(fixture.token_encryption_key),
        // In-process requests name `/xrpc/...` at the root, and a test run
        // serves no dashboard.
        base_path: None,
        static_dir: None,
        // Blank, as the fixture's are, so nothing in a run can reach the real
        // directory, relay, Jetstream or telemetry collector.
        plc_url: String::new(),
        relay_url: String::new(),
        jetstream_url: String::new(),
        telemetry_collector_url: String::new(),
        ..config
    }
}

fn url(path: &Path) -> String {
    format!("sqlite://{}?mode=rwc", path.display())
}

impl TestEnv {
    /// `config` is the project's, when the command loaded one; its script
    /// variable file reaches every run as `ctx.env`. `None` keeps the
    /// fixture configuration, which is what the host's own tests use.
    pub async fn new(
        registry: Arc<PluginRegistry>,
        runtime: Arc<WasmRuntime>,
        config: Option<Config>,
    ) -> std::io::Result<TestEnv> {
        // The system temp directory rather than `.happyview/`, which the dev
        // watcher would see change on every file a run writes.
        let temp = std::env::temp_dir();
        sweep_stale(&temp);
        let dir = temp.join(format!(
            "{DIR_PREFIX}{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir)?;
        let template =
            crate::db::open(&url(&dir.join("template.db")), DatabaseBackend::Sqlite).await;
        crate::db::migrate(&template, DatabaseBackend::Sqlite).await;

        // The fixture's fixed keys are fine here: the state is a throwaway
        // whose keys protect nothing.
        let base = crate::test_support::test_state_with_pool_on(template, DatabaseBackend::Sqlite);
        base.domain_cache
            .insert(crate::domain::Domain {
                id: "test".into(),
                url: format!("https://{}", super::TEST_HOST),
                is_primary: true,
                created_at: crate::db::now_rfc3339(),
                updated_at: crate::db::now_rfc3339(),
            })
            .await;
        let config = run_config(config.unwrap_or_else(|| base.config.clone()), &base.config);
        // The budgets a served instance would run its handlers under, so a
        // handler that fits a raised limit in production fits it here too.
        let script_limits = Arc::new(
            crate::lua::limits::ScriptLimits::load(&base.db, DatabaseBackend::Sqlite).await,
        );
        let base = AppState {
            config,
            script_limits,
            http: crate::http_retry::shared_client().clone(),
            plugin_registry: registry,
            wasm_runtime: runtime,
            // A forged caller's unknown method has to reach the in-process
            // PDS, which only service-proxy routing asks.
            proxy_config: Arc::new(arc_swap::ArcSwap::new(Arc::new(
                crate::proxy_config::ProxyConfig {
                    routing: crate::proxy_config::ProxyRouting::ServiceProxy,
                    ..Default::default()
                },
            ))),
            ..base
        };
        Ok(TestEnv {
            base,
            dir,
            next: AtomicU64::new(0),
        })
    }

    /// Replace the project's configuration, for a watch session that saw
    /// it change. Every file state made afterwards reads the new one. The
    /// script budgets are read again from the database and the environment;
    /// `happyview.toml` supplies neither today, so this keeps them in step
    /// with the configuration layer only if it ever does.
    pub async fn set_config(&mut self, config: Config) {
        let fixture = crate::test_support::test_config(DatabaseBackend::Sqlite);
        self.base.config = run_config(config, &fixture);
        self.base
            .script_limits
            .refresh(&self.base.db, DatabaseBackend::Sqlite)
            .await;
    }

    /// A state for test file `file` on a fresh copy of the template. Shares
    /// the base's plugins, runtime and installed snapshot.
    ///
    /// A field a file can write through, or that caches what it read, is
    /// rebuilt on the copy, or a file's writes would land in the template
    /// and every later copy would inherit them: `records` writes and mirrors
    /// through its own pool and interns ids against it, `scripts` reads
    /// manual rows from its pool, and `lexicons` shares its stored rows by
    /// `Arc`, so each file's catalog is loaded from its own copy over the
    /// base's snapshot handle.
    ///
    /// `oauth` and `linked_repos_client` still point at the template pool.
    /// They are left shared because a test run never writes through them:
    /// callers are forged, so no OAuth flow runs and no session exists to
    /// refresh.
    pub async fn file_state(&self, file: &str) -> std::io::Result<FileState> {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let path = self.dir.join(format!("{n}.db"));
        let escaped = path.display().to_string().replace('\'', "''");
        crate::db::query(&format!("VACUUM INTO '{escaped}'"))
            .execute(&self.base.db)
            .await
            .map_err(std::io::Error::other)?;
        let pool = crate::db::open(&url(&path), DatabaseBackend::Sqlite).await;
        let lexicons = crate::catalog::LexiconCatalog::new(self.base.project.clone());
        lexicons
            .load_from_db(&pool)
            .await
            .map_err(std::io::Error::other)?;
        let harness = Arc::new(TestHarness::new(file));
        let state = AppState {
            db: pool.clone(),
            backfill_db: pool.clone(),
            records: crate::record_store::RecordStore::new(pool.clone(), DatabaseBackend::Sqlite),
            scripts: crate::catalog::ScriptCatalog::new(
                pool.clone(),
                DatabaseBackend::Sqlite,
                self.base.project.clone(),
            ),
            lexicons,
            oauth_state_store: crate::auth::oauth_store::DbStateStore::new(
                pool,
                DatabaseBackend::Sqlite,
            ),
            rate_limiter: crate::rate_limit::RateLimiter::new(
                crate::rate_limit::RateLimitDefaults {
                    query_cost: 1,
                    procedure_cost: 1,
                    proxy_cost: 1,
                },
            ),
            // Caches the active labeler subscriptions read from `db`, so it is
            // per database: the base's would hand one file's rows to the next.
            labeler_subscription_cache: Arc::new(crate::labeler::SubscriptionCache::default()),
            test: Some(harness.clone()),
            ..self.base.clone()
        };
        Ok(FileState {
            state,
            harness,
            path,
        })
    }
}

impl FileState {
    /// Close the pool and delete the copy, with its WAL and shared-memory
    /// files.
    pub async fn finish(self) {
        self.state.db.close().await;
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{TestCall, TestReport, TestStatus};
    use crate::record_store::{PreparedRecord, WriteKind};

    async fn env() -> TestEnv {
        TestEnv::new(
            Arc::new(crate::plugin::PluginRegistry::new()),
            Arc::new(crate::plugin::WasmRuntime::new().unwrap()),
            None,
        )
        .await
        .unwrap()
    }

    /// Isolation between files is the whole point of the copy: a row one
    /// file writes must not be visible to the next.
    #[tokio::test]
    async fn each_file_gets_its_own_copy_of_the_template() {
        let env = env().await;
        let first = env.file_state("src/a.test.lua").await.unwrap();
        crate::db::query(
            "INSERT INTO happyview_script_variables (key, value) VALUES ('ONLY_A', '1')",
        )
        .execute(&first.state.db)
        .await
        .unwrap();
        let record = PreparedRecord {
            did: "did:plc:only-a".into(),
            collection: "com.example.thing".into(),
            rkey: "a".into(),
            record_json: r#"{"n":1}"#.into(),
            cid: String::new(),
            rev: None,
        };
        first
            .state
            .records
            .write_local(WriteKind::IndexOnly, &record)
            .await
            .unwrap();
        assert!(
            first
                .state
                .records
                .get(&record.uri())
                .await
                .unwrap()
                .is_some()
        );

        let second = env.file_state("src/b.test.lua").await.unwrap();
        let (count,): (i64,) = crate::db::query_as(
            "SELECT COUNT(*) FROM happyview_script_variables WHERE key = 'ONLY_A'",
        )
        .fetch_one(&second.state.db)
        .await
        .unwrap();
        assert_eq!(count, 0);
        assert!(
            second
                .state
                .records
                .get(&record.uri())
                .await
                .unwrap()
                .is_none()
        );
        assert!(env.base.records.get(&record.uri()).await.unwrap().is_none());
        first.finish().await;
        second.finish().await;
    }

    /// A project's configuration names the real network; a run must not.
    #[tokio::test]
    async fn a_project_config_loses_its_network_addresses() {
        let project = Config {
            plc_url: "https://plc.directory".into(),
            relay_url: "https://bsky.network".into(),
            jetstream_url: "wss://jetstream.example".into(),
            telemetry_collector_url: "https://telemetry.example".into(),
            ..crate::test_support::test_config(DatabaseBackend::Sqlite)
        };
        let env = TestEnv::new(
            Arc::new(crate::plugin::PluginRegistry::new()),
            Arc::new(crate::plugin::WasmRuntime::new().unwrap()),
            Some(project),
        )
        .await
        .unwrap();
        let config = &env.base.config;
        assert_eq!(
            [
                config.plc_url.as_str(),
                config.relay_url.as_str(),
                config.jetstream_url.as_str(),
                config.telemetry_collector_url.as_str(),
            ],
            [""; 4]
        );
    }

    #[test]
    fn a_stale_run_directory_is_swept_and_a_fresh_one_kept() {
        let temp = tempfile::tempdir().unwrap();
        let stale = temp.path().join(format!("{DIR_PREFIX}1-stale"));
        let fresh = temp.path().join(format!("{DIR_PREFIX}2-fresh"));
        let other = temp.path().join("someone-else");
        for dir in [&stale, &fresh, &other] {
            std::fs::create_dir(dir).unwrap();
        }
        let old = std::time::SystemTime::now() - STALE_AFTER * 2;
        std::fs::File::open(&stale)
            .unwrap()
            .set_modified(old)
            .unwrap();
        std::fs::File::open(&other)
            .unwrap()
            .set_modified(old)
            .unwrap();
        sweep_stale(temp.path());
        assert!(!stale.exists());
        assert!(fresh.exists());
        assert!(other.exists());
    }

    #[tokio::test]
    async fn starting_a_run_sweeps_what_an_interrupted_one_left() {
        let stale = std::env::temp_dir().join(format!("{DIR_PREFIX}0-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&stale).unwrap();
        std::fs::File::open(&stale)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - STALE_AFTER * 2)
            .unwrap();
        let _env = env().await;
        assert!(!stale.exists());
    }

    fn lexicon(id: &str) -> crate::catalog::LexiconWrite {
        crate::catalog::LexiconWrite {
            lexicon_json: serde_json::json!({
                "lexicon": 1,
                "id": id,
                "defs": { "main": { "type": "query" } },
            }),
            backfill: false,
            target_collection: None,
            action: crate::lexicon::ProcedureAction::Upsert,
            token_cost: None,
            authority_did: None,
        }
    }

    /// A lexicon or script one file stores is answered by that file's
    /// catalogs and by no other file's, nor the template's.
    #[tokio::test]
    async fn a_file_catalog_answers_only_for_its_own_database() {
        let env = env().await;
        let first = env.file_state("src/a.test.lua").await.unwrap();
        first
            .state
            .lexicons
            .put_manual(
                &first.state.db,
                DatabaseBackend::Sqlite,
                lexicon("com.example.onlyA"),
            )
            .await
            .unwrap();
        first
            .state
            .scripts
            .put_manual(
                &first.state.db,
                DatabaseBackend::Sqlite,
                crate::catalog::ScriptWrite {
                    id: "job.run:only-a".into(),
                    script_type: "lua".into(),
                    body: "function handle() end".into(),
                    description: None,
                },
            )
            .await
            .unwrap();
        let second = env.file_state("src/b.test.lua").await.unwrap();

        assert!(
            first
                .state
                .lexicons
                .get("com.example.onlyA")
                .await
                .is_some()
        );
        assert!(
            second
                .state
                .lexicons
                .get("com.example.onlyA")
                .await
                .is_none()
        );
        assert!(env.base.lexicons.get("com.example.onlyA").await.is_none());
        let script = |state: &AppState| {
            let scripts = state.scripts.clone();
            async move { scripts.get_manual("job.run:only-a").await.unwrap() }
        };
        assert!(script(&first.state).await.is_some());
        assert!(script(&second.state).await.is_none());
        assert!(script(&env.base).await.is_none());
        first.finish().await;
        second.finish().await;
    }

    /// The copy carries the template's lexicon rows, so the file's catalog
    /// has to read them from it.
    #[tokio::test]
    async fn a_file_catalog_loads_the_lexicons_its_copy_holds() {
        let env = env().await;
        env.base
            .lexicons
            .put_manual(
                &env.base.db,
                DatabaseBackend::Sqlite,
                lexicon("com.example.inTemplate"),
            )
            .await
            .unwrap();
        let file = env.file_state("src/a.test.lua").await.unwrap();
        assert!(
            file.state
                .lexicons
                .get("com.example.inTemplate")
                .await
                .is_some()
        );
        file.finish().await;
    }

    #[tokio::test]
    async fn a_file_state_carries_its_harness_and_the_base_has_none() {
        let env = env().await;
        let file = env.file_state("src/lib/a.test.lua").await.unwrap();
        assert_eq!(
            file.state.test.as_ref().unwrap().file(),
            "src/lib/a.test.lua"
        );
        assert!(Arc::ptr_eq(
            file.state.test.as_ref().unwrap(),
            &file.harness
        ));
        assert!(env.base.test.is_none());
        file.finish().await;
    }

    #[test]
    fn a_report_is_stamped_with_the_file_and_clears_what_the_test_observed() {
        let harness = TestHarness::new("src/lib/a.test.lua");
        harness.record_call(
            "http",
            TestCall {
                method: "get".into(),
                args: serde_json::json!([]),
            },
        );
        harness.report(TestReport {
            path: "anything".into(),
            name: "a".into(),
            status: TestStatus::Pass,
            message: None,
            file: None,
            line: None,
            duration_ms: 1,
        });
        assert_eq!(harness.reports()[0].path, "src/lib/a.test.lua");
        assert!(harness.calls("http").is_empty());
    }
}

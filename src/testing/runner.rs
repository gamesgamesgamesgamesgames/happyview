//! One run: load the project's interpreters, build its snapshot with the
//! tests in it, and run each test file on its own database.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use futures_util::stream::{self, StreamExt};
use serde_json::Value;

use crate::AppState;
use crate::cli::TestArgs;
use crate::config::Config;
use crate::config::project::ConfigMode;
use crate::plugin::{ExecutionError, PluginRegistry, ScriptExecuteOutput, WasmRuntime, loader};
use crate::project::build::{BuildEnv, BuildOptions, Diagnostic, build_snapshot_with, install};
use crate::project::snapshot::{ProjectScript, ProjectSnapshot, ScriptBody};
use crate::script::{DispatchError, Invocation, ScriptOrigin, Trigger, dispatch};

use super::env::TestEnv;
use super::report::{FileError, FileResult, exit_code, render};

/// The first SDK whose interpreters accept a test file as input.
const TEST_RUN_SDK: &str = "0.3.0";

/// Held for as long as a `Runner` lives. Preparing one installs its project's
/// settings as the process-wide configuration layer, which is read through
/// for every later build and run, so a second runner in the same process
/// would read the first's.
static RUN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub struct Runner {
    pub env: TestEnv,
    pub root: PathBuf,
    _run: tokio::sync::MutexGuard<'static, ()>,
}

/// The configuration a test run starts from. The defaults fill only what
/// neither the environment nor `happyview.toml` sets.
fn load_config(root: &Path) -> Result<Config, String> {
    let env_file = root.join(".env.test").to_string_lossy().into_owned();
    Config::load_from_with(Some(root), ConfigMode::Serve, |_| {
        vec![
            // `Config::from_env` requires one; the run opens its own template
            // and never this URL.
            ("DATABASE_URL".to_string(), "sqlite::memory:".to_string()),
            (
                "PUBLIC_URL".to_string(),
                format!("https://{}", super::TEST_HOST),
            ),
            // A test's `ctx.env` comes from here, never from the developer's
            // `.env`, which holds real secrets.
            ("HAPPYVIEW_SCRIPT_ENV_FILE".to_string(), env_file),
        ]
    })
    .map_err(|e| e.to_string())
}

/// A run that never produced an outcome. An interpreter built on an SDK from
/// before test runs cannot decode a test file's input: its `BAD_INPUT` names
/// the unknown `test` kind, and reads as the file's fault unless it is named
/// for what it is. Any other `BAD_INPUT` is the plugin's to explain.
fn dispatch_failure(error: DispatchError, interpreter: Option<&str>) -> FileError {
    let message = match error {
        DispatchError::NoInterpreter { language } => {
            crate::script::no_interpreter_message(&language)
        }
        DispatchError::Execution(ExecutionError::PluginError { code, message, .. })
            if code == "BAD_INPUT" && message.contains("`test`") =>
        {
            format!(
                "the '{}' interpreter predates test runs and refused this file ({message}); \
                 it needs happyview-plugin-sdk {TEST_RUN_SDK} or later",
                interpreter.unwrap_or("unknown")
            )
        }
        // The display text names no budget. A test file is always source,
        // never a compiled module, so what stopped it is its own code running
        // past its wall-clock budget.
        DispatchError::Execution(ExecutionError::Timeout) => format!(
            "the file ran past its {}-second limit for its own code",
            crate::plugin::executor::TEST_FILE_GUEST_SECONDS
        ),
        other => other.to_string(),
    };
    FileError {
        message,
        file: None,
        line: None,
    }
}

impl Runner {
    pub async fn prepare(args: &TestArgs) -> Result<Runner, String> {
        let run = RUN.lock().await;
        let root = args.root.clone();
        let config = load_config(&root)?;
        let dirs = if args.plugin_dirs.is_empty() {
            crate::project::check::default_plugin_dirs(&root)
        } else {
            args.plugin_dirs.clone()
        };
        let cache_dir = config
            .plugin_cache_dir
            .clone()
            .unwrap_or_else(|| root.join(".happyview").join("plugin-cache"));
        let runtime = Arc::new(
            WasmRuntime::with_cache_dir(Some(cache_dir))
                .map_err(|e| format!("could not start the wasm runtime: {e}"))?,
        );
        let registry = Arc::new(PluginRegistry::new().with_runtime(runtime.clone()));
        let mut plugins = Vec::new();
        for dir in &dirs {
            plugins.push(
                loader::load_from_file(dir)
                    .await
                    .map_err(|e| format!("{}: could not load the plugin: {e}", dir.display()))?,
            );
        }
        if let Some((id, e)) = registry.register_all(plugins).await.into_iter().next() {
            return Err(format!("plugin {id}: {e}"));
        }
        let env = TestEnv::new(registry, runtime, Some(config))
            .await
            .map_err(|e| format!("could not create the test database: {e}"))?;
        Ok(Runner {
            env,
            root,
            _run: run,
        })
    }

    /// Load the project's configuration again, after a watch saw
    /// `happyview.toml` or `.env.test` change. A refused load keeps the
    /// current one.
    pub async fn reload_config(&mut self) -> Result<(), String> {
        let config = load_config(&self.root)?;
        self.env.set_config(config).await;
        Ok(())
    }

    /// Build with the tests included and install the result on the base
    /// state, which every file's state shares. Answers the installed
    /// snapshot, pinned, and the build's warnings.
    pub async fn build(&self) -> Result<(Arc<ProjectSnapshot>, Vec<Diagnostic>), Vec<Diagnostic>> {
        // The base state's executor is what prepares each test file with
        // `ChunkRole::Test`.
        let report = build_snapshot_with(
            &BuildEnv::from_state(&self.env.base),
            &self.root,
            BuildOptions {
                include_tests: true,
            },
        )
        .await?;
        install(&self.env.base, report.snapshot).await;
        Ok((self.env.base.project.load_full(), report.warnings))
    }

    pub async fn run(
        &self,
        snapshot: &Arc<ProjectSnapshot>,
        only: Option<&BTreeSet<String>>,
        filter: Option<&str>,
    ) -> Vec<FileResult> {
        let files: Vec<(&String, &ProjectScript)> = snapshot
            .tests
            .iter()
            .filter(|(key, _)| only.is_none_or(|only| only.contains(*key)))
            .filter(|(key, _)| filter.is_none_or(|f| key.contains(f)))
            .collect();
        let jobs = std::thread::available_parallelism().map_or(4, |n| n.get());
        // Built before they reach the stream: a closure inside the stream
        // leaves the run's future unprovably `Send`, and it could not be
        // spawned.
        let runs: Vec<_> = files
            .into_iter()
            .map(|(key, script)| self.run_file(snapshot, key, script))
            .collect();
        let mut results: Vec<FileResult> =
            stream::iter(runs).buffer_unordered(jobs).collect().await;
        results.sort_by(|a, b| a.path.cmp(&b.path));
        results
    }

    async fn run_file(
        &self,
        snapshot: &Arc<ProjectSnapshot>,
        key: &str,
        script: &ProjectScript,
    ) -> FileResult {
        let started = Instant::now();
        let fail = |message: String| FileResult {
            path: key.to_string(),
            reports: Vec::new(),
            error: Some(FileError {
                message,
                file: None,
                line: None,
            }),
            duration: started.elapsed(),
        };
        let file = match self.env.file_state(key).await {
            Ok(file) => file,
            Err(e) => return fail(format!("could not create the test database: {e}")),
        };
        // A task of its own: a guest runs on the thread that polls it until
        // its deadline, so a file that spins would otherwise hold up every
        // file sharing its task, and a host panic is caught at the join
        // rather than ending the run.
        let outcome = AbortOnDrop(tokio::spawn(execute(
            file.state.clone(),
            snapshot.clone(),
            key.to_string(),
            script.clone(),
        )))
        .await;
        let reports = file.harness.reports();
        let error = match outcome {
            Err(join) => Some(panic_error(join)),
            Ok(Ok(ScriptExecuteOutput::Returned { .. })) if reports.is_empty() => Some(FileError {
                message: "the file reported no tests; define them with describe and it".into(),
                file: None,
                line: None,
            }),
            Ok(Ok(ScriptExecuteOutput::Returned { .. })) => None,
            Ok(Ok(ScriptExecuteOutput::Error {
                message,
                line,
                chunk,
                ..
            })) => Some(FileError {
                message,
                file: chunk,
                line,
            }),
            Ok(Err(e)) => {
                let interpreter = file
                    .state
                    .plugin_registry
                    .get_interpreter_by_language(&script.language_id)
                    .await
                    .map(|p| p.info.id.clone());
                Some(dispatch_failure(e, interpreter.as_deref()))
            }
        };
        file.finish().await;
        FileResult {
            path: key.to_string(),
            reports,
            error,
            duration: started.elapsed(),
        }
    }
}

/// Run one test file on its own state.
async fn execute(
    state: AppState,
    snapshot: Arc<ProjectSnapshot>,
    key: String,
    script: ProjectScript,
) -> Result<ScriptExecuteOutput, DispatchError> {
    // A test file is always source; dispatch runs a compiled module as
    // itself, reading no source, should one ever arrive here.
    let source = match &script.body {
        ScriptBody::Source { source, .. } => source.as_ref(),
        ScriptBody::Module { .. } => "",
    };
    let trigger_id = format!("test:{key}");
    // A project origin: dispatch names the chunk by the file's path, sends its
    // prepared artifact, and resolves its imports against this snapshot for
    // the whole run.
    dispatch(
        &state,
        &Invocation {
            trigger_id: &trigger_id,
            trigger: Trigger::Test,
            language: &script.language_id,
            source,
            input: &Value::Null,
            caller_did: None,
            has_pds_auth: false,
            method: None,
            collection: None,
            params: None,
            delegate_did: None,
            space: None,
            origin: ScriptOrigin::Project {
                snapshot: &snapshot,
                script: &script,
            },
        },
        None,
    )
    .await
}

/// A file's task, aborted when the run that awaits it is dropped. Otherwise
/// its guest would carry on writing to a database in a directory the
/// dropped run's `TestEnv` deletes.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl<T> std::future::Future for AbortOnDrop<T> {
    type Output = Result<T, tokio::task::JoinError>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        std::pin::Pin::new(&mut self.0).poll(cx)
    }
}

/// A file whose task never finished. A panic is a bug in the host, and says
/// so, so it is not read as the test's; a cancelled task was stopped from
/// outside and is no one's bug.
fn panic_error(join: tokio::task::JoinError) -> FileError {
    let message = match join.try_into_panic() {
        Ok(panic) => {
            let reason = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "no message".into());
            format!(
                "happyview panicked running this file ({reason}); this is a bug in happyview, not in the test"
            )
        }
        Err(join) if join.is_cancelled() => {
            "the run was cancelled before this file finished".into()
        }
        Err(join) => join.to_string(),
    };
    FileError {
        message,
        file: None,
        line: None,
    }
}

pub fn print_diagnostics(diagnostics: &[Diagnostic], out: &mut dyn Write) {
    for diagnostic in diagnostics {
        let _ = writeln!(out, "{diagnostic}");
    }
}

/// Runs one at a time in a process, since the runner it prepares holds the
/// process-wide configuration.
pub async fn run(args: &TestArgs, out: &mut (dyn Write + Send)) -> u8 {
    // Before anything resolves: a test run never asks PLC or DNS.
    super::identity::forge_identities();
    let mut runner = match Runner::prepare(args).await {
        Ok(runner) => runner,
        Err(e) => {
            let _ = writeln!(out, "{e}");
            return 2;
        }
    };
    let snapshot = match runner.build().await {
        Ok((snapshot, warnings)) => {
            print_diagnostics(&warnings, out);
            snapshot
        }
        Err(diagnostics) => {
            print_diagnostics(&diagnostics, out);
            return 2;
        }
    };
    let started = Instant::now();
    let results = runner.run(&snapshot, None, args.filter.as_deref()).await;
    let code = if results.is_empty() {
        let _ = match &args.filter {
            Some(filter) => writeln!(out, "no test file matches '{filter}'"),
            None => writeln!(out, "no test files: name one *.test.<ext> under src/"),
        };
        1
    } else {
        match render(&results, started.elapsed(), out) {
            Ok(summary) => exit_code(&summary),
            Err(_) => 1,
        }
    };
    if args.watch {
        // The watch reuses this runner, and with it the lock it holds.
        return super::watch::watch(&mut runner, snapshot, code, args.filter.as_deref(), out).await;
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_interpreter_refusing_a_test_file_is_named_with_the_sdk_it_needs() {
        let error = dispatch_failure(
            DispatchError::Execution(ExecutionError::PluginError {
                code: "BAD_INPUT".into(),
                message: "unknown variant `test`".into(),
                retryable: false,
            }),
            Some("lua"),
        );
        assert!(
            error.message.contains("'lua' interpreter"),
            "{}",
            error.message
        );
        assert!(
            error
                .message
                .contains("happyview-plugin-sdk 0.3.0 or later"),
            "{}",
            error.message
        );
        let other = dispatch_failure(
            DispatchError::Execution(ExecutionError::PluginError {
                code: "HOST_ERROR".into(),
                message: "boom".into(),
                retryable: false,
            }),
            Some("lua"),
        );
        assert!(!other.message.contains("predates"), "{}", other.message);
        let malformed = dispatch_failure(
            DispatchError::Execution(ExecutionError::PluginError {
                code: "BAD_INPUT".into(),
                message: "missing field `source`".into(),
                retryable: false,
            }),
            Some("lua"),
        );
        assert!(
            !malformed.message.contains("predates"),
            "{}",
            malformed.message
        );
        assert!(
            malformed.message.contains("missing field `source`"),
            "{}",
            malformed.message
        );
        let timeout = dispatch_failure(DispatchError::Execution(ExecutionError::Timeout), None);
        assert_eq!(
            timeout.message,
            "the file ran past its 60-second limit for its own code"
        );
    }

    #[tokio::test]
    async fn a_panicking_file_becomes_its_own_error() {
        let join = tokio::spawn(async { panic!("boom") }).await.unwrap_err();
        let message = panic_error(join).message;
        assert!(message.contains("panicked"), "{message}");
        assert!(message.contains("boom"), "{message}");

        let handle = tokio::spawn(std::future::pending::<()>());
        handle.abort();
        assert_eq!(
            panic_error(handle.await.unwrap_err()).message,
            "the run was cancelled before this file finished"
        );
    }
}

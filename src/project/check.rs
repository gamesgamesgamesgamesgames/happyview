//! `happyview check <root>`: the boot-time build, without a database and
//! without serving, for CI.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::cli::CheckArgs;
use crate::plugin::{PluginExecutor, PluginRegistry, WasmRuntime, loader};

use super::build::{BuildEnv, BuildOptions, build_snapshot_with};
use super::diagnostic::Severity;
use super::snapshot::ProjectSnapshot;

/// The interpreters a check or a test run loads when none are named:
/// `$PLUGIN_DIR`, else the project's own, where `hpy` puts the locked plugins.
pub(crate) fn default_plugin_dirs(root: &Path) -> Vec<PathBuf> {
    let dir = crate::config::vars::var("PLUGIN_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join(".happyview").join("plugins"));
    loader::plugin_dirs_in(&dir)
}

/// One connection that is never reaped: a connection closed for idling or age
/// would reopen as a fresh, unmigrated `sqlite::memory:`.
fn in_memory_pool() -> sqlx::pool::PoolOptions<sqlx::Any> {
    crate::db::install_drivers();
    sqlx::pool::PoolOptions::<sqlx::Any>::new()
        .max_connections(1)
        .min_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
}

/// An executor with no instance behind it. `validate` reads no database; the
/// pool is there because every plugin instance's host state carries one. It is
/// migrated so that what a plugin reads or writes through it (its log lines,
/// its allowed hosts) behaves as it does on an instance.
async fn standalone_executor(registry: Arc<PluginRegistry>) -> Result<PluginExecutor, String> {
    let pool = in_memory_pool()
        .connect("sqlite::memory:")
        .await
        .map_err(|e| format!("could not open an in-memory database: {e}"))?;
    crate::db::SQLITE_MIGRATIONS
        .run(&pool)
        .await
        .map_err(|e| format!("could not prepare the in-memory database: {e}"))?;
    crate::record_store::upgrade::run(&pool, crate::db::DatabaseBackend::Sqlite)
        .await
        .map_err(|e| format!("could not prepare the in-memory database: {e}"))?;
    let runtime =
        Arc::new(WasmRuntime::new().map_err(|e| format!("could not start the wasm runtime: {e}"))?);
    Ok(PluginExecutor::new(
        runtime,
        registry,
        pool,
        crate::db::DatabaseBackend::Sqlite,
        reqwest::Client::new(),
        Arc::new(crate::catalog::LexiconCatalog::detached()),
    ))
}

/// The plugins in `dirs`, registered, and an executor over them with no
/// instance behind it. A plugin that will not load is printed and is 2, the
/// exit code for a run that cannot start.
pub(crate) async fn load_standalone(
    dirs: &[PathBuf],
    out: &mut dyn Write,
) -> Result<(Arc<PluginRegistry>, PluginExecutor), u8> {
    let registry = Arc::new(PluginRegistry::new());
    let mut plugins = Vec::new();
    for dir in dirs {
        match loader::load_from_file(dir).await {
            Ok(plugin) => plugins.push(plugin),
            Err(e) => {
                let _ = writeln!(out, "{}: could not load the plugin: {e}", dir.display());
                return Err(2);
            }
        }
    }
    let failures = registry.register_all(plugins).await;
    if !failures.is_empty() {
        for (id, err) in failures {
            let _ = writeln!(out, "plugin {id}: {err}");
        }
        return Err(2);
    }
    match standalone_executor(registry.clone()).await {
        Ok(executor) => Ok((registry, executor)),
        Err(message) => {
            let _ = writeln!(out, "{message}");
            Err(2)
        }
    }
}

/// The plugin directories a run names, else the default ones.
pub(crate) fn plugin_dirs_or_default(named: &[PathBuf], root: &Path) -> Vec<PathBuf> {
    if named.is_empty() {
        default_plugin_dirs(root)
    } else {
        named.to_vec()
    }
}

/// Print a failed build's diagnostics and its error count.
pub(crate) fn print_failed_build(diagnostics: &[super::Diagnostic], out: &mut dyn Write) {
    for diagnostic in diagnostics {
        let _ = writeln!(out, "{diagnostic}");
    }
    let errors = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    let _ = writeln!(out, "{errors} {}", plural(errors, "error", "errors"));
}

/// 0 when the project builds, 1 when it doesn't, 2 when the check can't run.
pub async fn run(args: &CheckArgs, out: &mut dyn Write) -> u8 {
    let dirs = plugin_dirs_or_default(&args.plugin_dirs, &args.root);
    let (registry, executor) = match load_standalone(&dirs, out).await {
        Ok(loaded) => loaded,
        Err(code) => return code,
    };
    let env = BuildEnv {
        plugins: registry,
        executor,
        current: Arc::new(ProjectSnapshot::empty()),
        catalog: None,
    };
    match build_snapshot_with(&env, &args.root, BuildOptions::default()).await {
        Ok(report) => {
            for warning in &report.warnings {
                let _ = writeln!(out, "{warning}");
            }
            let snap = &report.snapshot;
            let _ = writeln!(
                out,
                "ok: {} {}, {} {}, {} {}",
                snap.lexicons.len(),
                plural(snap.lexicons.len(), "lexicon", "lexicons"),
                snap.scripts.len(),
                plural(snap.scripts.len(), "script", "scripts"),
                snap.modules.len(),
                plural(snap.modules.len(), "module", "modules"),
            );
            0
        }
        Err(diagnostics) => {
            print_failed_build(&diagnostics, out);
            1
        }
    }
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

#[cfg(test)]
mod pool_tests {
    use super::*;

    #[test]
    fn the_in_memory_database_is_never_reaped() {
        let options = in_memory_pool();
        assert_eq!(options.get_min_connections(), 1);
        assert_eq!(options.get_idle_timeout(), None);
        assert_eq!(options.get_max_lifetime(), None);
    }
}

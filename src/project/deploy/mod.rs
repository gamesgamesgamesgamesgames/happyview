//! A project deployed through the admin API rather than read from
//! `HAPPYVIEW_PROJECT`: unpacked, built by the same loader, stored in the
//! database and installed as one snapshot.
//!
//! A deployed project is unpacked into a directory of its own and served
//! from there exactly as a `HAPPYVIEW_PROJECT` would be, so the interpreter
//! watcher rebuilds it like any other. The database copy is the durable one:
//! every boot unpacks it again, so no volume is needed to keep it.

pub mod bundle;
pub mod changes;
pub mod store;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use bytes::Bytes;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::config::Config;
use crate::config::project::{PROJECT_FILE, ProjectConfig, load_project_config};
use crate::project::boot::{self, Installed};
use crate::project::{BuildOptions, Diagnostic, build_snapshot, install};

use changes::Changes;

/// How a deployed project's directory is named, which is also how one is
/// told apart from a `HAPPYVIEW_PROJECT` root when it is replaced.
const DIR_PREFIX: &str = "happyview-deploy-";

/// What the last boot could not build of the stored project. Empty once a
/// deploy succeeds. Process-wide because a boot is.
static BOOT_PROBLEMS: Mutex<Vec<Diagnostic>> = Mutex::new(Vec::new());

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub message: String,
}

impl From<&Diagnostic> for Problem {
    fn from(d: &Diagnostic) -> Self {
        Self {
            path: crate::catalog::path_string(&d.path),
            line: d.line,
            message: d.message.clone(),
        }
    }
}

pub fn problems(diagnostics: &[Diagnostic]) -> Vec<Problem> {
    diagnostics.iter().map(Problem::from).collect()
}

#[derive(Debug, Serialize)]
pub struct DeployReport {
    pub sha256: String,
    pub dry_run: bool,
    /// When the bundle now being served was deployed; `None` on a dry run.
    pub deployed_at: Option<String>,
    pub changes: Changes,
    /// Backfills started, or on a dry run, the ones that would start.
    pub backfills: Vec<String>,
    pub warnings: Vec<Problem>,
}

#[derive(Debug)]
pub enum DeployError {
    /// The instance serves `HAPPYVIEW_PROJECT`, which a deploy cannot replace.
    FromFile(PathBuf),
    TooLarge,
    /// `If-Match` named a deployment other than the current one, which is
    /// carried here (`None` when nothing is deployed).
    PreconditionFailed(Option<String>),
    /// Nothing was applied.
    Invalid(Vec<Diagnostic>),
    Internal(String),
}

pub struct DeployRequest<'a> {
    pub bundle: Bytes,
    pub dry_run: bool,
    pub if_match: Option<&'a str>,
    pub deployed_by: &'a str,
}

/// Validate `request.bundle` with the boot loader and, unless it is a dry
/// run, store it and serve it. Any problem leaves the served project as it
/// was.
pub async fn deploy(
    state: &AppState,
    request: DeployRequest<'_>,
) -> Result<DeployReport, DeployError> {
    if let Some(root) = &state.config.project_root {
        return Err(DeployError::FromFile(root.clone()));
    }
    if request.bundle.len() > bundle::MAX_COMPRESSED_BYTES {
        return Err(DeployError::TooLarge);
    }
    let sha256 = hex::encode(Sha256::digest(&request.bundle));

    let turn = boot::install_turn().await;
    let current = store::current(&state.db, state.db_backend)
        .await
        .map_err(|e| DeployError::Internal(format!("could not read the deployed project: {e}")))?;
    if let Some(expected) = request.if_match.map(etag_value) {
        let current_sha = current.as_ref().map(|d| d.sha256.as_str());
        if current_sha != Some(expected) {
            return Err(DeployError::PreconditionFailed(current.map(|d| d.sha256)));
        }
    }
    let staged = unpack(request.bundle.clone()).await?;

    let report = build_snapshot(state, staged.path(), BuildOptions::default())
        .await
        .map_err(DeployError::Invalid)?;
    // The build has just read it, so it parses.
    let config = load_project_config(staged.path())
        .ok()
        .flatten()
        .unwrap_or_default();
    let previous = state.project.load_full();
    let previous_config = previous
        .root
        .as_deref()
        .filter(|root| is_deploy_dir(root))
        .and_then(|root| load_project_config(root).ok().flatten());
    let changes = changes::between(
        &previous,
        previous_config.as_ref(),
        &report.snapshot,
        &config,
    );
    let mut warnings = report.warnings;
    warnings.extend(ignored_sections(&config, &state.config));

    if request.dry_run {
        let backfills = boot::declared_backfills(state, &report.snapshot).await;
        return Ok(DeployReport {
            sha256,
            dry_run: true,
            deployed_at: None,
            changes,
            backfills,
            warnings: problems(&warnings),
        });
    }

    // Unchanged bytes are already stored; only the install is repeated, so a
    // redeploy still recovers an instance whose boot build failed.
    let deployed_at = match current.filter(|d| d.sha256 == sha256) {
        Some(same) => same.deployed_at,
        None => store::save(
            &state.db,
            state.db_backend,
            &sha256,
            &request.bundle,
            request.deployed_by,
        )
        .await
        .map_err(|e| DeployError::Internal(format!("could not store the project: {e}")))?,
    };
    let root = staged.keep();
    install(state, report.snapshot).await;
    // Under the lock: an interpreter rebuild that read the old root before
    // this deploy took it would otherwise build that directory and install
    // the old project over this one. Gone, it fails and changes nothing.
    retire(previous.root.as_deref(), &root);
    drop(turn);
    BOOT_PROBLEMS.lock().unwrap().clear();

    apply_declarations(state, &config).await;
    if !changes.labelers.added.is_empty() {
        let _ = state.labeler_subscriptions_tx.send(());
    }
    let backfills = boot::start_declared_backfills(state).await;
    warnings.extend(boot::shadow_warnings(state).await);
    Ok(DeployReport {
        sha256,
        dry_run: false,
        deployed_at: Some(deployed_at),
        changes,
        backfills,
        warnings: problems(&warnings),
    })
}

/// Serve the stored project at boot, when `HAPPYVIEW_PROJECT` names none.
/// A build that fails here does not stop the instance, which would leave
/// nothing to deploy a fix to: the problems are kept for `GET` to report and
/// the instance serves no project.
pub async fn serve_stored(state: &AppState) -> Option<Installed> {
    if state.config.project_root.is_some() {
        return None;
    }
    let (deployment, archive) = match store::current_bundle(&state.db, state.db_backend).await {
        Ok(Some(found)) => found,
        Ok(None) => return None,
        Err(e) => {
            tracing::error!(error = %e, "could not read the deployed project; serving none");
            return None;
        }
    };
    let staged = match unpack(Bytes::from(archive)).await {
        Ok(staged) => staged,
        Err(DeployError::Invalid(found)) => return failed_boot(&deployment.sha256, found),
        Err(e) => {
            let found = vec![Diagnostic::error("", format!("{e:?}"))];
            return failed_boot(&deployment.sha256, found);
        }
    };
    match boot::build_and_install(state, staged.path()).await {
        Ok(installed) => {
            let root = staged.keep();
            tracing::info!(
                sha256 = %deployment.sha256,
                root = %root.display(),
                "serving the deployed project"
            );
            if let Ok(Some(config)) = load_project_config(&root) {
                apply_declarations(state, &config).await;
            }
            Some(installed)
        }
        Err(found) => failed_boot(&deployment.sha256, found),
    }
}

fn failed_boot(sha256: &str, found: Vec<Diagnostic>) -> Option<Installed> {
    for d in &found {
        tracing::error!("{d}");
    }
    tracing::error!(
        sha256,
        "the deployed project did not build, so none is being served; deploy a fixed one"
    );
    *BOOT_PROBLEMS.lock().unwrap() = found;
    None
}

/// Every NSID a `happyview.toml` declares under `[network_lexicons]`: the
/// boot file's and the deployed project's. Either would restore a row the
/// dashboard deleted, so deleting one is refused and listings say so.
pub fn declared_network_lexicons(state: &AppState) -> Vec<String> {
    let mut declared = state
        .config
        .project
        .as_deref()
        .map(|p| p.network_lexicons.include.clone())
        .unwrap_or_default();
    let snapshot = state.project.load();
    if let Some(config) = snapshot
        .root
        .as_deref()
        .filter(|root| is_deploy_dir(root))
        .and_then(|root| load_project_config(root).ok().flatten())
    {
        declared.extend(config.network_lexicons.include);
    }
    declared
}

pub fn declares_network_lexicon(state: &AppState, nsid: &str) -> bool {
    declared_network_lexicons(state).iter().any(|n| n == nsid)
}

/// What the last boot could not build, for `GET /admin/project`.
pub fn boot_problems() -> Vec<Diagnostic> {
    BOOT_PROBLEMS.lock().unwrap().clone()
}

/// Unpack into a new directory, which is removed again unless the caller
/// keeps it.
async fn unpack(archive: Bytes) -> Result<tempfile::TempDir, DeployError> {
    let staged = tempfile::Builder::new()
        .prefix(DIR_PREFIX)
        .tempdir()
        .map_err(|e| {
            DeployError::Internal(format!("could not create a directory to unpack into: {e}"))
        })?;
    let dest = staged.path().to_path_buf();
    tokio::task::spawn_blocking(move || bundle::unpack(&archive, &dest))
        .await
        .map_err(|e| DeployError::Internal(format!("unpacking stopped: {e}")))?
        .map_err(DeployError::Invalid)?;
    Ok(staged)
}

fn is_deploy_dir(root: &Path) -> bool {
    root.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(DIR_PREFIX))
}

/// Remove the directory the replaced snapshot was built from. A run that
/// pinned that snapshot reads nothing from disk, so this cannot pull files
/// out from under it.
fn retire(previous: Option<&Path>, current: &Path) {
    let Some(previous) = previous.filter(|p| *p != current && is_deploy_dir(p)) else {
        return;
    };
    if let Err(e) = std::fs::remove_dir_all(previous) {
        tracing::warn!(
            path = %previous.display(),
            error = %e,
            "could not remove the replaced project's directory"
        );
    }
}

/// `If-Match: "abc"` and `W/"abc"` both name `abc`.
fn etag_value(header: &str) -> &str {
    let value = header.trim();
    let value = value.strip_prefix("W/").unwrap_or(value);
    value.trim_matches('"')
}

/// The tables a deployed project may not set: they configure the instance
/// rather than the project, and on a hosted one belong to its operator.
/// `[instance]` is only reported for the keys whose value differs from the
/// instance's own, so a project kept in step with its instance deploys
/// quietly. `[interpreters]`, `[libraries]` and `[happyview] version` are
/// ignored too, silently: the deploying client installs the plugins and
/// checks the version itself, and every project names them.
fn ignored_sections(config: &ProjectConfig, live: &Config) -> Vec<Diagnostic> {
    let mut warnings = Vec::new();
    let differing = instance_differences(config, live);
    if !differing.is_empty() {
        let (differ, is) = if differing.len() == 1 {
            ("differs", "is")
        } else {
            ("differ", "are")
        };
        warnings.push(Diagnostic::warning(
            PROJECT_FILE,
            format!(
                "[instance] {} {differ} from this instance's and {is} ignored by a deploy; [instance] configures the instance, not the project",
                differing.join(", ")
            ),
        ));
    }
    let mut ignored = Vec::new();
    if config.dev != Default::default() {
        ignored.push("[dev]");
    }
    if config.jetstream != Default::default() {
        ignored.push("[jetstream]");
    }
    warnings.extend(ignored.into_iter().map(|section| {
        Diagnostic::warning(
            PROJECT_FILE,
            format!(
                "{section} is ignored by a deploy; it configures the instance, not the project"
            ),
        )
    }));
    warnings
}

/// The `[instance]` keys the project sets to something other than what the
/// instance runs with.
fn instance_differences(config: &ProjectConfig, live: &Config) -> Vec<&'static str> {
    let mut differing = Vec::new();
    if let Some(name) = &config.instance.app_name
        && live.app_name.as_deref() != Some(name.as_str())
    {
        differing.push("app_name");
    }
    if let Some(did) = &config.instance.superadmin
        && crate::config::parse_superadmin_did(Some(did))
            .ok()
            .flatten()
            != live.superadmin_did
    {
        differing.push("superadmin");
    }
    differing
}

async fn apply_declarations(state: &AppState, config: &ProjectConfig) {
    crate::config::declarations::apply_project_declarations(
        config,
        &state.config.plc_url,
        &state.db,
        state.db_backend,
        &state.http,
        &state.lexicons,
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_etag_is_read_strong_or_weak() {
        assert_eq!(etag_value("\"abc\""), "abc");
        assert_eq!(etag_value(" W/\"abc\" "), "abc");
        assert_eq!(etag_value("abc"), "abc");
    }

    fn live(app_name: Option<&str>, superadmin: Option<&str>) -> Config {
        let mut config = crate::test_support::test_config(crate::db::DatabaseBackend::Sqlite);
        config.app_name = app_name.map(str::to_string);
        config.superadmin_did = superadmin.map(str::to_string);
        config
    }

    fn warnings(toml: &str, live: &Config) -> Vec<String> {
        let config: ProjectConfig = toml::from_str(toml).unwrap();
        ignored_sections(&config, live)
            .iter()
            .map(|d| d.to_string())
            .collect()
    }

    #[test]
    fn instance_tables_are_named_as_ignored() {
        let found = warnings(
            "[happyview]\nversion = \"3.0.0\"\n[instance]\napp_name = \"x\"\n[jetstream]\ncompression = true\n[lexicons.\"com.example.thing\"]\nbackfill = true\n",
            &live(None, None),
        );
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(
            found
                .iter()
                .all(|w| w.starts_with("happyview.toml: warning: "))
        );
        assert!(
            found
                .iter()
                .any(|w| w.contains("[instance] app_name differs"))
        );
        assert!(found.iter().any(|w| w.contains("[jetstream]")));
    }

    #[test]
    fn an_instance_table_matching_the_instance_is_not_warned_about() {
        let found = warnings(
            "[instance]\napp_name = \"Test 4\"\nsuperadmin = \"did:plc:4jrld6fwpnwqehtce56qshzv\"\n",
            &live(Some("Test 4"), Some("did:plc:4jrld6fwpnwqehtce56qshzv")),
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn only_the_instance_keys_that_differ_are_named() {
        let toml =
            "[instance]\napp_name = \"Mine\"\nsuperadmin = \"did:plc:4jrld6fwpnwqehtce56qshzv\"\n";
        let found = warnings(
            toml,
            &live(Some("Theirs"), Some("did:plc:4jrld6fwpnwqehtce56qshzv")),
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].contains("[instance] app_name differs"),
            "{found:?}"
        );
        assert!(!found[0].contains("superadmin"), "{found:?}");

        let found = warnings(toml, &live(Some("Mine"), None));
        assert!(
            found[0].contains("[instance] superadmin differs"),
            "{found:?}"
        );

        let found = warnings(toml, &live(None, Some("did:plc:someoneelse")));
        assert!(
            found[0].contains(
                "[instance] app_name, superadmin differ from this instance's and are ignored"
            ),
            "{found:?}"
        );
    }

    #[test]
    fn only_a_deploy_directory_is_ever_retired() {
        let project = tempfile::tempdir().unwrap();
        retire(Some(project.path()), Path::new("/elsewhere"));
        assert!(
            project.path().exists(),
            "a HAPPYVIEW_PROJECT root was removed"
        );

        let deployed = tempfile::Builder::new()
            .prefix(DIR_PREFIX)
            .tempdir()
            .unwrap();
        let path = deployed.path().to_path_buf();
        retire(Some(&path), &path);
        assert!(path.exists(), "the directory being served was removed");
        retire(Some(&path), Path::new("/elsewhere"));
        assert!(!path.exists());
    }
}

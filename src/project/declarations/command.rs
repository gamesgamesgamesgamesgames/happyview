//! `happyview types <root>`: build the project as `check` does, describe
//! everything a script can see as types, and have each interpreter render
//! that into `.happyview/types/<language_id>/`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use happyview_plugin_sdk::declarations::{DeclarationsInput, LibraryDeclaration};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::cli::TypesArgs;
use crate::plugin::prepared::ArtifactStamp;
use crate::plugin::{ExecutionError, LoadedPlugin, PluginExecutor, PluginRegistry, PluginType};
use crate::project::build::{BuildEnv, BuildOptions, build_snapshot_with};
use crate::project::check::{load_standalone, plugin_dirs_or_default, print_failed_build};
use crate::project::snapshot::ProjectSnapshot;

use super::ctx::{ctx_shapes, event_shapes};
use super::lexicon::lexicon_declaration;
use super::output::{check_path, sweep, up_to_date, write_language};

/// What became of one interpreter's directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Wrote(usize),
    UpToDate,
    /// The interpreter has no types to offer, and why.
    Skipped(String),
    Failed(String),
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outcome::Wrote(1) => f.write_str("wrote 1 file"),
            Outcome::Wrote(n) => write!(f, "wrote {n} files"),
            Outcome::UpToDate => f.write_str("up to date"),
            Outcome::Skipped(why) => write!(f, "skipped: {why}"),
            Outcome::Failed(why) => write!(f, "failed: {why}"),
        }
    }
}

/// One pass over every interpreter: what each did, and what the input could
/// not describe (a library whose surface does not parse, a lexicon field
/// type with no word in the model).
#[derive(Debug, Default)]
pub struct Generated {
    pub notes: Vec<String>,
    pub languages: Vec<(String, Outcome)>,
}

impl Generated {
    pub fn failed(&self) -> bool {
        self.languages
            .iter()
            .any(|(_, outcome)| matches!(outcome, Outcome::Failed(_)))
    }

    pub fn wrote(&self) -> bool {
        self.languages
            .iter()
            .any(|(_, outcome)| matches!(outcome, Outcome::Wrote(_)))
    }
}

/// 0 when every interpreter wrote, was up to date or had no types to offer;
/// 1 when the build failed or an interpreter did; 2 when the run can't start.
pub async fn run(args: &TypesArgs, out: &mut dyn Write) -> u8 {
    let dirs = plugin_dirs_or_default(&args.plugin_dirs, &args.root);
    let (registry, executor) = match load_standalone(&dirs, out).await {
        Ok(loaded) => loaded,
        Err(code) => return code,
    };
    let env = BuildEnv {
        plugins: registry.clone(),
        executor: executor.clone(),
        current: Arc::new(ProjectSnapshot::empty()),
        catalog: None,
    };
    let report = match build_snapshot_with(&env, &args.root, BuildOptions::default()).await {
        Ok(report) => report,
        Err(diagnostics) => {
            print_failed_build(&diagnostics, out);
            return 1;
        }
    };
    for warning in &report.warnings {
        let _ = writeln!(out, "{warning}");
    }

    let out_dir = args.out.clone().unwrap_or_else(|| default_out(&args.root));
    let generated = generate(&registry, &executor, &report.snapshot, &out_dir).await;
    for note in &generated.notes {
        let _ = writeln!(out, "types: {note}");
    }
    for (language_id, outcome) in &generated.languages {
        let _ = writeln!(out, "types: {language_id}: {outcome}");
    }
    u8::from(generated.failed())
}

/// Bring every interpreter's directory under `out_dir` up to date with
/// `snapshot` and the installed libraries. Shared by `happyview types` and
/// the dev server, which runs it after every build.
pub async fn generate(
    registry: &PluginRegistry,
    executor: &PluginExecutor,
    snapshot: &ProjectSnapshot,
    out_dir: &Path,
) -> Generated {
    let mut generated = Generated::default();
    let lexicons = snapshot.lexicons.values().map(|l| &l.parsed.raw);
    let input = describe(registry, executor, lexicons, &mut generated.notes).await;
    let input_sha256 = hex::encode(Sha256::digest(
        serde_json::to_vec(&input).unwrap_or_default(),
    ));
    let libraries: Vec<Value> = sorted(registry.list_by_type(PluginType::Library).await)
        .iter()
        .map(|plugin| json!({"id": plugin.info.id, "version": plugin.info.version}))
        .collect();

    for plugin in sorted(registry.list_by_type(PluginType::Interpreter).await) {
        let Some(language_id) = plugin.language_id().map(str::to_string) else {
            continue;
        };
        let manifest = json!({
            "interpreter": {
                "id": plugin.info.id,
                "version": plugin.info.version,
                "stamp": ArtifactStamp::of(&plugin).version,
            },
            "libraries": libraries,
            "input_sha256": input_sha256,
        });
        let outcome = render(executor, &plugin, &input, out_dir, &language_id, &manifest).await;
        generated.languages.push((language_id, outcome));
    }
    generated
}

/// One interpreter's directory brought up to date.
async fn render(
    executor: &PluginExecutor,
    plugin: &Arc<LoadedPlugin>,
    input: &DeclarationsInput,
    out_dir: &Path,
    language_id: &str,
    manifest: &Value,
) -> Outcome {
    let io =
        |e: std::io::Error| Outcome::Failed(format!("could not write {}: {e}", out_dir.display()));
    if let Err(e) = std::fs::create_dir_all(out_dir).and_then(|()| sweep(out_dir, language_id)) {
        return io(e);
    }
    if up_to_date(out_dir, language_id, manifest) {
        return Outcome::UpToDate;
    }
    let declarations = match executor.declarations_for(plugin, input).await {
        Ok(Some(declarations)) => declarations,
        Ok(None) => {
            return Outcome::Skipped(format!(
                "{} needs happyview-plugin-sdk 0.4.0 or later for types",
                plugin.info.id
            ));
        }
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    for file in &declarations.files {
        if let Err(why) = check_path(&file.path) {
            return Outcome::Failed(why);
        }
    }
    match write_language(out_dir, language_id, &declarations.files, manifest) {
        Ok(()) => Outcome::Wrote(declarations.files.len()),
        Err(e) => io(e),
    }
}

/// The input every interpreter renders: computed once, so each gets the same.
/// `lexicons` are raw documents: a project's for `happyview types`, every one
/// the instance holds for the dashboard.
pub async fn describe<'a>(
    registry: &PluginRegistry,
    executor: &PluginExecutor,
    lexicons: impl IntoIterator<Item = &'a Value>,
    notes: &mut Vec<String>,
) -> DeclarationsInput {
    let mut libraries = Vec::new();
    for plugin in registry.list_by_type(PluginType::Library).await {
        let Some(namespace) = plugin.namespace() else {
            continue;
        };
        match executor.typed_surface(&plugin.info.id).await {
            Ok(surface) => libraries.push(LibraryDeclaration {
                namespace: namespace.to_string(),
                surface,
            }),
            Err(e @ ExecutionError::BadSurface { .. }) => notes.push(e.to_string()),
            Err(e) => notes.push(format!("library {}: {e}", plugin.info.id)),
        }
    }
    libraries.sort_by(|a, b| a.namespace.cmp(&b.namespace));

    let mut declarations = Vec::new();
    for raw in lexicons {
        let (declaration, warnings) = lexicon_declaration(raw);
        notes.extend(warnings);
        declarations.push(declaration);
    }

    DeclarationsInput {
        libraries,
        lexicons: declarations,
        ctx: ctx_shapes(),
        events: event_shapes(),
    }
}

fn sorted(mut plugins: Vec<Arc<LoadedPlugin>>) -> Vec<Arc<LoadedPlugin>> {
    plugins.sort_by(|a, b| a.info.id.cmp(&b.info.id));
    plugins
}

/// Where `types` writes when `--out` is not given, and where the dev server
/// always does.
pub fn default_out(root: &Path) -> PathBuf {
    root.join(".happyview").join("types")
}

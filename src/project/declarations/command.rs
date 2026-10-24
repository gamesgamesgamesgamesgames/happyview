//! `happyview types <root>`: build the project as `check` does, describe
//! everything a script can see as types, and have each interpreter render
//! that into `.happyview/types/<language_id>/`.

use std::io::Write;
use std::path::PathBuf;
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

    let input = describe(&registry, &executor, &report.snapshot, out).await;
    let input_sha256 = hex::encode(Sha256::digest(
        serde_json::to_vec(&input).unwrap_or_default(),
    ));
    let libraries: Vec<Value> = sorted(registry.list_by_type(PluginType::Library).await)
        .iter()
        .map(|plugin| json!({"id": plugin.info.id, "version": plugin.info.version}))
        .collect();

    let out_dir = args.out.clone().unwrap_or_else(|| default_out(&args.root));
    let mut code = 0;
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
        let line = match render(
            &executor,
            &plugin,
            &input,
            &out_dir,
            &language_id,
            &manifest,
        )
        .await
        {
            Ok(line) => line,
            Err(reason) => {
                code = 1;
                format!("failed: {reason}")
            }
        };
        let _ = writeln!(out, "types: {language_id}: {line}");
    }
    code
}

/// One interpreter's directory brought up to date. `Ok` is what happened,
/// `Err` why it could not.
async fn render(
    executor: &PluginExecutor,
    plugin: &Arc<LoadedPlugin>,
    input: &DeclarationsInput,
    out_dir: &std::path::Path,
    language_id: &str,
    manifest: &Value,
) -> Result<String, String> {
    let io = |e: std::io::Error| format!("could not write {}: {e}", out_dir.display());
    std::fs::create_dir_all(out_dir).map_err(io)?;
    sweep(out_dir, language_id).map_err(io)?;
    if up_to_date(out_dir, language_id, manifest) {
        return Ok("up to date".into());
    }
    let declarations = match executor.declarations_for(plugin, input).await {
        Ok(Some(declarations)) => declarations,
        Ok(None) => {
            return Ok(format!(
                "skipped: {} needs happyview-plugin-sdk 0.4.0 or later for types",
                plugin.info.id
            ));
        }
        Err(e) => return Err(e.to_string()),
    };
    for file in &declarations.files {
        check_path(&file.path)?;
    }
    write_language(out_dir, language_id, &declarations.files, manifest).map_err(io)?;
    let n = declarations.files.len();
    Ok(format!(
        "wrote {n} {}",
        if n == 1 { "file" } else { "files" }
    ))
}

/// The input every interpreter renders: computed once, so each gets the same.
async fn describe(
    registry: &PluginRegistry,
    executor: &PluginExecutor,
    snapshot: &ProjectSnapshot,
    out: &mut dyn Write,
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
            Err(e @ ExecutionError::BadSurface { .. }) => {
                let _ = writeln!(out, "types: {e}");
            }
            Err(e) => {
                let _ = writeln!(out, "types: library {}: {e}", plugin.info.id);
            }
        }
    }
    libraries.sort_by(|a, b| a.namespace.cmp(&b.namespace));

    let mut lexicons = Vec::new();
    for lexicon in snapshot.lexicons.values() {
        let (declaration, warnings) = lexicon_declaration(&lexicon.parsed.raw);
        for warning in warnings {
            let _ = writeln!(out, "types: {warning}");
        }
        lexicons.push(declaration);
    }

    DeclarationsInput {
        libraries,
        lexicons,
        ctx: ctx_shapes(),
        events: event_shapes(),
    }
}

fn sorted(mut plugins: Vec<Arc<LoadedPlugin>>) -> Vec<Arc<LoadedPlugin>> {
    plugins.sort_by(|a, b| a.info.id.cmp(&b.info.id));
    plugins
}

/// Where `types` writes when `--out` is not given.
pub fn default_out(root: &std::path::Path) -> PathBuf {
    root.join(".happyview").join("types")
}

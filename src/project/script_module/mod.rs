//! Compiled script modules: wasm built from a project's `modules/` crates,
//! which run through the interpreter interface with no script body.
//!
//! A module is read, checked and precompiled when the snapshot is built, so
//! everything that can be wrong with one is a build error naming the module,
//! not a trap on its first request.

pub mod sections;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::config::project::ModuleDecl;
use crate::lexicon::LexiconType;
use crate::lua::scripts::{ParsedTrigger, TriggerKind};
use crate::plugin::loader::is_valid_language_id;
use crate::plugin::{LoadedPlugin, PluginManifest, PluginSource, WasmRuntime};
use crate::project::Diagnostic;
use crate::project::snapshot::{ProjectLexicon, ProjectScript, ScriptBody};

pub use sections::{ModuleHandler, SCRIPT_INTERFACE_VERSION};

/// Where `hpy` writes each module's build, relative to the project root.
pub const BUILD_DIR: &str = ".happyview/build";

/// The languages a module can be written in today.
const LANGUAGES: &[&str] = &["rust"];

pub fn artifact_path(name: &str) -> PathBuf {
    Path::new(BUILD_DIR).join(format!("{name}.wasm"))
}

/// The id a module runs under. No installed plugin's id contains a `:`, so a
/// module can never take a plugin's cache entry or log attribution.
///
/// One id per module also means one in-memory cache entry per module: a run
/// still pinned to an older build after a rebuild recompiles it rather than
/// keeping both, which only a dev rebuild overlapping a request can cause.
pub fn module_plugin_id(name: &str) -> String {
    format!("module:{name}")
}

pub struct ScriptModule {
    pub name: String,
    /// The module's crate, project-relative, as `[modules.<name>].path` says.
    pub source_path: PathBuf,
    /// The build this was read from, project-relative.
    pub artifact_path: PathBuf,
    /// Hex sha256 of the wasm bytes.
    pub hash: String,
    pub handlers: Vec<ModuleHandler>,
    /// The module as the executor runs it: interpreter-shaped, under
    /// [`module_plugin_id`], with the capabilities `happyview.toml` declares.
    pub plugin: Arc<LoadedPlugin>,
}

impl std::fmt::Debug for ScriptModule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptModule")
            .field("name", &self.name)
            .field("source_path", &self.source_path)
            .field("hash", &self.hash)
            .field("handlers", &self.handlers)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
pub struct LoadedModules {
    /// One entry per handler, by trigger id.
    pub scripts: BTreeMap<String, ProjectScript>,
    /// By hex sha256.
    pub modules: BTreeMap<String, Arc<ScriptModule>>,
}

/// The wasm a boot keeps in the compiled-module cache: every installed
/// plugin's and every project module's, by hex sha256.
pub async fn boot_live_hashes(
    plugins: &crate::plugin::PluginRegistry,
    snapshot: &crate::project::snapshot::ProjectSnapshot,
) -> std::collections::HashSet<String> {
    let mut live = plugins.live_wasm_sha256s().await;
    live.extend(snapshot.script_modules.keys().cloned());
    live
}

/// Read, check and precompile every module `happyview.toml` declares.
/// `files` maps each trigger a source file claims to that file, since a
/// module handler may not share a trigger with one. Every module is checked
/// before any error is returned, so one build reports everything wrong with
/// the project. It compiles each module, so an async caller runs it off the
/// runtime's worker threads.
pub fn load_modules(
    runtime: &WasmRuntime,
    root: &Path,
    decls: &BTreeMap<String, ModuleDecl>,
    lexicons: &BTreeMap<String, ProjectLexicon>,
    files: &BTreeMap<String, PathBuf>,
) -> Result<LoadedModules, Vec<Diagnostic>> {
    let mut out = LoadedModules::default();
    let mut claimed: BTreeMap<String, String> = BTreeMap::new();
    let mut errors = Vec::new();
    for (name, decl) in decls {
        match load_one(runtime, root, name, decl, lexicons) {
            Ok(module) => {
                for handler in &module.handlers {
                    if let Some(file) = files.get(&handler.trigger) {
                        errors.push(error(
                            &module.source_path,
                            format!(
                                "{} is claimed by both module '{name}' and {}",
                                handler.trigger,
                                file.display()
                            ),
                        ));
                    } else if let Some(other) = claimed.get(&handler.trigger) {
                        errors.push(error(
                            &module.source_path,
                            format!(
                                "{} is claimed by both module '{other}' and module '{name}'",
                                handler.trigger
                            ),
                        ));
                    } else {
                        claimed.insert(handler.trigger.clone(), name.clone());
                        out.scripts.insert(
                            handler.trigger.clone(),
                            ProjectScript {
                                trigger: handler.trigger.clone(),
                                path: module.source_path.clone(),
                                language_id: decl.language.clone(),
                                body: ScriptBody::Module {
                                    module: name.clone(),
                                    hash: module.hash.clone(),
                                },
                                description: None,
                                outbound_xrpcs: Vec::new(),
                            },
                        );
                    }
                }
                out.modules.insert(module.hash.clone(), Arc::new(module));
            }
            Err(mut e) => errors.append(&mut e),
        }
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

fn load_one(
    runtime: &WasmRuntime,
    root: &Path,
    name: &str,
    decl: &ModuleDecl,
    lexicons: &BTreeMap<String, ProjectLexicon>,
) -> Result<ScriptModule, Vec<Diagnostic>> {
    let config = Path::new("happyview.toml");
    // Config parsing already enforces this; it is checked again because it
    // is what keeps the build path inside `BUILD_DIR`, whoever the caller is.
    if !is_valid_language_id(name) {
        return Err(vec![error(
            config,
            format!(
                "module '{name}' needs a name that starts with a lowercase letter and holds \
                 only lowercase letters, digits, '-' and '_', since it names its build file"
            ),
        )]);
    }
    if !LANGUAGES.contains(&decl.language.as_str()) {
        return Err(vec![error(
            config,
            format!(
                "module '{name}': '{}' modules are not supported; supported: {}",
                decl.language,
                LANGUAGES.join(", ")
            ),
        )]);
    }
    let artifact = artifact_path(name);
    let bytes = std::fs::read(root.join(&artifact)).map_err(|e| {
        let message = if e.kind() == std::io::ErrorKind::NotFound {
            format!(
                "module '{name}' has no build at {}; run `hpy happyview build`",
                artifact.display()
            )
        } else {
            format!(
                "module '{name}': could not read its build at {}: {e}",
                artifact.display()
            )
        };
        vec![error(&artifact, message)]
    })?;
    let hash = hex::encode(Sha256::digest(&bytes));
    let sections = sections::read_sections(&bytes)
        .map_err(|e| vec![error(&artifact, format!("module '{name}': {e}"))])?;

    match sections.interface.as_deref() {
        Some(SCRIPT_INTERFACE_VERSION) => {}
        Some(other) => {
            return Err(vec![error(
                &artifact,
                format!(
                    "module '{name}' was built against script interface {other}, and this \
                     HappyView speaks interface {SCRIPT_INTERFACE_VERSION}; rebuild against a \
                     happyview-script release for interface {SCRIPT_INTERFACE_VERSION}"
                ),
            )]);
        }
        None => {
            return Err(vec![error(
                &artifact,
                format!(
                    "module '{name}' has no {} section, so it was not built with \
                     happyview-script's export! macro",
                    sections::INTERFACE_SECTION
                ),
            )]);
        }
    }
    // Fuel is not reliably charged in a frame that catches a throw, so a
    // module that could catch one could run past its instruction budget.
    if sections.uses_exceptions {
        return Err(vec![error(
            &artifact,
            format!(
                "module '{name}' uses wasm exception handling, and catching exceptions is not \
                 supported in a script module; build it with panic = \"abort\", the \
                 wasm32-wasip1 default"
            ),
        )]);
    }
    if sections.handlers.is_empty() {
        return Err(vec![error(
            &decl.path,
            format!(
                "module '{name}' declares no handlers; mark a function with #[query], \
                 #[procedure], #[record_index], #[job] or #[label] and list it in export!"
            ),
        )]);
    }

    let mut errors = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for handler in &sections.handlers {
        if handler.trigger.is_empty() {
            errors.push(error(
                &decl.path,
                format!(
                    "module '{name}' lists a handler{} with no trigger id",
                    handler
                        .function
                        .as_deref()
                        .map(|f| format!(" '{f}'"))
                        .unwrap_or_default()
                ),
            ));
            continue;
        }
        if !seen.insert(handler.trigger.as_str()) {
            errors.push(error(
                &decl.path,
                format!("module '{name}' lists {} twice", handler.trigger),
            ));
            continue;
        }
        if let Err(message) = check_trigger(name, &handler.trigger, lexicons) {
            errors.push(error(&decl.path, message));
        }
    }

    let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
        "id": module_plugin_id(name),
        "name": name,
        "version": "0.0.0",
        "api_version": "2",
        "plugin_type": "interpreter",
        "language_id": decl.language,
        "capabilities": decl.capabilities,
        "wasm_file": artifact,
    }))
    .expect("a module manifest is built from validated fields");
    if let Err(e) = crate::plugin::loader::validate_capabilities(&manifest, &bytes) {
        errors.push(error(
            &artifact,
            format!(
                "module '{name}': {e}; declare it under [modules.{name}] capabilities in \
                 happyview.toml"
            ),
        ));
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    let plugin = Arc::new(LoadedPlugin {
        info: manifest.clone().into(),
        source: PluginSource::File {
            path: root.join(&artifact),
        },
        wasm_bytes: bytes,
        manifest: Some(manifest),
    });
    // Compiling here makes a module wasmtime rejects a build error, and
    // leaves the compiled module in the runtime's cache for the first run.
    runtime.module_for_blocking(&plugin).map_err(|e| {
        vec![error(
            &artifact,
            format!("module '{name}' does not compile: {e}"),
        )]
    })?;

    Ok(ScriptModule {
        name: name.to_string(),
        source_path: decl.path.clone(),
        artifact_path: artifact,
        hash,
        handlers: sections.handlers,
        plugin,
    })
}

/// A handler's trigger must parse, and an XRPC or record trigger must name a
/// lexicon of its own kind in `src/`.
fn check_trigger(
    module: &str,
    trigger: &str,
    lexicons: &BTreeMap<String, ProjectLexicon>,
) -> Result<(), String> {
    let parsed = ParsedTrigger::parse(trigger)
        .map_err(|e| format!("module '{module}' handles '{trigger}': {e}"))?;
    let wanted = match parsed.kind {
        TriggerKind::XrpcQuery => LexiconType::Query,
        TriggerKind::XrpcProcedure => LexiconType::Procedure,
        TriggerKind::RecordIndex
        | TriggerKind::RecordCreate
        | TriggerKind::RecordUpdate
        | TriggerKind::RecordDelete => LexiconType::Record,
        TriggerKind::JobRun | TriggerKind::LabelerApply => return Ok(()),
    };
    let Some(lexicon) = lexicons.get(&parsed.suffix) else {
        return Err(format!(
            "module '{module}' handles {trigger}, but no lexicon {} is in src/lexicons",
            parsed.suffix
        ));
    };
    if lexicon.parsed.lexicon_type != wanted {
        return Err(format!(
            "module '{module}' handles {trigger}, but {} is a {} lexicon",
            lexicon.path.display(),
            lexicon_kind(&lexicon.parsed.lexicon_type)
        ));
    }
    Ok(())
}

fn lexicon_kind(kind: &LexiconType) -> &'static str {
    match kind {
        LexiconType::Record => "record",
        LexiconType::Query => "query",
        LexiconType::Procedure => "procedure",
        LexiconType::Space => "space",
        LexiconType::Definitions => "definitions",
    }
}

fn error(path: &Path, message: String) -> Diagnostic {
    Diagnostic::error(path.to_path_buf(), message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexicon::{ParsedLexicon, ProcedureAction};
    use crate::plugin::capabilities::PluginCapability;

    /// A module that answers nothing useful but carries the given handler
    /// lines, imports, interface section and (optionally) a tag.
    fn module_wat(handlers: &str, imports: &str, interface: Option<&str>, tag: bool) -> Vec<u8> {
        let interface = interface
            .map(|v| format!(r#"(@custom "happyview.interface" "{v}")"#))
            .unwrap_or_default();
        let tag = if tag { "(tag)" } else { "" };
        // A WAT string cannot hold a raw tab or newline; it takes escapes.
        let handlers = handlers.replace('\t', "\\t").replace('\n', "\\n");
        wat::parse_str(format!(
            r#"(module
                {imports}
                {tag}
                (memory (export "memory") 1)
                (func (export "alloc") (param i32) (result i32) i32.const 1024)
                (func (export "dealloc") (param i32 i32))
                (func (export "execute") (param i32 i32) (result i64) i64.const 0)
                (@custom "happyview.handlers" "{handlers}")
                {interface})"#
        ))
        .unwrap()
    }

    fn lexicon(id: &str, kind: &str) -> (String, ProjectLexicon) {
        let main = match kind {
            "record" => serde_json::json!({"type": "record", "key": "tid",
                "record": {"type": "object", "properties": {}}}),
            other => serde_json::json!({"type": other}),
        };
        let parsed = ParsedLexicon::parse(
            serde_json::json!({"lexicon": 1, "id": id, "defs": {"main": main}}),
            1,
            None,
            ProcedureAction::Upsert,
            None,
        )
        .unwrap();
        let path = PathBuf::from(format!("src/lexicons/{}.json", id.replace('.', "/")));
        (
            id.to_string(),
            ProjectLexicon {
                parsed,
                path,
                backfill: false,
            },
        )
    }

    struct Project {
        dir: tempfile::TempDir,
        decls: BTreeMap<String, ModuleDecl>,
        lexicons: BTreeMap<String, ProjectLexicon>,
        files: BTreeMap<String, PathBuf>,
    }

    impl Project {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
                decls: BTreeMap::new(),
                lexicons: [
                    lexicon("app.test.echo", "query"),
                    lexicon("app.test.make", "procedure"),
                    lexicon("app.test.thing", "record"),
                ]
                .into_iter()
                .collect(),
                files: BTreeMap::new(),
            }
        }

        fn module(&mut self, name: &str, wasm: &[u8], capabilities: Vec<PluginCapability>) {
            let path = self.dir.path().join(artifact_path(name));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, wasm).unwrap();
            self.decls.insert(
                name.into(),
                ModuleDecl {
                    language: "rust".into(),
                    path: PathBuf::from(format!("modules/{name}")),
                    capabilities,
                },
            );
        }

        fn load(&self) -> Result<LoadedModules, Vec<Diagnostic>> {
            let runtime = WasmRuntime::without_ticker().unwrap();
            load_modules(
                &runtime,
                self.dir.path(),
                &self.decls,
                &self.lexicons,
                &self.files,
            )
        }

        fn errors(&self) -> Vec<String> {
            self.load()
                .expect_err("the build should fail")
                .into_iter()
                .map(|d| d.message)
                .collect()
        }
    }

    const ECHO: &str = "xrpc.query:app.test.echo\techo\n";

    #[test]
    fn each_handler_becomes_a_project_script_naming_its_module() {
        let mut p = Project::new();
        let wasm = module_wat(
            "xrpc.query:app.test.echo\techo\nrecord.index:app.test.thing\tindex_thing\njob.run:sweep\tsweep\nlabeler.apply:_actor\tlabel\n",
            "",
            Some("1"),
            false,
        );
        p.module("registry", &wasm, vec![]);
        let loaded = p.load().unwrap();

        let hash = hex::encode(sha2::Sha256::digest(&wasm));
        let module = &loaded.modules[&hash];
        assert_eq!(module.name, "registry");
        assert_eq!(module.source_path, PathBuf::from("modules/registry"));
        assert_eq!(module.plugin.info.id, "module:registry");

        let script = &loaded.scripts["xrpc.query:app.test.echo"];
        assert_eq!(script.path, PathBuf::from("modules/registry"));
        assert_eq!(script.language_id, "rust");
        assert!(matches!(
            &script.body,
            ScriptBody::Module { module, hash: h } if module == "registry" && *h == hash
        ));
        assert_eq!(loaded.scripts.len(), 4);
    }

    /// Only a missing build is fixed by building; any other read failure is
    /// reported as itself.
    #[test]
    fn a_build_that_cannot_be_read_says_why() {
        let mut p = Project::new();
        std::fs::create_dir_all(p.dir.path().join(artifact_path("registry"))).unwrap();
        p.decls.insert(
            "registry".into(),
            ModuleDecl {
                language: "rust".into(),
                path: "modules/registry".into(),
                capabilities: vec![],
            },
        );
        let errors = p.errors();
        assert!(
            errors[0].contains("could not read its build at .happyview/build/registry.wasm"),
            "{errors:?}"
        );
        assert!(!errors[0].contains("hpy happyview build"), "{errors:?}");
    }

    #[tokio::test]
    async fn a_boot_keeps_both_plugins_and_project_modules() {
        let mut p = Project::new();
        p.module(
            "registry",
            &module_wat(ECHO, "", Some(SCRIPT_INTERFACE_VERSION), false),
            vec![],
        );
        let loaded = p.load().unwrap();
        let module_hash = loaded.modules.keys().next().unwrap().clone();
        let mut snapshot = crate::project::snapshot::ProjectSnapshot::empty();
        snapshot.script_modules = loaded.modules;

        let plugins = crate::plugin::PluginRegistry::new();
        let plugin = module_wat(ECHO, "", None, false);
        let plugin_hash = hex::encode(Sha256::digest(&plugin));
        let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "id": "installed", "name": "installed", "version": "1.0.0", "api_version": "2",
            "plugin_type": "library", "capabilities": [],
        }))
        .unwrap();
        plugins
            .register(LoadedPlugin {
                info: manifest.clone().into(),
                source: PluginSource::File { path: "x".into() },
                wasm_bytes: plugin,
                manifest: Some(manifest),
            })
            .await;

        assert_eq!(
            boot_live_hashes(&plugins, &snapshot).await,
            [module_hash, plugin_hash].into_iter().collect()
        );
    }

    // The boot sweep deletes every cached module not in this set, so a boot
    // that stopped asking would throw away each module's compiled code.
    #[test]
    fn the_boot_sweep_asks_for_the_live_hashes() {
        assert!(include_str!("../../run.rs").contains("script_module::boot_live_hashes("));
    }

    #[test]
    fn a_module_with_no_build_output_says_how_to_build_it() {
        let mut p = Project::new();
        p.decls.insert(
            "registry".into(),
            ModuleDecl {
                language: "rust".into(),
                path: "modules/registry".into(),
                capabilities: vec![],
            },
        );
        let errors = p.errors();
        assert!(
            errors[0].contains(".happyview/build/registry.wasm"),
            "{errors:?}"
        );
        assert!(errors[0].contains("hpy happyview build"), "{errors:?}");
    }

    #[test]
    fn an_unsupported_interface_version_asks_for_a_rebuild() {
        let mut p = Project::new();
        p.module("registry", &module_wat(ECHO, "", Some("2"), false), vec![]);
        let errors = p.errors();
        assert!(errors[0].contains("interface 2"), "{errors:?}");
        assert!(errors[0].contains("rebuild against"), "{errors:?}");
    }

    #[test]
    fn a_module_without_an_interface_section_was_not_built_with_the_crate() {
        let mut p = Project::new();
        p.module("registry", &module_wat(ECHO, "", None, false), vec![]);
        assert!(p.errors()[0].contains("happyview.interface"));
    }

    #[test]
    fn an_empty_handler_list_is_an_error() {
        let mut p = Project::new();
        p.module("registry", &module_wat("", "", Some("1"), false), vec![]);
        assert!(p.errors()[0].contains("declares no handlers"));
    }

    #[test]
    fn a_module_using_exception_handling_is_refused() {
        let mut p = Project::new();
        p.module("registry", &module_wat(ECHO, "", Some("1"), true), vec![]);
        let errors = p.errors();
        assert!(errors[0].contains("module 'registry'"), "{errors:?}");
        assert!(errors[0].contains("exception handling"), "{errors:?}");
        assert!(
            errors[0].contains("catching exceptions is not supported"),
            "{errors:?}"
        );
    }

    /// A tag can arrive as an import as well as a definition, and either one
    /// is enough for the module to catch a throw.
    #[test]
    fn a_module_importing_a_tag_is_refused() {
        let mut p = Project::new();
        p.module(
            "registry",
            &module_wat(ECHO, r#"(import "env" "t" (tag))"#, Some("1"), false),
            vec![],
        );
        let errors = p.errors();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("module 'registry'"), "{errors:?}");
        assert!(
            errors[0].contains("catching exceptions is not supported"),
            "{errors:?}"
        );
    }

    #[test]
    fn an_xrpc_handler_without_its_lexicon_names_both() {
        let mut p = Project::new();
        p.module(
            "registry",
            &module_wat("xrpc.query:app.test.missing\tgone\n", "", Some("1"), false),
            vec![],
        );
        let errors = p.errors();
        assert!(errors[0].contains("registry"), "{errors:?}");
        assert!(errors[0].contains("app.test.missing"), "{errors:?}");
        assert!(errors[0].contains("src/lexicons"), "{errors:?}");
    }

    #[test]
    fn a_handler_whose_lexicon_is_another_kind_is_refused() {
        let mut p = Project::new();
        p.module(
            "registry",
            &module_wat("xrpc.query:app.test.make\tmake\n", "", Some("1"), false),
            vec![],
        );
        assert!(p.errors()[0].contains("procedure"));
    }

    #[test]
    fn a_trigger_claimed_by_a_module_and_a_file_is_an_error() {
        let mut p = Project::new();
        p.files.insert(
            "xrpc.query:app.test.echo".into(),
            "src/lexicons/app/test/echo.lua".into(),
        );
        p.module("registry", &module_wat(ECHO, "", Some("1"), false), vec![]);
        let errors = p.errors();
        assert!(errors[0].contains("module 'registry'"), "{errors:?}");
        assert!(
            errors[0].contains("src/lexicons/app/test/echo.lua"),
            "{errors:?}"
        );
    }

    #[test]
    fn a_trigger_claimed_by_two_modules_is_an_error() {
        let mut p = Project::new();
        p.module("a", &module_wat(ECHO, "", Some("1"), false), vec![]);
        p.module(
            "b",
            &module_wat("xrpc.query:app.test.echo\tother\n", "", Some("1"), false),
            vec![],
        );
        let errors = p.errors();
        assert!(
            errors[0].contains("module 'a'") && errors[0].contains("module 'b'"),
            "{errors:?}"
        );
    }

    #[test]
    fn a_trigger_listed_twice_in_one_module_is_an_error() {
        let mut p = Project::new();
        p.module(
            "registry",
            &module_wat(
                "xrpc.query:app.test.echo\ta\nxrpc.query:app.test.echo\tb\n",
                "",
                Some("1"),
                false,
            ),
            vec![],
        );
        assert!(p.errors()[0].contains("twice"));
    }

    #[test]
    fn an_import_without_its_capability_points_at_happyview_toml() {
        let mut p = Project::new();
        let wasm = module_wat(
            ECHO,
            r#"(import "env" "host_call_library" (func (param i32 i32 i32 i32 i32 i32) (result i64)))"#,
            Some("1"),
            false,
        );
        p.module("registry", &wasm, vec![]);
        let errors = p.errors();
        assert!(errors[0].contains("library:call"), "{errors:?}");
        assert!(errors[0].contains("[modules.registry]"), "{errors:?}");

        p.decls.get_mut("registry").unwrap().capabilities = vec![PluginCapability::LibraryCall];
        assert!(p.load().is_ok());
    }

    #[test]
    fn a_module_name_that_is_not_a_plain_file_name_is_refused() {
        let mut p = Project::new();
        // A loadable module where the name's build path escapes to, so only
        // the name check stands between it and a successful load.
        let escaped = p.dir.path().join(artifact_path("../escape"));
        std::fs::create_dir_all(p.dir.path().join(BUILD_DIR)).unwrap();
        std::fs::write(&escaped, module_wat(ECHO, "", Some("1"), false)).unwrap();
        assert!(escaped.exists());
        p.decls.insert(
            "../escape".into(),
            ModuleDecl {
                language: "rust".into(),
                path: "modules/x".into(),
                capabilities: vec![],
            },
        );
        let errors = p.errors();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("'../escape'"), "{errors:?}");
        assert!(errors[0].contains("names its build file"), "{errors:?}");
    }

    #[test]
    fn a_handler_with_no_trigger_id_is_refused() {
        let mut p = Project::new();
        p.module(
            "registry",
            &module_wat(&format!("{ECHO}\torphan\n"), "", Some("1"), false),
            vec![],
        );
        let errors = p.errors();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("module 'registry' lists a handler 'orphan' with no trigger id"),
            "{errors:?}"
        );
    }

    #[test]
    fn a_language_without_module_support_is_named() {
        let mut p = Project::new();
        p.module("registry", &module_wat(ECHO, "", Some("1"), false), vec![]);
        p.decls.get_mut("registry").unwrap().language = "go".into();
        assert!(p.errors()[0].contains("'go'"));
    }

    #[test]
    fn every_failing_module_is_reported_at_once() {
        let mut p = Project::new();
        p.module("a", &module_wat("", "", Some("1"), false), vec![]);
        p.module("b", &module_wat(ECHO, "", Some("9"), false), vec![]);
        assert_eq!(p.errors().len(), 2);
    }
}

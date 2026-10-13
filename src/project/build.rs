//! Building a project snapshot from its files: all or nothing, and with no
//! database, so a failed build can't half-apply and `check` needs none.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::AppState;
use crate::catalog::{Layer, LexiconCatalog};
use crate::config::project::{PROJECT_FILE, ProjectConfigError, load_project_config};
use crate::lua::ParsedTrigger;
use crate::plugin::{PluginExecutor, PluginRegistry};
use crate::script::checks;

use super::describe;
use super::diagnostic;
use super::layout::{self, Claim, ExtensionClaims, LexiconIndex, Role};
use super::lexicons;
use super::prepare;
use super::script_module;
use super::snapshot::{ProjectLexicon, ProjectModule, ProjectScript, ProjectSnapshot, ScriptBody};
use super::walk::{self, SrcFile};

pub use super::diagnostic::{Diagnostic, Severity};

#[derive(Debug, Clone, Copy, Default)]
pub struct BuildOptions {
    pub include_tests: bool,
}

pub struct BuildReport {
    pub snapshot: ProjectSnapshot,
    pub warnings: Vec<Diagnostic>,
    pub changes: Vec<String>,
}

/// What a build reads besides the files. Narrower than `AppState` so that
/// `check` can build one without a database.
pub struct BuildEnv {
    /// The interpreters, which claim extensions and check bodies.
    pub plugins: Arc<PluginRegistry>,
    pub executor: PluginExecutor,
    /// The snapshot this build would replace; `changes` is measured
    /// against it.
    pub current: Arc<ProjectSnapshot>,
    /// The instance's lexicons, for the warnings that need more than the
    /// project's own. `None` for `check`, which reads no database.
    pub catalog: Option<LexiconCatalog>,
}

impl BuildEnv {
    pub fn from_state(state: &AppState) -> Self {
        Self {
            plugins: state.plugin_registry.clone(),
            executor: state.plugin_executor(),
            current: state.project.load_full(),
            catalog: Some(state.lexicons.clone()),
        }
    }
}

pub async fn build_snapshot(
    state: &AppState,
    root: &Path,
    opts: BuildOptions,
) -> Result<BuildReport, Vec<Diagnostic>> {
    build_snapshot_with(&BuildEnv::from_state(state), root, opts).await
}

enum Unit {
    Handler(ParsedTrigger),
    Module,
    Test,
}

struct Pending<'a> {
    file: &'a SrcFile,
    claim: Claim,
    unit: Unit,
}

pub async fn build_snapshot_with(
    env: &BuildEnv,
    root: &Path,
    opts: BuildOptions,
) -> Result<BuildReport, Vec<Diagnostic>> {
    let config = match load_project_config(root) {
        Ok(config) => config.unwrap_or_default(),
        Err(e) => return Err(vec![config_diagnostic(e)]),
    };
    let (files, mut diagnostics) = walk::walk_src(root);
    let claims = ExtensionClaims::from_registry(&env.plugins).await;

    // Lexicons first, because a script's trigger is read off the lexicon
    // beside it.
    let lexicon_paths: Vec<&SrcFile> = files.iter().filter(|f| layout::is_lexicon(f)).collect();
    // Every lexicon file, loadable or not: a file that fails to load still
    // names an id, and its twin would otherwise build alone once it is fixed.
    diagnostics.extend(lexicons::duplicate_ids(&lexicon_paths));
    let mut lexicon_files = BTreeSet::new();
    let mut project_lexicons: BTreeMap<String, ProjectLexicon> = BTreeMap::new();
    let mut index = LexiconIndex::default();
    for file in lexicon_paths {
        lexicon_files.insert(lexicons::expected_id(file));
        match lexicons::load_lexicon(file, &config.lexicons) {
            Ok(lexicon) => {
                index.insert(
                    file.dir(),
                    file.stem(),
                    lexicon.parsed.id.clone(),
                    lexicon.parsed.lexicon_type.clone(),
                );
                project_lexicons.insert(lexicon.parsed.id.clone(), lexicon);
            }
            Err(d) => diagnostics.push(d),
        }
    }

    let mut pending = Vec::new();
    let mut handled_by: BTreeMap<String, PathBuf> = BTreeMap::new();
    for file in files.iter().filter(|f| !layout::is_lexicon(f)) {
        diagnostics.extend(layout::wrong_case_extension(file, &claims));
        let role = match layout::classify(file, &claims, &index) {
            Ok(role) => role,
            Err(d) => {
                diagnostics.push(d);
                continue;
            }
        };
        match role {
            Role::Lexicon | Role::Ignored => {}
            Role::Handler { trigger, claim } => {
                let id = trigger.id();
                // Not a precedence rule: which of two files answers would
                // otherwise depend on the order they were found in.
                if let Some(first) = handled_by.get(&id) {
                    diagnostics.push(Diagnostic::error(
                        file.rel_path(),
                        format!(
                            "{} also handles {id}; one trigger runs one script, so remove or rename one of them",
                            first.display()
                        ),
                    ));
                    continue;
                }
                handled_by.insert(id, file.rel_path());
                pending.push(Pending {
                    file,
                    claim,
                    unit: Unit::Handler(trigger),
                });
            }
            Role::Module { claim } => pending.push(Pending {
                file,
                claim,
                unit: Unit::Module,
            }),
            Role::Test { claim } => {
                if let Some(warning) =
                    test_names_a_lexicon(file, &project_lexicons, env.catalog.as_ref()).await
                {
                    diagnostics.push(warning);
                }
                if !opts.include_tests {
                    continue;
                }
                match claim {
                    Some(claim) => pending.push(Pending {
                        file,
                        claim,
                        unit: Unit::Test,
                    }),
                    None => {
                        if let Some((_, extension)) = layout::split_name(file.name())
                            && !layout::is_document(extension)
                        {
                            diagnostics.push(layout::unclaimed(file, extension, &claims));
                        }
                    }
                }
            }
        }
    }

    // Compiled modules claim triggers as files do, so they join the same
    // map: a clash with a file is caught by `load_modules`, and a
    // `[scripts.*]` key naming a module trigger is not stale.
    let loaded_modules = match script_module::load_modules(
        env.executor.runtime(),
        root,
        &config.modules,
        &project_lexicons,
        &handled_by,
    ) {
        Ok(loaded) => {
            for (trigger_id, script) in &loaded.scripts {
                handled_by.insert(trigger_id.clone(), script.path.clone());
            }
            Some(loaded)
        }
        Err(mut errors) => {
            diagnostics.append(&mut errors);
            None
        }
    };

    let mut scripts = BTreeMap::new();
    let mut modules = BTreeMap::new();
    let mut tests = BTreeMap::new();
    for item in pending {
        let path = item.file.rel_path();
        let source = match item.file.read_text() {
            Ok(source) => source,
            Err(d) => {
                diagnostics.push(d);
                continue;
            }
        };
        let trigger_id = match &item.unit {
            Unit::Handler(trigger) => trigger.id(),
            Unit::Module | Unit::Test => String::new(),
        };
        if !passes_checks(&item, &trigger_id, &source, &mut diagnostics) {
            continue;
        }
        let language_id = item.claim.language_id;
        match item.unit {
            Unit::Handler(_) => {
                match describe::script_meta(
                    &trigger_id,
                    &path,
                    &source,
                    config.scripts.get(&trigger_id),
                ) {
                    Ok(meta) => {
                        scripts.insert(
                            trigger_id.clone(),
                            ProjectScript {
                                trigger: trigger_id,
                                path,
                                language_id,
                                body: source_body(source),
                                description: meta.description,
                                outbound_xrpcs: meta.outbound_xrpcs,
                            },
                        );
                    }
                    Err(d) => diagnostics.push(d),
                }
            }
            Unit::Module => {
                let id = item.file.id();
                modules.insert(id.clone(), project_module(id, path, language_id, source));
            }
            Unit::Test => {
                let id = item.file.id();
                let description = describe::leading_description(&source);
                tests.insert(
                    id.clone(),
                    ProjectScript {
                        trigger: id,
                        path,
                        language_id,
                        body: source_body(source),
                        description,
                        outbound_xrpcs: Vec::new(),
                    },
                );
            }
        }
    }

    let mut script_modules = BTreeMap::new();
    if let Some(loaded) = loaded_modules {
        for (trigger_id, mut script) in loaded.scripts {
            // A module has no leading comment to read, so its description
            // comes from `happyview.toml` alone.
            match describe::script_meta(
                &trigger_id,
                &script.path,
                "",
                config.scripts.get(&trigger_id),
            ) {
                Ok(meta) => {
                    script.description = meta.description;
                    script.outbound_xrpcs = meta.outbound_xrpcs;
                }
                Err(d) => diagnostics.push(d),
            }
            scripts.insert(trigger_id, script);
        }
        script_modules = loaded.modules;
    }

    diagnostics.extend(lexicons::stale_overrides(&config.lexicons, &lexicon_files));
    let handled: BTreeSet<String> = handled_by.into_keys().collect();
    diagnostics.extend(describe::stale_overrides(&config.scripts, &handled));

    let mut snapshot = ProjectSnapshot {
        root: Some(root.to_path_buf()),
        generation: 0,
        lexicons: project_lexicons,
        scripts,
        modules,
        tests,
        script_modules,
    };
    // The interpreters see the files only now that every module exists, so
    // an import can be checked in the same pass that prepares it. Every file
    // is prepared whatever else failed, so one run reports every error; the
    // imports wait for a clean build, because a module dropped for its own
    // error would read as missing.
    let (prepared, imports) = prepare::prepare_files(env, &mut snapshot).await;
    diagnostics.extend(prepared);
    if !diagnostic::has_errors(&diagnostics) {
        diagnostics.extend(prepare::check_imports(env, &snapshot, &imports).await);
    }

    diagnostic::sort(&mut diagnostics);
    if diagnostic::has_errors(&diagnostics) {
        return Err(diagnostics);
    }
    let changes = changes(&env.current, &snapshot);
    Ok(BuildReport {
        snapshot,
        warnings: diagnostics,
        changes,
    })
}

/// Serve `snapshot` from now on. One atomic swap, so a run that started
/// before it keeps the snapshot it pinned and a run that starts after sees
/// all of the new one.
///
/// Installs come from one place at a time, boot and then the dev watcher,
/// so reading the generation and storing the next one are not raced.
pub async fn install(state: &AppState, mut snapshot: ProjectSnapshot) {
    snapshot.generation = state.project.load().generation + 1;
    state.project.store(Arc::new(snapshot));

    // Jetstream filters on these, so a project record lexicon it isn't told
    // about never indexes. The subscription reconnects on every change it
    // hears of, so an install that changes nothing must stay silent. The
    // catalog answers sorted, which makes equality the set comparison.
    let collections = state.lexicons.get_record_collections().await;
    state.collections_tx.send_if_modified(|current| {
        if *current == collections {
            return false;
        }
        *current = collections;
        true
    });
}

/// A `happyview.toml` problem as a diagnostic on that file. A parse error
/// already knows its line; its own `Display` names the absolute path, which
/// the diagnostic's path makes redundant.
fn config_diagnostic(e: ProjectConfigError) -> Diagnostic {
    match e {
        ProjectConfigError::Parse { line, message, .. } => Diagnostic::error(PROJECT_FILE, message)
            .at_line(line.and_then(|l| u32::try_from(l).ok())),
        other => Diagnostic::error(PROJECT_FILE, other.to_string()),
    }
}

/// The admin API's checks that read the text alone: removed globals. The
/// interpreter's check runs once the snapshot is assembled, in
/// `prepare::prepare_files` and then `prepare::check_imports`, where every
/// module exists for imports to resolve against.
fn passes_checks(
    item: &Pending<'_>,
    trigger_id: &str,
    source: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let globals = checks::unmigrated_globals(trigger_id, &item.claim.language_id, source);
    if !globals.is_empty() {
        diagnostics.push(Diagnostic::error(
            item.file.rel_path(),
            checks::unmigrated_message(&globals),
        ));
        return false;
    }
    true
}

/// A test file whose name, without its extension, is a lexicon the instance
/// knows: most likely a label handler for a collection whose NSID ends in
/// `.test`, which a file can't express because tests win.
async fn test_names_a_lexicon(
    file: &SrcFile,
    project: &BTreeMap<String, ProjectLexicon>,
    catalog: Option<&LexiconCatalog>,
) -> Option<Diagnostic> {
    let (meant, extension) = layout::split_name(file.name())?;
    if layout::is_document(extension) {
        return None;
    }
    // The project layer is answered by `project`, which is this build's;
    // the catalog's copy is the previous snapshot's and would keep a deleted
    // lexicon alive for one more rebuild.
    let known = project.contains_key(meant)
        || match catalog {
            Some(catalog) => catalog
                .get_entry(meant)
                .await
                .is_some_and(|entry| entry.layer != Layer::Project),
            None => false,
        };
    known.then(|| {
        Diagnostic::warning(
            file.rel_path(),
            format!(
                "{meant} is a lexicon this instance knows, but a name ending in .test.<ext> is always a test; \
                 a handler for {meant} has to be created in the dashboard"
            ),
        )
    })
}

/// The one place a source body is built.
fn source_body(source: String) -> ScriptBody {
    ScriptBody::Source {
        source: source.into(),
        artifact: None,
    }
}

/// The one place a `ProjectModule` is built.
fn project_module(id: String, path: PathBuf, language_id: String, source: String) -> ProjectModule {
    ProjectModule {
        id,
        path,
        language_id,
        source: source.into(),
        artifact: None,
    }
}

fn body_text(body: &ScriptBody) -> &str {
    match body {
        ScriptBody::Source { source, .. } => source.as_ref(),
        // A rebuilt module is a changed script.
        ScriptBody::Module { hash, .. } => hash.as_str(),
    }
}

/// What differs between two snapshots, one line per entry, for the line a
/// rebuild prints.
pub fn changes(old: &ProjectSnapshot, new: &ProjectSnapshot) -> Vec<String> {
    let mut out = Vec::new();
    diff(&mut out, "lexicon", &old.lexicons, &new.lexicons, |a, b| {
        a.path == b.path
            && a.backfill == b.backfill
            && a.parsed.raw == b.parsed.raw
            && a.parsed.target_collection == b.parsed.target_collection
            && a.parsed.action == b.parsed.action
            && a.parsed.token_cost == b.parsed.token_cost
    });
    diff(&mut out, "script", &old.scripts, &new.scripts, |a, b| {
        a.path == b.path
            && a.language_id == b.language_id
            && body_text(&a.body) == body_text(&b.body)
            && a.description == b.description
            && a.outbound_xrpcs == b.outbound_xrpcs
    });
    diff(&mut out, "module", &old.modules, &new.modules, |a, b| {
        a.path == b.path && a.source == b.source
    });
    diff(&mut out, "test", &old.tests, &new.tests, |a, b| {
        body_text(&a.body) == body_text(&b.body)
    });
    out
}

fn diff<V>(
    out: &mut Vec<String>,
    noun: &str,
    old: &BTreeMap<String, V>,
    new: &BTreeMap<String, V>,
    same: impl Fn(&V, &V) -> bool,
) {
    for (key, value) in new {
        match old.get(key) {
            None => out.push(format!("added {noun} {key}")),
            Some(previous) if !same(previous, value) => out.push(format!("changed {noun} {key}")),
            Some(_) => {}
        }
    }
    for key in old.keys().filter(|key| !new.contains_key(*key)) {
        out.push(format!("removed {noun} {key}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::loader::{built_fixture, load_from_file};
    use crate::test_support::{memory_pool, test_state_with_pool};
    use serde_json::json;

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, contents) in files {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        dir
    }

    fn lexicon(id: &str, kind: &str) -> String {
        let main = match kind {
            "record" => {
                json!({"type": "record", "key": "tid", "record": {"type": "object", "properties": {}}})
            }
            "space" => json!({"type": "space", "key": "any", "name": "Room", "collections": []}),
            other => json!({"type": other}),
        };
        json!({"lexicon": 1, "id": id, "defs": {"main": main}}).to_string()
    }

    /// The echo fixture twice: as `echo` for `.echo` files, and as `lua` for
    /// `.lua` files, which is the language the removed-globals check reads.
    /// The fixture's `validate` accepts any source except one starting
    /// `invalid` (a syntax error at line 1), or `no-handle` in a handler.
    async fn env() -> Option<crate::AppState> {
        let dir = built_fixture("interpreter_echo", "wasm32-unknown-unknown")?;
        let state = test_state_with_pool(memory_pool().await);
        let echo = load_from_file(&dir).await.unwrap();
        let mut as_lua = load_from_file(&dir).await.unwrap();
        as_lua.info.id = "echo-as-lua".into();
        let manifest = as_lua.manifest.as_mut().unwrap();
        manifest.id = "echo-as-lua".into();
        manifest.language_id = Some("lua".into());
        manifest.extensions = vec!["lua".into()];
        state.plugin_registry.register(echo).await;
        state.plugin_registry.register(as_lua).await;
        Some(state)
    }

    /// No interpreter at all, so a test using it runs wherever the unit
    /// tests do.
    async fn bare_state() -> crate::AppState {
        test_state_with_pool(memory_pool().await)
    }

    fn shown(diagnostics: &[Diagnostic]) -> Vec<String> {
        diagnostics.iter().map(ToString::to_string).collect()
    }

    const LEX: &str = "src/lexicons/com/example";

    fn basic() -> tempfile::TempDir {
        project(&[
            (
                "happyview.toml",
                "[lexicons.\"com.example.getThing\"]\ntarget_collection = \"com.example.thing\"\n",
            ),
            (
                &format!("{LEX}/getThing.json"),
                &lexicon("com.example.getThing", "query"),
            ),
            (&format!("{LEX}/getThing.echo"), "-- Answers a thing.\necho"),
            (
                &format!("{LEX}/thing.json"),
                &lexicon("com.example.thing", "record"),
            ),
            (&format!("{LEX}/thing.index.echo"), "echo"),
            (
                &format!("{LEX}/room.json"),
                &lexicon("com.example.room", "space"),
            ),
            (&format!("{LEX}/README.md"), "# docs"),
            ("src/jobs/thing.reindex.echo", "echo"),
            ("src/jobs/thing.reindex.test.echo", "echo"),
            ("src/labels/_actor.echo", "echo"),
            ("src/lib/format.echo", "no-handle"),
        ])
    }

    #[tokio::test]
    async fn a_project_builds_every_convention_into_one_snapshot() {
        let Some(state) = env().await else { return };
        let root = basic();
        let report = build_snapshot(&state, root.path(), BuildOptions::default())
            .await
            .unwrap_or_else(|d| panic!("{:#?}", shown(&d)));
        let snap = &report.snapshot;

        assert_eq!(snap.root.as_deref(), Some(root.path()));
        assert_eq!(
            snap.lexicons.keys().collect::<Vec<_>>(),
            [
                "com.example.getThing",
                "com.example.room",
                "com.example.thing"
            ]
        );
        assert_eq!(
            snap.lexicons["com.example.getThing"]
                .parsed
                .target_collection
                .as_deref(),
            Some("com.example.thing")
        );
        assert_eq!(
            snap.scripts.keys().collect::<Vec<_>>(),
            [
                "job.run:thing.reindex",
                "labeler.apply:_actor",
                "record.index:com.example.thing",
                "xrpc.query:com.example.getThing"
            ]
        );
        let handler = &snap.scripts["xrpc.query:com.example.getThing"];
        assert_eq!(handler.path, PathBuf::from(format!("{LEX}/getThing.echo")));
        assert_eq!(handler.language_id, "echo");
        assert_eq!(handler.description.as_deref(), Some("Answers a thing."));
        assert!(
            matches!(&handler.body, ScriptBody::Source { source, .. } if source.as_ref() == "-- Answers a thing.\necho")
        );

        assert_eq!(
            snap.modules.keys().collect::<Vec<_>>(),
            ["src/lib/format.echo"]
        );
        assert_eq!(
            snap.modules["src/lib/format.echo"].path,
            PathBuf::from("src/lib/format.echo")
        );
        assert!(snap.tests.is_empty(), "tests enter only a test build");
        assert!(report.warnings.is_empty(), "{:?}", shown(&report.warnings));
        assert!(
            report
                .changes
                .contains(&"added script xrpc.query:com.example.getThing".to_string()),
            "{:?}",
            report.changes
        );
    }

    #[tokio::test]
    async fn a_test_build_adds_tests_and_never_as_handlers() {
        let Some(state) = env().await else { return };
        let root = basic();
        let report = build_snapshot(
            &state,
            root.path(),
            BuildOptions {
                include_tests: true,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            report.snapshot.tests.keys().collect::<Vec<_>>(),
            ["src/jobs/thing.reindex.test.echo"]
        );
        assert!(
            !report
                .snapshot
                .scripts
                .contains_key("job.run:thing.reindex.test")
        );
    }

    #[tokio::test]
    async fn an_interpreter_refusal_carries_the_file_and_line() {
        let Some(state) = env().await else { return };
        let root = project(&[
            (
                &format!("{LEX}/listThings.json"),
                &lexicon("com.example.listThings", "query"),
            ),
            (&format!("{LEX}/listThings.echo"), "invalid"),
        ]);
        let errors = build_snapshot(&state, root.path(), BuildOptions::default())
            .await
            .err()
            .unwrap();
        assert_eq!(
            shown(&errors),
            [format!(
                "{LEX}/listThings.echo:1: echo: the source says it is invalid"
            )]
        );
    }

    /// A module needs no `handle`, but it must still parse: both files are
    /// reported, the handler for its missing `handle` and the module for its
    /// syntax.
    #[tokio::test]
    async fn a_handler_needs_handle_and_a_module_does_not() {
        let Some(state) = env().await else { return };
        let root = project(&[
            (
                &format!("{LEX}/getThing.json"),
                &lexicon("com.example.getThing", "query"),
            ),
            (&format!("{LEX}/getThing.echo"), "no-handle"),
            ("src/lib/format.echo", "no-handle"),
            ("src/lib/broken.echo", "invalid"),
        ]);
        let errors = build_snapshot(&state, root.path(), BuildOptions::default())
            .await
            .err()
            .unwrap();
        assert_eq!(errors.len(), 2, "{:?}", shown(&errors));
        assert!(errors[0].path.ends_with("getThing.echo"));
        assert_eq!(errors[1].path, PathBuf::from("src/lib/broken.echo"));
        assert_eq!(errors[1].line, Some(1));
    }

    #[tokio::test]
    async fn two_scripts_for_one_trigger_is_an_error_naming_both() {
        let Some(state) = env().await else { return };
        let root = project(&[
            (
                &format!("{LEX}/getThing.json"),
                &lexicon("com.example.getThing", "query"),
            ),
            (&format!("{LEX}/getThing.echo"), "echo"),
            (&format!("{LEX}/getThing.lua"), "function handle() end"),
        ]);
        let errors = build_snapshot(&state, root.path(), BuildOptions::default())
            .await
            .err()
            .unwrap();
        assert_eq!(errors.len(), 1, "{:?}", shown(&errors));
        let message = errors[0].to_string();
        assert!(
            message.contains("getThing.echo") && message.contains("getThing.lua"),
            "{message}"
        );
        assert!(
            message.contains("xrpc.query:com.example.getThing"),
            "{message}"
        );
    }

    /// The same refusal the admin API gives, so a file can't reach
    /// production by a route the dashboard couldn't.
    #[tokio::test]
    async fn removed_globals_are_refused_as_the_admin_api_refuses_them() {
        let Some(state) = env().await else { return };
        let root = project(&[
            (
                &format!("{LEX}/getThing.json"),
                &lexicon("com.example.getThing", "query"),
            ),
            (
                &format!("{LEX}/getThing.lua"),
                "function handle()\n  return db.get(params.uri)\nend\n",
            ),
            ("src/lib/old.lua", "return db.get(x)\n"),
        ]);
        let errors = build_snapshot(&state, root.path(), BuildOptions::default())
            .await
            .err()
            .unwrap();
        assert_eq!(
            shown(&errors),
            [
                format!(
                    "{LEX}/getThing.lua: script references removed globals: db, params -- run the codemod first"
                ),
                "src/lib/old.lua: script references removed globals: db -- run the codemod first"
                    .to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn stale_overrides_are_errors() {
        let Some(state) = env().await else { return };
        let root = project(&[
            (
                "happyview.toml",
                "[lexicons.\"com.example.gone\"]\nbackfill = true\n\n[scripts.\"job.run:gone\"]\ndescription = \"x\"\n",
            ),
            ("src/jobs/here.echo", "echo"),
        ]);
        let errors = build_snapshot(&state, root.path(), BuildOptions::default())
            .await
            .err()
            .unwrap();
        let shown = shown(&errors);
        assert_eq!(shown.len(), 2, "{shown:?}");
        assert!(
            shown.iter().all(|d| d.starts_with("happyview.toml: ")),
            "{shown:?}"
        );
    }

    #[tokio::test]
    async fn a_test_named_like_a_known_lexicon_warns() {
        let Some(state) = env().await else { return };
        let root = project(&[
            (
                "src/lexicons/com/example/thing/test.json",
                &lexicon("com.example.thing.test", "record"),
            ),
            ("src/labels/com.example.thing.test.echo", "echo"),
            ("src/labels/com.example.other.test.echo", "echo"),
        ]);
        let report = build_snapshot(&state, root.path(), BuildOptions::default())
            .await
            .unwrap();
        assert_eq!(report.warnings.len(), 1, "{:?}", shown(&report.warnings));
        let warning = report.warnings[0].to_string();
        assert!(
            warning.starts_with("src/labels/com.example.thing.test.echo: warning: "),
            "{warning}"
        );
        assert!(warning.contains("dashboard"), "{warning}");
    }

    fn assert_empty(report: &BuildReport) {
        let snap = &report.snapshot;
        assert!(snap.lexicons.is_empty() && snap.scripts.is_empty());
        assert!(snap.modules.is_empty() && snap.tests.is_empty());
        assert!(report.warnings.is_empty() && report.changes.is_empty());
    }

    #[tokio::test]
    async fn a_project_without_happyview_toml_builds() {
        let state = bare_state().await;
        let root = project(&[("src/.gitkeep", "")]);
        assert!(!root.path().join("happyview.toml").exists());
        let report = build_snapshot(&state, root.path(), BuildOptions::default())
            .await
            .unwrap_or_else(|d| panic!("{:#?}", shown(&d)));
        assert_empty(&report);
    }

    #[tokio::test]
    async fn an_empty_src_builds_an_empty_snapshot() {
        let state = bare_state().await;
        let root = project(&[("happyview.toml", "# nothing configured yet\n")]);
        std::fs::create_dir(root.path().join("src")).unwrap();
        let report = build_snapshot(&state, root.path(), BuildOptions::default())
            .await
            .unwrap_or_else(|d| panic!("{:#?}", shown(&d)));
        assert_empty(&report);
    }

    #[tokio::test]
    async fn an_unclaimed_test_matters_only_to_a_test_build() {
        let Some(state) = env().await else { return };
        let root = project(&[
            ("src/lib/format.test.py", "x"),
            ("src/lib/notes.test.md", "x"),
        ]);
        assert!(
            build_snapshot(&state, root.path(), BuildOptions::default())
                .await
                .is_ok()
        );
        let errors = build_snapshot(
            &state,
            root.path(),
            BuildOptions {
                include_tests: true,
            },
        )
        .await
        .err()
        .unwrap();
        assert_eq!(errors.len(), 1, "{:?}", shown(&errors));
        assert!(errors[0].path.ends_with("format.test.py"));
    }

    #[tokio::test]
    async fn changes_are_measured_against_the_current_snapshot() {
        let Some(state) = env().await else { return };
        let root = basic();
        let first = build_snapshot(&state, root.path(), BuildOptions::default())
            .await
            .unwrap();
        std::fs::write(
            root.path().join(format!("{LEX}/getThing.echo")),
            "echo again",
        )
        .unwrap();
        std::fs::remove_file(root.path().join("src/lib/format.echo")).unwrap();

        let env = BuildEnv {
            current: Arc::new(first.snapshot),
            ..BuildEnv::from_state(&state)
        };
        let second = build_snapshot_with(&env, root.path(), BuildOptions::default())
            .await
            .unwrap();
        assert_eq!(
            second.changes,
            [
                "changed script xrpc.query:com.example.getThing",
                "removed module src/lib/format.echo"
            ]
        );
    }

    #[tokio::test]
    async fn a_config_error_stops_the_build_at_its_line() {
        let Some(state) = env().await else { return };
        let root = project(&[
            (
                "happyview.toml",
                "[lexicons.\"com.example.getThing\"]\nbackfill = \"yes\"\n",
            ),
            (
                &format!("{LEX}/getThing.json"),
                &lexicon("com.example.getThing", "query"),
            ),
            (&format!("{LEX}/getThing.echo"), "invalid"),
        ]);
        let errors = build_snapshot(&state, root.path(), BuildOptions::default())
            .await
            .err()
            .unwrap();
        assert_eq!(errors.len(), 1, "{:?}", shown(&errors));
        assert!(
            shown(&errors)[0].starts_with("happyview.toml:2: "),
            "{:?}",
            shown(&errors)
        );
    }

    /// Both spellings of one NSID's path load on their own, so only the
    /// duplicate check stops them.
    #[tokio::test]
    async fn two_lexicon_files_for_one_id_are_an_error() {
        let Some(state) = env().await else { return };
        let root = project(&[
            (
                "src/lexicons/com/example.thing.json",
                &lexicon("com.example.thing", "record"),
            ),
            (
                "src/lexicons/com/example/thing.json",
                &lexicon("com.example.thing", "record"),
            ),
        ]);
        let errors = build_snapshot(&state, root.path(), BuildOptions::default())
            .await
            .err()
            .unwrap();
        assert_eq!(
            shown(&errors),
            [
                "src/lexicons/com/example.thing.json: src/lexicons/com/example/thing.json also names the lexicon 'com.example.thing'; keep one of the two files"
            ]
        );
    }

    /// A file that fails to load still has a twin worth naming.
    #[tokio::test]
    async fn a_duplicate_is_reported_even_when_one_file_fails_to_load() {
        let Some(state) = env().await else { return };
        let root = project(&[
            ("src/lexicons/com/example.thing.json", "{ not json"),
            (
                "src/lexicons/com/example/thing.json",
                &lexicon("com.example.thing", "record"),
            ),
        ]);
        let errors = shown(
            &build_snapshot(&state, root.path(), BuildOptions::default())
                .await
                .err()
                .unwrap(),
        );
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(
            errors[1].starts_with("src/lexicons/com/example.thing.json:1: invalid JSON"),
            "{errors:?}"
        );
        assert!(
            errors[0].starts_with("src/lexicons/com/example.thing.json: src/lexicons/com/example/thing.json also names"),
            "{errors:?}"
        );
    }

    /// A lexicon that is present but broken is a broken lexicon, not a
    /// stale override.
    #[tokio::test]
    async fn an_override_for_a_broken_lexicon_is_not_called_stale() {
        let Some(state) = env().await else { return };
        let root = project(&[
            (
                "happyview.toml",
                "[lexicons.\"com.example.thing\"]\nbackfill = true\n",
            ),
            (&format!("{LEX}/thing.json"), "{ not json"),
        ]);
        let errors = shown(
            &build_snapshot(&state, root.path(), BuildOptions::default())
                .await
                .err()
                .unwrap(),
        );
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].starts_with(&format!("{LEX}/thing.json:1: invalid JSON")),
            "{errors:?}"
        );
    }

    #[tokio::test]
    async fn each_kind_of_stale_override_is_named() {
        let Some(state) = env().await else { return };
        let root = project(&[
            (
                "happyview.toml",
                "[lexicons.\"com.example.gone\"]\nbackfill = true\n\n[scripts.\"job.run:gone\"]\ndescription = \"x\"\n",
            ),
            ("src/jobs/here.echo", "echo"),
        ]);
        let errors = shown(
            &build_snapshot(&state, root.path(), BuildOptions::default())
                .await
                .err()
                .unwrap(),
        );
        assert!(
            errors
                .iter()
                .any(|d| d.contains("[lexicons.\"com.example.gone\"] names no lexicon")),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|d| d.contains("[scripts.\"job.run:gone\"] names no script")),
            "{errors:?}"
        );
    }

    #[tokio::test]
    async fn happyview_toml_overrides_a_scripts_description_and_calls() {
        let Some(state) = env().await else { return };
        let root = project(&[
            (
                "happyview.toml",
                "[scripts.\"job.run:thing.reindex\"]\ndescription = \"From the config.\"\noutbound_xrpcs = [\"com.example.other\"]\n",
            ),
            ("src/jobs/thing.reindex.echo", "-- From the file.\necho"),
        ]);
        let report = build_snapshot(&state, root.path(), BuildOptions::default())
            .await
            .unwrap_or_else(|d| panic!("{:#?}", shown(&d)));
        let script = &report.snapshot.scripts["job.run:thing.reindex"];
        assert_eq!(script.description.as_deref(), Some("From the config."));
        assert_eq!(script.outbound_xrpcs, ["com.example.other"]);
    }

    #[tokio::test]
    async fn an_overlong_description_fails_the_build() {
        let Some(state) = env().await else { return };
        let long = "x".repeat(crate::script::checks::MAX_DESCRIPTION_LEN + 1);
        let root = project(&[("src/jobs/thing.reindex.echo", &format!("-- {long}\necho"))]);
        let errors = shown(
            &build_snapshot(&state, root.path(), BuildOptions::default())
                .await
                .err()
                .unwrap(),
        );
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].starts_with("src/jobs/thing.reindex.echo:1: "),
            "{errors:?}"
        );
    }

    #[tokio::test]
    async fn a_test_carries_its_leading_description() {
        let Some(state) = env().await else { return };
        let root = basic();
        std::fs::write(
            root.path().join("src/jobs/thing.reindex.test.echo"),
            "-- Reindexes every thing.\necho",
        )
        .unwrap();
        let report = build_snapshot(
            &state,
            root.path(),
            BuildOptions {
                include_tests: true,
            },
        )
        .await
        .unwrap();
        let test = &report.snapshot.tests["src/jobs/thing.reindex.test.echo"];
        assert_eq!(test.description.as_deref(), Some("Reindexes every thing."));
        assert_eq!(test.path, PathBuf::from("src/jobs/thing.reindex.test.echo"));
    }

    /// The warning reads the instance's lexicons as well as the project's.
    #[tokio::test]
    async fn a_test_named_like_a_catalog_lexicon_warns() {
        let Some(dir) = built_fixture("interpreter_echo", "wasm32-unknown-unknown") else {
            return;
        };
        let state = test_state_with_pool(crate::test_support::migrated_memory_pool().await);
        state
            .plugin_registry
            .register(load_from_file(&dir).await.unwrap())
            .await;
        state
            .lexicons
            .put_manual(
                &state.db,
                state.db_backend,
                crate::catalog::LexiconWrite {
                    lexicon_json: serde_json::from_str(&lexicon(
                        "com.example.remote.test",
                        "record",
                    ))
                    .unwrap(),
                    backfill: false,
                    target_collection: None,
                    action: crate::lexicon::ProcedureAction::Upsert,
                    token_cost: None,
                    authority_did: None,
                },
            )
            .await
            .unwrap();
        let root = project(&[("src/labels/com.example.remote.test.echo", "echo")]);
        let report = build_snapshot(&state, root.path(), BuildOptions::default())
            .await
            .unwrap();
        assert_eq!(report.warnings.len(), 1, "{:?}", shown(&report.warnings));
        assert!(
            report.warnings[0]
                .to_string()
                .contains("com.example.remote.test is a lexicon")
        );
    }

    #[tokio::test]
    async fn a_wrong_case_extension_warns_through_the_build() {
        let state = bare_state().await;
        let root = project(&[("src/lexicons/com/example/getThing.JSON", "{}")]);
        let report = build_snapshot(&state, root.path(), BuildOptions::default())
            .await
            .unwrap();
        assert_eq!(report.warnings.len(), 1, "{:?}", shown(&report.warnings));
        assert!(
            report.warnings[0]
                .to_string()
                .contains("rename it to use .json")
        );
    }

    const NOTE_TEST: &str = "src/lexicons/com/example/note/test.json";

    #[tokio::test]
    async fn a_document_beside_a_test_named_lexicon_does_not_warn() {
        let state = bare_state().await;
        let root = project(&[
            (NOTE_TEST, &lexicon("com.example.note.test", "record")),
            ("src/labels/com.example.note.test.md", "a note"),
            ("src/labels/com.example.note.test.py", "x"),
        ]);
        let report = build_snapshot(&state, root.path(), BuildOptions::default())
            .await
            .unwrap();
        assert_eq!(report.warnings.len(), 1, "{:?}", shown(&report.warnings));
        assert!(report.warnings[0].to_string().contains(".test.py"));
    }

    /// The catalog answers from the snapshot being replaced first, so a
    /// lexicon deleted from the project would otherwise keep warning once.
    #[tokio::test]
    async fn a_test_does_not_warn_for_a_lexicon_only_the_old_snapshot_had() {
        let state = bare_state().await;
        let old = project(&[(NOTE_TEST, &lexicon("com.example.note.test", "record"))]);
        let report = build_snapshot(&state, old.path(), BuildOptions::default())
            .await
            .unwrap();
        install(&state, report.snapshot).await;
        assert!(state.lexicons.get("com.example.note.test").await.is_some());

        let new = project(&[("src/labels/com.example.note.test.py", "x")]);
        let report = build_snapshot(&state, new.path(), BuildOptions::default())
            .await
            .unwrap();
        assert!(report.warnings.is_empty(), "{:?}", shown(&report.warnings));
    }

    #[tokio::test]
    async fn an_installed_snapshot_is_what_the_catalog_answers() {
        let Some(state) = env().await else { return };
        let root = basic();
        let report = build_snapshot(&state, root.path(), BuildOptions::default())
            .await
            .unwrap();
        let before = state.project.load().generation;
        install(&state, report.snapshot).await;

        assert_eq!(state.project.load().generation, before + 1);
        let query = state
            .lexicons
            .get("com.example.getThing")
            .await
            .expect("the project query");
        assert_eq!(
            query.target_collection.as_deref(),
            Some("com.example.thing")
        );
        assert!(
            state
                .lexicons
                .get_space_declaration("com.example.room")
                .await
                .is_some()
        );
        assert!(
            state
                .collections_tx
                .borrow()
                .contains(&"com.example.thing".to_string())
        );

        // An install that changes no collection doesn't wake the Jetstream
        // subscription, which reconnects on every change it is told about.
        let mut rx = state.collections_tx.subscribe();
        let _ = rx.borrow_and_update();
        let again = build_snapshot(&state, root.path(), BuildOptions::default())
            .await
            .unwrap();
        assert!(again.changes.is_empty(), "{:?}", again.changes);
        install(&state, again.snapshot).await;
        assert_eq!(state.project.load().generation, before + 2);
        assert!(!rx.has_changed().unwrap());
    }

    /// A module with one query handler, `xrpc.query:app.test.echo`, that
    /// answers `{"from": <answer>}`.
    fn echo_module_wasm(answer: &str) -> Vec<u8> {
        let body = format!(
            r#"{{"ok":{{"status":"returned","value":{{"from":"{answer}"}},"value_kind":"object"}}}}"#
        );
        module_wasm(
            &body,
            &format!(
                "i64.const 32768 i64.const 32 i64.shl i64.const {} i64.or",
                body.len()
            ),
        )
    }

    /// The same handler, counting to two million before it answers `null`:
    /// roughly ten million fuel, far past a thousand-instruction budget.
    fn counting_module_wasm() -> Vec<u8> {
        let body = r#"{"ok":{"status":"returned","value":null,"value_kind":"other"}}"#;
        module_wasm(
            body,
            &format!(
                "(local i32)
                (loop
                    local.get 2 i32.const 1 i32.add local.tee 2
                    i32.const 2000000 i32.lt_u br_if 0)
                i64.const 32768 i64.const 32 i64.shl i64.const {} i64.or",
                body.len()
            ),
        )
    }

    fn module_wasm(answer: &str, execute: &str) -> Vec<u8> {
        wat::parse_str(format!(
            r#"(module
                (memory (export "memory") 1)
                (data (i32.const 32768) "{escaped}")
                (func (export "alloc") (param i32) (result i32) i32.const 1024)
                (func (export "dealloc") (param i32 i32))
                (func (export "execute") (param i32 i32) (result i64) {execute})
                (@custom "happyview.handlers" "xrpc.query:app.test.echo\techo\n")
                (@custom "happyview.interface" "1"))"#,
            escaped = answer.replace('"', "\\\""),
        ))
        .unwrap()
    }

    const MODULE_TOML: &str =
        "[modules.registry]\nlanguage = \"rust\"\npath = \"modules/registry\"\n";

    fn module_project(wasm: &[u8]) -> tempfile::TempDir {
        let root = project(&[
            ("happyview.toml", MODULE_TOML),
            (
                "src/lexicons/app/test/echo.json",
                &lexicon("app.test.echo", "query"),
            ),
        ]);
        write_module(&root, wasm);
        root
    }

    fn write_module(root: &tempfile::TempDir, wasm: &[u8]) {
        let path = root.path().join(".happyview/build/registry.wasm");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, wasm).unwrap();
    }

    #[tokio::test]
    async fn a_module_handler_is_a_snapshot_script_and_its_module_is_held_by_hash() {
        use sha2::Digest;
        let state = test_state_with_pool(memory_pool().await);
        let wasm = echo_module_wasm("module");
        let root = module_project(&wasm);
        let report = build_snapshot(&state, root.path(), BuildOptions::default())
            .await
            .unwrap_or_else(|d| panic!("{:?}", shown(&d)));

        let hash = hex::encode(sha2::Sha256::digest(&wasm));
        let script = &report.snapshot.scripts["xrpc.query:app.test.echo"];
        assert!(matches!(&script.body, ScriptBody::Module { hash: h, .. } if *h == hash));
        assert_eq!(script.path, PathBuf::from("modules/registry"));
        assert_eq!(report.snapshot.script_modules[&hash].name, "registry");
    }

    /// Preparing sends each script to the interpreter its language names, and
    /// a module's language names none: a module handler reaching it would
    /// fail every project that has one.
    #[tokio::test]
    async fn a_module_builds_where_no_interpreter_claims_its_language() {
        let Some(state) = env().await else { return };
        assert!(
            state
                .plugin_registry
                .get_interpreter_by_language("rust")
                .await
                .is_none()
        );
        let root = module_project(&echo_module_wasm("module"));
        let report = build_snapshot(&state, root.path(), BuildOptions::default())
            .await
            .unwrap_or_else(|d| panic!("{:?}", shown(&d)));
        assert!(report.warnings.is_empty(), "{:?}", shown(&report.warnings));
        assert_eq!(
            report.snapshot.scripts["xrpc.query:app.test.echo"].language_id,
            "rust"
        );
    }

    #[tokio::test]
    async fn a_module_handler_takes_its_description_from_happyview_toml() {
        let state = test_state_with_pool(memory_pool().await);
        let root = project(&[
            (
                "happyview.toml",
                &format!(
                    "{MODULE_TOML}\n[scripts.\"xrpc.query:app.test.echo\"]\ndescription = \"Echoes.\"\n"
                ),
            ),
            (
                "src/lexicons/app/test/echo.json",
                &lexicon("app.test.echo", "query"),
            ),
        ]);
        write_module(&root, &echo_module_wasm("module"));
        let report = build_snapshot(&state, root.path(), BuildOptions::default())
            .await
            .unwrap_or_else(|d| panic!("{:?}", shown(&d)));
        // Not reported as a stale override, and applied.
        assert_eq!(
            report.snapshot.scripts["xrpc.query:app.test.echo"]
                .description
                .as_deref(),
            Some("Echoes.")
        );
    }

    #[tokio::test]
    async fn a_module_and_a_file_for_one_trigger_fail_the_build() {
        let Some(state) = env().await else { return };
        let root = project(&[
            ("happyview.toml", MODULE_TOML),
            (
                "src/lexicons/app/test/echo.json",
                &lexicon("app.test.echo", "query"),
            ),
            ("src/lexicons/app/test/echo.echo", "echo"),
        ]);
        write_module(&root, &echo_module_wasm("module"));
        let errors = build_snapshot(&state, root.path(), BuildOptions::default())
            .await
            .err()
            .expect("two handlers for one trigger");
        let shown = shown(&errors).join("\n");
        assert!(shown.contains("module 'registry'"), "{shown}");
        assert!(shown.contains("src/lexicons/app/test/echo.echo"), "{shown}");
    }

    async fn run(
        state: &crate::AppState,
        resolved: &crate::lua::scripts::ResolvedScript,
    ) -> Result<crate::plugin::ScriptExecuteOutput, crate::script::DispatchError> {
        use crate::script::{Invocation, ScriptOrigin, Trigger, dispatch};
        let input = serde_json::json!({});
        dispatch(
            state,
            &Invocation {
                trigger_id: &resolved.id,
                trigger: Trigger::XrpcQuery,
                language: &resolved.script_type,
                source: &resolved.body,
                input: &input,
                caller_did: None,
                has_pds_auth: false,
                method: Some("app.test.echo"),
                collection: None,
                params: None,
                delegate_did: None,
                space: None,
                origin: ScriptOrigin::of(resolved),
            },
            None,
        )
        .await
    }

    async fn installed(
        state: &crate::AppState,
        root: &Path,
    ) -> crate::lua::scripts::ResolvedScript {
        let report = build_snapshot(state, root, BuildOptions::default())
            .await
            .unwrap_or_else(|d| panic!("{:?}", shown(&d)));
        install(state, report.snapshot).await;
        let trigger = ParsedTrigger::parse("xrpc.query:app.test.echo").unwrap();
        state
            .scripts
            .resolve_in(&state.project.load_full(), &trigger)
            .await
            .unwrap()
    }

    fn answered(word: &str) -> crate::plugin::ScriptExecuteOutput {
        crate::plugin::ScriptExecuteOutput::Returned {
            value: serde_json::json!({ "from": word }),
            value_kind: crate::plugin::ScriptValueKind::Object,
        }
    }

    #[tokio::test]
    async fn a_module_script_dispatches_to_its_module_and_a_pinned_run_keeps_it() {
        let state = test_state_with_pool(memory_pool().await);
        let root = module_project(&echo_module_wasm("module"));
        let old = installed(&state, root.path()).await;

        // Different bytes under the same module name: the runtime's cache
        // entry for `module:registry` is replaced.
        write_module(&root, &echo_module_wasm("rebuilt"));
        let new = installed(&state, root.path()).await;

        assert_eq!(run(&state, &new).await.unwrap(), answered("rebuilt"));
        assert_eq!(
            run(&state, &old).await.unwrap(),
            answered("module"),
            "a pinned run keeps its module"
        );
    }

    /// A module has no interpreter counting its instructions, so a dispatch
    /// that reached it without fuel would run unbounded until the wall clock.
    #[tokio::test]
    async fn a_dispatched_module_runs_on_a_finite_fuel_budget() {
        let state = test_state_with_pool(memory_pool().await);
        state.script_limits.set_instruction_limit(1_000);
        state.script_limits.set_wall_clock_seconds(300);
        let root = module_project(&counting_module_wasm());
        let resolved = installed(&state, root.path()).await;

        let started = std::time::Instant::now();
        let err = run(&state, &resolved)
            .await
            .expect_err("100,000 fuel cannot count to two million");
        assert!(
            matches!(
                err,
                crate::script::DispatchError::Execution(crate::plugin::ExecutionError::Timeout)
            ),
            "{err}"
        );
        // Fuel, not the wall clock, ended it.
        assert!(started.elapsed() < std::time::Duration::from_secs(60));
    }
}

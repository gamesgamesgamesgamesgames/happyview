//! Which role each file under `src/` plays, decided from its path alone.

use std::collections::BTreeMap;

use crate::lexicon::LexiconType;
use crate::lua::{ParsedTrigger, TriggerKind};

use super::diagnostic::Diagnostic;
use super::walk::SrcFile;

/// Extensions that mark prose or data. One of these where a handler would go
/// is a README or a fixture, not a script in a language nobody installed.
const DOCUMENT_EXTENSIONS: &[&str] = &["md", "markdown", "txt", "json"];

/// The interpreter a file's extension selects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub plugin_id: String,
    pub language_id: String,
}

#[derive(Debug, Clone, Default)]
pub struct ExtensionClaims {
    by_extension: BTreeMap<String, Claim>,
}

impl ExtensionClaims {
    /// The first claim on an extension stands. `PluginRegistry::install`
    /// refuses a second, so only the unchecked `register` used by tests can
    /// produce one.
    pub fn insert(&mut self, extension: &str, plugin_id: &str, language_id: &str) {
        self.by_extension
            .entry(extension.to_string())
            .or_insert_with(|| Claim {
                plugin_id: plugin_id.into(),
                language_id: language_id.into(),
            });
    }

    /// Every extension an installed interpreter claims. Sorted by plugin id
    /// so the rare duplicate an unchecked `register` allows resolves the
    /// same way on every build.
    pub async fn from_registry(registry: &crate::plugin::PluginRegistry) -> Self {
        let mut interpreters = registry
            .list_by_type(crate::plugin::PluginType::Interpreter)
            .await;
        interpreters.sort_by(|a, b| a.info.id.cmp(&b.info.id));
        let mut claims = Self::default();
        for plugin in &interpreters {
            let Some(language_id) = plugin.language_id() else {
                continue;
            };
            for extension in plugin.extensions() {
                claims.insert(extension, &plugin.info.id, language_id);
            }
        }
        claims
    }

    pub fn get(&self, extension: &str) -> Option<&Claim> {
        self.by_extension.get(extension)
    }

    /// What is installed, for a message about an extension nothing claims.
    pub fn describe(&self) -> String {
        if self.by_extension.is_empty() {
            return "no interpreter is installed".into();
        }
        let installed: Vec<String> = self
            .by_extension
            .iter()
            .map(|(extension, claim)| format!(".{extension} ({})", claim.plugin_id))
            .collect();
        format!("installed: {}", installed.join(", "))
    }
}

#[derive(Debug, Clone)]
pub struct IndexedLexicon {
    pub id: String,
    pub kind: LexiconType,
}

/// The lexicons of a build, by the directory and stem of their file, which is
/// how a script finds the lexicon beside it.
#[derive(Debug, Clone, Default)]
pub struct LexiconIndex {
    by_file: BTreeMap<(Vec<String>, String), IndexedLexicon>,
}

impl LexiconIndex {
    pub fn insert(&mut self, dir: &[String], stem: &str, id: String, kind: LexiconType) {
        self.by_file.insert(
            (dir.to_vec(), stem.to_string()),
            IndexedLexicon { id, kind },
        );
    }

    pub fn sibling(&self, dir: &[String], stem: &str) -> Option<&IndexedLexicon> {
        self.by_file.get(&(dir.to_vec(), stem.to_string()))
    }
}

#[derive(Debug, Clone)]
pub enum Role {
    Lexicon,
    Handler {
        trigger: ParsedTrigger,
        claim: Claim,
    },
    Module {
        claim: Claim,
    },
    /// `claim` is `None` when no interpreter runs the extension; whether
    /// that matters depends on whether the build includes tests.
    Test {
        claim: Option<Claim>,
    },
    Ignored,
}

/// `(base, extension)` split at the last dot, or `None` for a name without
/// one.
pub fn split_name(name: &str) -> Option<(&str, &str)> {
    name.rsplit_once('.')
        .filter(|(base, extension)| !base.is_empty() && !extension.is_empty())
}

/// `*.test.<ext>` with something before `.test`.
pub fn is_test_name(name: &str) -> bool {
    split_name(name).is_some_and(|(base, _)| base.len() > ".test".len() && base.ends_with(".test"))
}

pub fn is_document(extension: &str) -> bool {
    DOCUMENT_EXTENSIONS.contains(&extension)
}

pub fn is_lexicon(file: &SrcFile) -> bool {
    file.segments.len() > 1
        && file.segments[0] == "lexicons"
        && file.name().ends_with(".json")
        && !is_test_name(file.name())
}

pub fn unclaimed(file: &SrcFile, extension: &str, claims: &ExtensionClaims) -> Diagnostic {
    Diagnostic::error(
        file.rel_path(),
        format!(
            "no installed interpreter runs .{extension} files ({}); install one that does, or rename the file",
            claims.describe()
        ),
    )
}

pub fn classify(
    file: &SrcFile,
    claims: &ExtensionClaims,
    lexicons: &LexiconIndex,
) -> Result<Role, Diagnostic> {
    let name = file.name();
    let Some((base, extension)) = split_name(name) else {
        return Ok(Role::Ignored);
    };
    let claim = claims.get(extension).cloned();
    if is_test_name(name) {
        return Ok(Role::Test { claim });
    }
    let nested = file.segments.len() > 1;
    match file.segments[0].as_str() {
        "lexicons" if nested && extension == "json" => Ok(Role::Lexicon),
        "lexicons" if nested => beside_lexicons(file, base, extension, claim, claims, lexicons),
        "jobs" if nested => flat(file, Flat::Jobs, base, extension, claim, claims),
        "labels" if nested => flat(file, Flat::Labels, base, extension, claim, claims),
        _ => Ok(module_or_ignored(claim)),
    }
}

enum Flat {
    Jobs,
    Labels,
}

/// `src/jobs/` and `src/labels/` hold handlers named by their whole job type
/// or NSID. The name goes through the trigger grammar itself, so a file can
/// name exactly what the dashboard can.
fn flat(
    file: &SrcFile,
    which: Flat,
    base: &str,
    extension: &str,
    claim: Option<Claim>,
    claims: &ExtensionClaims,
) -> Result<Role, Diagnostic> {
    let (dir, prefix, what) = match which {
        Flat::Jobs => ("jobs", "job.run", "job type"),
        Flat::Labels => ("labels", "labeler.apply", "NSID or _actor"),
    };
    if file.segments.len() > 2 {
        return Err(Diagnostic::error(
            file.rel_path(),
            format!(
                "src/{dir}/ is flat: a file's name is its whole {what}, so it cannot sit in a subdirectory"
            ),
        ));
    }
    if claim.is_none() && is_document(extension) {
        return Ok(Role::Ignored);
    }
    let claim = require_claim(file, extension, claim, claims)?;
    let trigger = ParsedTrigger::parse(&format!("{prefix}:{base}"))
        .map_err(|e| Diagnostic::error(file.rel_path(), e))?;
    Ok(Role::Handler { trigger, claim })
}

fn module_or_ignored(claim: Option<Claim>) -> Role {
    claim.map_or(Role::Ignored, |claim| Role::Module { claim })
}

fn record_kind(segment: &str) -> Option<TriggerKind> {
    match segment {
        "index" => Some(TriggerKind::RecordIndex),
        "create" => Some(TriggerKind::RecordCreate),
        "update" => Some(TriggerKind::RecordUpdate),
        "delete" => Some(TriggerKind::RecordDelete),
        _ => None,
    }
}

fn type_name(kind: &LexiconType) -> &'static str {
    match kind {
        LexiconType::Record => "record",
        LexiconType::Query => "query",
        LexiconType::Procedure => "procedure",
        LexiconType::Space => "space",
        LexiconType::Definitions => "definitions",
    }
}

fn require_claim(
    file: &SrcFile,
    extension: &str,
    claim: Option<Claim>,
    claims: &ExtensionClaims,
) -> Result<Claim, Diagnostic> {
    claim.ok_or_else(|| unclaimed(file, extension, claims))
}

fn beside_lexicons(
    file: &SrcFile,
    base: &str,
    extension: &str,
    claim: Option<Claim>,
    claims: &ExtensionClaims,
    lexicons: &LexiconIndex,
) -> Result<Role, Diagnostic> {
    let dir = file.dir();
    let documentation = claim.is_none() && is_document(extension);

    if let Some((stem, action)) = base.rsplit_once('.')
        && let Some(kind) = record_kind(action)
    {
        if documentation {
            return Ok(Role::Ignored);
        }
        let Some(lexicon) = lexicons.sibling(dir, stem) else {
            return Err(Diagnostic::error(
                file.rel_path(),
                format!(
                    "'.{action}.' binds a record trigger, but there is no {stem}.json beside this file"
                ),
            ));
        };
        if lexicon.kind != LexiconType::Record {
            return Err(Diagnostic::error(
                file.rel_path(),
                format!(
                    "{stem}.json is a {} lexicon; only a record lexicon takes a .{action}. script",
                    type_name(&lexicon.kind)
                ),
            ));
        }
        let claim = require_claim(file, extension, claim, claims)?;
        return Ok(Role::Handler {
            trigger: ParsedTrigger::new(kind, lexicon.id.clone()),
            claim,
        });
    }

    let Some(lexicon) = lexicons.sibling(dir, base) else {
        return Ok(module_or_ignored(claim));
    };
    if documentation {
        return Ok(Role::Ignored);
    }
    let kind = match lexicon.kind {
        LexiconType::Query => TriggerKind::XrpcQuery,
        LexiconType::Procedure => TriggerKind::XrpcProcedure,
        LexiconType::Record => {
            return Err(Diagnostic::error(
                file.rel_path(),
                format!(
                    "{base}.json is a record lexicon, which has four triggers; name this file \
                     {base}.index.{extension} to handle every action, or {base}.create.{extension}, \
                     {base}.update.{extension} or {base}.delete.{extension} for one"
                ),
            ));
        }
        ref other => {
            return Err(Diagnostic::error(
                file.rel_path(),
                format!(
                    "{base}.json is a {} lexicon, which runs no script; give this file another name to use it as a module",
                    type_name(other)
                ),
            ));
        }
    };
    let claim = require_claim(file, extension, claim, claims)?;
    Ok(Role::Handler {
        trigger: ParsedTrigger::new(kind, lexicon.id.clone()),
        claim,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn file(path: &str) -> SrcFile {
        SrcFile {
            segments: path.split('/').map(str::to_string).collect(),
            abs: PathBuf::new(),
        }
    }

    fn claims() -> ExtensionClaims {
        let mut claims = ExtensionClaims::default();
        claims.insert("lua", "happyview-lua", "lua");
        claims.insert("ts", "happyview-ts", "typescript");
        claims
    }

    /// `com.example.getThing` (query), `com.example.putThing` (procedure),
    /// `com.example.thing` (record), `com.example.defs` (definitions) and
    /// `com.example.room` (space), all in `src/lexicons/com/example/`.
    fn index() -> LexiconIndex {
        let mut index = LexiconIndex::default();
        for (stem, kind) in [
            ("getThing", LexiconType::Query),
            ("putThing", LexiconType::Procedure),
            ("thing", LexiconType::Record),
            ("defs", LexiconType::Definitions),
            ("room", LexiconType::Space),
        ] {
            let f = file(&format!("lexicons/com/example/{stem}.json"));
            index.insert(f.dir(), f.stem(), format!("com.example.{stem}"), kind);
        }
        index
    }

    fn role(path: &str) -> Result<Role, Diagnostic> {
        classify(&file(path), &claims(), &index())
    }

    fn trigger(path: &str) -> String {
        match role(path) {
            Ok(Role::Handler { trigger, .. }) => trigger.id(),
            other => panic!("{path}: expected a handler, got {other:?}"),
        }
    }

    fn error(path: &str) -> String {
        match role(path) {
            Err(d) => {
                assert_eq!(d.path, PathBuf::from(format!("src/{path}")));
                d.message
            }
            other => panic!("{path}: expected an error, got {other:?}"),
        }
    }

    #[test]
    fn a_bare_script_takes_its_kind_from_the_lexicon_beside_it() {
        assert_eq!(
            trigger("lexicons/com/example/getThing.lua"),
            "xrpc.query:com.example.getThing"
        );
        assert_eq!(
            trigger("lexicons/com/example/putThing.ts"),
            "xrpc.procedure:com.example.putThing"
        );
        let Ok(Role::Handler { claim, .. }) = role("lexicons/com/example/putThing.ts") else {
            unreachable!()
        };
        assert_eq!(
            claim,
            Claim {
                plugin_id: "happyview-ts".into(),
                language_id: "typescript".into()
            }
        );
    }

    #[test]
    fn a_record_lexicon_takes_one_script_per_action() {
        assert_eq!(
            trigger("lexicons/com/example/thing.index.lua"),
            "record.index:com.example.thing"
        );
        assert_eq!(
            trigger("lexicons/com/example/thing.create.lua"),
            "record.create:com.example.thing"
        );
        assert_eq!(
            trigger("lexicons/com/example/thing.update.ts"),
            "record.update:com.example.thing"
        );
        assert_eq!(
            trigger("lexicons/com/example/thing.delete.lua"),
            "record.delete:com.example.thing"
        );
    }

    /// A record has four triggers, and guessing one is worse than asking.
    #[test]
    fn a_bare_script_beside_a_record_lexicon_asks_for_an_action() {
        let message = error("lexicons/com/example/thing.lua");
        assert!(message.contains("thing.index.lua"), "{message}");
        assert!(message.contains("record"), "{message}");
    }

    #[test]
    fn an_action_needs_a_record_lexicon_beside_it() {
        let message = error("lexicons/com/example/getThing.index.lua");
        assert!(message.contains("getThing.json is a query"), "{message}");
        let message = error("lexicons/com/example/missing.create.lua");
        assert!(message.contains("no missing.json beside"), "{message}");
    }

    #[test]
    fn definitions_and_space_lexicons_run_no_script() {
        assert!(error("lexicons/com/example/defs.lua").contains("definitions"));
        assert!(error("lexicons/com/example/room.lua").contains("space"));
    }

    #[test]
    fn json_under_lexicons_is_a_lexicon() {
        assert!(matches!(
            role("lexicons/com/example/getThing.json"),
            Ok(Role::Lexicon)
        ));
        assert!(is_lexicon(&file("lexicons/com/example/getThing.json")));
        assert!(!is_lexicon(&file("lib/data.json")));
        assert!(matches!(role("lib/data.json"), Ok(Role::Ignored)));
    }

    #[test]
    fn a_handler_in_a_language_nobody_installed_is_an_error_naming_what_is() {
        let message = error("lexicons/com/example/getThing.py");
        assert!(message.contains(".py"), "{message}");
        assert!(message.contains(".lua (happyview-lua)"), "{message}");
        assert!(message.contains(".ts (happyview-ts)"), "{message}");
    }

    /// READMEs and notes can live beside the code they describe.
    #[test]
    fn documentation_in_a_handler_position_is_ignored() {
        for path in [
            "lexicons/com/example/getThing.md",
            "lexicons/com/example/thing.index.txt",
            "lexicons/com/example/README.md",
            "lexicons/notes",
        ] {
            assert!(matches!(role(path), Ok(Role::Ignored)), "{path}");
        }
    }

    #[test]
    fn other_files_with_an_interpreter_extension_are_modules() {
        for path in [
            "lib/format.lua",
            "lexicons/com/example/helpers.lua",
            "util.ts",
        ] {
            assert!(matches!(role(path), Ok(Role::Module { .. })), "{path}");
        }
        assert!(matches!(role("lib/format.rb"), Ok(Role::Ignored)));
    }

    #[test]
    fn tests_win_over_every_other_rule() {
        for path in [
            "lexicons/com/example/getThing.test.lua",
            "lexicons/com/example/thing.index.test.lua",
            "jobs/instance.operation.test.lua",
            "labels/app.bsky.feed.post.test.ts",
            "lib/format.test.lua",
        ] {
            assert!(
                matches!(role(path), Ok(Role::Test { claim: Some(_) })),
                "{path}"
            );
        }
        assert!(matches!(
            role("lib/format.test.py"),
            Ok(Role::Test { claim: None })
        ));
        assert!(is_test_name("a.test.lua"));
        assert!(!is_test_name("test.lua"));
        assert!(!is_test_name(".test.lua"));
    }

    /// `test.lua` beside `test.json` has no `.test.` segment, so it is a
    /// handler like any other.
    #[test]
    fn a_lexicon_named_test_takes_a_bare_handler() {
        let mut index = LexiconIndex::default();
        let f = file("lexicons/com/example/test.json");
        index.insert(
            f.dir(),
            f.stem(),
            "com.example.test".into(),
            LexiconType::Query,
        );
        let role = classify(&file("lexicons/com/example/test.lua"), &claims(), &index).unwrap();
        assert!(
            matches!(role, Role::Handler { ref trigger, .. } if trigger.id() == "xrpc.query:com.example.test"),
            "{role:?}"
        );
    }

    #[test]
    fn a_job_file_is_named_by_its_type() {
        assert_eq!(
            trigger("jobs/instance.operation.lua"),
            "job.run:instance.operation"
        );
        assert_eq!(trigger("jobs/test.lua"), "job.run:test");
    }

    #[test]
    fn a_label_file_is_named_by_its_collection_or_actor() {
        assert_eq!(
            trigger("labels/app.bsky.feed.post.lua"),
            "labeler.apply:app.bsky.feed.post"
        );
        assert_eq!(trigger("labels/_actor.ts"), "labeler.apply:_actor");
    }

    #[test]
    fn a_name_the_grammar_refuses_is_an_error() {
        assert!(error("jobs/Nightly.lua").contains("must match"));
        assert!(error("jobs/happyview.purge.lua").contains("reserved"));
        assert!(error("labels/not-an-nsid.lua").contains("not-an-nsid"));
    }

    #[test]
    fn jobs_and_labels_are_flat() {
        let message = error("jobs/nightly/run.lua");
        assert!(message.contains("src/jobs/ is flat"), "{message}");
        let message = error("labels/app/bsky.lua");
        assert!(message.contains("src/labels/ is flat"), "{message}");
    }

    #[test]
    fn jobs_and_labels_hold_handlers_and_documentation_only() {
        assert!(matches!(role("jobs/README.md"), Ok(Role::Ignored)));
        assert!(error("jobs/nightly.py").contains(".py"));
    }

    #[tokio::test]
    async fn claims_come_from_the_installed_interpreters() {
        let registry = crate::plugin::PluginRegistry::new();
        for (id, language, extensions) in [
            ("lua", "lua", vec!["lua"]),
            ("ts", "typescript", vec!["ts", "mts"]),
        ] {
            let manifest: crate::plugin::PluginManifest =
                serde_json::from_value(serde_json::json!({
                    "id": id, "name": id, "version": "1.0.0", "api_version": "2",
                    "plugin_type": "interpreter", "language_id": language,
                    "extensions": extensions, "capabilities": [],
                }))
                .unwrap();
            registry
                .install(crate::plugin::LoadedPlugin {
                    info: manifest.clone().into(),
                    source: crate::plugin::PluginSource::File { path: id.into() },
                    wasm_bytes: vec![],
                    manifest: Some(manifest),
                })
                .await
                .unwrap();
        }
        let claims = ExtensionClaims::from_registry(&registry).await;
        assert_eq!(claims.get("mts").unwrap().language_id, "typescript");
        assert_eq!(claims.get("lua").unwrap().plugin_id, "lua");
        assert!(claims.get("py").is_none());
        assert_eq!(
            claims.describe(),
            "installed: .lua (lua), .mts (ts), .ts (ts)"
        );
    }
}

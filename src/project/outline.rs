//! What each file in a project is, for an editor: `happyview check --json`.
//!
//! The outline is drawn apart from the build, from the same walk, lexicon
//! loader and `layout::classify`, so a project that does not build still has
//! one. A failing build is when an author is editing, which is when an editor
//! most needs to know which file handles which lexicon.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use crate::lexicon::LexiconType;
use crate::lua::TriggerKind;
use crate::plugin::{PluginRegistry, PluginType};

use super::diagnostic::{Diagnostic, Severity};
use super::layout::{self, ExtensionClaims, LexiconIndex, Role};
use super::lexicons;
use super::walk;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Outline {
    pub files: Vec<FileEntry>,
    pub lexicons: Vec<LexiconEntry>,
    pub languages: Vec<LanguageEntry>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lexicon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LexiconEntry {
    pub nsid: String,
    pub path: String,
    pub kind: &'static str,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LanguageEntry {
    pub language_id: String,
    pub extensions: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct JsonDiagnostic {
    pub path: String,
    pub line: Option<u32>,
    pub severity: &'static str,
    pub message: String,
}

impl From<&Diagnostic> for JsonDiagnostic {
    fn from(d: &Diagnostic) -> Self {
        Self {
            path: d.path.to_string_lossy().into_owned(),
            line: d.line,
            severity: match d.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
            },
            message: d.message.clone(),
        }
    }
}

fn kind_name(kind: &LexiconType) -> &'static str {
    match kind {
        LexiconType::Record => "record",
        LexiconType::Query => "query",
        LexiconType::Procedure => "procedure",
        LexiconType::Space => "space",
        LexiconType::Definitions => "definitions",
    }
}

/// A trigger that runs for a lexicon, as opposed to a job type or a label.
fn names_a_lexicon(kind: TriggerKind) -> bool {
    !matches!(kind, TriggerKind::LabelerApply | TriggerKind::JobRun)
}

pub async fn outline(registry: &PluginRegistry, root: &Path) -> Outline {
    let claims = ExtensionClaims::from_registry(registry).await;
    let (files, _) = walk::walk_src(root);

    // Overrides change nothing an outline reports, and a `happyview.toml`
    // that does not parse is the build's diagnostic to give.
    let no_overrides = BTreeMap::new();
    let mut index = LexiconIndex::default();
    let mut lexicon_entries = Vec::new();
    let mut project_ids = BTreeSet::new();
    for file in files.iter().filter(|f| layout::is_lexicon(f)) {
        if let Ok(lexicon) = lexicons::load_lexicon(file, &no_overrides) {
            let parsed = &lexicon.parsed;
            index.insert(
                file.dir(),
                file.stem(),
                parsed.id.clone(),
                parsed.lexicon_type.clone(),
            );
            project_ids.insert(parsed.id.clone());
            lexicon_entries.push(LexiconEntry {
                nsid: parsed.id.clone(),
                path: file.id(),
                kind: kind_name(&parsed.lexicon_type),
            });
        }
    }

    let mut entries = Vec::new();
    for file in &files {
        let entry = |role, trigger, lexicon, language| FileEntry {
            path: file.id(),
            role,
            trigger,
            lexicon,
            language,
        };
        if layout::is_lexicon(file) {
            entries.push(entry("lexicon", None, None, None));
            continue;
        }
        match layout::classify(file, &claims, &index) {
            Ok(Role::Handler { trigger, claim }) => {
                let lexicon = (names_a_lexicon(trigger.kind)
                    && project_ids.contains(&trigger.suffix))
                .then(|| trigger.suffix.clone());
                entries.push(entry(
                    "handler",
                    Some(trigger.id()),
                    lexicon,
                    Some(claim.language_id),
                ));
            }
            Ok(Role::Module { claim }) => {
                entries.push(entry("module", None, None, Some(claim.language_id)))
            }
            Ok(Role::Test { claim }) => {
                entries.push(entry("test", None, None, claim.map(|c| c.language_id)))
            }
            Ok(Role::Lexicon | Role::Ignored) | Err(_) => {}
        }
    }

    let mut interpreters = registry.list_by_type(PluginType::Interpreter).await;
    interpreters.sort_by(|a, b| a.info.id.cmp(&b.info.id));
    let languages = interpreters
        .iter()
        .filter_map(|plugin| {
            Some(LanguageEntry {
                language_id: plugin.language_id()?.to_string(),
                extensions: plugin
                    .extensions()
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            })
        })
        .collect();

    Outline {
        files: entries,
        lexicons: lexicon_entries,
        languages,
    }
}

//! Nothing here reads files. A snapshot is built elsewhere and installed into
//! `AppState.project`; until one is, the layer is empty and every catalog read
//! falls through to the database layers.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::lexicon::ParsedLexicon;

/// The project's lexicons, scripts and modules, validated and keyed for
/// lookup. Immutable once built: a change is a new snapshot, stored whole.
#[derive(Debug, Clone, Default)]
pub struct ProjectSnapshot {
    /// The project root, or `None` when no project is loaded.
    pub root: Option<PathBuf>,
    /// Bumped on every install, so a run can say which build it pinned.
    pub generation: u64,
    /// By NSID.
    pub lexicons: BTreeMap<String, ProjectLexicon>,
    /// By trigger id.
    pub scripts: BTreeMap<String, ProjectScript>,
    /// By module id: the project-relative path with its extension
    /// (`src/lib/format.lua`), the same string a chunk is named by.
    pub modules: BTreeMap<String, ProjectModule>,
    /// By project-relative path (`src/...`). Filled only by a build that
    /// includes tests.
    pub tests: BTreeMap<String, ProjectScript>,
}

impl ProjectSnapshot {
    pub fn empty() -> Self {
        Self::default()
    }
}

#[derive(Debug, Clone)]
pub struct ProjectLexicon {
    pub parsed: ParsedLexicon,
    pub path: PathBuf,
    /// From `happyview.toml`: a lexicon document is published schema and has
    /// nowhere to carry it.
    pub backfill: bool,
}

#[derive(Debug, Clone)]
pub struct ProjectScript {
    pub trigger: String,
    pub path: PathBuf,
    pub language_id: String,
    pub body: ScriptBody,
    pub description: Option<String>,
    pub outbound_xrpcs: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum ScriptBody {
    Source {
        source: Arc<str>,
        /// What the file's interpreter prepared from `source`, stamped with
        /// that interpreter's build. `None` until the build prepares it, and
        /// for an interpreter that cannot.
        artifact: Option<crate::plugin::PreparedArtifact>,
    },
    /// A handler of a compiled module, which has no source to prepare.
    Module {
        /// The `[modules.<name>]` key.
        module: String,
        /// Hex sha256 of the build that was loaded.
        hash: String,
    },
}

#[derive(Debug, Clone)]
pub struct ProjectModule {
    pub id: String,
    pub path: PathBuf,
    pub language_id: String,
    pub source: Arc<str>,
    pub artifact: Option<crate::plugin::PreparedArtifact>,
}

/// How `AppState` and the catalogs hold the current snapshot. A reader that
/// must not see a swap mid-operation takes `load_full()` once and keeps it.
pub type SharedSnapshot = Arc<arc_swap::ArcSwap<ProjectSnapshot>>;

pub fn shared_empty() -> SharedSnapshot {
    Arc::new(arc_swap::ArcSwap::from_pointee(ProjectSnapshot::empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_snapshot_holds_nothing_and_names_no_root() {
        let snap = ProjectSnapshot::empty();
        assert!(snap.root.is_none());
        assert_eq!(snap.generation, 0);
        assert!(snap.lexicons.is_empty());
        assert!(snap.scripts.is_empty());
        assert!(snap.modules.is_empty());
        assert!(snap.tests.is_empty());
    }

    /// What lets a run pin a build: an `Arc` taken before a swap keeps
    /// describing the snapshot it was taken from.
    #[test]
    fn a_loaded_snapshot_survives_a_swap() {
        let shared = shared_empty();
        let pinned = shared.load_full();
        let mut next = ProjectSnapshot::empty();
        next.generation = 1;
        shared.store(Arc::new(next));

        assert_eq!(pinned.generation, 0);
        assert_eq!(shared.load().generation, 1);
    }
}

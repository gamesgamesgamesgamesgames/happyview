//! Hand-built project snapshots, for tests that need the project layer without
//! a project on disk.

use std::path::PathBuf;
use std::sync::Arc;

use happyview::lexicon::{ParsedLexicon, ProcedureAction};
use happyview::project::{ProjectLexicon, ProjectScript, ProjectSnapshot, ScriptBody};
use serde_json::Value;

pub fn snapshot() -> ProjectSnapshot {
    let mut snap = ProjectSnapshot::empty();
    snap.generation = 1;
    snap
}

pub fn add_lexicon(snap: &mut ProjectSnapshot, json: Value, path: &str) {
    let parsed = ParsedLexicon::parse(json, 0, None, ProcedureAction::Upsert, None)
        .expect("a test lexicon parses");
    snap.lexicons.insert(
        parsed.id.clone(),
        ProjectLexicon {
            parsed,
            path: PathBuf::from(path),
            backfill: false,
            indexed: true,
        },
    );
}

/// A `lua` script, the language the echo fixture claims.
pub fn add_script(snap: &mut ProjectSnapshot, trigger: &str, body: &str, path: &str) {
    snap.scripts.insert(
        trigger.to_string(),
        ProjectScript {
            trigger: trigger.to_string(),
            path: PathBuf::from(path),
            language_id: "lua".to_string(),
            body: ScriptBody::Source {
                source: Arc::from(body),
                artifact: None,
            },
            description: None,
            outbound_xrpcs: Vec::new(),
        },
    );
}

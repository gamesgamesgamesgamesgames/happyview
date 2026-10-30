//! Locating the built Lua interpreter plugin, shared by every target that
//! loads it.
//!
//! Here rather than in one target because the check below is only worth
//! having if nothing can route around it: a stale artefact loads, runs and
//! answers, so a target without the check reports green against a module
//! nobody meant to test — which is how three recorded figures came to
//! describe a build the source no longer produced.

use std::path::{Path, PathBuf};

/// A directory holding the plugin's `manifest.json` beside the `.wasm` file
/// the manifest names — the two files its release publishes.
pub const PLUGIN_DIR: &str = "HAPPYVIEW_LUA_PLUGIN";

/// The plugin crate's `src/`, when the runner names it. Optional, and
/// enforced when present.
pub const PLUGIN_SRC: &str = "HAPPYVIEW_LUA_SRC";

pub fn plugin_dir() -> Option<PathBuf> {
    let Some(dir) = std::env::var_os(PLUGIN_DIR) else {
        eprintln!(
            "skipping: {PLUGIN_DIR} is unset, so the built Lua plugin is not available. \
             See this file's header for how to assemble the directory it names."
        );
        return None;
    };
    let dir = PathBuf::from(dir);
    refuse_a_stale_artefact(&dir);
    Some(dir)
}

/// The module, with enough of its identity to tell one build from another in
/// a recorded run. A size and a hash beside a figure is what makes the figure
/// checkable afterwards.
pub fn identify(dir: &Path) -> String {
    let path = artefact(dir);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let digest = <sha2::Sha256 as sha2::Digest>::digest(&bytes);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("{} ({} bytes, sha256 {hex})", path.display(), bytes.len())
}

pub fn artefact(dir: &Path) -> PathBuf {
    let manifest = dir.join("manifest.json");
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&manifest)
            .unwrap_or_else(|e| panic!("{}: {e}", manifest.display())),
    )
    .expect("the plugin's manifest should parse");
    dir.join(
        manifest["wasm_file"]
            .as_str()
            .expect("the manifest names its wasm file"),
    )
}

/// Fails when the artefact predates the source that should have built it.
///
/// A stale artefact does not fail anything by itself — it loads, it runs, and
/// it answers — so a run against one reports a number that describes neither
/// the code nor a comparison anybody meant to make. That is how the
/// pre-`loslib` module came to carry three recorded figures. Pointing
/// `HAPPYVIEW_LUA_SRC` at the plugin crate's `src/` turns it into a failure
/// instead; leaving it unset says so rather than pretending.
pub fn refuse_a_stale_artefact(dir: &Path) {
    let Some(src) = std::env::var_os(PLUGIN_SRC) else {
        eprintln!(
            "note: {PLUGIN_SRC} is unset, so nothing here checks that {} was built \
             from the current source.",
            dir.display()
        );
        return;
    };
    let src = PathBuf::from(src);
    let built = std::fs::metadata(artefact(dir))
        .and_then(|m| m.modified())
        .expect("the artefact's timestamp");

    let mut newer: Vec<PathBuf> = Vec::new();
    let mut pending = vec![src.clone()];
    while let Some(path) = pending.pop() {
        let entries =
            std::fs::read_dir(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        for entry in entries {
            let path = entry.expect("a readable directory entry").path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let touched = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .expect("a source file's timestamp");
            if touched > built {
                newer.push(path);
            }
        }
    }
    assert!(
        newer.is_empty(),
        "the artefact under {} is older than {} file(s) in {}, so it is not what this \
         source builds; rebuild it and reassemble the directory. First: {}",
        dir.display(),
        newer.len(),
        src.display(),
        newer[0].display()
    );
}

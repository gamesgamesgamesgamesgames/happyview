//! Locating the built JavaScript and TypeScript interpreter plugins. Each
//! variable names a directory holding the plugin's `manifest.json` beside the
//! `.wasm` file the manifest names — the two files its release publishes.

use std::path::PathBuf;

pub const JAVASCRIPT_DIR: &str = "HAPPYVIEW_JAVASCRIPT_PLUGIN";
pub const TYPESCRIPT_DIR: &str = "HAPPYVIEW_TYPESCRIPT_PLUGIN";

/// The directory `var` names, or `None` with a message saying so. A skip is
/// not a pass: a run that means to cover these has to set the variable.
pub fn plugin_dir(var: &str) -> Option<PathBuf> {
    match std::env::var_os(var) {
        Some(dir) => Some(PathBuf::from(dir)),
        None => {
            eprintln!(
                "skipping: {var} is unset, so that built interpreter plugin is not available"
            );
            None
        }
    }
}

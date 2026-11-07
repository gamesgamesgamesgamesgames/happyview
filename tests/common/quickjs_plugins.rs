//! Locating the built JavaScript and TypeScript interpreter plugins. Each
//! variable names a directory holding the plugin's `manifest.json` beside the
//! `.wasm` file the manifest names — the two files its release publishes.

use std::path::PathBuf;

pub const JAVASCRIPT_DIR: &str = "HAPPYVIEW_JAVASCRIPT_PLUGIN";
pub const TYPESCRIPT_DIR: &str = "HAPPYVIEW_TYPESCRIPT_PLUGIN";

/// The directory `var` names. Unset, the test fails, or skips with
/// `HAPPYVIEW_ALLOW_SKIPS=1` (`happyview::test_support::unavailable`) and this
/// is `None`.
pub fn plugin_dir(var: &str) -> Option<PathBuf> {
    match std::env::var_os(var) {
        Some(dir) => Some(PathBuf::from(dir)),
        None => {
            happyview::test_support::unavailable(&format!(
                "{var} is unset, so that built interpreter plugin is not available. \
                 Run scripts/fetch-quickjs-plugins.sh and point it at the plugin's directory"
            ));
            None
        }
    }
}

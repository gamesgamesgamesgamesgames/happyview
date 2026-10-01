//! The `interpreter_echo` fixture installed as the interpreter a script row's
//! language names, shared by every target whose subject sits on the far side of
//! a script run.
//!
//! The fixture interprets nothing — its `source` is a directive, and anything
//! it does not recognise echoes the whole `execute` input back — so a target
//! that only needs a run to happen can seed any body at all, and one that needs
//! a library call reached through a request seeds `host:require`.
//!
//! Here rather than in one target because a script row's `script_type` is the
//! only thing that picks an interpreter, and three targets need the same
//! language claimed under the same id.

use happyview::plugin::{LoadedPlugin, PluginManifest, PluginSource};
use serde_json::json;

const MODULE: &str =
    "tests/fixtures/interpreter_echo/target/wasm32-unknown-unknown/release/interpreter_echo.wasm";

pub const BUILD: &str = "interpreter_echo fixture not built. Run: cargo build --manifest-path \
                         tests/fixtures/interpreter_echo/Cargo.toml --target \
                         wasm32-unknown-unknown --release";

pub fn is_built() -> bool {
    std::path::Path::new(MODULE).exists()
}

/// The fixture, claiming `language`.
pub fn plugin(language: &str) -> LoadedPlugin {
    let manifest: PluginManifest = serde_json::from_value(json!({
        "id": "interpreter_echo",
        "name": "interpreter_echo",
        "version": "1.0.0",
        "api_version": "2",
        "plugin_type": "interpreter",
        "language_id": language,
        "capabilities": ["library:call", "script:host"],
    }))
    .expect("the fixture manifest should parse");
    LoadedPlugin {
        info: manifest.clone().into(),
        source: PluginSource::File {
            path: "tests/fixtures/interpreter_echo".into(),
        },
        wasm_bytes: std::fs::read(MODULE).expect(BUILD),
        manifest: Some(manifest),
    }
}

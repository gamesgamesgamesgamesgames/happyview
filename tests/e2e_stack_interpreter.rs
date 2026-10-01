//! The manifest `docker-compose.e2e.yml` mounts over the `interpreter_echo`
//! fixture's own, claiming `lua` so the browser stack has an interpreter to
//! answer `validate`.
//!
//! It is a hand-maintained copy whose only other reference is that bind, so
//! nothing else here would open it. A capability the module imports but the
//! copy does not declare is refused by the loader, which would bring the
//! stack up with no interpreter at all and fail three specs for a reason
//! pointing nowhere near the manifest. This is where that fails instead.

mod common;

use std::path::{Path, PathBuf};

use common::echo_interpreter;

const STAND_IN: &str = "scripts/e2e-lua-stand-in-manifest.json";

/// The directory the compose bind mounts, and the root the manifest's
/// `wasm_file` is relative to.
const FIXTURE_DIR: &str = "tests/fixtures/interpreter_echo";

/// A directory shaped like the two binds: the stand-in manifest where the
/// fixture's own sits, and the module at the path that manifest names.
fn assembled_mount() -> PathBuf {
    let manifest = std::fs::read_to_string(STAND_IN).expect(STAND_IN);
    let wasm_file = serde_json::from_str::<serde_json::Value>(&manifest)
        .expect("the stand-in manifest should parse")["wasm_file"]
        .as_str()
        .expect("the stand-in manifest names its wasm file")
        .to_string();

    let dir = std::env::temp_dir().join(format!("hv-standin-{}", uuid::Uuid::new_v4()));
    let module = dir.join(&wasm_file);
    std::fs::create_dir_all(module.parent().expect("the module sits in a directory"))
        .expect("create the assembled mount");
    std::fs::write(dir.join("manifest.json"), &manifest).expect("write the stand-in manifest");
    std::fs::copy(Path::new(FIXTURE_DIR).join(&wasm_file), &module)
        .unwrap_or_else(|e| panic!("{}: {e}", module.display()));
    dir
}

/// Loading is where a manifest and its module are checked against each
/// other, and `language_id` is the one property the stack depends on: the
/// fixture's own manifest would answer `echo`, and a save under `lua` would
/// find nothing.
#[tokio::test]
async fn the_stacks_stand_in_manifest_loads_the_fixture_as_the_lua_interpreter() {
    if !echo_interpreter::is_built() {
        eprintln!("skipping: {}", echo_interpreter::BUILD);
        return;
    }
    let dir = assembled_mount();

    let loaded = happyview::plugin::loader::load_from_file(&dir).await;
    let _ = std::fs::remove_dir_all(&dir);
    let plugin = loaded.unwrap_or_else(|e| panic!("{STAND_IN} does not load the fixture: {e}"));

    assert_eq!(plugin.language_id(), Some("lua"));
    assert_eq!(
        plugin.manifest.as_ref().expect("a manifest").plugin_type,
        happyview::plugin::PluginType::Interpreter
    );
}

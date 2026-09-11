//! Checks the built module, not the source: that `library_plugin!`'s
//! `#[no_mangle]` items survive into the cdylib as wasm exports, and that no
//! unused host import comes along with the SDK. Both are only observable in
//! the artefact, so this reads the SDK-based `http` fixture at
//! `tests/fixtures/sdk_http`. A test cannot drive the wasm build itself, so it
//! skips when the fixture is absent; CI builds the fixture first.

use std::collections::BTreeSet;
use std::path::PathBuf;

use wasmparser::{ExternalKind, Parser, Payload};

/// Exactly what the host can call or address on a library plugin: `memory`
/// comes from the cdylib itself, the five functions from `library_plugin!`.
///
/// wasm-ld also exports `__data_end` and `__heap_base` as *globals* on every
/// cdylib, ours and the hand-rolled plugins alike. Those are layout constants,
/// not an entry point, so the assertion below covers functions and memories
/// exactly and separately requires that everything else be a global.
const EXPECTED_EXPORTS: &[&str] = &[
    "alloc",
    "call",
    "dealloc",
    "get_api_surface",
    "memory",
    "plugin_info",
];

/// The `http` fixture uses one host function. The SDK declares eleven, so this
/// also shows the unused ten are dropped at link time; a plugin that imported
/// them all would need capabilities it never declared, and the loader would
/// refuse it.
const EXPECTED_IMPORTS: &[&str] = &["env::host_http_request"];

fn wasm_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/sdk_http/target/wasm32-unknown-unknown/release/sdk_http.wasm")
}

#[test]
fn the_http_fixture_exports_the_library_abi_and_imports_only_what_it_uses() {
    let path = wasm_path();
    let Ok(bytes) = std::fs::read(&path) else {
        eprintln!(
            "skipping: {} not built. Run `cargo build --manifest-path tests/fixtures/sdk_http/Cargo.toml --target wasm32-unknown-unknown --release` first.",
            path.display()
        );
        return;
    };

    let mut exports = BTreeSet::new();
    let mut imports = BTreeSet::new();
    for payload in Parser::new(0).parse_all(&bytes) {
        match payload.expect("sdk_http.wasm should be a valid module") {
            Payload::ExportSection(reader) => {
                for export in reader {
                    let export = export.expect("valid export");
                    match export.kind {
                        ExternalKind::Func | ExternalKind::Memory => {
                            exports.insert(export.name.to_string());
                        }
                        ExternalKind::Global => {}
                        kind => panic!("unexpected export kind {kind:?} for {}", export.name),
                    }
                }
            }
            Payload::ImportSection(reader) => {
                for import in reader {
                    let import = import.expect("valid import");
                    imports.insert(format!("{}::{}", import.module, import.name));
                }
            }
            _ => {}
        }
    }

    let expected_exports: BTreeSet<String> =
        EXPECTED_EXPORTS.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        exports, expected_exports,
        "export set drifted; the host resolves these by name"
    );

    let expected_imports: BTreeSet<String> =
        EXPECTED_IMPORTS.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        imports, expected_imports,
        "import set drifted; an unused SDK host import leaked into the module"
    );
}

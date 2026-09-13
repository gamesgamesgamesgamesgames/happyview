//! Checks the built modules, not the source: that the `#[no_mangle]` items
//! `library_plugin!` and `auth_plugin!` emit survive into a cdylib as wasm
//! exports, and that no unused host import comes along with the SDK. Both are
//! only observable in the artefact, so this reads the SDK-based fixtures at
//! `tests/fixtures/sdk_http` and `tests/fixtures/sdk_auth`. A test cannot drive
//! the wasm build itself, so each skips when its fixture is absent; CI builds
//! them first.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use wasmparser::{ExternalKind, Parser, Payload};

/// Exactly what the host can call or address on a library plugin: `memory`
/// comes from the cdylib itself, the five functions from `library_plugin!`.
///
/// wasm-ld also exports `__data_end` and `__heap_base` as *globals* on every
/// cdylib, ours and the hand-rolled plugins alike. Those are layout constants,
/// not an entry point, so the assertion below covers functions and memories
/// exactly and separately requires that everything else be a global.
const EXPECTED_LIBRARY_EXPORTS: &[&str] = &[
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
const EXPECTED_LIBRARY_IMPORTS: &[&str] = &["env::host_http_request"];

/// The host resolves an auth plugin by these four names plus `plugin_info`;
/// `auth_plugin!` emits every one of them.
const EXPECTED_AUTH_EXPORTS: &[&str] = &[
    "alloc",
    "dealloc",
    "get_authorize_url",
    "get_profile",
    "handle_callback",
    "memory",
    "plugin_info",
    "refresh_tokens",
];

/// The `auth` fixture reads a secret and makes one request, which is exactly
/// what its manifest declares `secrets:read` and `network:request:unrestricted`
/// for.
const EXPECTED_AUTH_IMPORTS: &[&str] = &["env::host_get_secret", "env::host_http_request"];

/// The `objects` fixture uses the object-model constructor/method plumbing
/// (no host import of its own) plus every record/table query import.
const EXPECTED_OBJECTS_EXPORTS: &[&str] = &[
    "alloc",
    "call",
    "dealloc",
    "get_api_surface",
    "memory",
    "plugin_info",
];

const EXPECTED_OBJECTS_IMPORTS: &[&str] = &[
    "env::host_backlinks_query",
    "env::host_records_count",
    "env::host_records_get",
    "env::host_records_query",
    "env::host_records_search",
    "env::host_table_query",
];

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../tests/fixtures/{name}/target/wasm32-unknown-unknown/release/{name}.wasm"
    ))
}

/// The function/memory exports and the host imports of a built module, or
/// `None` when it has not been built.
fn module_interface(path: &Path) -> Option<(BTreeSet<String>, BTreeSet<String>)> {
    let Ok(bytes) = std::fs::read(path) else {
        return None;
    };

    let mut exports = BTreeSet::new();
    let mut imports = BTreeSet::new();
    for payload in Parser::new(0).parse_all(&bytes) {
        match payload.expect("fixture should be a valid wasm module") {
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
    Some((exports, imports))
}

fn check_fixture(name: &str, expected_exports: &[&str], expected_imports: &[&str]) {
    let path = fixture_path(name);
    let Some((exports, imports)) = module_interface(&path) else {
        eprintln!(
            "skipping: {} not built. Run `cargo build --manifest-path tests/fixtures/{name}/Cargo.toml --target wasm32-unknown-unknown --release` first.",
            path.display()
        );
        return;
    };

    let expected: BTreeSet<String> = expected_exports.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        exports, expected,
        "{name} export set drifted; the host resolves these by name"
    );

    let expected: BTreeSet<String> = expected_imports.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        imports, expected,
        "{name} import set drifted; an unused SDK host import leaked into the module"
    );
}

#[test]
fn the_http_fixture_exports_the_library_abi_and_imports_only_what_it_uses() {
    check_fixture(
        "sdk_http",
        EXPECTED_LIBRARY_EXPORTS,
        EXPECTED_LIBRARY_IMPORTS,
    );
}

#[test]
fn the_auth_fixture_exports_the_auth_abi_and_imports_only_what_it_uses() {
    check_fixture("sdk_auth", EXPECTED_AUTH_EXPORTS, EXPECTED_AUTH_IMPORTS);
}

#[test]
fn the_objects_fixture_exports_the_library_abi_and_imports_only_what_it_uses() {
    check_fixture(
        "sdk_objects",
        EXPECTED_OBJECTS_EXPORTS,
        EXPECTED_OBJECTS_IMPORTS,
    );
}

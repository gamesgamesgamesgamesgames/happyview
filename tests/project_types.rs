//! `happyview types` through the real binary, with no database anywhere.

mod common;

use std::path::Path;
use std::process::Command;

use common::echo_interpreter;
use serde_json::Value;

const ECHO: &str = "tests/fixtures/interpreter_echo";
const HTTP: &str = "tests/fixtures/sdk_http";
const HTTP_WASM: &str =
    "tests/fixtures/sdk_http/target/wasm32-unknown-unknown/release/sdk_http.wasm";

fn types(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_happyview"))
        .arg("types")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env_remove("DATABASE_URL")
        .env_remove("HAPPYVIEW_PROJECT")
        .output()
        .expect("run happyview types");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The fixtures this file needs, or why it is skipping.
fn built() -> bool {
    if !echo_interpreter::is_built() {
        happyview::test_support::unavailable(echo_interpreter::BUILD);
        return false;
    }
    if !Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(HTTP_WASM)
        .exists()
    {
        happyview::test_support::unavailable(
            "sdk_http fixture not built. Run: cargo build --manifest-path \
             tests/fixtures/sdk_http/Cargo.toml --target wasm32-unknown-unknown --release",
        );
        return false;
    }
    true
}

fn run_basic(out: &Path) -> (i32, String, String) {
    types(&[
        "tests/fixtures/projects/basic",
        "--plugin",
        ECHO,
        "--plugin",
        HTTP,
        "--out",
        out.to_str().unwrap(),
    ])
}

#[test]
fn writes_each_interpreters_files_and_a_manifest() {
    if !built() {
        return;
    }
    let out = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = run_basic(out.path());
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.lines().any(|l| l == "types: echo: wrote 1 file"),
        "{stdout}"
    );

    let input: Value =
        serde_json::from_slice(&std::fs::read(out.path().join("echo/input.json")).unwrap())
            .unwrap();
    let lexicons = input["lexicons"].as_array().unwrap();
    let get_thing = lexicons
        .iter()
        .find(|l| l["nsid"] == "com.example.project.getThing")
        .expect("getThing");
    assert_eq!(get_thing["kind"], "query");
    assert_eq!(get_thing["main"]["output"]["kind"], "map");
    assert_eq!(input["libraries"][0]["namespace"], "http");
    assert_eq!(
        input["libraries"][0]["surface"]["exports"][0]["name"],
        "get"
    );
    let procedure: Vec<&str> = input["ctx"]["procedure"]["properties"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert!(procedure.contains(&"delegate_did"), "{procedure:?}");
    assert_eq!(
        input["events"]["record"]["properties"][5]["type"]["members"][0]["kind"],
        "var"
    );

    let manifest: Value =
        serde_json::from_slice(&std::fs::read(out.path().join("echo/manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["interpreter"]["id"], "interpreter_echo");
    assert_eq!(manifest["libraries"][0]["id"], "sdk_http");
    assert_eq!(manifest["input_sha256"].as_str().unwrap().len(), 64);
}

#[test]
fn a_second_run_with_nothing_changed_is_up_to_date() {
    if !built() {
        return;
    }
    let out = tempfile::tempdir().unwrap();
    let (code, stdout, _) = run_basic(out.path());
    assert_eq!(code, 0, "{stdout}");
    let written = out.path().join("echo/input.json");
    let before = std::fs::metadata(&written).unwrap().modified().unwrap();

    let (code, stdout, _) = run_basic(out.path());
    assert_eq!(code, 0, "{stdout}");
    assert!(
        stdout.lines().any(|l| l == "types: echo: up to date"),
        "{stdout}"
    );
    assert_eq!(
        std::fs::metadata(&written).unwrap().modified().unwrap(),
        before
    );
}

#[test]
fn stale_files_and_leftovers_are_removed() {
    if !built() {
        return;
    }
    let out = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(out.path().join("echo")).unwrap();
    std::fs::write(out.path().join("echo/old.lua"), "").unwrap();
    std::fs::create_dir_all(out.path().join(".echo.partial")).unwrap();
    std::fs::write(out.path().join(".echo.partial/x"), "").unwrap();
    std::fs::create_dir_all(out.path().join(".echo.old")).unwrap();

    let (code, stdout, _) = run_basic(out.path());
    assert_eq!(code, 0, "{stdout}");
    assert!(!out.path().join("echo/old.lua").exists());
    assert!(!out.path().join(".echo.partial").exists());
    assert!(!out.path().join(".echo.old").exists());
    assert!(out.path().join("echo/input.json").exists());

    // An interrupted run that left a partial behind beside an up-to-date
    // directory is cleaned up too.
    std::fs::create_dir_all(out.path().join(".echo.partial")).unwrap();
    let (code, stdout, _) = run_basic(out.path());
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("types: echo: up to date"), "{stdout}");
    assert!(!out.path().join(".echo.partial").exists());
}

#[test]
fn a_path_that_escapes_fails_that_interpreter() {
    if !built() {
        return;
    }
    let out = tempfile::tempdir().unwrap();
    let (code, stdout, _) = types(&[
        "tests/fixtures/projects/declarations-badpath",
        "--plugin",
        ECHO,
        "--out",
        out.path().to_str().unwrap(),
    ]);
    assert_eq!(code, 1, "{stdout}");
    assert!(
        stdout.lines().any(|l| l
            == "types: echo: failed: '../escape.json' is not a relative path inside the output"),
        "{stdout}"
    );
    assert!(!out.path().join("echo").exists());
    assert!(!out.path().parent().unwrap().join("escape.json").exists());
}

#[test]
fn an_interpreter_that_traps_fails_and_leaves_its_directory_alone() {
    if !built() {
        return;
    }
    let out = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(out.path().join("echo")).unwrap();
    std::fs::write(out.path().join("echo/kept.lua"), "").unwrap();
    let (code, stdout, _) = types(&[
        "tests/fixtures/projects/declarations-trap",
        "--plugin",
        ECHO,
        "--out",
        out.path().to_str().unwrap(),
    ]);
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("types: echo: failed: "), "{stdout}");
    assert!(out.path().join("echo/kept.lua").exists());
}

#[test]
fn a_project_that_does_not_build_writes_nothing() {
    if !built() {
        return;
    }
    let out = tempfile::tempdir().unwrap();
    let (code, stdout, _) = types(&[
        "tests/fixtures/projects/broken",
        "--plugin",
        ECHO,
        "--out",
        out.path().to_str().unwrap(),
    ]);
    assert_eq!(code, 1, "{stdout}");
    assert_eq!(stdout.lines().last(), Some("2 errors"), "{stdout}");
    assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
}

#[test]
fn the_default_output_is_inside_the_project() {
    if !built() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("happyview.toml"), "").unwrap();
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    let echo = Path::new(env!("CARGO_MANIFEST_DIR")).join(ECHO);
    let (code, stdout, _) = types(&[
        root.path().to_str().unwrap(),
        "--plugin",
        echo.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{stdout}");
    assert!(
        root.path()
            .join(".happyview/types/echo/input.json")
            .exists()
    );
}

#[test]
fn a_missing_root_is_a_usage_error() {
    let (code, _, stderr) = types(&[]);
    assert_eq!(code, 2);
    assert!(stderr.contains("happyview types <root>"), "{stderr}");
}

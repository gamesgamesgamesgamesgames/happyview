//! `happyview check` through the real binary, with no database anywhere.

mod common;

use std::process::Command;

use common::echo_interpreter;

fn check(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_happyview"))
        .arg("check")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env_remove("DATABASE_URL")
        .env_remove("HAPPYVIEW_PROJECT")
        .output()
        .expect("run happyview check");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const ECHO: &str = "tests/fixtures/interpreter_echo";

#[test]
fn a_project_that_builds_passes() {
    if !echo_interpreter::is_built() {
        eprintln!("skipping: {}", echo_interpreter::BUILD);
        return;
    }
    let (code, stdout, stderr) = check(&["tests/fixtures/projects/basic", "--plugin", ECHO]);
    assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout
            .lines()
            .last()
            .unwrap_or_default()
            .starts_with("ok: 4 lexicons, 6 scripts, 1 module"),
        "{stdout}"
    );
}

#[test]
fn a_project_that_does_not_build_prints_every_error_and_fails() {
    if !echo_interpreter::is_built() {
        eprintln!("skipping: {}", echo_interpreter::BUILD);
        return;
    }
    let (code, stdout, _) = check(&["tests/fixtures/projects/broken", "--plugin", ECHO]);
    assert_eq!(code, 1, "{stdout}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        lines.iter().any(|l| l.starts_with(
            "src/lexicons/com/example/broken/getThing.json: the lexicon's id is 'com.example.broken.getOther' but its path names 'com.example.broken.getThing'"
        )),
        "{stdout}"
    );
    assert!(
        lines.contains(&"src/lexicons/com/example/broken/listThings.echo:1: echo: the source says it is invalid"),
        "{stdout}"
    );
    assert_eq!(lines.last(), Some(&"2 errors"), "{stdout}");
}

#[test]
fn a_missing_root_is_a_usage_error() {
    let (code, _, stderr) = check(&[]);
    assert_eq!(code, 2);
    assert!(stderr.contains("happyview check <root>"), "{stderr}");
}

#[test]
fn a_plugin_that_will_not_load_stops_the_check() {
    let (code, stdout, _) = check(&[
        "tests/fixtures/projects/basic",
        "--plugin",
        "tests/fixtures/projects",
    ]);
    assert_eq!(code, 2, "{stdout}");
    assert!(stdout.starts_with("tests/fixtures/projects: "), "{stdout}");
}

/// A project's `.env` holds script variables, not configuration for a check.
/// It points `PLUGIN_DIR` at a plugin that cannot load, so a check that read
/// it would exit 2 rather than build the empty project.
#[test]
fn a_check_does_not_load_the_projects_env() {
    let root = tempfile::tempdir().unwrap();
    let bogus = root.path().join("bogus/broken");
    std::fs::create_dir_all(&bogus).unwrap();
    std::fs::write(bogus.join("manifest.json"), "{}").unwrap();
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(
        root.path().join(".env"),
        format!("PLUGIN_DIR={}\n", root.path().join("bogus").display()),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_happyview"))
        .args(["check", "."])
        .current_dir(root.path())
        .env_remove("PLUGIN_DIR")
        .env_remove("DATABASE_URL")
        .env_remove("HAPPYVIEW_PROJECT")
        .output()
        .expect("run happyview check");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert!(stdout.starts_with("ok: 0 lexicons"), "{stdout}");
}

fn check_json(args: &[&str]) -> (i32, serde_json::Value) {
    let mut all = args.to_vec();
    all.push("--json");
    let (code, stdout, stderr) = check(&all);
    let report = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("{e}: stdout:\n{stdout}\nstderr:\n{stderr}"));
    (code, report)
}

fn file<'a>(report: &'a serde_json::Value, path: &str) -> &'a serde_json::Value {
    report["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == path)
        .unwrap_or_else(|| panic!("no entry for {path}: {report:#}"))
}

#[test]
fn json_outlines_a_project_that_builds() {
    if !echo_interpreter::is_built() {
        eprintln!("skipping: {}", echo_interpreter::BUILD);
        return;
    }
    let (code, report) = check_json(&["tests/fixtures/projects/basic", "--plugin", ECHO]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["ok"], true);
    assert_eq!(report["diagnostics"], serde_json::json!([]));
    assert_eq!(
        file(&report, "src/lexicons/com/example/project/thing.index.echo"),
        &serde_json::json!({
            "path": "src/lexicons/com/example/project/thing.index.echo",
            "role": "handler",
            "trigger": "record.index:com.example.project.thing",
            "lexicon": "com.example.project.thing",
            "language": "echo",
        })
    );
    // A job is not a lexicon's handler, even when its type reads like one.
    let job = file(&report, "src/jobs/thing.reindex.echo");
    assert_eq!(job["trigger"], "job.run:thing.reindex");
    assert!(job.get("lexicon").is_none(), "{job}");
    assert_eq!(file(&report, "src/lib/format.echo")["role"], "module");
    assert_eq!(
        file(&report, "src/jobs/thing.reindex.test.echo")["role"],
        "test"
    );
    assert_eq!(
        file(&report, "src/lexicons/com/example/project/thing.json")["role"],
        "lexicon"
    );
    // Prose beside the lexicons is nobody's handler.
    assert!(
        !report["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"].as_str().unwrap().ends_with("README.md")),
        "{report:#}"
    );
    assert!(
        report["lexicons"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!({
                "nsid": "com.example.project.getThing",
                "path": "src/lexicons/com/example/project/getThing.json",
                "kind": "query",
            })),
        "{report:#}"
    );
    assert_eq!(
        report["languages"],
        serde_json::json!([{"language_id": "echo", "extensions": ["echo"]}])
    );
}

#[test]
fn json_still_outlines_a_project_that_does_not_build() {
    if !echo_interpreter::is_built() {
        eprintln!("skipping: {}", echo_interpreter::BUILD);
        return;
    }
    let (code, report) = check_json(&["tests/fixtures/projects/broken", "--plugin", ECHO]);
    assert_eq!(code, 1, "{report:#}");
    assert_eq!(report["ok"], false);
    assert!(
        report["diagnostics"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!({
                "path": "src/lexicons/com/example/broken/listThings.echo",
                "line": 1,
                "severity": "error",
                "message": "echo: the source says it is invalid",
            })),
        "{report:#}"
    );
    // The handler whose body fails is still the lexicon's handler.
    assert_eq!(
        file(&report, "src/lexicons/com/example/broken/listThings.echo")["lexicon"],
        "com.example.broken.listThings"
    );
    // A lexicon that does not load is still a lexicon file, but names no NSID.
    assert_eq!(
        file(&report, "src/lexicons/com/example/broken/getThing.json")["role"],
        "lexicon"
    );
    assert_eq!(
        report["lexicons"].as_array().unwrap().len(),
        1,
        "{report:#}"
    );
}

#[test]
fn json_says_why_a_check_cannot_start() {
    let (code, report) = check_json(&[
        "tests/fixtures/projects/basic",
        "--plugin",
        "tests/fixtures/projects",
    ]);
    assert_eq!(code, 2);
    assert_eq!(report["ok"], false);
    assert!(
        report["error"]
            .as_str()
            .unwrap()
            .starts_with("tests/fixtures/projects: could not load the plugin"),
        "{report:#}"
    );
}

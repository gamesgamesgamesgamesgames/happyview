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

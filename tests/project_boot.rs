//! The binary refuses to start when its project doesn't build.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn a_project_that_does_not_build_stops_the_boot() {
    let db = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_happyview"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env(
            "DATABASE_URL",
            format!("sqlite://{}/happyview.db?mode=rwc", db.path().display()),
        )
        .env("PUBLIC_URL", "http://127.0.0.1:3999")
        .env("PORT", "3999")
        .env("PLUGIN_URLS", "")
        .env("HAPPYVIEW_PROJECT", "tests/fixtures/projects/broken")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start happyview");

    // A boot that went on to serve would never exit, so wait with a deadline.
    let deadline = Instant::now() + Duration::from_secs(300);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            panic!("happyview kept running with a project that does not build");
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let mut stderr = String::new();
    std::io::Read::read_to_string(child.stderr.as_mut().unwrap(), &mut stderr).unwrap();

    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(
        stderr.lines().any(|l| l.starts_with(
            "src/lexicons/com/example/broken/getThing.json: the lexicon's id is 'com.example.broken.getOther'"
        )),
        "{stderr}"
    );
    assert!(stderr.contains("the project did not build"), "{stderr}");
}

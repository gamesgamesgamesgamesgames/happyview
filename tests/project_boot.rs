//! The binary builds its project at boot: it refuses to start when the
//! project doesn't build, and applies what a project declares when it does.

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
        .stderr(std::fs::File::create(db.path().join("stderr.log")).unwrap())
        .spawn()
        .expect("start happyview");

    // stderr goes to a file rather than a pipe: a pipe nobody reads until
    // exit fills up and blocks the child.
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
    let stderr = std::fs::read_to_string(db.path().join("stderr.log")).unwrap();

    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(
        stderr.lines().any(|l| l.starts_with(
            "src/lexicons/com/example/broken/getThing.json: the lexicon's id is 'com.example.broken.getOther'"
        )),
        "{stderr}"
    );
    assert!(stderr.contains("the project did not build"), "{stderr}");
}

/// Kills the server when the test ends, however it ends.
struct Running(std::process::Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn a_project_that_builds_is_applied_at_boot() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    let write = |rel: &str, contents: &str| {
        let path = project.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    };
    write(
        "happyview.toml",
        "[labels]\nsubscribe = [\"did:plc:bootlabeler\"]\n\n\
         [lexicons.\"com.example.boot.thing\"]\nbackfill = true\n",
    );
    write(
        "src/lexicons/com/example/boot/thing.json",
        r#"{"lexicon":1,"id":"com.example.boot.thing","defs":{"main":{"type":"record","key":"tid","record":{"type":"object","properties":{}}}}}"#,
    );
    let db_path = dir.path().join("happyview.db");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let log = std::fs::File::create(dir.path().join("stderr.log")).unwrap();

    // Every outbound service points at a closed local port, so the backfill
    // and the labeler subscription fail at once instead of reaching the
    // network or hanging on it.
    let closed = "127.0.0.1:9";
    let _server = Running(
        Command::new(env!("CARGO_BIN_EXE_happyview"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .env(
                "DATABASE_URL",
                format!("sqlite://{}?mode=rwc", db_path.display()),
            )
            .env("PUBLIC_URL", format!("http://127.0.0.1:{port}"))
            .env("PORT", port.to_string())
            .env("PLUGIN_URLS", "")
            .env("PLUGIN_DIR", dir.path().join("plugins"))
            .env("JETSTREAM_URL", format!("ws://{closed}"))
            .env("RELAY_URL", format!("http://{closed}"))
            .env("PLC_URL", format!("http://{closed}"))
            .env("TELEMETRY_COLLECTOR_URL", format!("http://{closed}"))
            .env("HAPPYVIEW_PROJECT", &project)
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .expect("start happyview"),
    );

    let deadline = Instant::now() + Duration::from_secs(300);
    let mut db = None;
    loop {
        if db.is_none() && db_path.exists() {
            db = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect(&format!("sqlite://{}?mode=ro", db_path.display()))
                .await
                .ok();
        }
        if let Some(db) = &db {
            let jobs: Option<(i64,)> =
                sqlx::query_as("SELECT COUNT(*) FROM happyview_backfill_jobs WHERE collection = ?")
                    .bind("com.example.boot.thing")
                    .fetch_one(db)
                    .await
                    .ok();
            let labelers: Option<(i64,)> = sqlx::query_as(
                "SELECT COUNT(*) FROM happyview_labeler_subscriptions WHERE did = ?",
            )
            .bind("did:plc:bootlabeler")
            .fetch_one(db)
            .await
            .ok();
            if jobs == Some((1,)) && labelers == Some((1,)) {
                return;
            }
        }
        if Instant::now() > deadline {
            let stderr = std::fs::read_to_string(dir.path().join("stderr.log")).unwrap();
            panic!("the declared backfill and labeler never appeared; the server said:\n{stderr}");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

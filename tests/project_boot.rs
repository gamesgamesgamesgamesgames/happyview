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
            // Seeded by the same boot, which no TestApp runs.
            let hpy: Option<(i64,)> =
                sqlx::query_as("SELECT COUNT(*) FROM happyview_api_clients WHERE client_key = ?")
                    .bind(happyview::auth::cli_client::CLIENT_KEY)
                    .fetch_one(db)
                    .await
                    .ok();
            if jobs == Some((1,)) && labelers == Some((1,)) && hpy == Some((1,)) {
                return;
            }
        }
        if Instant::now() > deadline {
            let stderr = std::fs::read_to_string(dir.path().join("stderr.log")).unwrap();
            panic!(
                "the declared backfill, the labeler or the hpy client never appeared; the server said:\n{stderr}"
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Upgrading must not turn a serving instance into one that exits 1.
///
/// The fixture's lexicon names a def it does not define, which the spec
/// checks refuse and every HappyView before them accepted. Boot builds
/// leniently: it warns, names the file, and goes on to serve. `happyview
/// check` on the very same project refuses it, which is where someone is
/// watching.
#[test]
fn a_lexicon_the_spec_checks_refuse_warns_at_boot_and_fails_check() {
    const FIXTURE: &str = "tests/fixtures/projects/legacy-lexicon";

    // `check` reads no database, so it answers on its own.
    let checked = Command::new(env!("CARGO_BIN_EXE_happyview"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["check", FIXTURE])
        .output()
        .expect("run happyview check");
    let said = String::from_utf8_lossy(&checked.stderr).to_string()
        + &String::from_utf8_lossy(&checked.stdout);
    assert_eq!(
        checked.status.code(),
        Some(1),
        "check must refuse it: {said}"
    );
    assert!(
        said.contains("src/lexicons/com/example/legacy/thing.json"),
        "check should name the file: {said}"
    );
    assert!(
        said.contains("names no def in this lexicon"),
        "check should say what is wrong: {said}"
    );

    // Boot takes the same project and serves anyway.
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("happyview.db");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let log_path = dir.path().join("stderr.log");
    let closed = "127.0.0.1:9";
    let mut server = Running(
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
            .env("HAPPYVIEW_PROJECT", FIXTURE)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log_path).unwrap())
            .spawn()
            .expect("start happyview"),
    );

    // It has to still be running, and it has to have said why it is unhappy.
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        let said = std::fs::read_to_string(&log_path).unwrap_or_default();
        if let Some(status) = server.0.try_wait().unwrap() {
            panic!(
                "boot exited with {:?} over a lexicon it should only have warned about:\n{said}",
                status.code()
            );
        }
        if said.contains("names no def in this lexicon") {
            assert!(
                said.contains("src/lexicons/com/example/legacy/thing.json"),
                "the warning should name the file: {said}"
            );
            assert!(
                !said.contains("the project did not build"),
                "it must not read as a failed build: {said}"
            );
            return;
        }
        if Instant::now() > deadline {
            panic!("boot never warned about the lexicon:\n{said}");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

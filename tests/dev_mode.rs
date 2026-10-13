//! `happyview dev` against a temporary project, through the real binary.
//!
//! One process, one project and several edits: each boot costs seconds, and
//! every assertion is about the same running instance.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const THING: &str = r#"{"lexicon":1,"id":"com.example.thing","defs":{"main":{"type":"record","key":"tid","record":{"type":"object","properties":{"text":{"type":"string"}}}}}}"#;
const LIST: &str = r#"{"lexicon":1,"id":"com.example.listThings","defs":{"main":{"type":"query","output":{"encoding":"application/json","schema":{"type":"object","properties":{"records":{"type":"array","items":{"type":"unknown"}}}}}}}}"#;
const STILL_SERVING: &str = "build failed; still serving the previous build";
const BUILT: &str = "built in";

struct Dev {
    child: Child,
    out: Arc<Mutex<Vec<String>>>,
}

impl Drop for Dev {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Dev {
    fn lines(&self) -> Vec<String> {
        self.out.lock().unwrap().clone()
    }

    fn count(&self, prefix: &str) -> usize {
        self.lines()
            .iter()
            .filter(|l| l.starts_with(prefix))
            .count()
    }

    /// Every `built in` line from the `skip`th on, which is how a step reads
    /// only the builds its own edits caused.
    fn builds_since(&self, skip: usize) -> Vec<String> {
        self.lines()
            .into_iter()
            .filter(|l| l.starts_with(BUILT))
            .skip(skip)
            .collect()
    }

    /// A child that exited fails the wait at once with what it printed,
    /// rather than leaving the wait to run out its timeout.
    fn assert_running(&mut self, what: &str) {
        if let Some(status) = self.child.try_wait().unwrap() {
            panic!(
                "happyview dev exited ({status}) while waiting for {what}; it printed:\n{:#?}",
                self.lines()
            );
        }
    }
}

/// Editors save by writing a dotfile beside the file and renaming it over
/// the original; the watcher has to see that as a change to the final path.
fn save(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let name = path.file_name().unwrap().to_string_lossy();
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    std::fs::write(&tmp, contents).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn start(root: &Path) -> Dev {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_happyview"));
    cmd.arg("dev").arg(root).current_dir(root).env_clear();
    for key in ["PATH", "HOME", "TMPDIR", "SystemRoot", "USERPROFILE"] {
        if let Ok(v) = std::env::var(key) {
            cmd.env(key, v);
        }
    }
    // Every outbound service points at a closed local port, so nothing
    // reaches the network or hangs on it.
    let closed = "127.0.0.1:9";
    let mut child = cmd
        .env("JETSTREAM_URL", format!("ws://{closed}"))
        .env("PLC_URL", format!("http://{closed}"))
        .env("RELAY_URL", format!("http://{closed}"))
        .env("TELEMETRY_COLLECTOR_URL", format!("http://{closed}"))
        .env("PLUGIN_URLS", "")
        .env("PLUGIN_REGISTRY_URL", "off")
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("start happyview dev");
    let out = Arc::new(Mutex::new(Vec::new()));
    let sink = out.clone();
    let stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            eprintln!("[dev] {line}");
            sink.lock().unwrap().push(line);
        }
    });
    Dev { child, out }
}

async fn wait_for<F: FnMut(&Dev) -> bool>(
    dev: &mut Dev,
    what: &str,
    timeout: Duration,
    mut done: F,
) {
    let started = Instant::now();
    while !done(dev) {
        dev.assert_running(what);
        assert!(
            started.elapsed() < timeout,
            "timed out after {timeout:?} waiting for {what}; dev printed:\n{:#?}",
            dev.lines()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Waits until no build has been reported for `quiet`, so a count taken
/// afterwards includes every build a burst of saves caused.
async fn wait_until_quiet(dev: &mut Dev, what: &str, quiet: Duration, timeout: Duration) {
    let started = Instant::now();
    let mut seen = dev.count(BUILT);
    let mut since = Instant::now();
    while since.elapsed() < quiet {
        dev.assert_running(what);
        assert!(
            started.elapsed() < timeout,
            "builds never stopped after {what}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        let now = dev.count(BUILT);
        if now != seen {
            seen = now;
            since = Instant::now();
        }
    }
}

async fn get(url: &str) -> (u16, String) {
    match reqwest::get(url).await {
        Ok(r) => (r.status().as_u16(), r.text().await.unwrap_or_default()),
        Err(_) => (0, String::new()),
    }
}

async fn wait_for_status(dev: &mut Dev, url: &str, status: u16, timeout: Duration) -> String {
    let started = Instant::now();
    loop {
        let (got, body) = get(url).await;
        if got == status {
            return body;
        }
        dev.assert_running(&format!("{url} to answer {status}"));
        assert!(
            started.elapsed() < timeout,
            "{url} never answered {status}: last {got} {body}; dev printed:\n{:#?}",
            dev.lines()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn db(root: &Path) -> sqlx::AnyPool {
    sqlx::any::install_default_drivers();
    sqlx::AnyPool::connect(&format!(
        "sqlite://{}?mode=ro",
        root.join(".happyview/happyview.db")
            .to_string_lossy()
            .replace('\\', "/")
    ))
    .await
    .unwrap()
}

async fn is_super(db: &sqlx::AnyPool, did: &str) -> Option<i32> {
    sqlx::query_as::<_, (i32,)>("SELECT is_super FROM happyview_users WHERE did = ?")
        .bind(did)
        .fetch_optional(db)
        .await
        .unwrap()
        .map(|r| r.0)
}

#[tokio::test]
async fn dev_mode_rebuilds_on_save() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let port = free_port();
    let base = format!("http://127.0.0.1:{port}");
    let list_url = format!("{base}/xrpc/com.example.listThings");
    let toml = |superadmin: &str, extra: &str| {
        format!(
            "[instance]\nsuperadmin = \"{superadmin}\"\n\n[dev]\nport = {port}\n\n[lexicons.\"com.example.thing\"]\nbackfill = true\n{extra}"
        )
    };
    let with_target =
        "\n[lexicons.\"com.example.listThings\"]\ntarget_collection = \"com.example.thing\"\n";
    save(&root.join("happyview.toml"), &toml("did:plc:devadmin", ""));
    // Script variables only: were dev to read these as server settings it
    // would connect to Postgres and listen on port 1, and never come up.
    save(
        &root.join(".env"),
        "DATABASE_URL=postgres://nowhere/none\nPORT=1\n",
    );
    let lexicons = root.join("src/lexicons/com/example");
    save(&lexicons.join("thing.json"), THING);
    // Broken at boot: dev starts anyway and waits for the fix.
    save(&lexicons.join("listThings.json"), "{ not json");

    let mut dev = start(&root);
    wait_for(&mut dev, "the listener", Duration::from_secs(180), |_| {
        std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
    })
    .await;
    assert!(
        root.join(".happyview/happyview.db").is_file(),
        "the project .env's DATABASE_URL must not replace the dev database"
    );
    wait_for(
        &mut dev,
        "the boot build's failure",
        Duration::from_secs(30),
        |d| d.count(STILL_SERVING) >= 1,
    )
    .await;
    assert!(
        dev.lines()
            .iter()
            .any(|l| l.starts_with("src/lexicons/com/example/listThings.json")),
        "the error names the file: {:#?}",
        dev.lines()
    );

    // Fixed: the query exists, but has no target collection yet.
    let builds = dev.count(BUILT);
    save(&lexicons.join("listThings.json"), LIST);
    wait_for(
        &mut dev,
        "a build after the fix",
        Duration::from_secs(30),
        |d| d.count(BUILT) > builds,
    )
    .await;
    let body = wait_for_status(&mut dev, &list_url, 400, Duration::from_secs(5)).await;
    assert!(body.contains("target_collection"), "{body}");

    // A happyview.toml edit: an override (applied by the build) and a new
    // superadmin (applied by the reload) take effect without a restart.
    save(
        &root.join("happyview.toml"),
        &toml("did:plc:second", with_target),
    );
    let body = wait_for_status(&mut dev, &list_url, 200, Duration::from_secs(30)).await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["records"],
        serde_json::json!([])
    );
    let pool = db(&root).await;
    let started = Instant::now();
    while is_super(&pool, "did:plc:second").await != Some(1) {
        dev.assert_running("the edited superadmin");
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the edited superadmin was never seeded; dev printed:\n{:#?}",
            dev.lines()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        is_super(&pool, "did:plc:devadmin").await,
        Some(1),
        "nobody is demoted"
    );

    // A port edit can't apply to a running listener, and says so.
    let other_port = free_port();
    save(
        &root.join("happyview.toml"),
        &toml("did:plc:second", with_target)
            .replace(&format!("port = {port}"), &format!("port = {other_port}")),
    );
    wait_for(
        &mut dev,
        "the restart notice",
        Duration::from_secs(30),
        |d| {
            d.lines()
                .iter()
                .any(|l| l.starts_with("restart required to apply:") && l.contains("PORT"))
        },
    )
    .await;
    assert_eq!(
        get(&list_url).await.0,
        200,
        "still listening on the old port"
    );

    // A burst of saves is a few builds, not one per file.
    let builds = dev.count(BUILT);
    for i in 0..20 {
        save(
            &lexicons.join(format!("burst{i}.json")),
            &THING.replace("com.example.thing", &format!("com.example.burst{i}")),
        );
    }
    wait_for(
        &mut dev,
        "a build that includes every saved file",
        Duration::from_secs(30),
        |d| {
            let built = d.builds_since(builds).join("\n");
            (0..20).all(|i| built.contains(&format!("added lexicon com.example.burst{i}")))
        },
    )
    .await;
    wait_until_quiet(
        &mut dev,
        "the burst",
        Duration::from_secs(2),
        Duration::from_secs(30),
    )
    .await;
    let burst_builds = dev.count(BUILT) - builds;
    assert!(
        (1..20).contains(&burst_builds),
        "{burst_builds} builds for 20 saves"
    );

    // A compiled script module is a project input, and .happyview/build/ is
    // noticed although it did not exist when dev started.
    assert!(
        !root.join(".happyview/build").exists(),
        "dev must not create build/ itself"
    );
    let builds = dev.count(BUILT);
    std::fs::create_dir_all(root.join(".happyview/build")).unwrap();
    // The directory appearing is itself a build, and the watch on it is in
    // place before that build reports.
    wait_for(
        &mut dev,
        "build/ appearing to be noticed",
        Duration::from_secs(30),
        |d| d.count(BUILT) > builds,
    )
    .await;
    let builds = dev.count(BUILT);
    // The smallest valid module: magic and version. Written by rename, as
    // hpy does, so the temporary name is never classified as a module.
    save(
        &root.join(".happyview/build/registry.wasm"),
        "\0asm\u{1}\0\0\0",
    );
    wait_for(&mut dev, "the module build", Duration::from_secs(30), |d| {
        d.count(BUILT) > builds
    })
    .await;

    // Broken again: the previous build keeps serving.
    let failures = dev.count(STILL_SERVING);
    save(&lexicons.join("listThings.json"), "{ broken again");
    wait_for(&mut dev, "the failure", Duration::from_secs(30), |d| {
        d.count(STILL_SERVING) > failures
    })
    .await;
    assert_eq!(get(&list_url).await.0, 200);

    // The state the instance made for itself.
    let state = root.join(".happyview");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(state.join("secrets"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
    drop(dev);

    let (mode, complete): (String, i32) = sqlx::query_as(
        "SELECT mode, CAST(setup_complete AS INTEGER) FROM happyview_service_identity WHERE id = 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((mode.as_str(), complete), ("not_exposed", 1));
    // `backfill = true` on a record lexicon, but [dev] backfill is off.
    let (jobs,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM happyview_backfill_jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(jobs, 0);
}

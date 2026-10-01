//! The interpreter plugin against the native `src/lua/` runner, case for
//! case. This is the only thing that pins the two to the same behaviour, and
//! it is what lets the native `validate_script` retire with evidence rather
//! than with hope. It stays until `src/lua/` is deleted.
//!
//! `#[ignore]`-gated because it runs 118 scripts through two interpreters. The
//! separate claim that the plugin loads and runs at all lives in
//! `tests/lua_interpreter_plugin.rs`, which is not ignored.
//!
//! `common::lua_plugin` locates the plugin, names the module in the output,
//! and refuses a stale one; every target that loads the plugin goes through
//! it, so none can be run against a build nobody meant to test.
//!
//! ```sh
//! D=$(mktemp -d)
//! cp <plugins>/plugins/happyview-lua/manifest.json "$D/"
//! cp <target>/wasm32-wasip1/release/happyview_lua.wasm "$D/happyview-lua.wasm"
//! HAPPYVIEW_LUA_PLUGIN=$D cargo test --features lua-reference \
//!   --test lua_differential -- --ignored
//! ```
//!
//! **What is compared, and what is not.** For every case: the returned value
//! with its kind, or the failure's kind, message and line; and the
//! `script.log` rows each run wrote, in order, with their level, message and
//! fields. A job case also compares the progress the run left on its job row.
//!
//! The transcript of library calls comes from
//! `tests/fixtures/differential_library`, registered under all ten standard
//! namespaces. It answers every call the corpus makes and appends each one to
//! its own key-value store, which the harness reads straight from the table —
//! so the transcript carries the library, the function and the arguments
//! verbatim, which is what catches an argument that changes no answer.
//! Nothing else in the host records a library call.
//!
//! Because that store is keyed per plugin id and the fixture is registered
//! once per namespace, the transcript is one ordered list **per library**.
//! Order within a library is compared exactly; order between two libraries is
//! not.

mod common;

use std::path::Path;

use serde_json::{Value, json};

use common::app::TestApp;
use common::lua_plugin::{identify, plugin_dir};
use happyview::plugin::{
    LoadedPlugin, PluginInfo, PluginManifest, PluginSource, ScriptExecuteContext,
    ScriptExecuteInput, ScriptExecuteLimits, ScriptExecuteOutput, ScriptJob, ScriptKind,
    ScriptLibraryRef, loader,
};

/// The key the fixture appends each call under.
const TRANSCRIPT_KEY: &str = "transcript";

const LIBRARY_FIXTURE: &str = "tests/fixtures/differential_library/target/wasm32-unknown-unknown/release/differential_library.wasm";

/// Every namespace the corpus and the bridge cases `require`. One fixture is
/// registered under each, so a script resolves what it asks for and the
/// comparison reaches past `require` into the bridge.
const NAMESPACES: [&str; 10] = [
    "happyview.db",
    "happyview.sql",
    "happyview.backlinks",
    "happyview.record",
    "happyview.xrpc",
    "happyview.atproto",
    "happyview.linked_repos",
    "happyview.jobs",
    "happyview.spaces",
    "happyview.http",
];

/// No case may ask a real host to wait. `ctx.job.wait` sleeps for real on
/// both paths, and comparing a sleep compares the clock rather than the
/// interpreter. This one asks for 99 999 seconds, which the host clamps to an
/// hour and then honours. What covers the same ground elsewhere: the wait's
/// clamp and the three job imports against the host in
/// `tests/plugin_interpreter.rs`, and `ctx.job`'s four Lua names in the
/// plugin's own `src/ctx.rs`.
const EXCLUDED: [&str; 1] = ["bridge/job.controls.lua"];

/// The two cases the paths answer differently, and why.
///
/// `bridge/query.builtins.lua` lists the members of the `os` table, and
/// `os.clock` is in the native sandbox's and absent from the plugin's:
/// preview 1 offers no process CPU clock, and a function that always fails
/// teaches a script author nothing.
///
/// The two `marker_*_at_file_scope` cases read a removed global while
/// loading. The plugin reports that as `runtime`, which is what it is, and
/// the native runner reports `syntax`, because `execute.rs` maps every load
/// failure to it. The plugin is the correct one — calling it a syntax error
/// sends an author looking for a missing `end` and leaves a client unable to
/// tell a parse failure from an unmigrated script — so this is a difference
/// the native path loses when it goes rather than one to reconcile.
///
/// `bridge/query.coroutines.lua` calls a library from inside a
/// `coroutine.wrap` coroutine, which its own comment predicts will differ. A
/// library call is an *async* function on the native path, under
/// `call_async`, and one inside a wrapped coroutine ends the run with the
/// execution-limit error; in the guest every import is a plain blocking call,
/// so the same script returns the rows. The plugin is the one that behaves,
/// and this is a capability a script gains rather than loses.
///
/// Anything else failing this is a difference nobody decided on; a case
/// leaving this list is one that has closed, and the count below fails then
/// too so the list cannot rot.
const KNOWN_DIFFERENCES: [&str; 4] = [
    "bridge/query.builtins.lua",
    "bridge/query.coroutines.lua",
    "procedure.marker_input_read_at_file_scope.expected.lua",
    "query.marker_context_read_at_file_scope.expected.lua",
];

/// The two cases that differ **only when the process is not on UTC**.
///
/// `os.time` on a table and a bare `os.date` both read local time, because
/// that is what PUC does; the guest has no zone to read and answers UTC,
/// while the native runner takes the server's. So the pair is a difference on
/// a server outside UTC and no difference on one inside it, and the harness
/// expects whichever the run it is in should produce rather than a fixed
/// verdict. Either way they are exercised, which is what was missing.
const ZONE_DEPENDENT: [&str; 2] = [
    "bridge/query.timezone_table.lua",
    "bridge/query.timezone_format.lua",
];

/// A case that has not finished by now is not going to tell us anything: mlua
/// cannot be interrupted from outside, and a job run has no deadline on
/// either path, so the harness needs its own or one script ends the run
/// rather than the case.
const PER_CASE: std::time::Duration = std::time::Duration::from_secs(30);

/// Every v3 script the repo knows the text of: the codemod's expected outputs
/// and the editor's templates, read where they live rather than copied, so
/// the harness cannot drift from what the codemod is pinned to produce.
fn corpus() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = Vec::new();
    // The codemod's directory holds its inputs beside its outputs, and only
    // the outputs are v3 scripts.
    for (dir, suffix) in [
        ("tests/codemod/cases", ".expected.lua"),
        ("web/src/lib/lua-templates", ".lua"),
    ] {
        let dir = root.join(dir);
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().ends_with(suffix))
            })
            .collect();
        paths.sort();
        for path in paths {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            out.push((name, std::fs::read_to_string(&path).unwrap()));
        }
    }
    out
}

/// The hand-written cases that exercise the bridge, the budgets and the error
/// shapes, carried over from the spike unmodified.
fn bridge_cases() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/lua_differential");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "lua"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            (
                format!("bridge/{}", path.file_name().unwrap().to_string_lossy()),
                std::fs::read_to_string(&path).unwrap(),
            )
        })
        .collect()
}

/// The fixture under one namespace. Ten copies share one module; the id is
/// what a transcript line names, so each is the namespace it serves.
fn library(namespace: &str, wasm: &[u8]) -> LoadedPlugin {
    let manifest: PluginManifest = serde_json::from_value(json!({
        "id": namespace, "name": namespace, "version": "1.0.0", "api_version": "2",
        "plugin_type": "library", "namespace": namespace,
        // What the fixture needs to record a call. Without them the transcript
        // is empty and every comparison of it passes vacuously.
        "capabilities": ["kv:read", "kv:write"],
    }))
    .expect("the fixture manifest should deserialize");
    LoadedPlugin {
        info: PluginInfo {
            id: namespace.into(),
            name: namespace.into(),
            version: "1.0.0".into(),
            api_version: "2".into(),
            icon_url: None,
            required_secrets: vec![],
            auth_type: "oauth2".into(),
            config_schema: None,
        },
        source: PluginSource::File {
            path: "tests/fixtures/differential_library".into(),
        },
        wasm_bytes: wasm.to_vec(),
        manifest: Some(manifest),
    }
}

/// Every library call a run made, as one ordered list per library. The
/// fixture's writes are awaited by the host, so this needs no settling and
/// cannot read a half-written transcript.
async fn transcript(app: &TestApp) -> Value {
    // `value` is bytes, not text, and nothing here may be swallowed: a read
    // that fails quietly makes the whole comparison vacuous and turns a
    // broken harness into a passing one.
    let rows: Vec<(String, Vec<u8>)> = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT plugin_id, value FROM happyview_plugin_kv WHERE key = ? ORDER BY plugin_id",
        app.state.db_backend,
    ))
    .bind(TRANSCRIPT_KEY)
    .fetch_all(&app.state.db)
    .await
    .expect("the transcript should be readable");
    let mut by_library = serde_json::Map::new();
    for (library, value) in rows {
        let calls: Value = serde_json::from_slice(&value)
            .unwrap_or_else(|e| panic!("{library} wrote a transcript that will not parse: {e}"));
        by_library.insert(library, scrub(&calls));
    }
    Value::Object(by_library)
}

async fn clear_transcript(app: &TestApp) {
    let _ = happyview::db::query(&happyview::db::adapt_sql(
        "DELETE FROM happyview_plugin_kv WHERE key = ?",
        app.state.db_backend,
    ))
    .bind(TRANSCRIPT_KEY)
    .execute(&app.state.db)
    .await;
}

/// Whether the native runner's process is on UTC, asked of the runner itself
/// because it is the side that has a zone.
async fn native_local_time_is_utc(app: &TestApp) -> bool {
    let probe = input(
        "query.zone",
        r#"function handle()
             return os.date("!%Y-%m-%dT%H:%M:%S", 0) == os.date("%Y-%m-%dT%H:%M:%S", 0)
           end"#,
        None,
    );
    matches!(
        happyview::lua::run_for_differential(&app.state, &probe).await,
        ScriptExecuteOutput::Returned { value, .. } if value == json!(true)
    )
}

/// The interpreter and the ten library registrations both paths share.
async fn registered(app: &TestApp) -> (String, happyview::plugin::PluginExecutor) {
    let dir = plugin_dir().expect("the caller checks the variable first");
    let plugin = loader::load_from_file(&dir)
        .await
        .expect("the plugin should load through the loader");
    let id = plugin.info.id.clone();
    app.state.plugin_registry.register(plugin).await;

    let wasm = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join(LIBRARY_FIXTURE)).expect(
        "library fixture not built. Run: cargo build --manifest-path \
         tests/fixtures/differential_library/Cargo.toml --target wasm32-unknown-unknown --release",
    );
    for namespace in NAMESPACES {
        app.state
            .plugin_registry
            .register(library(namespace, &wasm))
            .await;
        // The key-value store has a foreign key onto the plugins table, so a
        // plugin registered only in memory cannot record anything — which is
        // how the transcript came back empty rather than refused.
        happyview::db::query(&happyview::db::adapt_sql(
            "INSERT INTO happyview_plugins (id, source, api_version) VALUES (?, 'file', '2')
             ON CONFLICT (id) DO NOTHING",
            app.state.db_backend,
        ))
        .bind(namespace)
        .execute(&app.state.db)
        .await
        .expect("the plugin row the store's foreign key needs");
    }
    (id, app.state.plugin_executor())
}

/// The trigger each case runs under, taken from its own name the way the
/// spike's harness took it, so a job case gets a job and a label case does
/// not get a method.
fn kind_of(name: &str) -> (ScriptKind, &'static str) {
    let stem = name
        .trim_start_matches("bridge/")
        .split(['.', '-'])
        .next()
        .unwrap_or("query");
    match stem {
        "job" => (ScriptKind::Job, "job.run:test.export"),
        "label" => (ScriptKind::Label, "labeler.apply:app.example.post"),
        "procedure" => (
            ScriptKind::XrpcProcedure,
            "xrpc.procedure:app.example.doThing",
        ),
        "record" | "trigger" => (ScriptKind::RecordEvent, "record.create:app.example.post"),
        _ => (ScriptKind::XrpcQuery, "xrpc.query:app.example.listThings"),
    }
}

/// One input shape for every case, generous enough that a script reading any
/// field finds something there. The same object goes to both paths.
fn input(name: &str, source: &str, job_id: Option<&str>) -> ScriptExecuteInput {
    let (kind, trigger) = kind_of(name);
    let mut context = ScriptExecuteContext {
        trigger: trigger.to_string(),
        caller_did: Some("did:plc:alice".into()),
        // `run_for_differential` lends the run no session, so a library sees
        // `has_pds_auth: false` on the native side whatever this says.
        // Comparing under `true` would compare the seam rather than the
        // interpreter; the fixture records the flag, so the two are checked.
        has_pds_auth: false,
        env: [
            ("API_KEY".to_string(), "k".to_string()),
            ("URL".to_string(), "https://example.test".to_string()),
        ]
        .into_iter()
        .collect(),
        method: Some("app.example.doThing".into()),
        collection: Some("app.example.post".into()),
        params: Some(
            json!({ "q": "hi", "limit": 2, "uri": "at://did:plc:alice/app.example.post/3kabc1" })
                .as_object()
                .unwrap()
                .clone(),
        ),
        ..ScriptExecuteContext::default()
    };
    let mut limits = ScriptExecuteLimits {
        instructions: Some(1_000_000),
        memory_bytes: 64 * 1024 * 1024,
    };
    if kind == ScriptKind::Job {
        context.job = Some(ScriptJob {
            id: job_id.unwrap_or("job-1").to_string(),
        });
        // Running long is what a job is for, on both paths.
        limits.instructions = None;
    }

    ScriptExecuteInput {
        source: source.to_string(),
        kind,
        input: json!({
            "uri": "at://did:plc:alice/app.example.post/3kabc1", "title": "Hello",
            "limit": 2, "cursor": Value::Null, "q": "hi", "query": "hi",
            "did": "did:plc:alice", "collection": "app.example.post", "rkey": "3kabc1",
            "action": "create",
            "record": { "title": "t", "tags": [], "meta": {}, "n": 3, "ratio": 0.5 },
            "src": "did:plc:labeler", "val": "spam", "neg": false,
            "cts": "2026-09-17T00:00:00Z", "text": "hello", "name": "n",
            "items": [1, 2, 3], "empty_list": [], "empty_map": {},
            "handle": "alice.test",
            "space": "at://did:plc:alice/space/com.example.forum/main",
            "token": "tok", "blob": "aGk=", "when": "2026-09-17T10:20:30.123Z",
        }),
        context,
        libraries: NAMESPACES
            .iter()
            .map(|namespace| ScriptLibraryRef {
                namespace: namespace.to_string(),
                id: namespace.to_string(),
            })
            .collect(),
        limits,
        removed_globals: Vec::new(),
    }
}

/// Three things no two runs can agree on, replaced by a marker on both sides
/// so the comparison is of everything else.
///
/// A TID's low bits are drawn from entropy. An instant read during a run
/// differs by however long the first run took, so one close to now is
/// replaced while a fixed one — the epoch, say — is compared as written. An
/// address is an address.
///
/// Each is narrow on purpose: a marker that swallowed more than it had to
/// would hide the differences this harness exists to find.
fn scrub(value: &Value) -> Value {
    const ALPHABET: &[u8] = b"234567abcdefghijklmnopqrstuvwxyz";
    /// Wide enough for the gap between the two runs, far short of any
    /// timestamp a case writes as a literal.
    const RECENT: i64 = 600;

    fn is_tid(s: &str) -> bool {
        s.len() == 13 && s.bytes().all(|b| ALPHABET.contains(&b))
    }
    fn now_seconds() -> i64 {
        chrono::Utc::now().timestamp()
    }
    fn is_recent_instant(s: &str) -> bool {
        chrono::DateTime::parse_from_rfc3339(s)
            .map(|at| (now_seconds() - at.timestamp()).abs() < RECENT)
            .unwrap_or(false)
    }
    fn is_recent_epoch(n: i64) -> bool {
        let seconds = if n.abs() > 100_000_000_000 {
            n / 1000
        } else {
            n
        };
        (now_seconds() - seconds).abs() < RECENT
    }

    match value {
        Value::String(s) if is_tid(s) => json!("<tid>"),
        Value::String(s) if is_recent_instant(s) => json!("<instant>"),
        Value::String(s) if s.contains(": 0x") => {
            let (kind, _) = s.split_once(": 0x").expect("just matched");
            json!(format!("{kind}: <address>"))
        }
        Value::Number(n) => match n.as_i64() {
            Some(n) if is_recent_epoch(n) => json!("<epoch>"),
            _ => value.clone(),
        },
        Value::Array(items) => Value::Array(items.iter().map(scrub).collect()),
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), scrub(v))).collect())
        }
        other => other.clone(),
    }
}

/// The comparable shape of an outcome. `raw` is left out: it carries mlua's
/// traceback, which names the frames each path happens to have rather than
/// anything a caller sees.
fn comparable(outcome: &Result<ScriptExecuteOutput, String>) -> Value {
    let value = match outcome {
        Ok(ScriptExecuteOutput::Returned { value, value_kind }) => json!({
            "status": "returned", "value": value, "value_kind": value_kind,
        }),
        Ok(ScriptExecuteOutput::Error {
            kind,
            message,
            line,
            ..
        }) => json!({ "status": "error", "kind": kind, "message": message, "line": line }),
        Err(e) => json!({ "status": "host_error", "error": e }),
    };
    scrub(&value)
}

/// Every `script.log` row a run wrote, in the order it wrote them.
async fn log_rows(app: &TestApp) -> Value {
    let rows: Vec<(String,)> = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT detail FROM happyview_event_logs WHERE event_type = 'script.log' ORDER BY created_at, id",
        app.state.db_backend,
    ))
    .fetch_all(&app.state.db)
    .await
    .expect("the log rows should be readable");
    let entries: Vec<Value> = rows
        .into_iter()
        .map(|(detail,)| {
            // Dropping a row that will not parse is how the transcript came to
            // compare nothing; a log row is no different.
            serde_json::from_str::<Value>(&detail)
                .unwrap_or_else(|e| panic!("a script.log row will not parse: {e}"))
        })
        .map(|detail| {
            json!({
                "level": detail["level"], "message": detail["message"], "fields": detail["fields"],
            })
        })
        .collect();
    scrub(&Value::Array(entries))
}

async fn clear_logs(app: &TestApp) {
    let _ = happyview::db::query(&happyview::db::adapt_sql(
        "DELETE FROM happyview_event_logs WHERE event_type = 'script.log'",
        app.state.db_backend,
    ))
    .execute(&app.state.db)
    .await;
}

async fn seed_job(app: &TestApp, id: &str) {
    let _ = happyview::db::query(&happyview::db::adapt_sql(
        "DELETE FROM happyview_jobs WHERE id = ?",
        app.state.db_backend,
    ))
    .bind(id)
    .execute(&app.state.db)
    .await;
    happyview::db::query(&happyview::db::adapt_sql(
        "INSERT INTO happyview_jobs (id, job_type, status, input, progress, created_by, created_at, inherit_auth) \
         VALUES (?, 'test.export', 'running', '{}', '{}', 'did:plc:alice', ?, ?)",
        app.state.db_backend,
    ))
    .bind(id)
    .bind(happyview::db::now_rfc3339())
    .bind(false)
    .execute(&app.state.db)
    .await
    .expect("the job row should seed");
}

async fn job_progress(app: &TestApp, id: &str) -> Value {
    let row: Option<(String,)> = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT progress FROM happyview_jobs WHERE id = ?",
        app.state.db_backend,
    ))
    .bind(id)
    .fetch_optional(&app.state.db)
    .await
    .unwrap_or(None);
    row.and_then(|(progress,)| serde_json::from_str::<Value>(&progress).ok())
        .map(|value| scrub(&value))
        .unwrap_or(Value::Null)
}

/// What the transcript is for: an argument that differs while the answer does
/// not. A number is the case that matters, because JSON keeps `2` and `2.0`
/// apart, Lua keeps an integer and a float apart, and a bridge that rounded
/// one into the other would get the same answer back from the library either
/// way — so nothing but the transcript would notice.
///
/// Both halves are asserted: that each path records the two distinctly, and
/// that the two spellings are not equal as JSON, which is what makes the
/// comparison in the harness above able to fail on them.
///
/// **Load-bearing rather than a nicety.** The counts asserted above are of
/// calls, so a fixture that recorded every call with an empty argument list
/// would keep them and compare equal on both paths — the same vacuity one
/// level down. This is what closes it.
#[tokio::test]
#[ignore = "needs HAPPYVIEW_LUA_PLUGIN"]
async fn the_transcript_keeps_an_integer_apart_from_a_float() {
    common::require_db!();
    if plugin_dir().is_none() {
        return;
    }
    let app = TestApp::new().await;
    let (id, executor) = registered(&app).await;

    let source = r#"local db = require("happyview.db")
                    function handle() return { answered = db.search(2, 2.0) ~= nil } end"#;
    let sent = input("query.numbers", source, None);

    clear_transcript(&app).await;
    executor
        .execute_script(&id, &sent)
        .await
        .expect("the plugin should answer");
    let from_plugin = transcript(&app).await;

    clear_transcript(&app).await;
    happyview::lua::run_for_differential(&app.state, &sent).await;
    let from_native = transcript(&app).await;

    for (path, recorded) in [("plugin", &from_plugin), ("native", &from_native)] {
        let args = &recorded["happyview.db"][0]["args"];
        assert_eq!(args[0], json!(2), "{path}: {recorded}");
        assert!(
            args[0].is_i64(),
            "{path}: 2 should stay an integer: {recorded}"
        );
        assert!(
            args[1].is_f64(),
            "{path}: 2.0 should stay a float: {recorded}"
        );
    }
    assert_eq!(from_plugin, from_native);

    // The two spellings differ as JSON, so the harness's own equality check
    // would fail if one path sent the other's.
    assert_ne!(json!(2), json!(2.0));
}

#[tokio::test]
#[ignore = "runs 118 scripts through two interpreters; needs HAPPYVIEW_LUA_PLUGIN"]
async fn the_plugin_and_the_native_runner_agree_case_for_case() {
    common::require_db!();
    let Some(dir) = plugin_dir() else { return };
    let app = TestApp::new().await;
    let (id, executor) = registered(&app).await;

    let mut cases = corpus();
    cases.extend(bridge_cases());
    let excluded = cases.len()
        - cases
            .iter()
            .filter(|(name, _)| !EXCLUDED.contains(&name.as_str()))
            .count();
    cases.retain(|(name, _)| !EXCLUDED.contains(&name.as_str()));

    let on_utc = native_local_time_is_utc(&app).await;
    let mut agreed = 0usize;
    let mut calls_compared = 0usize;
    let mut logs_compared = 0usize;
    let mut progress_compared = 0usize;
    let mut differences: Vec<String> = Vec::new();

    for (name, source) in &cases {
        let is_job = kind_of(name).0 == ScriptKind::Job;

        // One job id for both sides, reseeded between them: a script that
        // returns `ctx.job.id` must read the same thing twice, and a run must
        // not see the progress the other left.
        let job = "job-differential";
        if is_job {
            seed_job(&app, job).await;
        }

        clear_logs(&app).await;
        clear_transcript(&app).await;
        let sent = input(name, source, Some(job));
        let from_plugin =
            match tokio::time::timeout(PER_CASE, executor.execute_script(&id, &sent)).await {
                Ok(outcome) => outcome.map_err(|e| e.to_string()),
                Err(_) => Err(format!("the plugin did not finish within {PER_CASE:?}")),
            };
        let plugin_logs = log_rows(&app).await;
        let plugin_calls = transcript(&app).await;
        let plugin_progress = if is_job {
            job_progress(&app, job).await
        } else {
            Value::Null
        };

        clear_logs(&app).await;
        clear_transcript(&app).await;
        if is_job {
            seed_job(&app, job).await;
        }
        let sent = input(name, source, Some(job));
        let from_native = match tokio::time::timeout(
            PER_CASE,
            happyview::lua::run_for_differential(&app.state, &sent),
        )
        .await
        {
            Ok(outcome) => Ok(outcome),
            Err(_) => Err(format!(
                "the native runner did not finish within {PER_CASE:?}"
            )),
        };
        let native_logs = log_rows(&app).await;
        let native_calls = transcript(&app).await;
        let native_progress = if is_job {
            job_progress(&app, job).await
        } else {
            Value::Null
        };

        logs_compared += native_logs.as_array().map_or(0, Vec::len);
        if native_progress != Value::Null && native_progress != json!({}) {
            progress_compared += 1;
        }
        calls_compared += native_calls
            .as_object()
            .map(|by_library| {
                by_library
                    .values()
                    .map(|calls| calls.as_array().map_or(0, Vec::len))
                    .sum::<usize>()
            })
            .unwrap_or(0);

        let compared = [
            ("result", comparable(&from_native), comparable(&from_plugin)),
            ("library calls", native_calls, plugin_calls),
            ("logs", native_logs, plugin_logs),
            ("job progress", native_progress, plugin_progress),
        ];
        let differing: Vec<&(&str, Value, Value)> =
            compared.iter().filter(|(_, a, b)| a != b).collect();

        if differing.is_empty() {
            agreed += 1;
        } else {
            for (what, native, plugin) in differing {
                differences.push(format!(
                    "{name} [{what}]\n    native: {native}\n    plugin: {plugin}"
                ));
            }
        }
    }

    println!(
        "module: {}\ncases: {} ({excluded} excluded) | identical in every respect: {agreed} \
         | library calls compared: {calls_compared} | log lines compared: {logs_compared} \
         | job progress compared: {progress_compared} | native process on UTC: {on_utc}",
        identify(&dir),
        cases.len()
    );

    // Anything that reads back empty makes every comparison of it pass, which
    // is a broken harness reporting success. It happened once — the store's
    // rows were refused by a foreign key the registry does not touch, and the
    // read swallowed it — and the agreement figure *rose* as a result.
    //
    // Exact counts rather than floors: the case list is fixed and
    // deterministic, so a floor would let a partial emptying through, and a
    // count that changes for a good reason should be re-recorded here
    // deliberately.
    assert_eq!(
        (calls_compared, logs_compared, progress_compared),
        (82, 18, 2),
        "the transcript, the log rows or the job progress are not being recorded; \
         a deliberate change to the case list has to be recorded here. Two of the \
         three job cases call `ctx.job.progress`, which is why that one is 2"
    );

    let expected_to_differ: Vec<&str> = KNOWN_DIFFERENCES
        .iter()
        .copied()
        .chain(if on_utc {
            [].iter().copied()
        } else {
            ZONE_DEPENDENT.iter().copied()
        })
        .collect();
    let unexpected: Vec<&String> = differences
        .iter()
        .filter(|d| !expected_to_differ.iter().any(|known| d.starts_with(known)))
        .collect();
    assert!(
        unexpected.is_empty(),
        "{agreed}/{} agreed; {} differences nobody decided on:\n{}",
        cases.len(),
        unexpected.len(),
        unexpected
            .iter()
            .map(|d| d.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert_eq!(
        agreed,
        cases.len() - expected_to_differ.len(),
        "expected every case but {expected_to_differ:?} to agree"
    );
}

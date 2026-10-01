//! The real Lua interpreter plugin, loaded and run by the host.
//!
//! The plugin's own tests read its built module's sections and run its two
//! handlers natively. Neither can say whether wasmtime will instantiate it,
//! whether the five capabilities it declares cover the imports it actually
//! links, or whether `execute_script` gets a result back through the ABI —
//! and this is the first thing that asserts any of them, which makes it the
//! first evidence that `interpreter_plugin!` produces a module a host can
//! load at all.
//!
//! Deliberately separate from `lua_differential.rs`, which is slow and
//! ignored. These are not ignored, which is why the staleness refusal in
//! `common::lua_plugin` matters here most: this is the target a developer
//! runs casually, and eight green tests against a module nobody can identify
//! afterwards is the claim that was wrong once already. It names the module it
//! loaded, and refuses one older than the plugin's source when
//! `HAPPYVIEW_LUA_SRC` says where that is.
//!
//! ```sh
//! D=$(mktemp -d)
//! cp <plugins>/plugins/happyview-lua/manifest.json "$D/"
//! cp <target>/wasm32-wasip1/release/happyview_lua.wasm "$D/happyview-lua.wasm"
//! HAPPYVIEW_LUA_PLUGIN=$D cargo test --test lua_interpreter_plugin
//! ```

mod common;

use serde_json::json;

use common::app::TestApp;
use common::lua_plugin::{identify, plugin_dir};
use happyview::plugin::{
    PluginType, ScriptErrorKind, ScriptExecuteContext, ScriptExecuteInput, ScriptExecuteLimits,
    ScriptExecuteOutput, ScriptKind, ScriptValueKind, loader,
};

/// What the plugin's manifest declares, and what its imports need: the two
/// bridge functions and the four `script:host` ones are `library:call` and
/// `script:host`, and the clock, randomness and `fd_write` are the three
/// WASI ones.
const CAPABILITIES: [&str; 5] = [
    "library:call",
    "script:host",
    "wasi:clock",
    "wasi:random",
    "wasi:stdio",
];

/// The raw-ABI library fixture, under a namespace `require` resolves the way
/// it resolves a real library.
const LIBRARY_NAMESPACE: &str = "happyview.testlib";
const LIBRARY_MODULE: &str =
    "tests/fixtures/test_library/target/wasm32-unknown-unknown/release/test_library.wasm";
const LIBRARY_BUILD: &str = "test_library fixture not built. Run: cargo build --manifest-path \
                             tests/fixtures/test_library/Cargo.toml --target \
                             wasm32-unknown-unknown --release";

fn library_fixture_built() -> bool {
    std::path::Path::new(LIBRARY_MODULE).exists()
}

fn library() -> happyview::plugin::LoadedPlugin {
    let manifest: happyview::plugin::PluginManifest = serde_json::from_value(json!({
        "id": LIBRARY_NAMESPACE, "name": LIBRARY_NAMESPACE, "version": "1.0.0",
        "api_version": "2", "plugin_type": "library", "namespace": LIBRARY_NAMESPACE,
        "capabilities": ["library:call", "database:read", "database:write"],
    }))
    .expect("the fixture manifest should parse");
    happyview::plugin::LoadedPlugin {
        info: manifest.clone().into(),
        source: happyview::plugin::PluginSource::File {
            path: "tests/fixtures/test_library".into(),
        },
        wasm_bytes: std::fs::read(LIBRARY_MODULE).expect(LIBRARY_BUILD),
        manifest: Some(manifest),
    }
}

/// The plugin registered under its own id, ready to run scripts.
///
/// The module it loaded is named in the output, because eight green tests
/// against an artefact nobody can identify afterwards is the claim this
/// whole round exists to stop being makeable.
async fn registered(app: &TestApp) -> Option<String> {
    let dir = plugin_dir()?;
    println!("module: {}", identify(&dir));
    let plugin = loader::load_from_file(&dir)
        .await
        .expect("the plugin should load through the loader");
    let id = plugin.info.id.clone();
    app.state.plugin_registry.register(plugin).await;
    Some(id)
}

fn input(source: &str, kind: ScriptKind) -> ScriptExecuteInput {
    ScriptExecuteInput {
        source: source.to_string(),
        kind,
        input: json!({ "q": "hi" }),
        context: ScriptExecuteContext {
            trigger: "xrpc.query:app.test.q".into(),
            caller_did: Some("did:plc:runner".into()),
            method: Some("app.test.q".into()),
            ..ScriptExecuteContext::default()
        },
        libraries: Vec::new(),
        limits: ScriptExecuteLimits {
            instructions: Some(1_000_000),
            memory_bytes: 64 * 1024 * 1024,
        },
        removed_globals: Vec::new(),
    }
}

fn failure(output: ScriptExecuteOutput) -> (ScriptErrorKind, String, Option<u32>) {
    match output {
        ScriptExecuteOutput::Error {
            kind,
            message,
            line,
            ..
        } => (kind, message, line),
        other => panic!("expected a failure, got {other:?}"),
    }
}

/// Loading is where the manifest and the module's import section are checked
/// against each other, so this is what says the five declared capabilities
/// are the ones the compiled module needs.
#[tokio::test]
async fn the_plugin_loads_with_the_capabilities_its_imports_need() {
    let Some(dir) = plugin_dir() else { return };
    let plugin = loader::load_from_file(&dir)
        .await
        .expect("the plugin should load through the loader");

    assert_eq!(plugin.info.id, "happyview-lua");
    let manifest = plugin.manifest.as_ref().expect("a manifest");
    assert_eq!(manifest.plugin_type, PluginType::Interpreter);
    assert_eq!(plugin.language_id(), Some("lua"));
    assert_eq!(manifest.supports_libraries, vec!["*".to_string()]);
    assert!(
        manifest.dependencies.is_empty(),
        "an interpreter bridges whatever is installed and depends on nothing"
    );

    let mut declared: Vec<String> = manifest
        .capabilities
        .iter()
        .map(|c| c.as_str().to_string())
        .collect();
    declared.sort();
    let mut expected: Vec<String> = CAPABILITIES.iter().map(|c| c.to_string()).collect();
    expected.sort();
    assert_eq!(declared, expected);

    // Each of the five is load-bearing: drop one and the module's own imports
    // no longer have a grant covering them.
    for dropped in CAPABILITIES {
        let mut narrowed = manifest.clone();
        narrowed.capabilities.retain(|c| c.as_str() != dropped);
        let refused = loader::validate_capabilities(&narrowed, &plugin.wasm_bytes)
            .expect_err(&format!("the module should need {dropped}"));
        assert!(
            refused.to_string().contains(dropped),
            "{dropped}: {refused}"
        );
    }
}

#[tokio::test]
async fn execute_script_runs_a_script_and_answers_what_it_returned() {
    common::require_db!();
    let app = TestApp::new().await;
    let Some(id) = registered(&app).await else {
        return;
    };

    let output = app
        .state
        .plugin_executor()
        .execute_script(
            &id,
            &input(
                "function handle(input, ctx) \
                 return { echoed = input.q, trigger = ctx.trigger } end",
                ScriptKind::XrpcQuery,
            ),
        )
        .await
        .expect("execute should answer");

    assert_eq!(
        output,
        ScriptExecuteOutput::Returned {
            value: json!({ "echoed": "hi", "trigger": "xrpc.query:app.test.q" }),
            value_kind: ScriptValueKind::Object,
        }
    );
}

/// Every built-in is the plugin's own, so this is the whole `require` path
/// short of a library: resolution, caching and the JSON rules.
#[tokio::test]
async fn a_script_reaches_the_built_ins_and_the_os_subset() {
    common::require_db!();
    let app = TestApp::new().await;
    let Some(id) = registered(&app).await else {
        return;
    };

    let output = app
        .state
        .plugin_executor()
        .execute_script(
            &id,
            &input(
                r#"local json = require("internal.json")
                   local tids = require("internal.tids")
                   local time = require("internal.time")
                   function handle()
                     return {
                       encoded = json.encode({ a = 1 }),
                       empty = json.encode(json.to_array({})),
                       tid = #tids.create(),
                       stamp = time.to_iso8601(0),
                       day = os.date("!%Y-%m-%d", 0),
                       no_clock = os.clock == nil,
                     }
                   end"#,
                ScriptKind::XrpcQuery,
            ),
        )
        .await
        .expect("execute should answer");

    assert_eq!(
        output,
        ScriptExecuteOutput::Returned {
            value: json!({
                "encoded": r#"{"a":1}"#,
                "empty": "[]",
                "tid": 13,
                "stamp": "1970-01-01T00:00:00.000Z",
                "day": "1970-01-01",
                "no_clock": true,
            }),
            value_kind: ScriptValueKind::Object,
        }
    );
}

/// The host sends its own guard list whatever the caller asked for, so this
/// pins the two halves together across the ABI.
#[tokio::test]
async fn a_removed_global_raises_the_migration_sentence() {
    common::require_db!();
    let app = TestApp::new().await;
    let Some(id) = registered(&app).await else {
        return;
    };

    let output = app
        .state
        .plugin_executor()
        .execute_script(
            &id,
            &input("function handle() return db.x end", ScriptKind::XrpcQuery),
        )
        .await
        .expect("execute should answer");

    let (kind, message, _) = failure(output);
    assert_eq!(kind, ScriptErrorKind::Runtime);
    assert!(
        message.contains("the 'db' global was removed in v3"),
        "{message}"
    );
    assert!(message.contains("run the script codemod"), "{message}");
}

#[tokio::test]
async fn a_runtime_error_comes_back_with_its_line() {
    common::require_db!();
    let app = TestApp::new().await;
    let Some(id) = registered(&app).await else {
        return;
    };

    let output = app
        .state
        .plugin_executor()
        .execute_script(
            &id,
            &input(
                "function handle()\n  local t = nil\n  return t.x\nend",
                ScriptKind::XrpcQuery,
            ),
        )
        .await
        .expect("execute should answer");

    let (kind, message, line) = failure(output);
    assert_eq!(kind, ScriptErrorKind::Runtime);
    assert_eq!(line, Some(3), "{message}");
    assert_eq!(message, "attempt to index a nil value (local 't')");
}

#[tokio::test]
async fn validate_script_answers_both_shapes() {
    common::require_db!();
    let app = TestApp::new().await;
    let Some(id) = registered(&app).await else {
        return;
    };
    let executor = app.state.plugin_executor();

    let valid = executor
        .validate_script(
            &id,
            r#"local db = require("happyview.db")
               function handle(input, ctx)
                 return db.records("app.test.rec"):limit(input.limit):run()
               end"#,
        )
        .await
        .expect("validate should answer");
    assert!(valid.valid, "{:?}", valid.errors);

    for (source, kind) in [
        ("function handle(", ScriptErrorKind::Syntax),
        ("local x = 1", ScriptErrorKind::MissingHandle),
    ] {
        let refused = executor
            .validate_script(&id, source)
            .await
            .expect("validate should answer");
        assert!(!refused.valid, "{source}");
        assert_eq!(refused.errors[0].kind, kind, "{source}");
    }

    // The editor's check reads the host's guard list too, so a name every run
    // refuses cannot be saved as valid.
    let refused = executor
        .validate_script(&id, "local base = env\nfunction handle() return base end")
        .await
        .expect("validate should answer");
    assert!(!refused.valid);
    assert!(
        refused.errors[0].message.contains("was removed in v3"),
        "{:?}",
        refused.errors[0]
    );
}

/// Every Lua sample in `plugins/happyview-lua/README.md`, so documentation
/// that stopped being true fails here rather than in someone's editor. The
/// two that reach a library are validated, which is as far as they go without
/// one installed; the one that reaches only built-ins is run.
#[tokio::test]
async fn the_readme_samples_still_work() {
    common::require_db!();
    let app = TestApp::new().await;
    let Some(id) = registered(&app).await else {
        return;
    };
    let executor = app.state.plugin_executor();

    let contract = r#"local db = require("happyview.db")
local log = require("internal.logging")

function handle(input, ctx)
  log.info("looking up", { q = input.q, who = ctx.caller_did })
  return db.records("app.example.post"):limit(input.limit or 10):run()
end"#;
    let chain = r#"local db = require("happyview.db")
function handle(input, ctx)
  return db.records("app.example.post"):where("author", ctx.caller_did):limit(5):run()
end"#;
    for sample in [contract, chain] {
        let validated = executor
            .validate_script(&id, sample)
            .await
            .expect("validate should answer");
        assert!(validated.valid, "{sample}\n{:?}", validated.errors);
    }

    let builtins = r#"local json = require("internal.json")
local time = require("internal.time")
local tids = require("internal.tids")
local log  = require("internal.logging")

function handle(input, ctx)
  log.warn("careful", { uri = input.uri })
  return {
    rkey = tids.create(),
    at = time.to_iso8601(time.now()),
    body = json.encode(json.to_array({})),
  }
end"#;
    let output = executor
        .execute_script(&id, &input(builtins, ScriptKind::XrpcQuery))
        .await
        .expect("execute should answer");
    match output {
        ScriptExecuteOutput::Returned { value, .. } => {
            assert_eq!(value["rkey"].as_str().map(str::len), Some(13), "{value}");
            assert!(
                value["at"].as_str().is_some_and(|at| at.ends_with('Z')),
                "{value}"
            );
            assert_eq!(value["body"], "[]");
        }
        other => panic!("the built-ins sample should return a table, got {other:?}"),
    }
}

/// The Lua hook ends a loop the script can see; the epoch deadline ends one it
/// cannot, and no Lua code can catch it. This is the deadline the host arms,
/// so the budget in the input is lifted to leave only the host's.
#[tokio::test]
async fn a_spinning_script_is_interrupted_by_the_host_deadline() {
    common::require_db!();
    let app = TestApp::new().await;
    let Some(id) = registered(&app).await else {
        return;
    };

    let mut sent = input(
        "function handle() while true do end end",
        ScriptKind::XrpcQuery,
    );
    sent.limits.instructions = None;

    let started = std::time::Instant::now();
    let outcome = app.state.plugin_executor().execute_script(&id, &sent).await;
    let elapsed = started.elapsed();

    match outcome {
        Err(happyview::plugin::ExecutionError::Timeout) => {}
        other => panic!("expected the deadline to interrupt the guest, got {other:?}"),
    }
    assert!(
        elapsed < std::time::Duration::from_secs(60),
        "interrupted after {elapsed:?}"
    );
}

/// The record-event runner's three-way branch reads `value_kind`, so the
/// interpreter's half of it is what makes the host's half mean anything:
/// `handle` returning nothing and returning `nil` are the same answer, and an
/// explicit null sentinel is not that answer.
#[tokio::test]
async fn the_value_kinds_a_record_event_branches_on() {
    common::require_db!();
    let app = TestApp::new().await;
    let Some(id) = registered(&app).await else {
        return;
    };

    for (source, expected) in [
        ("function handle() end", ScriptValueKind::None),
        ("function handle() return nil end", ScriptValueKind::None),
        (
            "function handle() return { a = 1 } end",
            ScriptValueKind::Object,
        ),
        ("function handle() return true end", ScriptValueKind::Other),
        (
            "function handle(input) return input.absent_key_is_nil end",
            ScriptValueKind::None,
        ),
    ] {
        let output = app
            .state
            .plugin_executor()
            .execute_script(&id, &input(source, ScriptKind::RecordEvent))
            .await
            .expect("execute should answer");
        match output {
            ScriptExecuteOutput::Returned { value_kind, .. } => {
                assert_eq!(value_kind, expected, "{source}")
            }
            other => panic!("{source}: {other:?}"),
        }
    }
}

/// A table handed to `os.time` is read as UTC, because the guest has no zone
/// to read. Two runs of one script on two machines therefore agree, which is
/// the property an indexer wants; a script that needs its server's zone adds
/// its own offset.
#[tokio::test]
async fn os_time_on_a_table_is_read_as_utc_whatever_zone_the_host_is_in() {
    common::require_db!();
    let app = TestApp::new().await;
    let Some(id) = registered(&app).await else {
        return;
    };

    let output = app
        .state
        .plugin_executor()
        .execute_script(
            &id,
            &input(
                "function handle() return { at = os.time({ year = 2026, month = 9, \
                 day = 17, hour = 12, min = 30, sec = 15 }) } end",
                ScriptKind::RecordEvent,
            ),
        )
        .await
        .expect("execute should answer");

    let civil = (2026, 9, 17, 12, 30, 15);
    let as_utc = chrono::TimeZone::with_ymd_and_hms(
        &chrono::Utc,
        civil.0,
        civil.1,
        civil.2,
        civil.3,
        civil.4,
        civil.5,
    )
    .unwrap()
    .timestamp();
    let as_local = chrono::TimeZone::with_ymd_and_hms(
        &chrono::Local,
        civil.0,
        civil.1,
        civil.2,
        civil.3,
        civil.4,
        civil.5,
    )
    .unwrap()
    .timestamp();
    println!("host zone offset from UTC: {} seconds", as_utc - as_local);

    match output {
        ScriptExecuteOutput::Returned { value, .. } => assert_eq!(value, json!({ "at": as_utc })),
        other => panic!("expected an instant, got {other:?}"),
    }
}

/// A library call from inside a coroutine answers its value: every host import
/// is a plain blocking call in the guest, where an async one would have to
/// yield through a coroutine that is not the host's to resume.
#[tokio::test]
async fn a_library_call_from_inside_a_coroutine_answers_its_value() {
    common::require_db!();
    if !library_fixture_built() {
        eprintln!("skipping: {LIBRARY_BUILD}");
        return;
    }
    let app = TestApp::new().await;
    let Some(id) = registered(&app).await else {
        return;
    };
    app.state.plugin_registry.register(library()).await;

    let mut sent = input(
        r#"local lib = require("happyview.testlib")
           function handle()
             local gen = coroutine.wrap(function() coroutine.yield(lib.add(2, 3)) end)
             return { sum = gen() }
           end"#,
        ScriptKind::RecordEvent,
    );
    sent.libraries = vec![happyview::plugin::ScriptLibraryRef {
        namespace: LIBRARY_NAMESPACE.into(),
        id: LIBRARY_NAMESPACE.into(),
    }];

    let output = app
        .state
        .plugin_executor()
        .execute_script(&id, &sent)
        .await
        .expect("execute should answer");

    assert_eq!(
        output,
        ScriptExecuteOutput::Returned {
            value: json!({ "sum": 5.0 }),
            value_kind: ScriptValueKind::Object,
        }
    );
}

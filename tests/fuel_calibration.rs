//! How much fuel a compiled script module needs for the work a Lua script
//! does inside one instruction budget, measured on three realistic handlers
//! written step for step in both languages.
//!
//! For each workload it finds, by binary search, the smallest
//! `limits.instructions` at which the Lua script returns and the smallest
//! fuel at which the module returns, and prints their ratio. The module's
//! figure is searched in units of `FUEL_PER_INSTRUCTION` and multiplied back,
//! so it is raw fuel whatever the constant is set to. Both counts are
//! deterministic, so three runs of one input must agree exactly.
//!
//! ```sh
//! cargo build --manifest-path tests/fixtures/script_module/Cargo.toml --target wasm32-wasip1 --release
//! HAPPYVIEW_LUA_PLUGIN=tests/fixtures/lua-plugin \
//!   cargo test --test fuel_calibration -- --ignored --nocapture
//! ```

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Value, json};

use common::lua_plugin::{identify, plugin_dir};
use happyview::config::project::ModuleDecl;
use happyview::lexicon::{ParsedLexicon, ProcedureAction};
use happyview::lua::limits::DEFAULT_INSTRUCTION_LIMIT;
use happyview::plugin::capabilities::PluginCapability;
use happyview::plugin::{
    ExecutionError, FUEL_PER_INSTRUCTION, ScriptErrorKind, ScriptExecuteContext,
    ScriptExecuteInput, ScriptExecuteLimits, ScriptExecuteOutput, ScriptKind, loader,
};
use happyview::project::script_module::{ScriptModule, load_modules};
use happyview::project::snapshot::ProjectLexicon;
use happyview::test_support::{migrated_memory_pool, test_state_with_pool};

const MODULE: &str = "tests/fixtures/script_module/target/wasm32-wasip1/release/script_module.wasm";

macro_rules! require_module {
    () => {
        if !std::path::Path::new(MODULE).exists() {
            eprintln!(
                "skipping: fixture not built. Run: cargo build --manifest-path \
                 tests/fixtures/script_module/Cargo.toml --target wasm32-wasip1 --release"
            );
            return;
        }
    };
}

/// The handlers this target runs, each beside its Lua twin.
const WORKLOADS: [(&str, &str); 3] = [
    (
        "app.test.calibration.shape",
        include_str!("fixture_scripts/calibration/shape.lua"),
    ),
    (
        "app.test.calibration.aggregate",
        include_str!("fixture_scripts/calibration/aggregate.lua"),
    ),
    (
        "app.test.calibration.render",
        include_str!("fixture_scripts/calibration/render.lua"),
    ),
];

/// The fixed per-run cost, apart from any per-step work.
const ECHO: (&str, &str) = ("app.test.echo", "function handle(input) return input end");

const SIZES: [usize; 2] = [50, 200];
const REPEATS: usize = 3;
const SEARCH_CEILING: u32 = 50_000_000;

/// A module given this fraction of a workload's Lua budget must not finish,
/// so the constant cannot drift to a value that makes the budget meaningless.
const STARVED_DIVISOR: u32 = 5;

fn lexicon(id: &str, kind: &str) -> (String, ProjectLexicon) {
    let main = match kind {
        "record" => {
            json!({"type": "record", "key": "tid", "record": {"type": "object", "properties": {}}})
        }
        other => json!({"type": other}),
    };
    let parsed = ParsedLexicon::parse(
        json!({"lexicon": 1, "id": id, "defs": {"main": main}}),
        1,
        None,
        ProcedureAction::Upsert,
        None,
    )
    .unwrap();
    (
        id.into(),
        ProjectLexicon {
            parsed,
            path: format!("src/lexicons/{id}.json").into(),
            backfill: false,
        },
    )
}

/// The fixture loaded the way a snapshot build loads it.
fn module(state: &happyview::AppState) -> Arc<ScriptModule> {
    let dir = tempfile::tempdir().unwrap();
    let build = dir.path().join(".happyview/build");
    std::fs::create_dir_all(&build).unwrap();
    std::fs::copy(MODULE, build.join("fixture.wasm")).unwrap();
    let decls = BTreeMap::from([(
        "fixture".to_string(),
        ModuleDecl {
            language: "rust".into(),
            path: "tests/fixtures/script_module".into(),
            capabilities: vec![PluginCapability::LibraryCall, PluginCapability::WasiStdio],
        },
    )]);
    let lexicons = [
        lexicon("app.test.echo", "query"),
        lexicon("app.test.pair", "query"),
        lexicon("app.test.boom", "query"),
        lexicon("app.test.spin", "query"),
        lexicon("app.test.thing", "record"),
        lexicon("app.test.calibration.shape", "query"),
        lexicon("app.test.calibration.aggregate", "query"),
        lexicon("app.test.calibration.render", "query"),
    ]
    .into_iter()
    .collect();
    let loaded = load_modules(
        &state.wasm_runtime,
        dir.path(),
        &decls,
        &lexicons,
        &BTreeMap::new(),
    )
    .unwrap_or_else(|d| panic!("{d:?}"));
    loaded.modules.into_values().next().unwrap()
}

fn input(trigger: &str, source: &str, value: Value, instructions: u32) -> ScriptExecuteInput {
    ScriptExecuteInput {
        source: source.into(),
        kind: ScriptKind::XrpcQuery,
        input: value,
        context: ScriptExecuteContext {
            trigger: trigger.into(),
            caller_did: Some("did:plc:caller".into()),
            ..ScriptExecuteContext::default()
        },
        libraries: Vec::new(),
        limits: ScriptExecuteLimits {
            instructions: Some(instructions),
            memory_bytes: 64 * 1024 * 1024,
        },
        removed_globals: Vec::new(),
        chunk_name: happyview::plugin::DEFAULT_CHUNK_NAME.into(),
        artifact: None,
    }
}

async fn state() -> happyview::AppState {
    test_state_with_pool(migrated_memory_pool().await)
}

/// `n` post envelopes from a formula over the index, so every run measures
/// the same bytes. `createdAt` is unique and out of order, so the sort has
/// work to do and one answer.
fn records(n: usize) -> Value {
    const WORDS: [&str; 16] = [
        "the", "quick", "atproto", "lexicon", "record", "indexer", "brown", "fox", "jumps", "over",
        "a", "lazy", "relay", "firehose", "appview", "space",
    ];
    const TAGS: [&str; 20] = [
        "rust", "lua", "wasm", "atproto", "bsky", "dev", "art", "music", "news", "photo", "books",
        "film", "food", "games", "code", "travel", "science", "sport", "design", "meta",
    ];
    let records: Vec<Value> = (0..n)
        .map(|i| {
            let order = (i * 7919) % n;
            let seconds = order * 37;
            let created_at = format!(
                "2026-01-01T{:02}:{:02}:{:02}.000Z",
                seconds / 3600,
                (seconds / 60) % 60,
                seconds % 60
            );
            let len = 80 + (i * 53) % 121;
            let mut text = String::new();
            let mut w = i;
            while text.len() < len {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(WORDS[w % WORDS.len()]);
                w = (w * 31 + 7) % 1009;
            }
            text.truncate(len);
            let tags: Vec<&str> = (0..3)
                .map(|k| TAGS[(i * i + k * 7 + i / 3) % TAGS.len()])
                .collect();
            let rkey = format!("3k{i:011}");
            let did = format!("did:plc:author{:04}", i % 17);
            json!({
                "uri": format!("at://{did}/app.test.post/{rkey}"),
                "did": did,
                "collection": "app.test.post",
                "rkey": rkey,
                "cid": format!("bafyreicalibration{i:06}"),
                "indexed_at": created_at,
                "record": {
                    "text": text,
                    "createdAt": created_at,
                    "tags": tags,
                    "likeCount": (i * 13) % 11,
                },
            })
        })
        .collect();
    json!({ "records": records })
}

/// Lua answers integers as floats in places; the comparison is between
/// values, not encodings.
fn normalized(value: &Value) -> Value {
    match value {
        Value::Number(n) => json!(n.as_f64().unwrap()),
        Value::Array(items) => Value::Array(items.iter().map(normalized).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), normalized(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

enum Outcome {
    Returned(Value),
    Timeout,
}

fn outcome(result: Result<ScriptExecuteOutput, ExecutionError>) -> Outcome {
    match result {
        Ok(ScriptExecuteOutput::Returned { value, .. }) => Outcome::Returned(value),
        Ok(ScriptExecuteOutput::Error {
            kind: ScriptErrorKind::Timeout,
            ..
        })
        | Err(ExecutionError::Timeout) => Outcome::Timeout,
        // A Lua budget smaller than the plugin's own set-up runs out before
        // `handle` is reached, and the plugin answers that as an error of its
        // own rather than as a timeout.
        Err(ExecutionError::PluginError { message, .. })
            if message.contains("exceeded execution limit") =>
        {
            Outcome::Timeout
        }
        other => panic!("expected a value or a timeout, got {other:?}"),
    }
}

struct Sides {
    state: happyview::AppState,
    lua: String,
    module: Arc<ScriptModule>,
}

impl Sides {
    async fn lua(&self, trigger: &str, source: &str, value: &Value, budget: u32) -> Outcome {
        let sent = input(trigger, source, value.clone(), budget);
        outcome(
            self.state
                .plugin_executor()
                .execute_script(&self.lua, &sent)
                .await,
        )
    }

    async fn module(&self, trigger: &str, value: &Value, budget: u32) -> Outcome {
        let sent = input(trigger, "", value.clone(), budget);
        outcome(
            self.state
                .plugin_executor()
                .execute_compiled_as(
                    self.module.plugin.clone(),
                    &self.module.source_path,
                    &sent,
                    None,
                )
                .await,
        )
    }
}

/// The smallest budget in `1..=SEARCH_CEILING` at which `run` returns.
async fn smallest<F, Fut>(run: F) -> u32
where
    F: Fn(u32) -> Fut,
    Fut: std::future::Future<Output = Outcome>,
{
    assert!(
        matches!(run(SEARCH_CEILING).await, Outcome::Returned(_)),
        "the workload does not finish within {SEARCH_CEILING}"
    );
    let (mut lo, mut hi) = (1u32, SEARCH_CEILING);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        match run(mid).await {
            Outcome::Returned(_) => hi = mid,
            Outcome::Timeout => lo = mid + 1,
        }
    }
    lo
}

struct Row {
    workload: &'static str,
    size: usize,
    trigger: String,
    value: Value,
    lua_min: Vec<u32>,
    module_fuel_min: Vec<u64>,
}

impl Row {
    fn ratio(&self) -> f64 {
        self.module_fuel_min[0] as f64 / self.lua_min[0] as f64
    }
}

async fn measure(
    sides: &Sides,
    workload: &'static str,
    trigger: &str,
    source: &str,
    value: &Value,
    size: usize,
) -> Row {
    let mut row = Row {
        workload,
        size,
        trigger: trigger.into(),
        value: value.clone(),
        lua_min: Vec::new(),
        module_fuel_min: Vec::new(),
    };
    for _ in 0..REPEATS {
        row.lua_min
            .push(smallest(|b| sides.lua(trigger, source, value, b)).await);
        let units = smallest(|b| sides.module(trigger, value, b)).await;
        row.module_fuel_min
            .push(u64::from(units) * FUEL_PER_INSTRUCTION);
    }
    row
}

/// Asserts both sides answer `value` alike within the default budget, and
/// answers the trigger they ran under.
async fn same_answer(
    sides: &Sides,
    nsid: &str,
    source: &str,
    value: &Value,
    size: usize,
) -> String {
    let trigger = format!("xrpc.query:{nsid}");
    let budget = DEFAULT_INSTRUCTION_LIMIT;
    let (Outcome::Returned(from_lua), Outcome::Returned(from_module)) = (
        sides.lua(&trigger, source, value, budget).await,
        sides.module(&trigger, value, budget).await,
    ) else {
        panic!("{nsid} at {size} records does not finish within the default budget");
    };
    assert_eq!(
        normalized(&from_lua),
        normalized(&from_module),
        "{nsid} at {size} records"
    );
    trigger
}

#[tokio::test]
#[ignore = "measurement, run by hand"]
async fn a_lua_scripts_budget_is_enough_for_its_module_twin_and_a_fifth_is_not() {
    require_module!();
    // `plugin_dir` says why it skipped.
    let Some(dir) = plugin_dir() else { return };
    println!("lua plugin: {}", identify(&dir));
    println!("module: {MODULE}");
    println!(
        "host test binary: {}",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );

    let state = state().await;
    let plugin = loader::load_from_file(&dir)
        .await
        .expect("the plugin should load through the loader");
    let lua = plugin.info.id.clone();
    state.plugin_registry.register(plugin).await;
    let module = module(&state);
    let sides = Sides { state, lua, module };

    let mut rows = Vec::new();
    // Equivalent work first: both sides answer the same value, and both
    // answer it within the default budget. No records at all is checked too,
    // since that is where an empty list and an absent cursor could differ.
    for (nsid, source) in WORKLOADS {
        same_answer(&sides, nsid, source, &records(0), 0).await;
    }
    for size in SIZES {
        let value = records(size);
        for (nsid, source) in WORKLOADS {
            let trigger = same_answer(&sides, nsid, source, &value, size).await;
            let workload = nsid.rsplit('.').next().unwrap();
            rows.push(measure(&sides, workload, &trigger, source, &value, size).await);
        }
    }
    let echo_trigger = format!("xrpc.query:{}", ECHO.0);
    let echo = measure(
        &sides,
        "echo",
        &echo_trigger,
        ECHO.1,
        &json!({"q": "hi"}),
        0,
    )
    .await;

    println!();
    println!(
        "{:<10} {:>7} {:>28} {:>34} {:>8}",
        "workload", "records", "lua_min (3 runs)", "module_fuel_min (3 runs)", "ratio"
    );
    for row in rows.iter().chain([&echo]) {
        println!(
            "{:<10} {:>7} {:>28} {:>34} {:>8.1}",
            row.workload,
            row.size,
            format!("{:?}", row.lua_min),
            format!("{:?}", row.module_fuel_min),
            row.ratio()
        );
    }
    println!("FUEL_PER_INSTRUCTION = {FUEL_PER_INSTRUCTION}");

    for row in rows.iter().chain([&echo]) {
        assert!(
            row.lua_min.iter().all(|m| *m == row.lua_min[0])
                && row
                    .module_fuel_min
                    .iter()
                    .all(|m| *m == row.module_fuel_min[0]),
            "{} at {}: three runs of one input disagree",
            row.workload,
            row.size
        );
    }
    // The budget a workload needs as a Lua script is enough for it as a
    // module, and a fifth of that budget is not.
    for row in rows.iter().chain([&echo]) {
        let lua_min = row.lua_min[0];
        assert!(
            matches!(
                sides.module(&row.trigger, &row.value, lua_min).await,
                Outcome::Returned(_)
            ),
            "{} at {} records does not finish as a module under the {lua_min} instructions \
             it needs as a Lua script",
            row.workload,
            row.size
        );
        let starved = lua_min / STARVED_DIVISOR;
        assert!(
            matches!(
                sides.module(&row.trigger, &row.value, starved).await,
                Outcome::Timeout
            ),
            "{} at {} records still finishes as a module under {starved} instructions, a \
             fifth of what it needs as a Lua script",
            row.workload,
            row.size
        );
    }
    // A trivial handler fits the setting's floor with wide headroom.
    let floor = *happyview::lua::limits::INSTRUCTION_LIMIT_RANGE.start();
    let echo_needs = echo.module_fuel_min[0].div_ceil(FUEL_PER_INSTRUCTION);
    assert!(
        echo_needs * 10 <= u64::from(floor),
        "echo needs {echo_needs} instructions as a module, too close to the {floor} floor"
    );
}

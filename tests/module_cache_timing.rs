//! What the module cache is worth, in milliseconds, measured on a real
//! plugin rather than a toy one.
//!
//! `#[ignore]`d, like `fuel_calibration`, because these are wall-clock
//! figures: they belong to one machine on one day and a CI runner under load
//! would report something else entirely while nothing was wrong. The
//! *mechanism* they are evidence for is asserted deterministically in
//! `src/plugin/runtime.rs` — `a_second_lookup_of_the_same_plugin_does_not_recompile`
//! counts one compile across two lookups, and
//! `a_disk_entry_written_by_one_runtime_is_used_by_a_fresh_one` counts none
//! in a runtime that finds the cache warm. Re-run this after a wasmtime or
//! rustc upgrade.
//!
//! **What it measures, exactly:** three outcomes of `module_for` — a compile,
//! a deserialize from the disk tier, and an in-memory hit — on the published
//! Lua interpreter, which at about 1.1 MB is the largest module anything
//! here loads. It does **not** measure instantiation, so its in-memory
//! figure is not the "instantiate against a cached module" number recorded
//! in CLAUDE.md; that one is a different operation on the small test
//! fixtures. Compare runs of this against each other, not against those.
//!
//! The in-memory tier is shared per process, so the disk figure cannot be
//! measured in the same run that compiled the module — a "fresh" runtime
//! would find it in memory. Give it a directory that survives the run and
//! call it twice:
//!
//! ```sh
//! bash scripts/fetch-lua-plugin.sh
//! export HAPPYVIEW_LUA_PLUGIN=tests/fixtures/lua-plugin
//! export HAPPYVIEW_MODULE_CACHE_BENCH_DIR=/tmp/hv-module-cache-bench
//! rm -rf "$HAPPYVIEW_MODULE_CACHE_BENCH_DIR"
//! # first run: compiles, and reports the compile figure
//! cargo test --test module_cache_timing -- --ignored --nocapture
//! # second run: finds the directory warm, and reports the disk figure
//! cargo test --test module_cache_timing -- --ignored --nocapture
//! ```
//!
//! Without that variable it runs in a temp directory, which measures the
//! compile and the in-memory hit and says that the disk figure was skipped.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use common::lua_plugin::{identify, plugin_dir};
use happyview::plugin::WasmRuntime;
use happyview::plugin::loader;

/// Milliseconds, to one decimal: the figures are tenths of a millisecond at
/// the fast end and hundreds at the slow one.
fn ms(d: std::time::Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    // The in-memory hit is a fraction of a millisecond, so one decimal
    // prints it as 0.0 and says nothing.
    if ms < 1.0 {
        format!("{ms:.3} ms")
    } else {
        format!("{ms:.1} ms")
    }
}

#[tokio::test]
#[ignore = "wall-clock figures; needs HAPPYVIEW_LUA_PLUGIN"]
async fn module_cache_timings() {
    let Some(dir) = plugin_dir() else { return };
    let plugin = Arc::new(
        loader::load_from_file(&dir)
            .await
            .unwrap_or_else(|e| panic!("load the lua plugin: {e}")),
    );

    let (cache, persistent) = match std::env::var_os("HAPPYVIEW_MODULE_CACHE_BENCH_DIR") {
        Some(dir) => (PathBuf::from(dir), true),
        None => (
            std::env::temp_dir().join(format!("hv-module-cache-{}", std::process::id())),
            false,
        ),
    };
    // Deliberately not created here: `prepare_cache_dir` makes it 0700, and
    // refuses a directory carrying any group or other bit — which is what
    // `create_dir_all` would leave behind, quietly turning this into a
    // memory-only run that recompiles every time.

    let runtime = WasmRuntime::with_cache_dir(Some(cache.clone())).expect("a runtime");

    // First lookup: a compile, or a deserialize when the directory is warm.
    let started = Instant::now();
    let first = runtime
        .module_for_blocking(&plugin)
        .unwrap_or_else(|e| panic!("first lookup: {e}"));
    let cold = started.elapsed();
    let compiled = runtime.compile_count() > 0;

    // Second lookup: the in-memory tier, which is what a hot call pays.
    let started = Instant::now();
    let second = runtime
        .module_for_blocking(&plugin)
        .unwrap_or_else(|e| panic!("second lookup: {e}"));
    let warm = started.elapsed();

    // The floor under the figures: whatever the clock said, the second
    // lookup must not have compiled anything.
    let after = runtime.compile_count();
    assert_eq!(
        after,
        u64::from(compiled),
        "the second lookup compiled again"
    );
    drop((first, second));

    println!("\nmodule cache, {}", identify(&dir));
    println!("  cache dir:        {}", cache.display());
    if compiled {
        println!("  compile:          {}", ms(cold));
        println!("  in-memory hit:    {}", ms(warm));
        if persistent {
            println!(
                "  disk load:        not measured in this run — the module is in \
                 this process's memory now. Run the same command again to measure it."
            );
        } else {
            println!(
                "  disk load:        skipped — set HAPPYVIEW_MODULE_CACHE_BENCH_DIR \
                 to a directory that survives the run, then run twice."
            );
        }
    } else {
        println!("  disk load:        {} (compiled nothing)", ms(cold));
        println!("  in-memory hit:    {}", ms(warm));
    }
    println!();

    if !persistent {
        let _ = std::fs::remove_dir_all(&cache);
    }
}

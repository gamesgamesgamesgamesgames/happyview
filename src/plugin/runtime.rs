use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use wasmtime::*;

use crate::plugin::{LoadedPlugin, PluginType};

/// Default fuel for plugin execution (≈100ms CPU time). It also has to
/// cover instantiation: on a host without a copy-on-write memory image
/// (macOS; Linux maps one through memfd) wasmtime charges one fuel per byte
/// of initialized data copied in, so a module's data image is bounded by
/// this constant there, and instantiation fails outright rather than
/// timing out when it is not.
pub const DEFAULT_FUEL: u64 = 10_000_000;

/// The fuel a store is given at instantiation and before each call.
pub fn fuel_for(plugin_type: PluginType) -> u64 {
    match plugin_type {
        PluginType::Library | PluginType::Auth => DEFAULT_FUEL,
        // Fuel cannot bound an interpreter: wasmtime 49's Cranelift does not
        // reload the fuel counter in a frame that catches a `throw`, so a
        // guest that catches an error erases the fuel its body spent, and
        // every Lua error on the wasi route is such a throw (reproduction:
        // `.superpowers/spikes/lua-wasm/candidates/eh-probe/fuel_loss.wat`).
        // Epoch interruption is the boundary that holds, which is why the
        // engine keeps exceptions enabled: `executor::deadline`'s catching
        // guest test shows the deadline stopping a `try_table` loop that
        // fuel does not. Fuel stays enabled engine-wide because it is what
        // bounds libraries. The whole range also covers the per-byte
        // instantiation charge a megabytes-sized interpreter image draws on
        // a host without copy-on-write memory.
        PluginType::Interpreter => u64::MAX,
    }
}

/// How often the epoch advances. Against a budget whose floor is one second
/// this is a one-percent granularity, for a few hundred atomic increments a
/// second.
pub const EPOCH_TICK: Duration = Duration::from_millis(10);

/// A deadline no ticker reaches: at one tick per 10 ms it lies billions of
/// years out. Half the range rather than all of it because wasmtime adds
/// the delta to the current epoch, and that sum must not overflow.
pub const UNBOUNDED_TICKS: u64 = u64::MAX / 2;

/// WASM runtime for executing plugins
pub struct WasmRuntime {
    engine: Engine,
    /// Mirrors the engine's epoch tick for tick. Wasmtime exposes neither
    /// the current epoch nor a store's remaining deadline, and the guest-only
    /// accounting in [`GuestDeadline`] needs both.
    epoch: Arc<AtomicU64>,
    ticker_stop: Arc<AtomicBool>,
    modules: ModuleCache,
}

impl WasmRuntime {
    pub fn new() -> Result<Self, anyhow::Error> {
        Self::with_cache_dir(None)
    }

    /// A runtime whose compiled modules are also kept on disk under `dir`.
    /// Without one, a module compiled in this process outlives only this
    /// process.
    pub fn with_cache_dir(dir: Option<PathBuf>) -> Result<Self, anyhow::Error> {
        let mut runtime = Self::without_ticker()?;
        runtime.modules.dir = dir.and_then(|dir| match prepare_cache_dir(&dir) {
            Ok(()) => Some(dir),
            Err(reason) => {
                runtime.modules.warned.store(true, Ordering::Relaxed);
                tracing::warn!(
                    dir = %dir.display(),
                    "{reason}; compiled plugin modules are cached in memory only"
                );
                None
            }
        });
        let engine = runtime.engine.clone();
        let epoch = runtime.epoch.clone();
        let stop = runtime.ticker_stop.clone();
        std::thread::Builder::new()
            .name("wasm-epoch".into())
            .spawn(move || {
                // Scheduled against absolute instants so sleep overhead does
                // not accumulate into a slow clock; a stalled thread catches
                // up with rapid ticks, which is what wall time means.
                let mut next = Instant::now() + EPOCH_TICK;
                while !stop.load(Ordering::Relaxed) {
                    let now = Instant::now();
                    if next > now {
                        std::thread::sleep(next - now);
                    }
                    next += EPOCH_TICK;
                    tick(&engine, &epoch);
                }
            })?;
        Ok(runtime)
    }

    /// A runtime whose epoch advances only through [`WasmRuntime::tick`],
    /// for tests that need the clock under their own control.
    pub fn without_ticker() -> Result<Self, anyhow::Error> {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.epoch_interruption(true);

        let engine = Engine::new(&config)?;

        Ok(Self {
            engine,
            epoch: Arc::new(AtomicU64::new(0)),
            ticker_stop: Arc::new(AtomicBool::new(false)),
            modules: ModuleCache::default(),
        })
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// The tick counter every deadline built from this runtime reads.
    pub fn epoch(&self) -> Arc<AtomicU64> {
        self.epoch.clone()
    }

    /// Advance the engine's epoch and the mirror by one.
    pub fn tick(&self) {
        tick(&self.engine, &self.epoch);
    }

    /// Compile a WASM module
    pub fn compile(&self, wasm_bytes: &[u8]) -> Result<Module, anyhow::Error> {
        self.modules.compiles.fetch_add(1, Ordering::Relaxed);
        Ok(Module::new(&self.engine, wasm_bytes)?)
    }

    /// The compiled module for `plugin`, from memory, from disk, or by
    /// compiling. The three tiers are checked in that order and a miss at
    /// any tier fills the ones above it.
    pub fn module_for(&self, plugin: &Arc<LoadedPlugin>) -> Result<Module, anyhow::Error> {
        let id = plugin.info.id.as_str();
        if let Some(module) = self.modules.get_by_identity(id, plugin) {
            return Ok(module);
        }
        let sha = hex::encode(Sha256::digest(&plugin.wasm_bytes));
        if let Some(module) = self.modules.get_by_hash(id, plugin, &sha) {
            return Ok(module);
        }
        let key = self.cache_key(&sha);
        if let Some(module) = self.modules.load_from_disk(&self.engine, &key) {
            self.modules.insert(id, plugin, &sha, module.clone());
            return Ok(module);
        }
        let module = self.compile(&plugin.wasm_bytes)?;
        self.modules.write_to_disk(&key, &module);
        self.modules.insert(id, plugin, &sha, module.clone());
        Ok(module)
    }

    /// Drop the in-memory module held for `plugin_id`.
    pub fn evict(&self, plugin_id: &str) {
        self.modules.evict(plugin_id);
    }

    /// Compiles performed by this runtime, cache hits excluded.
    pub fn compile_count(&self) -> u64 {
        self.modules.compiles.load(Ordering::Relaxed)
    }

    /// The file stem a module's serialized form is kept under: the wasm
    /// bytes' hash and this engine's compatibility fingerprint, which
    /// covers the wasmtime version and the config. A module compiled by a
    /// different engine is not loadable by this one, and a stale file must
    /// read as a miss rather than fail a call.
    pub fn cache_key(&self, wasm_sha256: &str) -> String {
        let mut hasher = std::hash::DefaultHasher::new();
        self.engine
            .precompile_compatibility_hash()
            .hash(&mut hasher);
        format!("{wasm_sha256}-{:016x}", hasher.finish())
    }

    /// Delete every serialized module on disk that no installed plugin
    /// would load: the content-addressed files are the only ones this
    /// runtime writes, so anything not in `live_wasm_sha256s` under this
    /// engine's fingerprint is an orphan.
    pub fn sweep_cache_dir(&self, live_wasm_sha256s: &HashSet<String>) {
        let Some(dir) = &self.modules.dir else {
            return;
        };
        let live: HashSet<String> = live_wasm_sha256s
            .iter()
            .map(|sha| self.cache_key(sha))
            .collect();
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) => {
                self.modules.warn_once(dir, "read", &e);
                return;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(stem) = name
                .strip_suffix(".cwasm")
                .or_else(|| name.strip_suffix(".cwasm.sha256"))
            else {
                continue;
            };
            if live.contains(stem) {
                continue;
            }
            if let Err(e) = std::fs::remove_file(&path) {
                tracing::warn!(path = %path.display(), error = %e, "could not remove orphaned plugin cache file");
            } else {
                tracing::debug!(path = %path.display(), "removed orphaned plugin cache file");
            }
        }
    }
}

/// Create the cache directory for this process alone, or refuse one that
/// is not. The files in it feed `Module::deserialize`, which runs whatever
/// it is handed as native code, so a directory another user can write to
/// is a directory another user can run code through.
fn prepare_cache_dir(dir: &Path) -> Result<(), String> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|e| format!("could not create the plugin module cache directory: {e}"))?;
    let metadata = std::fs::metadata(dir)
        .map_err(|e| format!("could not read the plugin module cache directory: {e}"))?;
    if !metadata.is_dir() {
        return Err("the plugin module cache path is not a directory".into());
    }
    #[cfg(unix)]
    {
        // SAFETY: `geteuid` reads a process attribute and cannot fail.
        let euid = unsafe { libc::geteuid() };
        directory_is_private(&metadata, euid)?;
    }
    Ok(())
}

/// A directory is private when this user owns it and nobody else holds a
/// permission bit on it. The sidecar hash guards the files against
/// corruption; this guards them against an author.
#[cfg(unix)]
fn directory_is_private(metadata: &std::fs::Metadata, euid: u32) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    if metadata.uid() != euid {
        return Err(format!(
            "the plugin module cache directory is owned by uid {} rather than this process's uid {euid}",
            metadata.uid()
        ));
    }
    let mode = metadata.mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(format!(
            "the plugin module cache directory has mode {mode:o}; it must grant nothing to group or other"
        ));
    }
    Ok(())
}

struct CachedModule {
    /// The registration this module was compiled from. A live match is a
    /// hit without hashing the wasm bytes; a dead or different one falls
    /// through to the hash comparison.
    plugin: Weak<LoadedPlugin>,
    wasm_sha256: String,
    module: Module,
}

/// Compiled modules by plugin id in memory, and serialized ones by content
/// on disk. Everything on disk is best-effort: a directory that cannot be
/// created, read or written is one warning and a compile, never a failed
/// plugin call.
#[derive(Default)]
struct ModuleCache {
    entries: Mutex<HashMap<String, CachedModule>>,
    dir: Option<PathBuf>,
    compiles: AtomicU64,
    warned: AtomicBool,
}

impl ModuleCache {
    fn get_by_identity(&self, id: &str, plugin: &Arc<LoadedPlugin>) -> Option<Module> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.get(id)?;
        let live = entry.plugin.upgrade()?;
        Arc::ptr_eq(&live, plugin).then(|| entry.module.clone())
    }

    fn get_by_hash(&self, id: &str, plugin: &Arc<LoadedPlugin>, sha: &str) -> Option<Module> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.get_mut(id)?;
        if entry.wasm_sha256 != sha {
            return None;
        }
        entry.plugin = Arc::downgrade(plugin);
        Some(entry.module.clone())
    }

    fn insert(&self, id: &str, plugin: &Arc<LoadedPlugin>, sha: &str, module: Module) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                id.to_string(),
                CachedModule {
                    plugin: Arc::downgrade(plugin),
                    wasm_sha256: sha.to_string(),
                    module,
                },
            );
    }

    fn evict(&self, id: &str) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }

    fn paths(&self, key: &str) -> Option<(PathBuf, PathBuf)> {
        let dir = self.dir.as_ref()?;
        Some((
            dir.join(format!("{key}.cwasm")),
            dir.join(format!("{key}.cwasm.sha256")),
        ))
    }

    /// `Module::deserialize` trusts its bytes, so a file is loaded only when
    /// the sidecar hash written beside it still matches; a mismatch, a
    /// missing file or a deserialize error is a miss.
    fn load_from_disk(&self, engine: &Engine, key: &str) -> Option<Module> {
        let (module_path, sidecar_path) = self.paths(key)?;
        let bytes = std::fs::read(&module_path).ok()?;
        let expected = std::fs::read_to_string(&sidecar_path).ok()?;
        let actual = hex::encode(Sha256::digest(&bytes));
        if expected.trim() != actual {
            tracing::warn!(path = %module_path.display(), "plugin cache file does not match its recorded hash; recompiling");
            return None;
        }
        // SAFETY: the bytes were written by `write_to_disk` from
        // `Module::serialize` and their hash matches the sidecar that was
        // written with them, so they are what this process (or one with the
        // same engine fingerprint) produced. Wasmtime still verifies the
        // engine fingerprint embedded in the bytes.
        match unsafe { Module::deserialize(engine, &bytes) } {
            Ok(module) => Some(module),
            Err(e) => {
                tracing::warn!(path = %module_path.display(), error = %e, "plugin cache file could not be loaded; recompiling");
                None
            }
        }
    }

    fn write_to_disk(&self, key: &str, module: &Module) {
        let Some((module_path, sidecar_path)) = self.paths(key) else {
            return;
        };
        let Some(dir) = self.dir.as_ref() else {
            return;
        };
        let result = (|| -> std::io::Result<()> {
            prepare_cache_dir(dir).map_err(std::io::Error::other)?;
            let bytes = module
                .serialize()
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            let sha = hex::encode(Sha256::digest(&bytes));
            // Written beside the final name and renamed into place, so a
            // reader never sees a half-written module.
            let tmp = module_path.with_extension(format!("cwasm.{}.tmp", std::process::id()));
            std::fs::write(&tmp, &bytes)?;
            std::fs::rename(&tmp, &module_path)?;
            std::fs::write(&sidecar_path, sha)?;
            Ok(())
        })();
        if let Err(e) = result {
            self.warn_once(dir, "write", &e);
        }
    }

    fn warn_once(&self, dir: &Path, action: &str, error: &std::io::Error) {
        if !self.warned.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                dir = %dir.display(),
                error = %error,
                "could not {action} the plugin module cache; every plugin will be compiled on each use"
            );
        }
    }
}

fn tick(engine: &Engine, epoch: &AtomicU64) {
    engine.increment_epoch();
    epoch.fetch_add(1, Ordering::Relaxed);
}

impl Drop for WasmRuntime {
    fn drop(&mut self) {
        self.ticker_stop.store(true, Ordering::Relaxed);
    }
}

impl Default for WasmRuntime {
    fn default() -> Self {
        Self::new().expect("Failed to create WASM runtime")
    }
}

/// Linear memory a library or auth plugin may grow to. Roomy enough for a
/// response at `MAX_HTTP_RESPONSE_SIZE` and the guest's own copy of it,
/// and a quarter of the 4 GiB a 32-bit guest could otherwise reach.
pub const LIBRARY_MEMORY_CEILING: usize = 512 * 1024 * 1024;

/// The Lua-level allocation limit a script runs under until an operator
/// setting supplies one.
pub const DEFAULT_SCRIPT_MEMORY_BYTES: usize = 64 * 1024 * 1024;

/// The store ceiling for a script memory limit. Twice the Lua-level limit,
/// because the interpreter's own footprint and its allocator's overhead sit
/// on top of the bytes Lua counts (a 64 MiB Lua limit measured at 87 MiB of
/// linear memory). The Lua limit is the one that produces a message; this
/// is the backstop, reached only by something the Lua limit cannot see.
pub fn script_memory_ceiling(script_memory_bytes: usize) -> usize {
    script_memory_bytes.saturating_mul(2)
}

/// The ceiling a store starts with, by plugin type.
pub fn memory_ceiling(plugin_type: PluginType) -> usize {
    match plugin_type {
        PluginType::Library | PluginType::Auth => LIBRARY_MEMORY_CEILING,
        PluginType::Interpreter => script_memory_ceiling(DEFAULT_SCRIPT_MEMORY_BYTES),
    }
}

/// A store's memory ceiling, and whether it has refused a growth. A refusal
/// reaches the guest as `memory.grow` returning -1 and reaches the host, if
/// the guest then traps, as a trap with nothing on it naming the cause;
/// the flag is what tells "out of memory" from "the guest has a bug".
pub struct MemoryLimiter {
    limits: StoreLimits,
    ceiling: usize,
    refused: Option<usize>,
}

impl MemoryLimiter {
    pub fn new(ceiling: usize) -> Self {
        Self {
            // `memory_size` caps one linear memory, so the count is pinned
            // too; otherwise a module declaring several would multiply the
            // ceiling. One instance and one table is every plugin's shape.
            limits: StoreLimitsBuilder::new()
                .memory_size(ceiling)
                .memories(1)
                .tables(1)
                .instances(1)
                .build(),
            ceiling,
            refused: None,
        }
    }

    /// Forget an earlier refusal, so one classifies only the call it
    /// happened in: the guest sees a refused growth as `memory.grow`
    /// returning -1 and may carry on.
    pub fn clear_refusal(&mut self) {
        self.refused = None;
    }

    pub fn ceiling(&self) -> usize {
        self.ceiling
    }

    /// The size the guest asked for when a growth was refused.
    pub fn refused(&self) -> Option<usize> {
        self.refused
    }
}

impl ResourceLimiter for MemoryLimiter {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> Result<bool> {
        let allowed = self.limits.memory_growing(current, desired, maximum)?;
        if !allowed {
            self.refused = Some(desired);
        }
        Ok(allowed)
    }

    fn memory_grow_failed(&mut self, error: Error) -> Result<()> {
        self.limits.memory_grow_failed(error)
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> Result<bool> {
        self.limits.table_growing(current, desired, maximum)
    }

    fn table_grow_failed(&mut self, error: Error) -> Result<()> {
        self.limits.table_grow_failed(error)
    }

    fn instances(&self) -> usize {
        self.limits.instances()
    }

    fn tables(&self) -> usize {
        self.limits.tables()
    }

    fn memories(&self) -> usize {
        self.limits.memories()
    }
}

/// A store's budget of guest execution, in ticks, counting only the time
/// the guest is running. Entering a host import suspends the clock and
/// returning resumes it, so a script awaiting a PDS write or an HTTP fetch
/// is not charged for the host's latency. The methods keep the books; the
/// caller applies the returned delta with `set_epoch_deadline`, since the
/// store owns this value and cannot be borrowed through it.
pub struct GuestDeadline {
    epoch: Arc<AtomicU64>,
    remaining: u64,
    armed_at: u64,
    running: bool,
}

impl GuestDeadline {
    pub fn unbounded(epoch: Arc<AtomicU64>) -> Self {
        Self {
            epoch,
            remaining: UNBOUNDED_TICKS,
            armed_at: 0,
            running: false,
        }
    }

    /// The ticks a wall-clock budget spans. Rounded up, plus one: the epoch
    /// is partway through a tick whenever a budget is armed, so the first
    /// tick is short, and without the extra one a budget could end up to a
    /// tick before the seconds an operator typed.
    pub fn ticks_for(budget: Duration) -> u64 {
        budget.as_millis().div_ceil(EPOCH_TICK.as_millis()) as u64 + 1
    }

    fn now(&self) -> u64 {
        self.epoch.load(Ordering::Relaxed)
    }

    /// Charge the ticks the guest has run since the clock last started.
    fn settle(&mut self) {
        let now = self.now();
        if self.running {
            self.remaining = self.remaining.saturating_sub(now - self.armed_at);
        }
        self.armed_at = now;
    }

    /// Start a fresh budget. Returns the delta for `set_epoch_deadline`.
    pub fn arm(&mut self, ticks: u64) -> u64 {
        self.remaining = ticks;
        self.armed_at = self.now();
        self.running = true;
        ticks
    }

    /// The guest has entered a host import: spend what it ran and stop
    /// the clock.
    pub fn suspend(&mut self) {
        self.settle();
        self.running = false;
    }

    /// The guest is about to run again. Returns the delta for
    /// `set_epoch_deadline`; a budget that reached zero re-arms at zero, so
    /// the guest is interrupted the moment it resumes rather than being
    /// allowed one more unbounded stretch.
    pub fn resume(&mut self) -> u64 {
        self.settle();
        self.running = true;
        self.remaining
    }

    pub fn remaining(&self) -> u64 {
        self.remaining
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fuel_constant_value() {
        // 10M fuel ≈ 100ms CPU time per spec
        assert_eq!(DEFAULT_FUEL, 10_000_000);
    }

    #[test]
    fn test_runtime_has_fuel_enabled() {
        let runtime = WasmRuntime::new().expect("Failed to create runtime");
        // We can verify fuel is enabled by checking we can set it on a store
        let mut store = wasmtime::Store::new(runtime.engine(), ());
        assert!(store.set_fuel(1000).is_ok());
    }

    #[test]
    fn test_compile_invalid_wasm_fails() {
        let runtime = WasmRuntime::new().expect("Failed to create runtime");
        let result = runtime.compile(b"not valid wasm");
        assert!(result.is_err());
    }

    /// A store left at wasmtime's default deadline traps on its first
    /// instruction once epoch interruption is on; the unbounded delta does
    /// not, and does not overflow the engine's addition either.
    #[test]
    fn unbounded_deadline_neither_traps_nor_overflows() {
        let runtime = WasmRuntime::without_ticker().unwrap();
        let module = runtime
            .compile(&wat::parse_str(r#"(module (func (export "f")))"#).unwrap())
            .unwrap();
        for _ in 0..3 {
            runtime.tick();
        }

        let mut store = Store::new(runtime.engine(), ());
        store.set_fuel(DEFAULT_FUEL).unwrap();
        let instance = Instance::new(&mut store, &module, &[]).unwrap();
        let f = instance.get_typed_func::<(), ()>(&mut store, "f").unwrap();
        let err = f.call(&mut store, ()).unwrap_err();
        assert_eq!(err.downcast_ref::<Trap>(), Some(&Trap::Interrupt));

        let mut store = Store::new(runtime.engine(), ());
        store.set_fuel(DEFAULT_FUEL).unwrap();
        store.set_epoch_deadline(UNBOUNDED_TICKS);
        let instance = Instance::new(&mut store, &module, &[]).unwrap();
        let f = instance.get_typed_func::<(), ()>(&mut store, "f").unwrap();
        f.call(&mut store, ()).unwrap();
    }

    /// The mirror and the engine's epoch move together: a deadline of a
    /// thousand ticks holds through the 999th tick and trips on the
    /// thousandth, exactly where the mirror says it should.
    #[test]
    fn the_mirror_and_the_engine_epoch_do_not_drift_over_a_thousand_ticks() {
        let runtime = WasmRuntime::without_ticker().unwrap();
        let module = runtime
            .compile(&wat::parse_str(r#"(module (func (export "f")))"#).unwrap())
            .unwrap();
        let mut store = Store::new(runtime.engine(), ());
        store.set_fuel(DEFAULT_FUEL).unwrap();
        store.set_epoch_deadline(1000);
        let instance = Instance::new(&mut store, &module, &[]).unwrap();
        let f = instance.get_typed_func::<(), ()>(&mut store, "f").unwrap();

        let start = runtime.epoch().load(Ordering::Relaxed);
        for _ in 0..999 {
            runtime.tick();
        }
        assert_eq!(runtime.epoch().load(Ordering::Relaxed) - start, 999);
        f.call(&mut store, ()).unwrap();
        runtime.tick();
        assert_eq!(runtime.epoch().load(Ordering::Relaxed) - start, 1000);
        let err = f.call(&mut store, ()).unwrap_err();
        assert_eq!(err.downcast_ref::<Trap>(), Some(&Trap::Interrupt));
    }

    #[test]
    fn the_ticker_advances_the_mirror() {
        let runtime = WasmRuntime::new().unwrap();
        let start = runtime.epoch().load(Ordering::Relaxed);
        std::thread::sleep(EPOCH_TICK * 5);
        assert!(runtime.epoch().load(Ordering::Relaxed) > start);
    }

    #[test]
    fn ticks_round_a_budget_up() {
        assert_eq!(GuestDeadline::ticks_for(Duration::from_secs(1)), 101);
        assert_eq!(GuestDeadline::ticks_for(Duration::from_millis(15)), 3);
        assert_eq!(GuestDeadline::ticks_for(Duration::ZERO), 1);
    }

    /// Ticks that pass while suspended are not charged; ticks that pass
    /// while running are, and the budget re-arms with what is left.
    #[test]
    fn a_suspended_deadline_does_not_spend_host_time() {
        let epoch = Arc::new(AtomicU64::new(0));
        let mut deadline = GuestDeadline::unbounded(epoch.clone());
        assert_eq!(deadline.arm(10), 10);

        epoch.fetch_add(3, Ordering::Relaxed);
        deadline.suspend();
        assert_eq!(deadline.remaining(), 7);

        epoch.fetch_add(100, Ordering::Relaxed);
        assert_eq!(deadline.resume(), 7);
        assert_eq!(deadline.remaining(), 7);

        epoch.fetch_add(2, Ordering::Relaxed);
        deadline.suspend();
        deadline.suspend();
        assert_eq!(deadline.remaining(), 5);

        epoch.fetch_add(9, Ordering::Relaxed);
        assert_eq!(deadline.resume(), 5);
        epoch.fetch_add(9, Ordering::Relaxed);
        assert_eq!(deadline.resume(), 0);
        assert_eq!(deadline.resume(), 0);
    }

    #[test]
    fn fuel_by_plugin_type() {
        assert_eq!(fuel_for(PluginType::Library), DEFAULT_FUEL);
        assert_eq!(fuel_for(PluginType::Auth), DEFAULT_FUEL);
        assert_eq!(fuel_for(PluginType::Interpreter), u64::MAX);
    }

    #[test]
    fn ceilings_by_plugin_type() {
        assert_eq!(memory_ceiling(PluginType::Library), 512 * 1024 * 1024);
        assert_eq!(memory_ceiling(PluginType::Auth), 512 * 1024 * 1024);
        assert_eq!(memory_ceiling(PluginType::Interpreter), 128 * 1024 * 1024);
        assert_eq!(script_memory_ceiling(64 * 1024 * 1024), 128 * 1024 * 1024);
    }

    /// One memory per store, so the ceiling is the store's and not each
    /// memory's.
    #[test]
    fn the_limiter_pins_one_memory_one_table_one_instance() {
        let limiter = MemoryLimiter::new(1024 * 1024);
        assert_eq!(limiter.memories(), 1);
        assert_eq!(limiter.tables(), 1);
        assert_eq!(limiter.instances(), 1);
    }

    /// The limiter records what it refused, and forgets it on request.
    #[test]
    fn the_limiter_records_a_refusal() {
        let mut limiter = MemoryLimiter::new(1024 * 1024);
        assert!(limiter.memory_growing(0, 512 * 1024, None).unwrap());
        assert_eq!(limiter.refused(), None);
        assert!(
            !limiter
                .memory_growing(512 * 1024, 2 * 1024 * 1024, None)
                .unwrap()
        );
        assert_eq!(limiter.refused(), Some(2 * 1024 * 1024));
        assert_eq!(limiter.ceiling(), 1024 * 1024);
        limiter.clear_refusal();
        assert_eq!(limiter.refused(), None);
    }

    fn plugin(id: &str, wat: &str) -> Arc<LoadedPlugin> {
        let manifest: crate::plugin::PluginManifest = serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "version": "1.0.0", "api_version": "2",
            "plugin_type": "library", "namespace": id, "capabilities": [],
        }))
        .unwrap();
        Arc::new(LoadedPlugin {
            info: manifest.clone().into(),
            source: crate::plugin::PluginSource::File { path: id.into() },
            wasm_bytes: wat::parse_str(wat).unwrap(),
            manifest: Some(manifest),
        })
    }

    fn answering(value: i32) -> String {
        format!(r#"(module (func (export "answer") (result i32) i32.const {value}))"#)
    }

    fn answer(runtime: &WasmRuntime, module: &Module) -> i32 {
        let mut store = Store::new(runtime.engine(), ());
        store.set_fuel(DEFAULT_FUEL).unwrap();
        store.set_epoch_deadline(UNBOUNDED_TICKS);
        let instance = Instance::new(&mut store, module, &[]).unwrap();
        instance
            .get_typed_func::<(), i32>(&mut store, "answer")
            .unwrap()
            .call(&mut store, ())
            .unwrap()
    }

    /// A directory of this test's own under the system temp directory.
    struct CacheDir(PathBuf);

    impl CacheDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "happyview-module-cache-test-{tag}-{}-{}",
                std::process::id(),
                Instant::now().elapsed().as_nanos()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }

        fn files(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(&self.0)
                .map(|entries| {
                    entries
                        .flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            names.sort();
            names
        }
    }

    impl Drop for CacheDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn a_second_lookup_of_the_same_plugin_does_not_recompile() {
        let runtime = WasmRuntime::without_ticker().unwrap();
        let plugin = plugin("p", &answering(7));
        let first = runtime.module_for(&plugin).unwrap();
        let second = runtime.module_for(&plugin).unwrap();
        assert_eq!(runtime.compile_count(), 1);
        assert_eq!(answer(&runtime, &first), 7);
        assert_eq!(answer(&runtime, &second), 7);
    }

    /// The same bytes under a fresh registration hit by hash; different
    /// bytes under the same id miss and replace the entry.
    #[test]
    fn a_re_registration_hits_by_hash_and_new_bytes_replace_the_module() {
        let runtime = WasmRuntime::without_ticker().unwrap();
        runtime.module_for(&plugin("p", &answering(1))).unwrap();
        runtime.module_for(&plugin("p", &answering(1))).unwrap();
        assert_eq!(runtime.compile_count(), 1);
        let module = runtime.module_for(&plugin("p", &answering(2))).unwrap();
        assert_eq!(runtime.compile_count(), 2);
        assert_eq!(answer(&runtime, &module), 2);
    }

    #[test]
    fn a_disk_entry_written_by_one_runtime_is_used_by_a_fresh_one() {
        let dir = CacheDir::new("disk");
        let plugin = plugin("p", &answering(9));

        let first = WasmRuntime::with_cache_dir(Some(dir.0.clone())).unwrap();
        first.module_for(&plugin).unwrap();
        assert_eq!(first.compile_count(), 1);
        let key = first.cache_key(&hex::encode(Sha256::digest(&plugin.wasm_bytes)));
        assert_eq!(
            dir.files(),
            vec![format!("{key}.cwasm"), format!("{key}.cwasm.sha256")]
        );

        let second = WasmRuntime::with_cache_dir(Some(dir.0.clone())).unwrap();
        let module = second.module_for(&plugin).unwrap();
        assert_eq!(second.compile_count(), 0);
        assert_eq!(answer(&second, &module), 9);
    }

    #[test]
    fn a_cache_file_whose_sidecar_does_not_match_is_recompiled_and_rewritten() {
        let dir = CacheDir::new("sidecar");
        let plugin = plugin("p", &answering(3));
        let first = WasmRuntime::with_cache_dir(Some(dir.0.clone())).unwrap();
        first.module_for(&plugin).unwrap();
        let key = first.cache_key(&hex::encode(Sha256::digest(&plugin.wasm_bytes)));
        let sidecar = dir.0.join(format!("{key}.cwasm.sha256"));
        std::fs::write(&sidecar, "0000").unwrap();

        let second = WasmRuntime::with_cache_dir(Some(dir.0.clone())).unwrap();
        let module = second.module_for(&plugin).unwrap();
        assert_eq!(second.compile_count(), 1);
        assert_eq!(answer(&second, &module), 3);
        assert_ne!(std::fs::read_to_string(&sidecar).unwrap(), "0000");

        // Corrupt bytes with a matching sidecar are a miss too.
        let module_path = dir.0.join(format!("{key}.cwasm"));
        std::fs::write(&module_path, b"not a module").unwrap();
        std::fs::write(&sidecar, hex::encode(Sha256::digest(b"not a module"))).unwrap();
        let third = WasmRuntime::with_cache_dir(Some(dir.0.clone())).unwrap();
        assert_eq!(answer(&third, &third.module_for(&plugin).unwrap()), 3);
        assert_eq!(third.compile_count(), 1);
    }

    /// The key carries the engine fingerprint, so a module serialized by
    /// another engine lives under another name and is never opened.
    #[test]
    fn a_key_under_a_different_engine_fingerprint_is_ignored() {
        let dir = CacheDir::new("engine");
        let plugin = plugin("p", &answering(4));
        let runtime = WasmRuntime::with_cache_dir(Some(dir.0.clone())).unwrap();
        let sha = hex::encode(Sha256::digest(&plugin.wasm_bytes));
        let key = runtime.cache_key(&sha);
        assert!(key.starts_with(&format!("{sha}-")));
        assert_eq!(key.len(), sha.len() + 1 + 16);

        let module = runtime.compile(&plugin.wasm_bytes).unwrap();
        let bytes = module.serialize().unwrap();
        std::fs::create_dir_all(&dir.0).unwrap();
        let foreign = format!("{sha}-{:016x}", 0xdead_beef_u64);
        std::fs::write(dir.0.join(format!("{foreign}.cwasm")), &bytes).unwrap();
        std::fs::write(
            dir.0.join(format!("{foreign}.cwasm.sha256")),
            hex::encode(Sha256::digest(&bytes)),
        )
        .unwrap();

        let fresh = WasmRuntime::with_cache_dir(Some(dir.0.clone())).unwrap();
        fresh.module_for(&plugin).unwrap();
        assert_eq!(fresh.compile_count(), 1);
    }

    #[test]
    fn an_unwritable_cache_directory_warns_once_and_every_call_succeeds() {
        let dir = CacheDir::new("unwritable");
        // A file where the directory should be: nothing under it can be
        // created.
        std::fs::write(&dir.0, b"").unwrap();
        let runtime = WasmRuntime::with_cache_dir(Some(dir.0.clone())).unwrap();
        for value in [1, 2, 3] {
            let module = runtime.module_for(&plugin("p", &answering(value))).unwrap();
            assert_eq!(answer(&runtime, &module), value);
        }
        assert_eq!(runtime.compile_count(), 3);
        assert!(runtime.modules.warned.load(Ordering::Relaxed));
    }

    /// A fresh directory is created for this user alone.
    #[cfg(unix)]
    #[test]
    fn a_fresh_cache_directory_is_created_private() {
        use std::os::unix::fs::MetadataExt;
        let dir = CacheDir::new("fresh");
        let runtime = WasmRuntime::with_cache_dir(Some(dir.0.clone())).unwrap();
        assert!(runtime.modules.dir.is_some());
        let metadata = std::fs::metadata(&dir.0).unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o700);
        assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
        assert!(!runtime.modules.warned.load(Ordering::Relaxed));
    }

    /// A directory another user can enter is refused: the cache stays in
    /// memory, one warning is logged, and nothing is written there.
    #[cfg(unix)]
    #[test]
    fn a_cache_directory_with_group_or_other_bits_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = CacheDir::new("shared");
        std::fs::create_dir_all(&dir.0).unwrap();
        std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o755)).unwrap();
        let runtime = WasmRuntime::with_cache_dir(Some(dir.0.clone())).unwrap();
        assert!(runtime.modules.dir.is_none());
        assert!(runtime.modules.warned.load(Ordering::Relaxed));
        let module = runtime.module_for(&plugin("p", &answering(5))).unwrap();
        assert_eq!(answer(&runtime, &module), 5);
        assert!(dir.files().is_empty());
    }

    /// Ownership by any other uid is refused even at mode 0700; the check
    /// takes the uid as an argument because no test can create a directory
    /// owned by someone else.
    #[cfg(unix)]
    #[test]
    fn a_cache_directory_owned_by_another_user_is_refused() {
        let dir = CacheDir::new("owner");
        WasmRuntime::with_cache_dir(Some(dir.0.clone())).unwrap();
        let metadata = std::fs::metadata(&dir.0).unwrap();
        let me = unsafe { libc::geteuid() };
        assert!(directory_is_private(&metadata, me).is_ok());
        let err = directory_is_private(&metadata, me.wrapping_add(1)).unwrap_err();
        assert!(err.contains("owned by uid"), "{err}");
    }

    #[test]
    fn the_sweep_deletes_orphans_and_keeps_live_modules_and_other_files() {
        let dir = CacheDir::new("sweep");
        let live = plugin("live", &answering(1));
        let gone = plugin("gone", &answering(2));
        let runtime = WasmRuntime::with_cache_dir(Some(dir.0.clone())).unwrap();
        runtime.module_for(&live).unwrap();
        runtime.module_for(&gone).unwrap();
        let live_sha = hex::encode(Sha256::digest(&live.wasm_bytes));
        let live_key = runtime.cache_key(&live_sha);
        std::fs::write(dir.0.join("notes.txt"), b"keep").unwrap();
        std::fs::write(
            dir.0.join(format!("{live_sha}-0000000000000000.cwasm")),
            b"x",
        )
        .unwrap();
        assert_eq!(dir.files().len(), 6);

        runtime.sweep_cache_dir(&HashSet::from([live_sha]));
        assert_eq!(
            dir.files(),
            vec![
                format!("{live_key}.cwasm"),
                format!("{live_key}.cwasm.sha256"),
                "notes.txt".to_string(),
            ]
        );
    }

    #[test]
    fn an_unbounded_deadline_stays_unbounded_across_suspension() {
        let epoch = Arc::new(AtomicU64::new(0));
        let mut deadline = GuestDeadline::unbounded(epoch.clone());
        epoch.fetch_add(1_000_000, Ordering::Relaxed);
        deadline.suspend();
        assert_eq!(deadline.resume(), UNBOUNDED_TICKS);
        epoch.fetch_add(1_000_000, Ordering::Relaxed);
        deadline.suspend();
        assert!(deadline.resume() > UNBOUNDED_TICKS - 2_000_000);
    }
}

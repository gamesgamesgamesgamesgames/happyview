use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::{Duration, Instant};

use lru::LruCache;
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
        // An interpreter is bounded by the operator's wall clock, an epoch
        // deadline armed per run, rather than by fuel: nothing has calibrated
        // a fuel budget against a script's instruction budget. Fuel is still
        // charged, including across a caught `throw` (every Lua error on the
        // wasi route is one): on wasmtime 49.0.5 and 50.0.1, a frame that
        // catches a throw keeps the fuel its callees spent, whether they were
        // reached by `call`, `call_indirect` or `call_ref`, and runs out of
        // fuel when it should. Fuel stays enabled engine-wide because it is
        // what bounds libraries. The whole range also covers the per-byte
        // instantiation charge a megabytes-sized interpreter image draws on
        // a host without copy-on-write memory.
        PluginType::Interpreter => u64::MAX,
    }
}

/// Fuel per unit of a script's instruction budget, for a compiled module.
///
/// The budget is an operator setting written for the Lua interpreter, where a
/// unit is one bytecode instruction and everything a script does in a C
/// builtin is free: decoding the run's JSON input, encoding its result,
/// `table.sort`, `string.format`, `table.concat`. A compiled module does all
/// of that in wasm and pays fuel for it, input decoding included, which is
/// most of what a typical handler spends. The value therefore sits above the
/// top of the measured range, so that every measured workload that fits a Lua
/// budget also fits as a module under the same setting; the wall clock
/// remains the hard CPU bound for both.
///
/// That does not hold for every handler. A module pays roughly 97 fuel per
/// byte to decode its input (the slope of a handler that only decodes, from
/// 22,852 to 91,886 bytes of records), which a Lua script does not pay at
/// all, so a handler that does little with a large input gets far less from
/// one setting as a module: at the setting's floor of 1,000, about 40 KB of
/// input exhausts the budget before the handler runs.
///
/// Measured by `tests/fuel_calibration.rs` on 2026-10-13: the smallest budget
/// at which each handler returns, as a Lua script under happyview-lua 1.0.0
/// (sha256 `9496eb497590e44c5c8a66441156499c63dd6f9c8b9ec01bf59c9b8a5e11382b`)
/// and as a release wasm32-wasip1 module in raw fuel, under wasmtime 49.0.2
/// and rustc 1.96.1, with module fuel resolved to 100. Three runs of each
/// input agreed exactly.
///
/// | handler   | records | Lua instructions | module fuel | fuel per instruction |
/// |-----------|---------|------------------|-------------|----------------------|
/// | shape     | 50      | 1,575            | 2,798,400   | 1,777                |
/// | aggregate | 50      | 2,321            | 2,461,700   | 1,061                |
/// | render    | 50      | 852              | 2,748,900   | 3,226                |
/// | shape     | 200     | 6,429            | 9,899,300   | 1,540                |
/// | aggregate | 200     | 7,445            | 9,486,600   | 1,274                |
/// | render    | 200     | 3,252            | 10,797,700  | 3,320                |
/// | echo      | -       | 37               | 67,700      | 1,830                |
///
/// `shape` filters, sorts and reshapes post envelopes, `aggregate` counts and
/// ranks their tags, and `render` formats one line per post. The value is the
/// largest ratio, 3,320, plus about a fifth for toolchain drift: a rustc,
/// wasmtime or serde upgrade moves these figures by a few percent, and the
/// calibration test should fail only on a real shift. That test is ignored
/// and needs the plugin, so it runs only by hand: re-run it after upgrading
/// rustc, wasmtime, serde or the SDK's input decoding, or happyview-lua. A
/// trivial handler needs 17 units under this value, far inside the setting's
/// floor of 1,000, and the setting's ceiling of 1,000,000,000 is 4 × 10^12
/// fuel, nowhere near overflowing.
pub const FUEL_PER_INSTRUCTION: u64 = 4_000;

/// The fuel each call into a compiled module gets. `None` is a job, which
/// has no budget and stops through `should_stop`.
///
/// A compiled module is bounded by fuel, where an interpreter is bounded by
/// the wall clock (see [`fuel_for`]). It never catches a wasm `throw`:
/// stable Rust on wasm32-wasip1 aborts on panic and emits no
/// exception-handling instructions, and a module that defines a tag is
/// refused before it runs (see `project::script_module`).
pub fn module_fuel(instructions: Option<u32>) -> u64 {
    instructions.map_or(u64::MAX, |n| {
        u64::from(n).saturating_mul(FUEL_PER_INSTRUCTION)
    })
}

/// Fuel a guest is granted for each byte the host writes into it — a call's
/// input, or an import's answer. Data the host hands over brings the fuel to
/// handle it: parsing it, decoding `$bytes`, and encoding it again on the way
/// out each cost a few instructions a byte, so a fixed budget would cap every
/// payload a library plugin can move at a couple of hundred kilobytes, far
/// below what the host itself allows. Computation that brings no data stays
/// bounded by [`DEFAULT_FUEL`].
///
/// Measured on `tests/plugin_blobs.rs`'s 4 MiB round trip, where a blob is
/// decoded and encoded again in each direction: 64 a byte runs out and 128
/// does not. 256 is that with room to spare.
pub const FUEL_PER_BYTE_RECEIVED: u64 = 256;

/// Add [`FUEL_PER_BYTE_RECEIVED`] for `len` bytes to a store's remaining
/// fuel. A store whose fuel is already unbounded stays so. An interpreter
/// store is left alone: an installed interpreter's fuel is unbounded, and a
/// compiled module's is the operator's instruction budget, which
/// [`FUEL_PER_INSTRUCTION`] already sizes to cover decoding its input.
pub fn grant_fuel_for_bytes(
    store: &mut impl wasmtime::AsContextMut<Data = crate::plugin::host::PluginState>,
    len: usize,
) {
    let mut store = store.as_context_mut();
    if store.data().plugin_type == PluginType::Interpreter {
        return;
    }
    if let Ok(fuel) = store.get_fuel() {
        let grant = (len as u64).saturating_mul(FUEL_PER_BYTE_RECEIVED);
        let _ = store.set_fuel(fuel.saturating_add(grant));
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

/// The native stack a guest's frames may take, which wasmtime's default holds
/// to 512 KiB. A guest cannot see this stack at all: a C interpreter measures
/// the shadow stack in its own linear memory, and each level of its recursion
/// costs far more native stack than shadow stack, so the native one runs out
/// first and the run ends in a trap rather than the language's own error.
/// QuickJS-ng's parser is the steepest case measured, at well over twenty
/// times; 16 MiB is what lets an interpreter's own limit fire first for every
/// shape tried, nested source included, where 8 MiB still trapped on source
/// nested five thousand deep. It is reserved per fiber and committed only as
/// it is touched, so it costs a run that recurses that deep and nothing else.
pub const MAX_WASM_STACK: usize = 16 << 20;

/// Each call runs on a fiber of this size, and what is left above
/// [`MAX_WASM_STACK`] is all the stack a host function gets. Wasmtime does not
/// bound that: a host function that overruns it aborts the process. 2 MiB is
/// more than the 1.5 MiB its defaults left.
pub const ASYNC_STACK_SIZE: usize = MAX_WASM_STACK + (2 << 20);

/// WASM runtime for executing plugins
pub struct WasmRuntime {
    engine: Engine,
    /// Mirrors the engine's epoch tick for tick. Wasmtime exposes neither
    /// the current epoch nor a store's remaining deadline, and the guest-only
    /// accounting in [`GuestDeadline`] needs both.
    epoch: Arc<AtomicU64>,
    ticker_stop: Arc<AtomicBool>,
    modules: Arc<ModuleCache>,
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
        let modules = Arc::get_mut(&mut runtime.modules).expect("a new runtime's cache is its own");
        modules.dir = dir.and_then(|dir| match prepare_cache_dir(&dir) {
            Ok(()) => Some(dir),
            Err(reason) => {
                modules.warned.store(true, Ordering::Relaxed);
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
        config.max_wasm_stack(MAX_WASM_STACK);
        config.async_stack_size(ASYNC_STACK_SIZE);

        let engine = Engine::new(&config)?;

        Ok(Self {
            engine,
            epoch: Arc::new(AtomicU64::new(0)),
            ticker_stop: Arc::new(AtomicBool::new(false)),
            modules: Arc::default(),
        })
    }

    /// This runtime, compiling every module it does not hold itself rather
    /// than taking it from the process's compiled modules: for a test that
    /// counts its compiles while other tests compile the same bytes.
    #[cfg(test)]
    pub(crate) fn without_shared_modules(mut self) -> Self {
        Arc::get_mut(&mut self.modules)
            .expect("a new runtime's cache is its own")
            .unshared = true;
        self
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

    /// Compile a WASM module, or take it from the modules this process
    /// has already compiled. It blocks for as long as a compile takes, so
    /// an async caller goes through [`WasmRuntime::module_for`].
    pub fn compile(&self, wasm_bytes: &[u8]) -> Result<Module, anyhow::Error> {
        let sha = hex::encode(Sha256::digest(wasm_bytes));
        compile(
            &self.engine,
            &self.modules,
            wasm_bytes,
            &cache_key(&self.engine, &sha),
        )
        .map(|compiled| compiled.module)
    }

    /// The compiled module for `plugin`, from this runtime's memory, from
    /// disk, from the process's compiled modules, or by compiling. The
    /// tiers are checked in that order and a miss at any tier fills the
    /// ones above it. Everything past this runtime's memory runs on a
    /// blocking thread: a compile takes up to seconds.
    pub async fn module_for(&self, plugin: &Arc<LoadedPlugin>) -> Result<Module, anyhow::Error> {
        if let Some(module) = self.modules.get_by_identity(&plugin.info.id, plugin) {
            return Ok(module);
        }
        let engine = self.engine.clone();
        let modules = self.modules.clone();
        let plugin = plugin.clone();
        tokio::task::spawn_blocking(move || resolve(&engine, &modules, &plugin)).await?
    }

    /// [`WasmRuntime::module_for`] on the calling thread, for a caller
    /// already off the async workers.
    pub fn module_for_blocking(&self, plugin: &Arc<LoadedPlugin>) -> Result<Module, anyhow::Error> {
        if let Some(module) = self.modules.get_by_identity(&plugin.info.id, plugin) {
            return Ok(module);
        }
        resolve(&self.engine, &self.modules, plugin)
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
        cache_key(&self.engine, wasm_sha256)
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

fn cache_key(engine: &Engine, wasm_sha256: &str) -> String {
    let mut hasher = std::hash::DefaultHasher::new();
    engine.precompile_compatibility_hash().hash(&mut hasher);
    format!("{wasm_sha256}-{:016x}", hasher.finish())
}

/// `plugin`'s module past the identity check: by hash in memory, from
/// disk, or through [`compile`].
fn resolve(
    engine: &Engine,
    modules: &ModuleCache,
    plugin: &Arc<LoadedPlugin>,
) -> Result<Module, anyhow::Error> {
    let id = plugin.info.id.as_str();
    let sha = hex::encode(Sha256::digest(&plugin.wasm_bytes));
    if let Some(module) = modules.get_by_hash(id, plugin, &sha) {
        return Ok(module);
    }
    let key = cache_key(engine, &sha);
    if let Some(module) = modules.load_from_disk(engine, &key) {
        modules.insert(id, plugin, &sha, module.clone());
        return Ok(module);
    }
    let Compiled { module, serialized } = compile(engine, modules, &plugin.wasm_bytes, &key)?;
    modules.write_to_disk(&key, &module, serialized.as_deref());
    modules.insert(id, plugin, &sha, module.clone());
    Ok(module)
}

/// The total size of the compiled modules the process keeps for its
/// runtimes. A serialized interpreter runs to several megabytes, and
/// installed plugins and project modules number a handful; the bound is for
/// a long run that keeps rebuilding a project's modules, each build new
/// bytes.
const SHARED_MODULE_BYTES: usize = 64 * 1024 * 1024;

/// Modules compiled in this process, serialized, by [`cache_key`]. A
/// `Module` belongs to the engine that compiled it, and each runtime has an
/// engine of its own because each keeps its own epoch clock, so another
/// runtime takes a module from here by deserializing it, a small fraction of
/// what compiling it again would cost.
static SHARED: LazyLock<Mutex<SharedModules>> =
    LazyLock::new(|| Mutex::new(SharedModules::new(SHARED_MODULE_BYTES)));

/// Serialized modules by key, at most `limit` bytes of them, the least
/// recently used evicted first.
struct SharedModules {
    entries: LruCache<String, Arc<[u8]>>,
    bytes: usize,
    limit: usize,
}

impl SharedModules {
    fn new(limit: usize) -> Self {
        Self {
            entries: LruCache::unbounded(),
            bytes: 0,
            limit,
        }
    }

    fn get(&mut self, key: &str) -> Option<Arc<[u8]>> {
        self.entries.get(key).cloned()
    }

    /// Keep `module` under `key`, evicting until the total fits. A module
    /// larger than the whole limit is not kept.
    fn put(&mut self, key: String, module: Arc<[u8]>) {
        if module.len() > self.limit {
            return;
        }
        self.bytes += module.len();
        if let Some(replaced) = self.entries.put(key, module) {
            self.bytes -= replaced.len();
        }
        while self.bytes > self.limit {
            let Some((_, evicted)) = self.entries.pop_lru() else {
                break;
            };
            self.bytes -= evicted.len();
        }
    }
}

/// A module from [`compile`], with its serialized form when one is in hand,
/// so the disk tier need not serialize it again.
struct Compiled {
    module: Module,
    serialized: Option<Arc<[u8]>>,
}

/// The module for `wasm_bytes`, whose cache key is `key`: deserialized from
/// the process's compiled modules, or compiled and added to them.
fn compile(
    engine: &Engine,
    modules: &ModuleCache,
    wasm_bytes: &[u8],
    key: &str,
) -> Result<Compiled, anyhow::Error> {
    let shared = (!modules.unshared).then_some(&*SHARED);
    let held = shared.and_then(|shared| shared.lock().unwrap_or_else(|e| e.into_inner()).get(key));
    if let Some(bytes) = held {
        // SAFETY: the bytes are `Module::serialize`'s output from this
        // process, held in memory nothing else writes, under a key carrying
        // this engine's fingerprint. Wasmtime still verifies the
        // fingerprint embedded in them.
        match unsafe { Module::deserialize(engine, &bytes) } {
            Ok(module) => {
                return Ok(Compiled {
                    module,
                    serialized: Some(bytes),
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, "shared plugin module could not be loaded; recompiling")
            }
        }
    }
    modules.compiles.fetch_add(1, Ordering::Relaxed);
    let module = Module::new(engine, wasm_bytes)?;
    let Some(shared) = shared else {
        return Ok(Compiled {
            module,
            serialized: None,
        });
    };
    match module.serialize() {
        Ok(bytes) => {
            let bytes: Arc<[u8]> = bytes.into();
            shared
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .put(key.to_string(), bytes.clone());
            Ok(Compiled {
                module,
                serialized: Some(bytes),
            })
        }
        Err(e) => {
            tracing::warn!(error = %e, "compiled plugin module could not be serialized for reuse");
            Ok(Compiled {
                module,
                serialized: None,
            })
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
    use crate::fs_private::{NotPrivate, private_to};
    private_to(metadata, euid).map_err(|reason| match reason {
        NotPrivate::OtherOwner { uid, euid } => format!(
            "the plugin module cache directory is owned by uid {uid} rather than this process's uid {euid}"
        ),
        NotPrivate::OpenMode { mode } => format!(
            "the plugin module cache directory has mode {mode:o}; it must grant nothing to group or other"
        ),
    })
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
    /// Set by [`WasmRuntime::without_shared_modules`].
    unshared: bool,
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

    /// Write `module` under `key`, from `serialized` when the caller already
    /// holds its serialized form.
    fn write_to_disk(&self, key: &str, module: &Module, serialized: Option<&[u8]>) {
        let Some((module_path, sidecar_path)) = self.paths(key) else {
            return;
        };
        let Some(dir) = self.dir.as_ref() else {
            return;
        };
        let result = (|| -> std::io::Result<()> {
            prepare_cache_dir(dir).map_err(std::io::Error::other)?;
            let bytes: std::borrow::Cow<[u8]> = match serialized {
                Some(bytes) => bytes.into(),
                None => module
                    .serialize()
                    .map_err(|e| std::io::Error::other(e.to_string()))?
                    .into(),
            };
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
    fn module_fuel_scales_the_budget_and_leaves_none_unbounded() {
        assert_eq!(module_fuel(Some(1_000)), 1_000 * FUEL_PER_INSTRUCTION);
        assert_eq!(module_fuel(None), u64::MAX);
        assert_eq!(
            module_fuel(Some(u32::MAX)),
            u64::from(u32::MAX) * FUEL_PER_INSTRUCTION
        );
    }

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

    /// Calls itself `depth` times; each frame holds a return address and a
    /// frame pointer at the least, so 100 000 of them is well past the 512 KiB
    /// wasmtime allows by default.
    const RECURSE: &str = r#"(module
        (func $down (export "down") (param i32) (result i32)
            local.get 0
            i32.eqz
            if (result i32)
                i32.const 0
            else
                local.get 0
                i32.const 1
                i32.sub
                call $down
                i32.const 1
                i32.add
            end))"#;

    async fn recurse(depth: i32) -> Result<i32, wasmtime::Error> {
        let runtime = WasmRuntime::without_ticker().unwrap();
        let module = runtime.compile(&wat::parse_str(RECURSE).unwrap()).unwrap();
        let mut store = wasmtime::Store::new(runtime.engine(), ());
        store.set_fuel(u64::MAX).unwrap();
        store.set_epoch_deadline(UNBOUNDED_TICKS);
        let instance = Instance::new_async(&mut store, &module, &[]).await.unwrap();
        let down = instance
            .get_typed_func::<i32, i32>(&mut store, "down")
            .unwrap();
        down.call_async(&mut store, depth).await
    }

    #[tokio::test]
    async fn a_guest_recursing_past_the_default_stack_still_returns() {
        assert_eq!(recurse(100_000).await.unwrap(), 100_000);
    }

    /// A guest with no limit of its own still ends at the ceiling, as a trap
    /// the store survives rather than a fault that takes the process down.
    #[tokio::test]
    async fn unbounded_recursion_ends_in_a_stack_overflow_trap() {
        let error = recurse(i32::MAX).await.unwrap_err();
        assert_eq!(
            error.downcast_ref::<Trap>(),
            Some(&Trap::StackOverflow),
            "{error:?}"
        );
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

    /// `wasm` with a custom section holding a fresh id appended, so its
    /// hash is one no other test has compiled into the process's modules.
    fn unique(mut wasm: Vec<u8>) -> Vec<u8> {
        const NAME: &[u8] = b"unique";
        let id = uuid::Uuid::new_v4();
        wasm.push(0);
        wasm.push((1 + NAME.len() + id.as_bytes().len()) as u8);
        wasm.push(NAME.len() as u8);
        wasm.extend_from_slice(NAME);
        wasm.extend_from_slice(id.as_bytes());
        wasm
    }

    /// The process's modules are bounded by their total size: the least
    /// recently used go first, and one larger than the whole bound is not
    /// kept at all.
    #[test]
    fn shared_modules_evict_the_least_recently_used_past_their_byte_bound() {
        let bytes = |n: usize| -> Arc<[u8]> { vec![0u8; n].into() };
        let mut shared = SharedModules::new(100);
        shared.put("a".into(), bytes(40));
        shared.put("b".into(), bytes(40));
        assert!(shared.get("a").is_some());
        shared.put("c".into(), bytes(40));
        assert!(shared.get("b").is_none(), "b was the least recently used");
        assert!(shared.get("a").is_some());
        assert!(shared.get("c").is_some());
        assert_eq!(shared.bytes, 80);

        shared.put("a".into(), bytes(10));
        assert_eq!(shared.bytes, 50, "a replaced entry gives its bytes back");

        shared.put("huge".into(), bytes(101));
        assert!(shared.get("huge").is_none());
        assert_eq!(shared.bytes, 50);
    }

    /// The same bytes compile once per process: again on the same runtime,
    /// and on another runtime, as each app state builds its own.
    #[test]
    fn the_same_bytes_compile_once_per_process() {
        let wasm = unique(wat::parse_str(answering(42)).unwrap());
        let first = WasmRuntime::without_ticker().unwrap();
        first.compile(&wasm).unwrap();
        first.compile(&wasm).unwrap();
        assert_eq!(first.compile_count(), 1);

        let second = WasmRuntime::without_ticker().unwrap();
        let module = second.compile(&wasm).unwrap();
        assert_eq!(second.compile_count(), 0);
        assert_eq!(answer(&second, &module), 42);
    }

    /// A compile runs off the async workers: on a single-threaded runtime,
    /// a task spawned beside it keeps running while the real Lua
    /// interpreter compiles. Skips unless `HAPPYVIEW_LUA_PLUGIN` names it.
    #[tokio::test(flavor = "current_thread")]
    async fn a_compile_leaves_the_async_runtime_free() {
        let Some(dir) = std::env::var_os("HAPPYVIEW_LUA_PLUGIN") else {
            eprintln!("skipping: HAPPYVIEW_LUA_PLUGIN is unset");
            return;
        };
        let mut plugin = crate::plugin::loader::load_from_file(Path::new(&dir))
            .await
            .expect("the Lua plugin should load");
        plugin.wasm_bytes = unique(plugin.wasm_bytes);
        let plugin = Arc::new(plugin);
        let runtime = WasmRuntime::without_ticker().unwrap();

        let ticks = Arc::new(AtomicU64::new(0));
        let ticker = tokio::spawn({
            let ticks = ticks.clone();
            async move {
                loop {
                    ticks.fetch_add(1, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }
        });
        tokio::task::yield_now().await;
        let before = ticks.load(Ordering::Relaxed);
        runtime.module_for(&plugin).await.unwrap();
        let during = ticks.load(Ordering::Relaxed) - before;
        ticker.abort();

        assert_eq!(runtime.compile_count(), 1);
        assert!(during > 0, "the ticker did not run during the compile");
    }

    #[test]
    fn a_second_lookup_of_the_same_plugin_does_not_recompile() {
        let runtime = WasmRuntime::without_ticker()
            .unwrap()
            .without_shared_modules();
        let plugin = plugin("p", &answering(7));
        let first = runtime.module_for_blocking(&plugin).unwrap();
        let second = runtime.module_for_blocking(&plugin).unwrap();
        assert_eq!(runtime.compile_count(), 1);
        assert_eq!(answer(&runtime, &first), 7);
        assert_eq!(answer(&runtime, &second), 7);
    }

    /// The same bytes under a fresh registration hit by hash; different
    /// bytes under the same id miss and replace the entry.
    #[test]
    fn a_re_registration_hits_by_hash_and_new_bytes_replace_the_module() {
        let runtime = WasmRuntime::without_ticker()
            .unwrap()
            .without_shared_modules();
        runtime
            .module_for_blocking(&plugin("p", &answering(1)))
            .unwrap();
        runtime
            .module_for_blocking(&plugin("p", &answering(1)))
            .unwrap();
        assert_eq!(runtime.compile_count(), 1);
        let module = runtime
            .module_for_blocking(&plugin("p", &answering(2)))
            .unwrap();
        assert_eq!(runtime.compile_count(), 2);
        assert_eq!(answer(&runtime, &module), 2);
    }

    #[test]
    fn a_disk_entry_written_by_one_runtime_is_used_by_a_fresh_one() {
        let dir = CacheDir::new("disk");
        let plugin = plugin("p", &answering(9));

        let first = WasmRuntime::with_cache_dir(Some(dir.0.clone()))
            .unwrap()
            .without_shared_modules();
        first.module_for_blocking(&plugin).unwrap();
        assert_eq!(first.compile_count(), 1);
        let key = first.cache_key(&hex::encode(Sha256::digest(&plugin.wasm_bytes)));
        assert_eq!(
            dir.files(),
            vec![format!("{key}.cwasm"), format!("{key}.cwasm.sha256")]
        );

        let second = WasmRuntime::with_cache_dir(Some(dir.0.clone()))
            .unwrap()
            .without_shared_modules();
        let module = second.module_for_blocking(&plugin).unwrap();
        assert_eq!(second.compile_count(), 0);
        assert_eq!(answer(&second, &module), 9);
    }

    #[test]
    fn a_cache_file_whose_sidecar_does_not_match_is_recompiled_and_rewritten() {
        let dir = CacheDir::new("sidecar");
        let plugin = plugin("p", &answering(3));
        let first = WasmRuntime::with_cache_dir(Some(dir.0.clone()))
            .unwrap()
            .without_shared_modules();
        first.module_for_blocking(&plugin).unwrap();
        let key = first.cache_key(&hex::encode(Sha256::digest(&plugin.wasm_bytes)));
        let sidecar = dir.0.join(format!("{key}.cwasm.sha256"));
        std::fs::write(&sidecar, "0000").unwrap();

        let second = WasmRuntime::with_cache_dir(Some(dir.0.clone()))
            .unwrap()
            .without_shared_modules();
        let module = second.module_for_blocking(&plugin).unwrap();
        assert_eq!(second.compile_count(), 1);
        assert_eq!(answer(&second, &module), 3);
        assert_ne!(std::fs::read_to_string(&sidecar).unwrap(), "0000");

        // Corrupt bytes with a matching sidecar are a miss too.
        let module_path = dir.0.join(format!("{key}.cwasm"));
        std::fs::write(&module_path, b"not a module").unwrap();
        std::fs::write(&sidecar, hex::encode(Sha256::digest(b"not a module"))).unwrap();
        let third = WasmRuntime::with_cache_dir(Some(dir.0.clone()))
            .unwrap()
            .without_shared_modules();
        assert_eq!(
            answer(&third, &third.module_for_blocking(&plugin).unwrap()),
            3
        );
        assert_eq!(third.compile_count(), 1);
    }

    /// The key carries the engine fingerprint, so a module serialized by
    /// another engine lives under another name and is never opened.
    #[test]
    fn a_key_under_a_different_engine_fingerprint_is_ignored() {
        let dir = CacheDir::new("engine");
        let plugin = plugin("p", &answering(4));
        let runtime = WasmRuntime::with_cache_dir(Some(dir.0.clone()))
            .unwrap()
            .without_shared_modules();
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

        let fresh = WasmRuntime::with_cache_dir(Some(dir.0.clone()))
            .unwrap()
            .without_shared_modules();
        fresh.module_for_blocking(&plugin).unwrap();
        assert_eq!(fresh.compile_count(), 1);
    }

    #[test]
    fn an_unwritable_cache_directory_warns_once_and_every_call_succeeds() {
        let dir = CacheDir::new("unwritable");
        // A file where the directory should be: nothing under it can be
        // created.
        std::fs::write(&dir.0, b"").unwrap();
        let runtime = WasmRuntime::with_cache_dir(Some(dir.0.clone()))
            .unwrap()
            .without_shared_modules();
        for value in [1, 2, 3] {
            let module = runtime
                .module_for_blocking(&plugin("p", &answering(value)))
                .unwrap();
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
        let module = runtime
            .module_for_blocking(&plugin("p", &answering(5)))
            .unwrap();
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
        runtime.module_for_blocking(&live).unwrap();
        runtime.module_for_blocking(&gone).unwrap();
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

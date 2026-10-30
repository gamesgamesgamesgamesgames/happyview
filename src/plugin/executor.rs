// src/plugin/executor.rs

use crate::catalog::LexiconCatalog;
use crate::db::DatabaseBackend;
use crate::plugin::PluginType;
use crate::plugin::caller::CallerSession;
use crate::plugin::capabilities::{self, PluginCapability};
use crate::plugin::host::ScriptRun;
use crate::plugin::host::{PluginState, register_host_functions};
use crate::plugin::library::{
    ApiSurface, LibraryCallContext, LibraryCallInput, LibraryEntry, MAX_LIBRARY_CALL_DEPTH,
};
use crate::plugin::memory::{
    PluginEnvelopeError, PluginResponse, dealloc_guest, read_from_guest, write_to_guest,
};
use crate::plugin::prepared::ArtifactStamp;
use crate::plugin::runtime::{
    GuestDeadline, MemoryLimiter, UNBOUNDED_TICKS, WasmRuntime, fuel_for, memory_ceiling,
    script_memory_ceiling,
};
use crate::plugin::secrets::load_plugin_secrets;
use crate::plugin::{
    ChunkRole, ExternalProfile, LoadedPlugin, ModuleScope, PluginInfo, PluginRegistry,
    ScriptExecuteInput, ScriptExecuteOutput, ScriptKind, ScriptValidateInput, ScriptValidateOutput,
    TokenSet,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use thiserror::Error;
use wasmtime::{Instance, Linker, Memory, Store, TypedFunc};

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("Plugin not found: {0}")]
    PluginNotFound(String),

    /// `anyhow` rather than `wasmtime::Error`: instantiation also fails for
    /// reasons wasmtime never sees (loading allowed hosts, a missing export),
    /// and this variant is public surface, where `Trap` below is only ever
    /// wasmtime's own.
    #[error("WASM instantiation failed: {0}")]
    Instantiation(#[source] anyhow::Error),

    #[error("Memory allocation failed")]
    MemoryAllocation,

    #[error("Plugin function trapped: {0}")]
    Trap(#[source] wasmtime::Error),

    #[error("Invalid response from plugin: {0}")]
    InvalidResponse(String),

    #[error("Plugin returned error: {code} - {message}")]
    PluginError {
        code: String,
        message: String,
        retryable: bool,
    },

    #[error("Resource limit exceeded: {0}")]
    ResourceLimit(String),

    /// Fuel bounds a library or a compiled module and a wall clock bounds
    /// an interpreted script, so the text names neither.
    #[error("Timeout: the run exceeded its execution limit")]
    Timeout,

    #[error(
        "Memory limit exceeded: the plugin asked for {requested} bytes against a {ceiling}-byte ceiling"
    )]
    MemoryLimit { requested: usize, ceiling: usize },

    #[error("Missing export: {0}")]
    MissingExport(String),

    /// A library's `get_api_surface` answered something the type grammar
    /// does not read. The message says where.
    #[error("library {plugin_id} describes itself wrongly: {message}")]
    BadSurface { plugin_id: String, message: String },

    #[error("Plugin is not a library: {0}")]
    NotALibrary(String),

    #[error("Plugin is not an interpreter: {0}")]
    NotAnInterpreter(String),

    #[error("Library call depth limit ({MAX_LIBRARY_CALL_DEPTH}) exceeded")]
    DepthExceeded,
}

impl ExecutionError {
    /// How a script runner reports this to a caller: the two limits have
    /// their own types, everything else is the script's own failure.
    pub fn script_error_type(&self) -> crate::error::ScriptErrorType {
        match self {
            ExecutionError::Timeout => crate::error::ScriptErrorType::Timeout,
            ExecutionError::MemoryLimit { .. } => crate::error::ScriptErrorType::Memory,
            _ => crate::error::ScriptErrorType::Runtime,
        }
    }
}

impl From<PluginEnvelopeError> for ExecutionError {
    fn from(e: PluginEnvelopeError) -> Self {
        ExecutionError::PluginError {
            code: e.code,
            message: e.message,
            retryable: e.retryable,
        }
    }
}

/// Single-use wrapper around a WASM instance
#[allow(dead_code)]
pub struct PluginInstance {
    pub(crate) store: Store<PluginState>,
    pub(crate) instance: Instance,
    pub(crate) memory: Memory,
    pub(crate) alloc: TypedFunc<u32, u32>,
    pub(crate) dealloc: TypedFunc<(u32, u32), ()>,
}

impl PluginInstance {
    /// The capability set this instance was granted at instantiation.
    pub fn capabilities(&self) -> &HashSet<PluginCapability> {
        &self.store.data().capabilities
    }

    /// Give the guest `budget` of its own execution time, counted only
    /// while it runs; a trap past it classifies as [`ExecutionError::Timeout`].
    pub fn arm_guest_deadline(&mut self, budget: std::time::Duration) {
        let ticks = GuestDeadline::ticks_for(budget);
        let delta = self.store.data_mut().deadline.arm(ticks);
        self.store.set_epoch_deadline(delta);
    }

    /// Remove the guest's deadline: a job runs as long as it needs to and
    /// stops through `should_stop`.
    pub fn lift_guest_deadline(&mut self) {
        let delta = self.store.data_mut().deadline.arm(UNBOUNDED_TICKS);
        self.store.set_epoch_deadline(delta);
    }

    /// The panic the guest reported during the last call, if it reported one.
    pub fn take_panic(&mut self) -> Option<happyview_plugin_sdk::wire::PanicReport> {
        self.store.data_mut().panic.take()
    }

    /// Charge every later call `fuel`, replacing the plugin type's default.
    pub fn set_call_fuel(&mut self, fuel: u64) {
        self.store.data_mut().call_fuel = fuel;
    }

    /// Size the store's memory ceiling from the Lua-level limit a script
    /// runs under.
    pub fn set_script_memory_limit(&mut self, script_memory_bytes: usize) {
        self.store.data_mut().limiter =
            MemoryLimiter::new(script_memory_ceiling(script_memory_bytes));
    }

    /// Let the library calls this instance started and never waited on finish,
    /// for up to `bound`, then abort what is left. A run that returned asked
    /// for those calls and should not lose them because it did not read the
    /// answer — a fire-and-forget write still lands — but the run is over, so
    /// nothing is read either: each failure, and each call still running at
    /// the bound, is a line in the plugin's log rather than an error.
    pub async fn settle_pending_calls(&mut self, bound: std::time::Duration) {
        let calls = self.store.data_mut().pending_calls.drain();
        if calls.is_empty() {
            return;
        }
        let state = self.store.data();
        let (plugin_id, db, db_backend) =
            (state.plugin_id.clone(), state.db.clone(), state.db_backend);
        let warn = |message: String| {
            crate::plugin::host::log(
                &plugin_id,
                crate::plugin::host::LogLevel::Warn,
                &message,
                db.clone(),
                db_backend,
            )
        };
        let deadline = tokio::time::Instant::now() + bound;
        for (handle, call) in calls {
            match tokio::time::timeout_at(deadline, call).await {
                Ok(joined) => {
                    if let PluginResponse::Err { error } =
                        crate::plugin::host::pending_call_outcome(joined)
                    {
                        warn(format!(
                            "library call {handle} started by this run failed after it returned: {} - {}",
                            error.code, error.message
                        ));
                    }
                }
                // Dropping the timed-out call is what aborts it.
                Err(_) => warn(format!(
                    "library call {handle} started by this run was still running {}s after it returned and was stopped",
                    bound.as_secs()
                )),
            }
        }
    }

    /// Call plugin_info() - no input required
    pub async fn call_plugin_info(&mut self) -> Result<PluginInfo, ExecutionError> {
        self.call_no_input_function("plugin_info").await
    }

    /// Call get_api_surface() on a library plugin.
    pub async fn call_get_api_surface(&mut self) -> Result<ApiSurface, ExecutionError> {
        self.call_no_input_function("get_api_surface").await
    }

    /// Call `execute` on an interpreter with the whole run as one JSON object.
    /// A script that *failed* is an `Ok(ScriptExecuteOutput::Error { .. })`;
    /// an `Err` here means the interpreter itself did not answer.
    pub async fn call_execute(
        &mut self,
        input: &ScriptExecuteInput,
    ) -> Result<ScriptExecuteOutput, ExecutionError> {
        let input = serde_json::to_value(input)
            .map_err(|e| ExecutionError::InvalidResponse(e.to_string()))?;
        self.call_plugin_function("execute", &input).await
    }

    /// Call `validate` on an interpreter.
    pub async fn call_validate(
        &mut self,
        input: &ScriptValidateInput,
    ) -> Result<ScriptValidateOutput, ExecutionError> {
        let input = serde_json::to_value(input)
            .map_err(|e| ExecutionError::InvalidResponse(e.to_string()))?;
        self.call_plugin_function("validate", &input).await
    }

    /// Call an interpreter's optional `get_declarations`. `Ok(None)` when the
    /// module has no such export: an interpreter built before it existed.
    pub async fn call_declarations(
        &mut self,
        input: &happyview_plugin_sdk::declarations::DeclarationsInput,
    ) -> Result<Option<happyview_plugin_sdk::declarations::Declarations>, ExecutionError> {
        const EXPORT: &str = "get_declarations";
        if self
            .instance
            .get_typed_func::<(u32, u32), i64>(&mut self.store, EXPORT)
            .is_err()
        {
            return Ok(None);
        }
        let input = serde_json::to_value(input)
            .map_err(|e| ExecutionError::InvalidResponse(e.to_string()))?;
        self.call_plugin_function(EXPORT, &input).await.map(Some)
    }

    /// Call `call` on a library plugin with `{function, args, context}`.
    pub async fn call_library_function(
        &mut self,
        function: &str,
        args: &[serde_json::Value],
        ctx: &LibraryCallContext,
    ) -> Result<serde_json::Value, ExecutionError> {
        // `LibraryCallInput` is the SDK's owned `CallInput`, so the args are
        // cloned here rather than borrowed; a call's argument list is small and
        // this runs once per call.
        //
        // `db_backend` is filled in here rather than required of every caller:
        // a script runner has no reason to know which backend is live, but a
        // plugin reading `ctx.db_backend` to pick a placeholder style does.
        let mut context = ctx.clone();
        if context.db_backend.is_none() {
            context.db_backend = Some(
                match self.store.data().db_backend {
                    DatabaseBackend::Sqlite => "sqlite",
                    DatabaseBackend::Postgres => "postgres",
                }
                .to_string(),
            );
        }
        let input = serde_json::to_value(LibraryCallInput {
            function: function.to_string(),
            args: args.to_vec(),
            context,
        })
        .map_err(|e| ExecutionError::InvalidResponse(e.to_string()))?;
        self.call_plugin_function("call", &input).await
    }

    /// Generic helper for `() -> i64` exports returning a JSON envelope.
    async fn call_no_input_function<T: serde::de::DeserializeOwned>(
        &mut self,
        name: &str,
    ) -> Result<T, ExecutionError> {
        let func = self
            .instance
            .get_typed_func::<(), i64>(&mut self.store, name)
            .map_err(|_| ExecutionError::MissingExport(name.into()))?;

        self.store.data_mut().limiter.clear_refusal();
        self.store
            .set_fuel(self.store.data().call_fuel)
            .map_err(ExecutionError::Trap)?;

        let packed = func
            .call_async(&mut self.store, ())
            .await
            .map_err(|e| classify_error(self.store.data(), e))?;

        let ptr = (packed >> 32) as u32;
        let len = (packed & 0xFFFFFFFF) as u32;

        let bytes =
            read_from_guest(&self.store, ptr, len).map_err(|_| ExecutionError::MemoryAllocation)?;
        self.release_guest(ptr, len).await;

        let response: PluginResponse<T> = serde_json::from_slice(&bytes)
            .map_err(|e| ExecutionError::InvalidResponse(e.to_string()))?;
        response.into_result().map_err(ExecutionError::from)
    }

    /// Hand the response's bytes back to the guest allocator once they have
    /// been copied out. Best effort: the instance is torn down after the
    /// call, so a failure here frees nothing that dropping the store will not,
    /// and failing the call for it would report an answer already in hand as
    /// an allocation error — or, when the epoch crossed the deadline in the
    /// microseconds since the guest returned, as the wrong kind of one.
    async fn release_guest(&mut self, ptr: u32, len: u32) {
        if let Err(e) = dealloc_guest(&mut self.store, ptr, len).await {
            tracing::debug!(
                plugin = %self.store.data().plugin_id,
                error = %e,
                "guest dealloc failed after the response was read"
            );
        }
    }

    /// Call get_authorize_url(state, redirect_uri, config)
    pub async fn call_get_authorize_url(
        &mut self,
        state: &str,
        redirect_uri: &str,
        config: &serde_json::Value,
    ) -> Result<String, ExecutionError> {
        let input = serde_json::json!({
            "state": state,
            "redirect_uri": redirect_uri,
            "config": config
        });
        self.call_plugin_function("get_authorize_url", &input).await
    }

    /// Call handle_callback with all callback parameters
    ///
    /// For OAuth2: params contains "code" and "state"
    /// For OpenID 2.0: params contains "openid.claimed_id", "openid.identity", etc.
    pub async fn call_handle_callback(
        &mut self,
        params: &HashMap<String, String>,
        config: &serde_json::Value,
    ) -> Result<TokenSet, ExecutionError> {
        // Build input with all params flattened at the top level
        let mut input = serde_json::Map::new();
        for (k, v) in params {
            input.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
        input.insert("config".to_string(), config.clone());

        self.call_plugin_function("handle_callback", &serde_json::Value::Object(input))
            .await
    }

    /// Call refresh_tokens(refresh_token, config)
    pub async fn call_refresh_tokens(
        &mut self,
        refresh_token: &str,
        config: &serde_json::Value,
    ) -> Result<TokenSet, ExecutionError> {
        let input = serde_json::json!({
            "refresh_token": refresh_token,
            "config": config
        });
        self.call_plugin_function("refresh_tokens", &input).await
    }

    /// Call get_profile(access_token, config)
    pub async fn call_get_profile(
        &mut self,
        access_token: &str,
        config: &serde_json::Value,
    ) -> Result<ExternalProfile, ExecutionError> {
        let input = serde_json::json!({
            "access_token": access_token,
            "config": config
        });
        self.call_plugin_function("get_profile", &input).await
    }

    /// Generic helper for plugin functions with input and typed output
    async fn call_plugin_function<T: serde::de::DeserializeOwned>(
        &mut self,
        name: &str,
        input: &serde_json::Value,
    ) -> Result<T, ExecutionError> {
        let input_bytes = serde_json::to_vec(input)
            .map_err(|e| ExecutionError::InvalidResponse(e.to_string()))?;

        let func = self
            .instance
            .get_typed_func::<(u32, u32), i64>(&mut self.store, name)
            .map_err(|_| ExecutionError::MissingExport(name.into()))?;

        self.store.data_mut().limiter.clear_refusal();
        // Placing the input is the host's work, not the call's: it runs on the
        // plugin type's own bound (an interpreter's is the epoch deadline), and
        // the call's fuel is armed once the input is in place. A small budget
        // spent inside the guest's `alloc` would otherwise surface as an
        // allocation failure rather than a timeout.
        self.store
            .set_fuel(fuel_for(self.store.data().plugin_type))
            .map_err(ExecutionError::Trap)?;
        let (input_ptr, input_len) = write_to_guest(&mut self.store, &input_bytes)
            .await
            .map_err(|_| ExecutionError::MemoryAllocation)?;
        self.store
            .set_fuel(self.store.data().call_fuel)
            .map_err(ExecutionError::Trap)?;
        crate::plugin::runtime::grant_fuel_for_bytes(&mut self.store, input_bytes.len());

        let packed = func
            .call_async(&mut self.store, (input_ptr, input_len))
            .await
            .map_err(|e| classify_error(self.store.data(), e))?;

        // Unpack i64: upper 32 bits = ptr, lower 32 bits = len
        let ptr = (packed >> 32) as u32;
        let len = (packed & 0xFFFFFFFF) as u32;

        let bytes =
            read_from_guest(&self.store, ptr, len).map_err(|_| ExecutionError::MemoryAllocation)?;

        self.release_guest(ptr, len).await;

        let response: PluginResponse<T> = serde_json::from_slice(&bytes)
            .map_err(|e| ExecutionError::InvalidResponse(e.to_string()))?;

        response.into_result().map_err(ExecutionError::from)
    }
}

/// The names a script's free-variable read must refuse, for both `execute`
/// and `validate`.
///
/// It is the host's list rather than the caller's, and it replaces whatever a
/// caller sent. The list travels as data so the guard and the codemod stay one
/// source — a separately released interpreter cannot be pinned to the host's —
/// and that property survives only if nothing between the two can substitute a
/// different list. A runner that passed an empty one would weaken every guard
/// it ran with nothing to notice, and step 5 builds several runners.
fn host_removed_globals() -> Vec<String> {
    crate::codemod::REMOVED_GLOBALS
        .iter()
        .map(|name| name.to_string())
        .collect()
}

/// What differs between running an installed interpreter and a compiled
/// module. An interpreter runs under its plugin type's defaults.
#[derive(Default)]
struct RunOptions {
    /// Fuel for each call into the guest, replacing the plugin type's.
    call_fuel: Option<u64>,
    /// The crate a relative panic location is placed under.
    source_root: Option<std::path::PathBuf>,
}

/// Hand the run its artifact only when the build about to load it made it,
/// and then without the source: an interpreter is given one or the other,
/// never both. An artifact from any other build, or a run with no stamp
/// because its module was replaced mid-flight, runs from source.
fn settle_artifact(input: &mut ScriptExecuteInput, stamp: Option<&ArtifactStamp>) {
    let Some(artifact) = &input.artifact else {
        return;
    };
    if stamp.is_some_and(|stamp| stamp.matches(artifact)) {
        input.source.clear();
    } else {
        input.artifact = None;
    }
}

/// Classify a wasmtime error as Timeout, MemoryLimit or Trap. A refused
/// memory growth is read off the store rather than the error, because the
/// guest's reaction to it is an ordinary trap; an interrupt is read first,
/// because a guest that survived a refusal and then ran out of time timed
/// out.
/// How long an interpreter may spend rendering declarations.
const DECLARATIONS_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

fn typed(
    plugin_id: &str,
    surface: &ApiSurface,
) -> Result<happyview_plugin_sdk::declarations::TypedSurface, ExecutionError> {
    happyview_plugin_sdk::declarations::parse_surface(surface).map_err(|e| {
        ExecutionError::BadSurface {
            plugin_id: plugin_id.to_string(),
            message: e.to_string(),
        }
    })
}

fn classify_error(state: &PluginState, e: wasmtime::Error) -> ExecutionError {
    match e.downcast_ref::<wasmtime::Trap>() {
        Some(wasmtime::Trap::Interrupt) | Some(wasmtime::Trap::OutOfFuel) => {
            return ExecutionError::Timeout;
        }
        _ if e.to_string().contains("fuel") => return ExecutionError::Timeout,
        _ => {}
    }
    if let Some(requested) = state.limiter.refused() {
        return ExecutionError::MemoryLimit {
            requested,
            ceiling: state.limiter.ceiling(),
        };
    }
    ExecutionError::Trap(e)
}

/// The `execute` answer for a run whose guest panicked: a runtime error at
/// the panic's line, in the shape an interpreter gives a script error. A
/// relative location is relative to the module's own crate, so it is placed
/// under `source_root` for a terminal to open. An absolute one is a
/// dependency on the build machine, and reaches an API caller, so it keeps
/// only the part from the dependency's `<crate>-<version>` directory on; one
/// with no such directory loses its location and keeps the message.
pub(crate) fn panic_output(
    report: happyview_plugin_sdk::wire::PanicReport,
    source_root: Option<&std::path::Path>,
) -> ScriptExecuteOutput {
    let path = std::path::Path::new(&report.file);
    let file = if is_absolute_anywhere(&report.file) {
        dependency_location(&report.file)
    } else {
        Some(match source_root {
            Some(root) if !path.starts_with(root) => {
                root.join(path).to_string_lossy().replace('\\', "/")
            }
            _ => report.file.clone(),
        })
    };
    let Some(file) = file else {
        return ScriptExecuteOutput::Error {
            kind: crate::plugin::ScriptErrorKind::Runtime,
            raw: format!("panicked: {}", report.message),
            message: report.message,
            line: None,
            chunk: None,
        };
    };
    ScriptExecuteOutput::Error {
        kind: crate::plugin::ScriptErrorKind::Runtime,
        raw: format!(
            "panicked at {file}:{}:{}: {}",
            report.line, report.column, report.message
        ),
        message: report.message,
        line: Some(report.line),
        chunk: Some(file),
    }
}

/// Whether `file` is absolute on the machine that built the module, which
/// need not be this one's platform.
fn is_absolute_anywhere(file: &str) -> bool {
    let bytes = file.as_bytes();
    file.starts_with(['/', '\\'])
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

/// `file` from its last `<crate>-<major>.<minor>.<patch>` directory onwards,
/// the way cargo names an unpacked registry crate.
fn dependency_location(file: &str) -> Option<String> {
    let components: Vec<&str> = file.split(['/', '\\']).collect();
    let crate_dir = components.iter().rposition(|name| {
        name.match_indices('-').any(|(at, _)| {
            let mut parts = name[at + 1..].splitn(3, '.');
            at > 0
                && parts.next().is_some_and(is_number)
                && parts.next().is_some_and(is_number)
                && parts
                    .next()
                    .and_then(|patch| patch.split(['-', '+']).next())
                    .is_some_and(is_number)
        })
    })?;
    // The crate directory alone names no file.
    (crate_dir + 1 < components.len()).then(|| components[crate_dir..].join("/"))
}

fn is_number(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Factory for creating plugin instances
#[derive(Clone)]
pub struct PluginExecutor {
    runtime: Arc<WasmRuntime>,
    registry: Arc<PluginRegistry>,
    db: sqlx::AnyPool,
    db_backend: DatabaseBackend,
    http_client: reqwest::Client,
    lexicons: Arc<LexiconCatalog>,
    encryption_key: Option<[u8; 32]>,
    /// Set only by [`AppState::plugin_executor`](crate::AppState::plugin_executor).
    /// A library reaching a host import that speaks to the wider instance —
    /// today, `host_caller_xrpc_query` run with no session — needs this; the
    /// component handles above are not enough to build a query response on
    /// their own. Everything else on `PluginExecutor` stays a bare handle, so
    /// this is optional rather than a required constructor argument: the
    /// direct-construction call sites (external auth, and the lower-level
    /// tests under `tests/`) have no full `AppState` to give it and don't
    /// exercise that import.
    app_state: Option<crate::AppState>,
}

/// Guest time a test file may spend outside the triggers it fires. Each
/// trigger's own run is bounded by the operator's wall clock, and the time
/// spent inside one is host time for the file, so this bounds only the
/// file's own code: assertions, loops, building inputs. A job fired from a
/// test file gets the same budget, and as much again in waits.
pub const TEST_FILE_GUEST_SECONDS: u64 = 60;

/// The guest deadline a run of `kind` is armed with, `None` meaning none.
/// Running long is what a job is for, and `should_stop` is its stop; every
/// other kind is bounded by the operator's wall clock. A job fired from a
/// test file is bounded anyway: nothing would ever ask it to stop, and the
/// run awaits it.
fn guest_deadline(
    kind: ScriptKind,
    in_test_run: bool,
    wall_clock: std::time::Duration,
) -> Option<std::time::Duration> {
    let test_file = std::time::Duration::from_secs(TEST_FILE_GUEST_SECONDS);
    match kind {
        ScriptKind::Job if in_test_run => Some(test_file),
        ScriptKind::Job => None,
        ScriptKind::Test => Some(test_file),
        ScriptKind::XrpcQuery
        | ScriptKind::XrpcProcedure
        | ScriptKind::RecordEvent
        | ScriptKind::Label => Some(wall_clock),
    }
}

impl PluginExecutor {
    pub fn new(
        runtime: Arc<WasmRuntime>,
        registry: Arc<PluginRegistry>,
        db: sqlx::AnyPool,
        db_backend: DatabaseBackend,
        http_client: reqwest::Client,
        lexicons: Arc<LexiconCatalog>,
    ) -> Self {
        Self {
            runtime,
            registry,
            db,
            db_backend,
            http_client,
            lexicons,
            encryption_key: None,
            app_state: None,
        }
    }

    /// Key for decrypting stored plugin secrets. Without it, library calls
    /// only see `PLUGIN_<ID>_*` environment variables.
    pub fn with_encryption_key(mut self, key: Option<[u8; 32]>) -> Self {
        self.encryption_key = key;
        self
    }

    /// The full instance state, for the host imports that need more than a
    /// database pool and a lexicon registry.
    pub fn with_app_state(mut self, app_state: crate::AppState) -> Self {
        self.app_state = Some(app_state);
        self
    }

    /// Dispatch `function(args)` to library `lib_id` as `ctx`. `depth` is the
    /// number of `host_call_library` hops already taken.
    pub async fn call_library(
        &self,
        lib_id: &str,
        function: &str,
        args: &[serde_json::Value],
        ctx: &LibraryCallContext,
        depth: u8,
    ) -> Result<serde_json::Value, ExecutionError> {
        self.call_library_as(lib_id, function, args, ctx, None, depth)
            .await
    }

    /// As [`call_library`](Self::call_library), lending the library the
    /// caller's credentials. The session rides on the instance rather than on
    /// the arguments, so a library cannot forge one or pass a different user's
    /// along to a library it calls in turn.
    pub async fn call_library_as(
        &self,
        lib_id: &str,
        function: &str,
        args: &[serde_json::Value],
        ctx: &LibraryCallContext,
        caller: Option<Arc<CallerSession>>,
        depth: u8,
    ) -> Result<serde_json::Value, ExecutionError> {
        if depth >= MAX_LIBRARY_CALL_DEPTH {
            return Err(ExecutionError::DepthExceeded);
        }
        // A test run sees every library call, and answers a mocked namespace
        // from data without instantiating the library at all. This is the one
        // dispatch path for a library call, so a mock cannot be bypassed.
        let test = self.app_state.as_ref().and_then(|state| state.test.clone());
        if let Some(test) = &test
            && let Some(namespace) = self
                .registry
                .get(lib_id)
                .await
                .and_then(|plugin| plugin.namespace().map(str::to_string))
        {
            test.record_call(
                &namespace,
                crate::plugin::TestCall {
                    method: function.to_string(),
                    args: serde_json::Value::Array(args.to_vec()),
                },
            );
            let answer = test.mocks().lock().unwrap().answer(&namespace, function);
            if let Some(answer) = answer {
                return answer.map_err(|e| ExecutionError::PluginError {
                    code: e.code,
                    message: e.message,
                    retryable: false,
                });
            }
        }
        let call = async {
            let mut inst = self.instantiate_library(lib_id, ctx, caller, depth).await?;
            let value = inst.call_library_function(function, args, ctx).await?;
            inst.settle_pending_calls(self.script_wall_clock()).await;
            Ok(value)
        };
        match test {
            Some(test) => tokio::time::timeout(test.library_call_limit(), call)
                .await
                .unwrap_or(Err(ExecutionError::Timeout)),
            None => call.await,
        }
    }

    /// Run a script through interpreter `plugin_id`.
    pub async fn execute_script(
        &self,
        plugin_id: &str,
        input: &ScriptExecuteInput,
    ) -> Result<ScriptExecuteOutput, ExecutionError> {
        self.execute_script_as(plugin_id, input, None).await
    }

    /// As [`execute_script`](Self::execute_script), lending the run the
    /// caller's credentials. The session rides on the instance rather than on
    /// the input, so a script cannot forge one and nothing in the guest ever
    /// sees it.
    pub async fn execute_script_as(
        &self,
        plugin_id: &str,
        input: &ScriptExecuteInput,
        caller: Option<Arc<CallerSession>>,
    ) -> Result<ScriptExecuteOutput, ExecutionError> {
        self.execute_script_in(plugin_id, input, caller, ModuleScope::Dashboard)
            .await
    }

    /// As [`execute_script_as`](Self::execute_script_as), with the scope a
    /// relative import resolves in. A project run passes the snapshot it
    /// pinned, so a rebuild mid-run cannot change what its imports see.
    pub async fn execute_script_in(
        &self,
        plugin_id: &str,
        input: &ScriptExecuteInput,
        caller: Option<Arc<CallerSession>>,
        modules: ModuleScope,
    ) -> Result<ScriptExecuteOutput, ExecutionError> {
        let plugin = self.interpreter(plugin_id).await?;
        self.execute_loaded(&plugin, input, caller, modules, RunOptions::default())
            .await
    }

    /// Run a compiled script module. It belongs to the project snapshot that
    /// resolved it rather than to the plugin registry, so it arrives as a
    /// plugin instead of an id, and it runs through the same path an
    /// interpreter does. No interpreter counts instructions inside compiled
    /// code, so the run's budget becomes fuel; `source_root` is the module's
    /// crate, where its panics are placed.
    pub async fn execute_compiled_as(
        &self,
        module: Arc<LoadedPlugin>,
        source_root: &std::path::Path,
        input: &ScriptExecuteInput,
        caller: Option<Arc<CallerSession>>,
    ) -> Result<ScriptExecuteOutput, ExecutionError> {
        let options = RunOptions {
            call_fuel: Some(crate::plugin::runtime::module_fuel(
                input.limits.instructions,
            )),
            source_root: Some(source_root.to_path_buf()),
        };
        // A module's imports are its own crate's, resolved by the compiler;
        // it has nothing to resolve at run time.
        self.execute_loaded(&module, input, caller, ModuleScope::Dashboard, options)
            .await
    }

    /// The runtime this executor compiles and instantiates with.
    pub fn runtime(&self) -> &Arc<WasmRuntime> {
        &self.runtime
    }

    /// Run on this exact module, which is what every stamp the run reads
    /// describes.
    async fn execute_loaded(
        &self,
        plugin: &Arc<LoadedPlugin>,
        input: &ScriptExecuteInput,
        caller: Option<Arc<CallerSession>>,
        modules: ModuleScope,
        options: RunOptions,
    ) -> Result<ScriptExecuteOutput, ExecutionError> {
        let mut inst = self
            .instantiate_interpreter(plugin, Some(input), caller, modules)
            .await?;
        // The guard list is the host's, whatever the caller sent: see
        // `host_removed_globals`.
        let mut input = ScriptExecuteInput {
            removed_globals: host_removed_globals(),
            ..input.clone()
        };
        let stamp = inst
            .store
            .data()
            .script_run
            .as_ref()
            .and_then(|run| run.stamp.clone());
        settle_artifact(&mut input, stamp.as_ref());
        let input = &input;
        inst.set_script_memory_limit(
            usize::try_from(input.limits.memory_bytes).unwrap_or(usize::MAX),
        );
        if let Some(fuel) = options.call_fuel {
            inst.set_call_fuel(fuel);
        }
        // Re-armed here so a run starts with a whole budget rather than what
        // instantiation left.
        match guest_deadline(input.kind, self.in_test_run(), self.script_wall_clock()) {
            Some(budget) => inst.arm_guest_deadline(budget),
            None => inst.lift_guest_deadline(),
        }
        // A run that failed takes its outstanding calls down with it when
        // `inst` drops; only one that answered waits for them.
        let output = match inst.call_execute(input).await {
            Ok(output) => output,
            // A guest that reported a panic answered the run; the trap is only
            // how an aborting guest stops. A timeout or a refused growth after
            // a report is still that failure: the guest did not stop by
            // aborting.
            Err(ExecutionError::Trap(e)) => {
                return match inst.take_panic() {
                    Some(report) => Ok(panic_output(report, options.source_root.as_deref())),
                    None => Err(ExecutionError::Trap(e)),
                };
            }
            Err(e) => return Err(e),
        };
        inst.settle_pending_calls(self.script_wall_clock()).await;
        Ok(output)
    }

    /// Ask interpreter `plugin_id` whether `source` is a script it can run.
    /// Validation reads no run, so it carries no context and no session.
    pub async fn validate_script(
        &self,
        plugin_id: &str,
        source: &str,
    ) -> Result<ScriptValidateOutput, ExecutionError> {
        let plugin = self.interpreter(plugin_id).await?;
        let mut inst = self
            .instantiate_interpreter(&plugin, None, None, ModuleScope::Dashboard)
            .await?;
        inst.call_validate(&ScriptValidateInput {
            source: source.to_string(),
            removed_globals: host_removed_globals(),
            ..ScriptValidateInput::default()
        })
        .await
    }

    /// Ask interpreter `plugin` to render `input` as editor declarations.
    /// Rendering reads no run, so it gets no context and no session, and a
    /// fixed deadline of its own rather than a script's.
    pub async fn declarations_for(
        &self,
        plugin: &Arc<LoadedPlugin>,
        input: &happyview_plugin_sdk::declarations::DeclarationsInput,
    ) -> Result<Option<happyview_plugin_sdk::declarations::Declarations>, ExecutionError> {
        let mut inst = self
            .instantiate_interpreter(plugin, None, None, ModuleScope::Dashboard)
            .await?;
        inst.arm_guest_deadline(DECLARATIONS_DEADLINE);
        inst.call_declarations(input).await
    }

    /// Validate `source` as `chunk_name` and keep what the interpreter
    /// prepared from it. The artifact comes back stamped with this
    /// interpreter's build, and is dropped when the script is not valid: an
    /// artifact of a script that cannot run is nothing a run should load.
    /// Stamped from the module that prepared it, so one replaced mid-flight
    /// yields no artifact rather than one labelled as its successor's.
    pub async fn prepare_script(
        &self,
        plugin_id: &str,
        source: &str,
        chunk_name: &str,
        role: ChunkRole,
    ) -> Result<ScriptValidateOutput, ExecutionError> {
        let plugin = self.interpreter(plugin_id).await?;
        self.prepare_loaded(&plugin, source, chunk_name, role).await
    }

    /// As [`prepare_script`](Self::prepare_script), keeping the result in the
    /// prepared cache against `source`. The module that prepares is the one
    /// the entry is stored for, so a re-registration landing in between
    /// stores nothing rather than filing one build's artifact under the next.
    /// The output's artifact is what the cache now holds: `None` when the
    /// interpreter returned none, the script is not valid, or the module was
    /// superseded.
    pub async fn prepare_and_store(
        &self,
        plugin_id: &str,
        source: &str,
        chunk_name: &str,
        role: ChunkRole,
    ) -> Result<ScriptValidateOutput, ExecutionError> {
        self.prepare_and_store_if(plugin_id, source, chunk_name, role, |_| true)
            .await
    }

    /// As [`prepare_and_store`](Self::prepare_and_store), storing only when
    /// `keep` accepts what the interpreter answered, for a caller that will
    /// refuse some valid bodies and must not leave an entry for one. When
    /// nothing is stored the output's artifact is `None`.
    pub async fn prepare_and_store_if(
        &self,
        plugin_id: &str,
        source: &str,
        chunk_name: &str,
        role: ChunkRole,
        keep: impl FnOnce(&ScriptValidateOutput) -> bool,
    ) -> Result<ScriptValidateOutput, ExecutionError> {
        let plugin = self.interpreter(plugin_id).await?;
        let mut out = self
            .prepare_loaded(&plugin, source, chunk_name, role)
            .await?;
        out.artifact = if keep(&out) {
            self.registry
                .store_prepared(&plugin, source, out.artifact.take())
                .as_deref()
                .cloned()
        } else {
            None
        };
        Ok(out)
    }

    /// Prepare on this exact module and stamp the result for it alone.
    async fn prepare_loaded(
        &self,
        plugin: &Arc<LoadedPlugin>,
        source: &str,
        chunk_name: &str,
        role: ChunkRole,
    ) -> Result<ScriptValidateOutput, ExecutionError> {
        let mut inst = self
            .instantiate_interpreter(plugin, None, None, ModuleScope::Dashboard)
            .await?;
        let mut out = inst
            .call_validate(&ScriptValidateInput {
                source: source.to_string(),
                removed_globals: host_removed_globals(),
                chunk_name: Some(chunk_name.to_string()),
                prepare: true,
                role,
            })
            .await?;
        let stamp = self.registry.artifact_stamp_for(plugin);
        out.artifact = match (out.valid, out.artifact.take(), stamp) {
            (true, Some(mut artifact), Some(stamp)) => {
                stamp.apply(&mut artifact);
                Some(artifact)
            }
            _ => None,
        };
        Ok(out)
    }

    /// A library's API surface, computed once per registration.
    pub async fn api_surface(&self, lib_id: &str) -> Result<Arc<ApiSurface>, ExecutionError> {
        if let Some(cached) = self.registry.cached_api_surface(lib_id).await {
            return Ok(cached);
        }
        let mut inst = self
            .instantiate_library(lib_id, &LibraryCallContext::default(), None, 0)
            .await?;
        let surface = inst.call_get_api_surface().await?;
        typed(lib_id, &surface)?;
        let surface = Arc::new(surface);
        self.registry
            .cache_api_surface(lib_id, surface.clone())
            .await;
        Ok(surface)
    }

    /// A library's surface read into the type model. Only a surface that
    /// parses is ever cached, so this parse is the cheap second one.
    pub async fn typed_surface(
        &self,
        lib_id: &str,
    ) -> Result<happyview_plugin_sdk::declarations::TypedSurface, ExecutionError> {
        typed(lib_id, &*self.api_surface(lib_id).await?)
    }

    /// Read the surface of a library that is not installed yet, so an install
    /// can refuse one that does not parse. A module without
    /// `get_api_surface` is `Ok(None)`: it has nothing to describe, and
    /// refusing it is not this check's business.
    pub async fn surface_of(
        &self,
        plugin: &Arc<LoadedPlugin>,
    ) -> Result<Option<happyview_plugin_sdk::declarations::TypedSurface>, ExecutionError> {
        let mut inst = self
            .instantiate_loaded(plugin, "library", HashMap::new(), serde_json::Value::Null)
            .await?;
        match inst.call_get_api_surface().await {
            Ok(surface) => typed(&plugin.info.id, &surface).map(Some),
            Err(ExecutionError::MissingExport(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Every installed library with its surface. Libraries whose surface
    /// cannot be read are logged and skipped rather than failing the script.
    pub async fn library_index(&self) -> Vec<LibraryEntry> {
        let mut out = Vec::new();
        for plugin in self.registry.list_by_type(PluginType::Library).await {
            let Some(namespace) = plugin.namespace() else {
                continue;
            };
            match self.api_surface(&plugin.info.id).await {
                Ok(surface) => out.push(LibraryEntry {
                    id: plugin.info.id.clone(),
                    namespace: namespace.to_string(),
                    surface,
                }),
                Err(e) => {
                    tracing::error!(plugin_id = %plugin.info.id, error = %e, "library API surface unavailable")
                }
            }
        }
        out.sort_by(|a, b| a.namespace.cmp(&b.namespace));
        out
    }

    async fn instantiate_library(
        &self,
        lib_id: &str,
        ctx: &LibraryCallContext,
        caller: Option<Arc<CallerSession>>,
        depth: u8,
    ) -> Result<PluginInstance, ExecutionError> {
        let plugin = self
            .registry
            .get(lib_id)
            .await
            .ok_or_else(|| ExecutionError::PluginNotFound(lib_id.to_string()))?;
        if plugin.plugin_type() != PluginType::Library {
            return Err(ExecutionError::NotALibrary(lib_id.to_string()));
        }
        let secrets = load_plugin_secrets(
            &self.db,
            self.db_backend,
            self.encryption_key.as_ref(),
            lib_id,
        )
        .await;
        let scope = ctx
            .caller_did
            .clone()
            .unwrap_or_else(|| "library".to_string());
        let mut inst = self
            .instantiate(lib_id, &scope, secrets, serde_json::Value::Null)
            .await?;
        inst.store.data_mut().call_ctx = ctx.clone();
        inst.store.data_mut().depth = depth;
        inst.store.data_mut().caller = caller;
        Ok(inst)
    }

    /// Sibling of [`instantiate_library`](Self::instantiate_library) for an
    /// interpreter: the same call context, session and depth, so
    /// `host_call_library` forwards the same identity from a script as from
    /// anywhere else, plus the run the five `script:host` imports act on.
    ///
    /// No plugin secrets are loaded. An interpreter has no configuration of
    /// its own — `ctx.env` is the script's and travels in the input — and a
    /// secrets query per script run would be on the hottest path there is.
    ///
    /// Takes the module rather than its id, and the scope its relative
    /// imports resolve in: the run is stamped from the very module it
    /// instantiates, so a re-registration landing in between cannot pair one
    /// build's stamp with another's code.
    async fn instantiate_interpreter(
        &self,
        plugin: &Arc<LoadedPlugin>,
        input: Option<&ScriptExecuteInput>,
        caller: Option<Arc<CallerSession>>,
        modules: ModuleScope,
    ) -> Result<PluginInstance, ExecutionError> {
        let ctx = input
            .map(|input| LibraryCallContext {
                caller_did: input.context.caller_did.clone(),
                has_pds_auth: input.context.has_pds_auth,
                db_backend: None,
            })
            .unwrap_or_default();
        let scope = ctx
            .caller_did
            .clone()
            .unwrap_or_else(|| "script".to_string());
        let language_id = plugin.language_id().unwrap_or_default().to_string();
        let extensions: Vec<String> = plugin
            .extensions()
            .into_iter()
            .map(str::to_string)
            .collect();
        let stamp = self.registry.artifact_stamp_for(plugin);
        let mut inst = self
            .instantiate_loaded(plugin, &scope, HashMap::new(), serde_json::Value::Null)
            .await?;
        let state = inst.store.data_mut();
        state.call_ctx = ctx;
        state.depth = 0;
        state.caller = caller;
        // The run's job is whatever the input carries, independently of its
        // kind: only the host builds this input, so a guest can forge neither,
        // and a caller that sets `context.job` on a kind other than `job`
        // hands the three job controls a job that kind has no business
        // touching. Keeping the two in agreement is the caller's.
        state.script_run = input.map(|input| ScriptRun {
            trigger_id: input.context.trigger.clone(),
            caller_did: input.context.caller_did.clone(),
            job_id: input.context.job.as_ref().map(|job| job.id.clone()),
            lease_lost: input
                .context
                .job
                .as_ref()
                .and_then(|job| crate::jobs::held::current(&job.id))
                .map(|held| held.lost),
            test: input.kind == ScriptKind::Test,
            language_id,
            extensions,
            stamp,
            modules,
            waited_ms: Default::default(),
        });
        Ok(inst)
    }

    /// The interpreter registered as `plugin_id`, fetched once so everything
    /// a run derives from it — its stamp, its claims, its code — describes
    /// one build.
    async fn interpreter(&self, plugin_id: &str) -> Result<Arc<LoadedPlugin>, ExecutionError> {
        let plugin = self
            .registry
            .get(plugin_id)
            .await
            .ok_or_else(|| ExecutionError::PluginNotFound(plugin_id.to_string()))?;
        if plugin.plugin_type() != PluginType::Interpreter {
            return Err(ExecutionError::NotAnInterpreter(plugin_id.to_string()));
        }
        Ok(plugin)
    }

    /// The per-run execution budget an interpreter store is armed with. The
    /// operator's setting when this executor carries the instance, and the
    /// default otherwise, which is what the direct-construction call sites
    /// have.
    fn script_wall_clock(&self) -> std::time::Duration {
        self.app_state
            .as_ref()
            .map(|state| state.script_limits.wall_clock())
            .unwrap_or(std::time::Duration::from_secs(u64::from(
                crate::lua::limits::DEFAULT_WALL_CLOCK_SECONDS,
            )))
    }

    fn in_test_run(&self) -> bool {
        self.app_state
            .as_ref()
            .is_some_and(|state| state.test.is_some())
    }

    /// The capability set a plugin will be granted at instantiation.
    pub fn effective_capabilities(
        plugin: &LoadedPlugin,
    ) -> Result<HashSet<PluginCapability>, ExecutionError> {
        capabilities::effective_set(plugin).map_err(ExecutionError::InvalidResponse)
    }

    /// The host functions, plus preview 1 only when the module's import
    /// section names it, so a `wasm32-unknown-unknown` plugin's linker
    /// defines nothing but `env`.
    fn build_linker(
        &self,
        module: &wasmtime::Module,
    ) -> Result<Linker<PluginState>, ExecutionError> {
        let mut linker = Linker::new(self.runtime.engine());
        register_host_functions(&mut linker)
            .map_err(|e| ExecutionError::Instantiation(e.into()))?;
        if module
            .imports()
            .any(|import| import.module() == capabilities::WASI_MODULE)
        {
            wasmtime_wasi::p1::add_to_linker_async(&mut linker, |state| &mut state.wasi)
                .map_err(|e| ExecutionError::Instantiation(e.into()))?;
        }
        Ok(linker)
    }

    /// The store state for one instance of `plugin`.
    async fn build_state(
        &self,
        plugin: &LoadedPlugin,
        scope: &str,
        secrets: HashMap<String, String>,
        config: serde_json::Value,
    ) -> Result<PluginState, ExecutionError> {
        let plugin_id = plugin.info.id.as_str();
        let capabilities = Self::effective_capabilities(plugin)?;
        // `network:request:defined` sources its hosts from the operator's
        // plugin settings, not the manifest — the two capabilities are
        // mutually exclusive, so only one of these ever applies.
        let allowed_hosts = if capabilities.contains(&PluginCapability::NetworkRequestDefined) {
            crate::plugin::config::load_allowed_hosts(&self.db, self.db_backend, plugin_id)
                .await
                .map_err(|e| ExecutionError::Instantiation(e.into()))?
        } else if capabilities.contains(&PluginCapability::NetworkRequest) {
            plugin.allowed_hosts().to_vec()
        } else {
            Vec::new()
        };

        Ok(PluginState {
            plugin_id: plugin_id.to_string(),
            scope: scope.to_string(),
            secrets,
            config,
            db: Some(self.db.clone()),
            db_backend: self.db_backend,
            http_client: self.http_client.clone(),
            lexicons: self.lexicons.clone(),
            usage: Default::default(),
            memory: None,
            alloc: None,
            dealloc: None,
            capabilities,
            allowed_hosts,
            plugin_type: plugin.plugin_type(),
            executor: Some(self.clone()),
            call_ctx: LibraryCallContext::default(),
            depth: 0,
            caller: None,
            script_run: None,
            app_state: self.app_state.clone(),
            wasi: crate::plugin::host::build_wasi_context(
                plugin_id,
                Some(self.db.clone()),
                self.db_backend,
            ),
            deadline: GuestDeadline::unbounded(self.runtime.epoch()),
            limiter: MemoryLimiter::new(memory_ceiling(plugin.plugin_type())),
            pending_calls: Default::default(),
            panic: None,
            call_fuel: fuel_for(plugin.plugin_type()),
        })
    }

    /// Instantiate a plugin with the given scope
    pub async fn instantiate(
        &self,
        plugin_id: &str,
        scope: &str,
        secrets: HashMap<String, String>,
        config: serde_json::Value,
    ) -> Result<PluginInstance, ExecutionError> {
        let plugin = self
            .registry
            .get(plugin_id)
            .await
            .ok_or_else(|| ExecutionError::PluginNotFound(plugin_id.to_string()))?;
        self.instantiate_loaded(&plugin, scope, secrets, config)
            .await
    }

    /// Instantiate this exact module, for a caller that has already read
    /// something from it and must run the build it read.
    async fn instantiate_loaded(
        &self,
        plugin: &Arc<LoadedPlugin>,
        scope: &str,
        secrets: HashMap<String, String>,
        config: serde_json::Value,
    ) -> Result<PluginInstance, ExecutionError> {
        let module = self
            .runtime
            .module_for(plugin)
            .await
            .map_err(ExecutionError::Instantiation)?;

        let linker = self.build_linker(&module)?;
        let state = self.build_state(plugin, scope, secrets, config).await?;

        let mut store = Store::new(self.runtime.engine(), state);
        store.limiter(|state| &mut state.limiter);
        // Instantiation itself burns fuel where the data image is copied
        // rather than mapped, and each call refuels before it starts, so a
        // library's first call never begins with a budget instantiation
        // pre-spent.
        store
            .set_fuel(fuel_for(plugin.plugin_type()))
            .map_err(|e| ExecutionError::Instantiation(e.into()))?;
        // A store without a deadline traps on its first instruction, and
        // instantiation already runs compiled code. Library and auth stores
        // stay bounded by fuel.
        store.set_epoch_deadline(UNBOUNDED_TICKS);
        // Fuel does not bound an interpreter, so its store leaves here
        // already under the operator's wall clock; a runner re-arms per run
        // and the job runner lifts it. A runner that forgets fails closed.
        // Arming before the module is instantiated covers its start section
        // and its `_initialize`, which are guest code with no runner around
        // them to interrupt a loop.
        if plugin.plugin_type() == PluginType::Interpreter {
            let ticks = GuestDeadline::ticks_for(self.script_wall_clock());
            let delta = store.data_mut().deadline.arm(ticks);
            store.set_epoch_deadline(delta);
        }

        // Instantiate module
        let instance = linker
            .instantiate_async(&mut store, &module)
            .await
            .map_err(|e| ExecutionError::Instantiation(e.into()))?;

        // A reactor's constructors run from `_initialize`, which the linker
        // does not call; a guest allocator that has not run hands back null
        // on the first host write. Its failure is classified rather than
        // wrapped, so an initializer that spins reads as the timeout it is.
        if let Ok(initialize) = instance.get_typed_func::<(), ()>(&mut store, "_initialize") {
            initialize
                .call_async(&mut store, ())
                .await
                .map_err(|e| classify_error(store.data(), e))?;
        }

        // Get memory export
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| ExecutionError::MissingExport("memory".into()))?;

        // Get alloc/dealloc exports
        let alloc = instance
            .get_typed_func::<u32, u32>(&mut store, "alloc")
            .map_err(|_| ExecutionError::MissingExport("alloc".into()))?;
        let dealloc = instance
            .get_typed_func::<(u32, u32), ()>(&mut store, "dealloc")
            .map_err(|_| ExecutionError::MissingExport("dealloc".into()))?;

        // Store memory/alloc/dealloc in state
        store.data_mut().memory = Some(memory);
        store.data_mut().alloc = Some(alloc.clone());
        store.data_mut().dealloc = Some(dealloc.clone());

        Ok(PluginInstance {
            store,
            instance,
            memory,
            alloc,
            dealloc,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execution_error_plugin_not_found() {
        let err = ExecutionError::PluginNotFound("steam".into());
        assert!(err.to_string().contains("steam"));
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_execution_error_timeout() {
        let err = ExecutionError::Timeout;
        assert!(
            err.to_string().to_lowercase().contains("timeout") || err.to_string().contains("fuel")
        );
    }

    #[test]
    fn test_plugin_error_conversion() {
        let plugin_err = PluginEnvelopeError {
            code: "AUTH_FAILED".into(),
            message: "Bad token".into(),
            retryable: true,
        };
        let exec_err: ExecutionError = plugin_err.into();
        match exec_err {
            ExecutionError::PluginError {
                code,
                message,
                retryable,
            } => {
                assert_eq!(code, "AUTH_FAILED");
                assert_eq!(message, "Bad token");
                assert!(retryable);
            }
            _ => panic!("Wrong error variant"),
        }
    }

    #[test]
    fn test_all_error_variants_have_display() {
        let errors: Vec<ExecutionError> = vec![
            ExecutionError::PluginNotFound("test".into()),
            ExecutionError::MemoryAllocation,
            ExecutionError::InvalidResponse("bad json".into()),
            ExecutionError::ResourceLimit("too many requests".into()),
            ExecutionError::Timeout,
            ExecutionError::MissingExport("plugin_info".into()),
        ];
        for err in errors {
            assert!(!err.to_string().is_empty());
        }
    }

    #[test]
    fn test_plugin_executor_new_signature() {
        // Verify PluginExecutor::new exists with expected signature (compile-time check)
        fn _check_signature(
            _runtime: std::sync::Arc<crate::plugin::WasmRuntime>,
            _registry: std::sync::Arc<crate::plugin::PluginRegistry>,
            _db: sqlx::AnyPool,
            _db_backend: crate::db::DatabaseBackend,
            _http_client: reqwest::Client,
            _lexicons: std::sync::Arc<crate::catalog::LexiconCatalog>,
        ) -> PluginExecutor {
            PluginExecutor::new(
                _runtime,
                _registry,
                _db,
                _db_backend,
                _http_client,
                _lexicons,
            )
        }
    }

    fn stamped_input(source: &str, version: &str) -> ScriptExecuteInput {
        ScriptExecuteInput {
            source: source.to_string(),
            artifact: Some(crate::plugin::PreparedArtifact {
                interpreter: "lua".into(),
                version: version.into(),
                engine: "puc-lua-5.4.7".into(),
                format: "lua-bytecode".into(),
                bytes: vec![1, 2, 3],
                source_map: None,
            }),
            kind: ScriptKind::XrpcQuery,
            input: serde_json::Value::Null,
            context: Default::default(),
            libraries: Vec::new(),
            limits: crate::plugin::ScriptExecuteLimits {
                instructions: None,
                memory_bytes: 8 * 1024 * 1024,
            },
            removed_globals: Vec::new(),
            chunk_name: crate::plugin::DEFAULT_CHUNK_NAME.to_string(),
        }
    }

    #[test]
    fn a_matching_artifact_replaces_the_source_and_any_other_is_dropped() {
        let stamp = ArtifactStamp {
            interpreter: "lua".into(),
            version: "1.0.0+abc".into(),
        };

        let mut ours = stamped_input("return 1", "1.0.0+abc");
        settle_artifact(&mut ours, Some(&stamp));
        assert_eq!(ours.source, "");
        assert!(ours.artifact.is_some());

        let mut theirs = stamped_input("return 1", "1.0.0+def");
        settle_artifact(&mut theirs, Some(&stamp));
        assert_eq!(theirs.source, "return 1");
        assert_eq!(theirs.artifact, None);

        let mut unstamped = stamped_input("return 1", "1.0.0+abc");
        settle_artifact(&mut unstamped, None);
        assert_eq!(unstamped.source, "return 1");
        assert_eq!(unstamped.artifact, None);
    }

    #[test]
    fn test_plugin_instance_struct_exists() {
        // Verify PluginInstance struct has expected fields (compile-time check)
        fn _check_fields(instance: PluginInstance) {
            let _ = instance.store;
            let _ = instance.instance;
            let _ = instance.memory;
            let _ = instance.alloc;
            let _ = instance.dealloc;
        }
    }

    #[test]
    fn test_plugin_instance_has_expected_methods() {
        // Compile-time check that methods exist with expected signatures
        fn _check_call_plugin_info<'a>(
            inst: &'a mut PluginInstance,
        ) -> impl std::future::Future<Output = Result<crate::plugin::PluginInfo, ExecutionError>> + 'a
        {
            inst.call_plugin_info()
        }

        fn _check_call_get_authorize_url<'a>(
            inst: &'a mut PluginInstance,
            state: &'a str,
            redirect_uri: &'a str,
            config: &'a serde_json::Value,
        ) -> impl std::future::Future<Output = Result<String, ExecutionError>> + 'a {
            inst.call_get_authorize_url(state, redirect_uri, config)
        }

        fn _check_call_handle_callback<'a>(
            inst: &'a mut PluginInstance,
            params: &'a HashMap<String, String>,
            config: &'a serde_json::Value,
        ) -> impl std::future::Future<Output = Result<crate::plugin::TokenSet, ExecutionError>> + 'a
        {
            inst.call_handle_callback(params, config)
        }

        fn _check_call_refresh_tokens<'a>(
            inst: &'a mut PluginInstance,
            refresh_token: &'a str,
            config: &'a serde_json::Value,
        ) -> impl std::future::Future<Output = Result<crate::plugin::TokenSet, ExecutionError>> + 'a
        {
            inst.call_refresh_tokens(refresh_token, config)
        }

        fn _check_call_get_profile<'a>(
            inst: &'a mut PluginInstance,
            access_token: &'a str,
            config: &'a serde_json::Value,
        ) -> impl std::future::Future<Output = Result<crate::plugin::ExternalProfile, ExecutionError>> + 'a
        {
            inst.call_get_profile(access_token, config)
        }
    }
}

/// The preview-1 probe fixture against the empty context: what the three
/// gated imports answer, where `fd_write` lands, and that a reactor's
/// `_initialize` runs before the host's first write.
#[cfg(test)]
mod wasi {
    use super::*;
    use crate::plugin::runtime::DEFAULT_FUEL;
    use crate::plugin::{LoadedPlugin, loader};
    use crate::test_support::migrated_memory_pool;

    const TEST_LIBRARY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/test_library");

    /// The probe's directory, or `None` when it is unbuilt and the test is to
    /// skip.
    fn probe_dir() -> Option<std::path::PathBuf> {
        loader::built_fixture("wasi_probe", "wasm32-wasip1")
    }

    async fn probe(dir: &std::path::Path) -> LoadedPlugin {
        loader::load_from_file(dir)
            .await
            .expect("the probe should load")
    }

    async fn executor(db: sqlx::AnyPool, plugins: Vec<LoadedPlugin>) -> PluginExecutor {
        let registry = Arc::new(PluginRegistry::new());
        for plugin in plugins {
            registry.register(plugin).await;
        }
        PluginExecutor::new(
            Arc::new(WasmRuntime::new().unwrap()),
            registry,
            db,
            DatabaseBackend::Sqlite,
            reqwest::Client::new(),
            Arc::new(LexiconCatalog::detached()),
        )
    }

    async fn call(inst: &mut PluginInstance, name: &str, arg: Option<i32>) -> serde_json::Value {
        let packed = match arg {
            None => {
                let f = inst
                    .instance
                    .get_typed_func::<(), i64>(&mut inst.store, name)
                    .unwrap();
                f.call_async(&mut inst.store, ()).await.unwrap()
            }
            Some(arg) => {
                let f = inst
                    .instance
                    .get_typed_func::<i32, i64>(&mut inst.store, name)
                    .unwrap();
                f.call_async(&mut inst.store, arg).await.unwrap()
            }
        };
        assert_ne!(packed, 0, "{name} returned no envelope");
        let (ptr, len) = ((packed >> 32) as u32, (packed & 0xFFFF_FFFF) as u32);
        let bytes = read_from_guest(&inst.store, ptr, len).unwrap();
        let envelope: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        envelope["ok"].clone()
    }

    #[tokio::test]
    async fn the_probe_reads_a_plausible_clock_and_fresh_randomness() {
        let Some(dir) = probe_dir() else { return };
        let executor = executor(migrated_memory_pool().await, vec![probe(&dir).await]).await;
        let mut inst = executor
            .instantiate(
                "wasi_probe",
                "test",
                HashMap::new(),
                serde_json::Value::Null,
            )
            .await
            .unwrap();

        let ns = call(&mut inst, "clock_ns", None).await;
        let ns = ns.as_u64().unwrap_or_else(|| panic!("clock errno: {ns}"));
        // 2020-01-01T00:00:00Z in nanoseconds.
        assert!(ns > 1_577_836_800_000_000_000, "{ns}");

        let first = call(&mut inst, "random_hex", None).await;
        let second = call(&mut inst, "random_hex", None).await;
        assert_eq!(first.as_str().unwrap().len(), 16, "{first}");
        assert_ne!(first, second);
        assert_ne!(first.as_str().unwrap(), "0000000000000000");
    }

    /// Standard output is the plugin log; a descriptor the context never
    /// opened answers `EBADF` (8) and the guest keeps running.
    #[tokio::test]
    async fn fd_write_lands_in_the_plugin_log_and_an_unknown_descriptor_fails_cleanly() {
        let Some(dir) = probe_dir() else { return };
        let db = migrated_memory_pool().await;
        let executor = executor(db.clone(), vec![probe(&dir).await]).await;
        let mut inst = executor
            .instantiate(
                "wasi_probe",
                "test",
                HashMap::new(),
                serde_json::Value::Null,
            )
            .await
            .unwrap();

        let stdout = call(&mut inst, "write_line", Some(1)).await;
        assert_eq!(stdout["errno"], 0, "{stdout}");
        assert_eq!(stdout["written"], "probe says hello\n".len(), "{stdout}");

        let unopened = call(&mut inst, "write_line", Some(3)).await;
        assert_eq!(unopened["errno"], 8, "{unopened}");
        assert_eq!(unopened["written"], 0, "{unopened}");

        // The log row is written by a spawned task.
        let mut rows: Vec<(String, String)> = Vec::new();
        for _ in 0..50 {
            rows = crate::db::query_as(
                "SELECT subject, detail FROM happyview_event_logs WHERE event_type = 'plugin.log'",
            )
            .fetch_all(&db)
            .await
            .unwrap();
            if !rows.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].0, "wasi_probe");
        let detail: serde_json::Value = serde_json::from_str(&rows[0].1).unwrap();
        assert_eq!(detail["message"], "probe says hello");
        assert_eq!(detail["level"], "info");
    }

    /// The probe's allocator returns null until `_initialize` has run, so
    /// an executor that skipped the initializer could not read a single
    /// envelope from it.
    #[tokio::test]
    async fn initialize_runs_before_the_first_host_write() {
        let Some(dir) = probe_dir() else { return };
        let plugin = probe(&dir).await;
        let executor = executor(migrated_memory_pool().await, vec![probe(&dir).await]).await;
        let module = executor.runtime.compile(&plugin.wasm_bytes).unwrap();

        // Without the initializer the fixture's `alloc` is inert.
        let linker = executor.build_linker(&module).unwrap();
        let state = executor
            .build_state(&plugin, "test", HashMap::new(), serde_json::Value::Null)
            .await
            .unwrap();
        let mut store = Store::new(executor.runtime.engine(), state);
        store.set_fuel(DEFAULT_FUEL).unwrap();
        store.set_epoch_deadline(UNBOUNDED_TICKS);
        let bare = linker.instantiate_async(&mut store, &module).await.unwrap();
        let alloc = bare
            .get_typed_func::<u32, u32>(&mut store, "alloc")
            .unwrap();
        assert_eq!(alloc.call_async(&mut store, 16).await.unwrap(), 0);

        // Through `instantiate` the initializer has run and every envelope
        // comes back.
        let mut inst = executor
            .instantiate(
                "wasi_probe",
                "test",
                HashMap::new(),
                serde_json::Value::Null,
            )
            .await
            .unwrap();
        assert_ne!(inst.alloc.call_async(&mut inst.store, 16).await.unwrap(), 0);
        let ns = call(&mut inst, "clock_ns", None).await;
        assert!(ns.is_u64(), "{ns}");
    }

    /// A module that imports nothing from preview 1 gets a linker that
    /// defines nothing from it.
    #[tokio::test]
    async fn preview1_is_linked_only_for_modules_that_import_it() {
        let Some(dir) = probe_dir() else { return };
        if loader::built_fixture("test_library", "wasm32-unknown-unknown").is_none() {
            return;
        }
        let plugin = probe(&dir).await;
        let executor = executor(migrated_memory_pool().await, vec![probe(&dir).await]).await;
        let runtime = &executor.runtime;
        let state = || async {
            executor
                .build_state(&plugin, "test", HashMap::new(), serde_json::Value::Null)
                .await
                .unwrap()
        };

        let library = std::fs::read(format!(
            "{TEST_LIBRARY}/target/wasm32-unknown-unknown/release/test_library.wasm"
        ))
        .expect("test_library fixture not built");
        let module = runtime.compile(&library).unwrap();
        let linker = executor.build_linker(&module).unwrap();
        let mut store = Store::new(runtime.engine(), state().await);
        let modules: HashSet<String> = linker
            .iter(&mut store)
            .map(|(module, _, _)| module.to_string())
            .collect();
        assert_eq!(modules, HashSet::from(["env".to_string()]));

        let module = runtime.compile(&plugin.wasm_bytes).unwrap();
        let linker = executor.build_linker(&module).unwrap();
        let mut store = Store::new(runtime.engine(), state().await);
        let names: HashSet<(String, String)> = linker
            .iter(&mut store)
            .map(|(module, name, _)| (module.to_string(), name.to_string()))
            .collect();
        for name in ["clock_time_get", "random_get", "fd_write", "path_open"] {
            assert!(
                names.contains(&("wasi_snapshot_preview1".to_string(), name.to_string())),
                "{name}"
            );
        }
    }
}

/// Epoch interruption as the guest's clock: a spinning guest is stopped at
/// its budget, a guest waiting on the host is not charged for the wait, and
/// every async import goes through the wrapper that keeps those books.
#[cfg(test)]
mod deadline {
    use super::*;
    use crate::plugin::host::{HostImports, define_host_functions};
    use crate::plugin::{LoadedPlugin, PluginManifest, PluginSource};
    use std::time::{Duration, Instant};

    const GUEST: &str = r#"(module
        (import "env" "test_sleep" (func $sleep (param i32) (result i64)))
        (memory (export "memory") 1)
        (func (export "alloc") (param i32) (result i32) i32.const 1024)
        (func (export "dealloc") (param i32 i32))
        (func (export "spin") (loop br 0))
        (func (export "spin_n") (param i32) (local i32)
            (loop
                local.get 1
                i32.const 1
                i32.add
                local.tee 1
                local.get 0
                i32.lt_u
                br_if 0))
        (func (export "sleep_then_return") (param i32) (result i64)
            local.get 0
            call $sleep)
        (func (export "sleep_then_spin") (param i32)
            local.get 0
            call $sleep
            drop
            (loop br 0))
    )"#;

    fn guest(id: &str, wat: &str) -> LoadedPlugin {
        let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "version": "1.0.0", "api_version": "2",
            "plugin_type": "library", "namespace": id, "capabilities": [],
        }))
        .unwrap();
        LoadedPlugin {
            info: manifest.clone().into(),
            source: PluginSource::File { path: id.into() },
            wasm_bytes: wat::parse_str(wat).unwrap(),
            manifest: Some(manifest),
        }
    }

    async fn executor(plugins: Vec<LoadedPlugin>) -> PluginExecutor {
        let registry = Arc::new(PluginRegistry::new());
        for plugin in plugins {
            registry.register(plugin).await;
        }
        PluginExecutor::new(
            Arc::new(WasmRuntime::new().unwrap()),
            registry,
            crate::test_support::memory_pool().await,
            DatabaseBackend::Sqlite,
            reqwest::Client::new(),
            Arc::new(LexiconCatalog::detached()),
        )
    }

    /// A store for `GUEST` with `test_sleep` linked through the same wrapper
    /// as every real import, so the sleep is host time.
    async fn guest_store(executor: &PluginExecutor) -> (Store<PluginState>, Instance) {
        let plugin = guest("guest", GUEST);
        let module = executor.runtime.compile(&plugin.wasm_bytes).unwrap();
        let mut linker = executor.build_linker(&module).unwrap();
        HostImports::new(&mut linker)
            .define("test_sleep", |_caller, (ms,): (i32,)| {
                Box::pin(async move {
                    tokio::time::sleep(Duration::from_millis(ms as u64)).await;
                    0i64
                })
            })
            .unwrap();
        let state = executor
            .build_state(&plugin, "test", HashMap::new(), serde_json::Value::Null)
            .await
            .unwrap();
        let mut store = Store::new(executor.runtime.engine(), state);
        // Fuel is not the boundary under test.
        store.set_fuel(u64::MAX).unwrap();
        store.set_epoch_deadline(UNBOUNDED_TICKS);
        let instance = linker.instantiate_async(&mut store, &module).await.unwrap();
        (store, instance)
    }

    fn arm(store: &mut Store<PluginState>, budget: Duration) {
        let delta = store
            .data_mut()
            .deadline
            .arm(GuestDeadline::ticks_for(budget));
        store.set_epoch_deadline(delta);
    }

    fn is_interrupt(e: &wasmtime::Error) -> bool {
        e.downcast_ref::<wasmtime::Trap>() == Some(&wasmtime::Trap::Interrupt)
    }

    /// A deadline's resolution is one tick, and the thread that advances the
    /// epoch sleeps rather than keeping time, so an interrupt lands within a
    /// tick either side of the budget it was armed with. Asserting a hard
    /// lower bound made three of the tests below fail a few hundred
    /// microseconds early.
    fn assert_interrupted_near(elapsed: Duration, expected: Duration, ceiling: Duration) {
        assert!(
            elapsed + crate::plugin::runtime::EPOCH_TICK >= expected,
            "{elapsed:?} is more than a tick before {expected:?}"
        );
        assert!(
            elapsed < expected + ceiling,
            "{elapsed:?} is more than {ceiling:?} past {expected:?}"
        );
    }

    /// `cargo test --lib plugin::executor::deadline -- --nocapture` prints
    /// the overshoot past the budget at the 10 ms tick.
    #[tokio::test]
    async fn a_spinning_guest_is_interrupted_at_its_budget_and_classifies_as_timeout() {
        let executor = executor(vec![guest("spin", GUEST)]).await;
        let (mut store, instance) = guest_store(&executor).await;
        let spin = instance
            .get_typed_func::<(), ()>(&mut store, "spin")
            .unwrap();

        let budget = Duration::from_millis(300);
        arm(&mut store, budget);
        let started = Instant::now();
        let err = spin.call_async(&mut store, ()).await.unwrap_err();
        let elapsed = started.elapsed();
        println!(
            "deadline {budget:?}: interrupted after {elapsed:?} (overshoot {:?})",
            elapsed.saturating_sub(budget)
        );
        assert!(is_interrupt(&err), "{err}");
        assert!(matches!(
            classify_error(store.data(), err),
            ExecutionError::Timeout
        ));
        assert_interrupted_near(elapsed, budget, Duration::from_millis(100));
    }

    /// The same budget through `PluginInstance::arm_guest_deadline`, which
    /// is what a script runner calls.
    #[tokio::test]
    async fn arm_guest_deadline_bounds_a_call_through_the_instance() {
        let executor = executor(vec![guest("spin", GUEST)]).await;
        // The module imports `test_sleep`, which `instantiate` does not link.
        let plugin = guest(
            "bare",
            r#"(module
                (memory (export "memory") 1)
                (func (export "alloc") (param i32) (result i32) i32.const 1024)
                (func (export "dealloc") (param i32 i32))
                (func (export "spin") (loop br 0))
            )"#,
        );
        executor.registry.register(plugin).await;
        let mut inst = executor
            .instantiate("bare", "test", HashMap::new(), serde_json::Value::Null)
            .await
            .unwrap();
        inst.store.set_fuel(u64::MAX).unwrap();
        inst.arm_guest_deadline(Duration::from_millis(200));
        let spin = inst
            .instance
            .get_typed_func::<(), ()>(&mut inst.store, "spin")
            .unwrap();
        let started = Instant::now();
        let err = spin.call_async(&mut inst.store, ()).await.unwrap_err();
        assert!(is_interrupt(&err), "{err}");
        assert!(started.elapsed() < Duration::from_millis(400));
    }

    /// Ten budgets' worth of host latency inside an import is not the
    /// guest's time: the call completes.
    #[tokio::test]
    async fn host_latency_inside_an_import_is_not_charged() {
        let executor = executor(vec![]).await;
        let (mut store, instance) = guest_store(&executor).await;
        let f = instance
            .get_typed_func::<i32, i64>(&mut store, "sleep_then_return")
            .unwrap();
        let budget = Duration::from_millis(100);
        arm(&mut store, budget);
        let started = Instant::now();
        f.call_async(&mut store, 1000).await.unwrap();
        assert!(started.elapsed() >= Duration::from_millis(1000));
        // The budget is intact but for the ticks the guest itself spent.
        assert!(store.data().deadline.remaining() >= GuestDeadline::ticks_for(budget) - 2);
    }

    /// After the import returns, the guest is bounded by what remained
    /// when it entered, not by a fresh budget and not by nothing.
    #[tokio::test]
    async fn a_guest_spinning_after_a_slow_import_ends_at_its_remaining_budget() {
        let executor = executor(vec![]).await;
        let (mut store, instance) = guest_store(&executor).await;
        let f = instance
            .get_typed_func::<i32, ()>(&mut store, "sleep_then_spin")
            .unwrap();
        let budget = Duration::from_millis(200);
        arm(&mut store, budget);
        let started = Instant::now();
        let err = f.call_async(&mut store, 1000).await.unwrap_err();
        let elapsed = started.elapsed();
        assert!(is_interrupt(&err), "{err}");
        let sleep = Duration::from_millis(1000);
        assert_interrupted_near(elapsed, sleep + budget, Duration::from_millis(100));
    }

    /// A budget spent to zero inside a host call interrupts the guest the
    /// moment it resumes.
    #[tokio::test]
    async fn a_budget_exhausted_before_an_import_returns_rearms_at_zero() {
        let executor = executor(vec![]).await;
        let (mut store, instance) = guest_store(&executor).await;
        let f = instance
            .get_typed_func::<i32, ()>(&mut store, "sleep_then_spin")
            .unwrap();
        // Zero ticks: the guest reaches the import (no epoch check precedes
        // the call in this function body) and is stopped on return.
        let delta = store.data_mut().deadline.arm(0);
        store.set_epoch_deadline(delta);
        let started = Instant::now();
        let err = f.call_async(&mut store, 50).await.unwrap_err();
        assert!(is_interrupt(&err), "{err}");
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    /// An unarmed store runs as long as it likes.
    #[tokio::test]
    async fn an_unbounded_store_runs_past_any_budget() {
        let executor = executor(vec![]).await;
        let (mut store, instance) = guest_store(&executor).await;
        let spin_n = instance
            .get_typed_func::<i32, ()>(&mut store, "spin_n")
            .unwrap();
        let started = Instant::now();
        // Enough iterations to cross many ticks on any machine.
        while started.elapsed() < Duration::from_millis(300) {
            spin_n.call_async(&mut store, 50_000_000).await.unwrap();
        }
    }

    /// Wasmtime 49 accepts exception handling, and a frame that catches a
    /// `throw` resumes with a stale fuel counter, so fuel cannot stop a
    /// guest that loops inside a catching frame; the epoch deadline can.
    /// `cargo test --lib plugin::executor::deadline::a_guest -- --nocapture`
    /// prints the fuel the loop was charged.
    #[tokio::test]
    async fn a_guest_catching_its_own_throws_is_stopped_by_the_deadline_not_by_fuel() {
        let plugin = guest(
            "catcher",
            r#"(module
                (tag $e)
                (memory (export "memory") 1)
                (global $n (mut i32) (i32.const 0))
                (func (export "alloc") (param i32) (result i32) i32.const 1024)
                (func (export "dealloc") (param i32 i32))
                (func $throws (throw $e))
                (func (export "iterations") (result i32) global.get $n)
                (func (export "catch_spin")
                    (loop $again
                        (block $caught
                            (try_table (catch_all $caught) (call $throws)))
                        (global.set $n (i32.add (global.get $n) (i32.const 1)))
                        (br $again))))"#,
        );
        assert!(capabilities::analyze_imports(&plugin.wasm_bytes).is_ok());
        let executor = executor(vec![plugin]).await;
        let mut inst = executor
            .instantiate("catcher", "test", HashMap::new(), serde_json::Value::Null)
            .await
            .unwrap();
        let fuel_before = inst.store.get_fuel().unwrap();
        let budget = Duration::from_millis(300);
        inst.arm_guest_deadline(budget);
        let spin = inst
            .instance
            .get_typed_func::<(), ()>(&mut inst.store, "catch_spin")
            .unwrap();
        let started = Instant::now();
        let err = spin.call_async(&mut inst.store, ()).await.unwrap_err();
        let elapsed = started.elapsed();
        assert!(is_interrupt(&err), "{err}");
        assert_interrupted_near(elapsed, budget, Duration::from_millis(100));

        let fuel_after = inst.store.get_fuel().unwrap();
        // The deadline has passed; lift it to read the counter back.
        inst.store.set_epoch_deadline(UNBOUNDED_TICKS);
        let iterations = inst
            .instance
            .get_typed_func::<(), i32>(&mut inst.store, "iterations")
            .unwrap()
            .call_async(&mut inst.store, ())
            .await
            .unwrap();
        println!(
            "catching loop: {iterations} iterations in {elapsed:?}, charged {} fuel of {fuel_before}",
            fuel_before - fuel_after
        );
        assert!(iterations > 0);
        assert!(fuel_after > 0, "fuel ran out before the deadline");
    }

    /// An interpreter store leaves `instantiate` under the operator's wall
    /// clock, so a guest that spins with no runner involved is interrupted;
    /// a job runner lifts the deadline explicitly.
    #[tokio::test]
    async fn an_interpreter_store_is_armed_at_instantiation() {
        let spinner = |id: &str| {
            let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
                "id": id, "name": id, "version": "1.0.0", "api_version": "2",
                "plugin_type": "interpreter", "capabilities": [],
            }))
            .unwrap();
            LoadedPlugin {
                info: manifest.clone().into(),
                source: PluginSource::File { path: id.into() },
                wasm_bytes: wat::parse_str(
                    r#"(module
                        (memory (export "memory") 1)
                        (func (export "alloc") (param i32) (result i32) i32.const 1024)
                        (func (export "dealloc") (param i32 i32))
                        (func (export "spin") (loop br 0))
                    )"#,
                )
                .unwrap(),
                manifest: Some(manifest),
            }
        };

        // Without an `AppState` the default wall clock applies.
        let executor = executor(vec![spinner("lua")]).await;
        let inst = executor
            .instantiate("lua", "test", HashMap::new(), serde_json::Value::Null)
            .await
            .unwrap();
        assert_eq!(
            inst.store.data().deadline.remaining(),
            GuestDeadline::ticks_for(Duration::from_secs(u64::from(
                crate::lua::limits::DEFAULT_WALL_CLOCK_SECONDS
            )))
        );

        // With one, the cached setting applies, and the spin is stopped.
        let mut state =
            crate::test_support::test_state_with_pool(crate::test_support::memory_pool().await);
        state.script_limits = Arc::new(crate::lua::limits::ScriptLimits::new(1_000_000, 1));
        let registry = Arc::new(PluginRegistry::new());
        registry.register(spinner("lua")).await;
        let executor = PluginExecutor::new(
            Arc::new(WasmRuntime::new().unwrap()),
            registry,
            crate::test_support::memory_pool().await,
            DatabaseBackend::Sqlite,
            reqwest::Client::new(),
            Arc::new(LexiconCatalog::detached()),
        )
        .with_app_state(state);
        let mut inst = executor
            .instantiate("lua", "test", HashMap::new(), serde_json::Value::Null)
            .await
            .unwrap();
        let spin = inst
            .instance
            .get_typed_func::<(), ()>(&mut inst.store, "spin")
            .unwrap();
        let started = Instant::now();
        let err = spin.call_async(&mut inst.store, ()).await.unwrap_err();
        let elapsed = started.elapsed();
        assert!(is_interrupt(&err), "{err}");
        assert!(matches!(
            classify_error(inst.store.data(), err),
            ExecutionError::Timeout
        ));
        // A generous ceiling: this machine runs these under load.
        assert_interrupted_near(elapsed, Duration::from_secs(1), Duration::from_secs(3));

        inst.lift_guest_deadline();
        assert_eq!(inst.store.data().deadline.remaining(), UNBOUNDED_TICKS);
    }

    /// An interpreter whose module runs `wat` somewhere the host never calls
    /// it, under a one-second budget.
    async fn looping_interpreter(wat: &str) -> PluginExecutor {
        let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "id": "lua", "name": "lua", "version": "1.0.0", "api_version": "2",
            "plugin_type": "interpreter", "capabilities": [],
        }))
        .unwrap();
        let plugin = LoadedPlugin {
            info: manifest.clone().into(),
            source: PluginSource::File { path: "lua".into() },
            wasm_bytes: wat::parse_str(wat).unwrap(),
            manifest: Some(manifest),
        };

        let mut state =
            crate::test_support::test_state_with_pool(crate::test_support::memory_pool().await);
        state.script_limits = Arc::new(crate::lua::limits::ScriptLimits::new(1_000_000, 1));
        let registry = Arc::new(PluginRegistry::new());
        registry.register(plugin).await;
        PluginExecutor::new(
            Arc::new(WasmRuntime::new().unwrap()),
            registry,
            crate::test_support::memory_pool().await,
            DatabaseBackend::Sqlite,
            reqwest::Client::new(),
            Arc::new(LexiconCatalog::detached()),
        )
        .with_app_state(state)
    }

    /// `_initialize` is guest code with no runner around it, so the budget
    /// has to be on the store before it runs.
    #[tokio::test]
    async fn a_spinning_initializer_is_interrupted_and_classifies_as_timeout() {
        let executor = looping_interpreter(
            r#"(module
                (memory (export "memory") 1)
                (func (export "alloc") (param i32) (result i32) i32.const 1024)
                (func (export "dealloc") (param i32 i32))
                (func (export "_initialize") (loop br 0))
            )"#,
        )
        .await;

        let started = Instant::now();
        let err = executor
            .instantiate("lua", "test", HashMap::new(), serde_json::Value::Null)
            .await
            .err()
            .expect("a looping initializer was allowed to run");
        let elapsed = started.elapsed();
        assert!(matches!(err, ExecutionError::Timeout), "{err}");
        assert_interrupted_near(elapsed, Duration::from_secs(1), Duration::from_secs(3));
    }

    /// A start section runs *inside* `instantiate_async`, earlier than
    /// `_initialize`, and is what distinguishes arming before the module is
    /// instantiated from arming between instantiation and the initializer.
    /// The wait is bounded so moving the arm fails the assertion rather than
    /// hanging the suite.
    #[tokio::test]
    async fn a_looping_start_section_is_interrupted_during_instantiation() {
        let executor = looping_interpreter(
            r#"(module
                (memory (export "memory") 1)
                (func (export "alloc") (param i32) (result i32) i32.const 1024)
                (func (export "dealloc") (param i32 i32))
                (func $start (loop br 0))
                (start $start)
            )"#,
        )
        .await;

        let instantiated = tokio::time::timeout(
            Duration::from_secs(10),
            executor.instantiate("lua", "test", HashMap::new(), serde_json::Value::Null),
        )
        .await
        .expect("a looping start section was never interrupted");
        let err = instantiated
            .err()
            .expect("the start section ran to completion");
        assert!(matches!(err, ExecutionError::Instantiation(_)), "{err}");
    }

    /// Every async `env` import is defined through the wrapper: the names
    /// the linker holds are the names the wrapper recorded, `host_log` and
    /// `host_script_panic` excepted.
    #[test]
    fn every_async_import_goes_through_the_deadline_wrapper() {
        let runtime = WasmRuntime::without_ticker().unwrap();
        let mut linker = Linker::new(runtime.engine());
        let wrapped: HashSet<&str> = define_host_functions(&mut linker)
            .unwrap()
            .into_iter()
            .collect();
        let registered: Vec<String> = linker
            .iter(&mut Store::new(
                runtime.engine(),
                bindings_test_state(&runtime),
            ))
            .map(|(module, name, _)| {
                assert_eq!(module, "env");
                name.to_string()
            })
            .collect();
        assert!(registered.len() > 50, "{registered:?}");
        for name in &registered {
            if name == "host_log" || name == "host_script_panic" {
                continue;
            }
            assert!(
                wrapped.contains(name.as_str()),
                "{name} bypasses the wrapper"
            );
        }
    }

    fn bindings_test_state(runtime: &WasmRuntime) -> PluginState {
        crate::db::install_drivers();
        PluginState {
            plugin_id: "p".into(),
            scope: "s".into(),
            secrets: HashMap::new(),
            config: serde_json::Value::Null,
            db: None,
            db_backend: DatabaseBackend::Sqlite,
            http_client: reqwest::Client::new(),
            lexicons: Arc::new(LexiconCatalog::detached()),
            usage: Default::default(),
            memory: None,
            alloc: None,
            dealloc: None,
            capabilities: HashSet::new(),
            allowed_hosts: Vec::new(),
            plugin_type: PluginType::Library,
            executor: None,
            call_ctx: LibraryCallContext::default(),
            depth: 0,
            caller: None,
            script_run: None,
            app_state: None,
            wasi: crate::plugin::host::build_wasi_context("p", None, DatabaseBackend::Sqlite),
            deadline: GuestDeadline::unbounded(runtime.epoch()),
            limiter: MemoryLimiter::new(memory_ceiling(PluginType::Library)),
            pending_calls: Default::default(),
            panic: None,
            call_fuel: u64::MAX,
        }
    }
}

/// The interpreter path against the `interpreter_echo` fixture, for the two
/// properties that need the instance itself rather than `execute_script`:
/// what `instantiate` bounds on its own, and what the five `script:host`
/// imports answer with no run in progress.
#[cfg(test)]
mod interpreter {
    use super::*;
    use crate::plugin::loader;
    use crate::plugin::{LoadedPlugin, PluginManifest, PluginSource};
    use std::time::{Duration, Instant};

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/interpreter_echo/target/wasm32-unknown-unknown/release/interpreter_echo.wasm"
    );

    /// False when the echo module is unbuilt and the test is to skip.
    fn fixture_is_built() -> bool {
        loader::built_fixture("interpreter_echo", "wasm32-unknown-unknown").is_some()
    }

    fn fixture(plugin_type: &str) -> LoadedPlugin {
        let mut manifest = serde_json::json!({
            "id": "echo", "name": "echo", "version": "1.0.0", "api_version": "2",
            "plugin_type": plugin_type, "capabilities": ["library:call", "script:host"],
        });
        if plugin_type == "interpreter" {
            manifest["language_id"] = serde_json::json!("echo");
        } else {
            manifest["namespace"] = serde_json::json!("echo");
        }
        let manifest: PluginManifest = serde_json::from_value(manifest).unwrap();
        LoadedPlugin {
            info: manifest.clone().into(),
            source: PluginSource::File {
                path: "tests/fixtures/interpreter_echo".into(),
            },
            wasm_bytes: std::fs::read(FIXTURE).expect("interpreter_echo fixture not built"),
            manifest: Some(manifest),
        }
    }

    async fn instance(plugin_type: &str, wall_clock_seconds: u32) -> PluginInstance {
        let mut state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
        state.script_limits = Arc::new(crate::lua::limits::ScriptLimits::new(
            1_000_000,
            wall_clock_seconds,
        ));
        let registry = Arc::new(PluginRegistry::new());
        registry.register(fixture(plugin_type)).await;
        let db = state.db.clone();
        PluginExecutor::new(
            Arc::new(WasmRuntime::new().unwrap()),
            registry,
            db,
            DatabaseBackend::Sqlite,
            reqwest::Client::new(),
            Arc::new(LexiconCatalog::detached()),
        )
        .with_app_state(state)
        .instantiate("echo", "test", HashMap::new(), serde_json::Value::Null)
        .await
        .unwrap()
    }

    /// An executor over a registry holding the echo interpreter, and that
    /// interpreter's module as it was before the same id was registered
    /// again: a module a run or a prepare may still hold when it is replaced.
    async fn superseded() -> (PluginExecutor, Arc<LoadedPlugin>, Arc<LoadedPlugin>) {
        let state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
        let registry = Arc::new(PluginRegistry::new());
        registry.register(fixture("interpreter")).await;
        let stale = registry.get("echo").await.unwrap();
        registry.register(fixture("interpreter")).await;
        let current = registry.get("echo").await.unwrap();
        assert!(!Arc::ptr_eq(&stale, &current));
        let db = state.db.clone();
        let executor = PluginExecutor::new(
            Arc::new(WasmRuntime::new().unwrap()),
            registry,
            db,
            DatabaseBackend::Sqlite,
            reqwest::Client::new(),
            Arc::new(LexiconCatalog::detached()),
        )
        .with_app_state(state);
        (executor, stale, current)
    }

    fn run_input(source: &str) -> ScriptExecuteInput {
        serde_json::from_value(directive(source)).unwrap()
    }

    /// The same bytes registered twice stamp alike, so only identity tells
    /// the outgoing module from its successor: a stamp read by id would hand
    /// the outgoing one a stamp that is not its own.
    #[tokio::test]
    async fn a_superseded_module_prepares_no_artifact() {
        if !fixture_is_built() {
            return;
        }
        let (executor, stale, current) = superseded().await;

        let out = executor
            .prepare_loaded(&stale, "returns:1", "script", ChunkRole::Handler)
            .await
            .unwrap();
        assert!(out.valid);
        assert_eq!(out.artifact, None);

        let out = executor
            .prepare_loaded(&current, "returns:1", "script", ChunkRole::Handler)
            .await
            .unwrap();
        assert!(out.artifact.is_some());
    }

    /// A run on a superseded module carries no stamp, so even an artifact
    /// stamped exactly as its successor would stamp it is dropped and the
    /// source runs.
    #[tokio::test]
    async fn a_run_on_a_superseded_module_runs_from_source() {
        if !fixture_is_built() {
            return;
        }
        let (executor, stale, current) = superseded().await;

        let inst = executor
            .instantiate_interpreter(
                &stale,
                Some(&run_input("returns:1")),
                None,
                ModuleScope::Dashboard,
            )
            .await
            .unwrap();
        assert_eq!(inst.store.data().script_run.as_ref().unwrap().stamp, None);

        let mut artifact = crate::plugin::PreparedArtifact {
            interpreter: String::new(),
            version: String::new(),
            engine: "echo-1".into(),
            format: "echo-source".into(),
            bytes: b"returns:\"artifact\"".to_vec(),
            source_map: None,
        };
        ArtifactStamp::of(&current).apply(&mut artifact);
        let mut input = run_input("returns:\"source\"");
        input.artifact = Some(artifact);

        let ran = |output| match output {
            ScriptExecuteOutput::Returned { value, .. } => value,
            other => panic!("expected a returned value, got {other:?}"),
        };
        let on_stale = executor
            .execute_loaded(
                &stale,
                &input,
                None,
                ModuleScope::Dashboard,
                RunOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(ran(on_stale), "source");
        let on_current = executor
            .execute_loaded(
                &current,
                &input,
                None,
                ModuleScope::Dashboard,
                RunOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(ran(on_current), "artifact");
    }

    fn directive(source: &str) -> serde_json::Value {
        serde_json::to_value(ScriptExecuteInput {
            source: source.to_string(),
            kind: ScriptKind::XrpcQuery,
            input: serde_json::Value::Null,
            context: Default::default(),
            libraries: Vec::new(),
            limits: crate::plugin::ScriptExecuteLimits {
                instructions: None,
                memory_bytes: 8 * 1024 * 1024,
            },
            removed_globals: Vec::new(),
            chunk_name: crate::plugin::DEFAULT_CHUNK_NAME.to_string(),
            artifact: None,
        })
        .unwrap()
    }

    /// The store leaves `instantiate` under the operator's wall clock, so a
    /// guest that spins is stopped whether or not a runner armed anything.
    #[tokio::test]
    async fn an_instantiated_interpreter_is_bounded_with_no_runner_involved() {
        if !fixture_is_built() {
            return;
        }
        let mut inst = instance("interpreter", 1).await;
        let started = Instant::now();
        let err = inst
            .call_plugin_function::<serde_json::Value>("execute", &directive("spin"))
            .await
            .expect_err("a spinning guest was allowed to run");
        assert!(matches!(err, ExecutionError::Timeout), "{err}");
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    /// An executor over the echo interpreter, and the state its runs write to.
    async fn job_executor() -> (PluginExecutor, crate::AppState) {
        let state = crate::test_support::test_state_with_pool(
            crate::test_support::migrated_memory_pool().await,
        );
        let registry = Arc::new(PluginRegistry::new());
        registry.register(fixture("interpreter")).await;
        let executor = PluginExecutor::new(
            Arc::new(WasmRuntime::new().unwrap()),
            registry,
            state.db.clone(),
            DatabaseBackend::Sqlite,
            reqwest::Client::new(),
            Arc::new(LexiconCatalog::detached()),
        )
        .with_app_state(state.clone());
        (executor, state)
    }

    /// A run of job `id`'s script `source` on `payload`.
    fn job_input(id: &str, source: &str, payload: serde_json::Value) -> ScriptExecuteInput {
        let mut input = directive(source);
        input["kind"] = serde_json::json!("job");
        input["context"]["job"] = serde_json::json!({ "id": id });
        input["input"] = payload;
        serde_json::from_value(input).unwrap()
    }

    fn a_lease(id: &str) -> crate::jobs::held::Held {
        crate::jobs::held::Held::new(
            id,
            crate::jobs::db::Lease {
                owner: "here".into(),
                attempt: 1,
            },
        )
    }

    /// A run whose worker has already lost the job stops at its next host
    /// import before the import acts: its log line is never written.
    #[tokio::test]
    async fn a_run_that_lost_its_lease_stops_before_its_next_import_acts() {
        if !fixture_is_built() {
            return;
        }
        let (executor, state) = job_executor().await;
        let held = a_lease("job-1");
        held.mark_lost();
        let input = job_input("job-1", "host:script_log", serde_json::json!({}));
        let out = crate::jobs::held::run_under(held, executor.execute_script("echo", &input)).await;

        assert!(out.is_err(), "the run went on: {out:?}");
        let (logged,): (i64,) = crate::db::query_as(
            "SELECT COUNT(*) FROM happyview_event_logs WHERE event_type = 'script.log'",
        )
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(logged, 0, "the import acted for a run that lost its job");
    }

    /// A lease lost during a host import stops the guest as soon as it
    /// resumes, rather than letting it run on. The import is a progress write
    /// whose fence fails (no row holds this lease), which is where the run
    /// finds out: it passed the check on entry, and the guest is entered
    /// again only to take the import's answer.
    #[tokio::test]
    async fn a_run_whose_lease_is_lost_during_an_import_stops_when_it_resumes() {
        if !fixture_is_built() {
            return;
        }
        let (executor, _state) = job_executor().await;
        let held = a_lease("job-2");
        let input = job_input(
            "job-2",
            "host:job_progress",
            serde_json::json!({ "step": 1 }),
        );
        let out =
            crate::jobs::held::run_under(held.clone(), executor.execute_script("echo", &input))
                .await;

        assert!(held.is_lost(), "the progress write did not find the loss");
        assert!(out.is_err(), "the run went on to its end: {out:?}");
    }

    /// The same five imports on an instance with no run refuse rather than
    /// acting on a job they were never given.
    #[tokio::test]
    async fn the_script_imports_are_unsupported_with_no_run_in_progress() {
        if !fixture_is_built() {
            return;
        }
        let mut inst = instance("library", 10).await;
        for source in [
            "host:script_log",
            "host:job_progress",
            "host:job_should_stop",
            "host:job_wait",
            "host:module_resolve",
        ] {
            let out: serde_json::Value = inst
                .call_plugin_function("execute", &directive(source))
                .await
                .unwrap_or_else(|e| panic!("{source}: {e}"));
            assert_eq!(
                out["value"]["error"]["code"], "UNSUPPORTED",
                "{source}: {out}"
            );
        }
    }
}

/// The store memory ceiling: a guest growing past it is refused, the
/// refusal is told apart from an ordinary trap, and a guest under it is
/// untouched.
#[cfg(test)]
mod memory {
    use super::*;
    use crate::plugin::runtime::LIBRARY_MEMORY_CEILING;
    use crate::plugin::{LoadedPlugin, PluginManifest, PluginSource};

    const PAGE: usize = 64 * 1024;

    /// `grow` returns the previous page count or -1; `grow_or_trap` traps
    /// on a refusal, as a compiled guest's allocator does; the two `i64`
    /// exports are reachable through the executor's own call path.
    const GUEST: &str = r#"(module
        (memory (export "memory") 1)
        (func (export "alloc") (param i32) (result i32) i32.const 1024)
        (func (export "dealloc") (param i32 i32))
        (func (export "grow") (param i32) (result i32) local.get 0 memory.grow)
        (func (export "grow_or_trap") (param i32)
            local.get 0
            memory.grow
            i32.const -1
            i32.eq
            if unreachable end)
        (func (export "trap") unreachable)
        (func (export "spin") (loop br 0))
        (func (export "grow_too_much_then_trap") (result i64)
            i32.const 9000
            memory.grow
            drop
            unreachable)
        (func (export "just_trap") (result i64) unreachable)
    )"#;

    fn guest(id: &str, plugin_type: &str) -> LoadedPlugin {
        let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "version": "1.0.0", "api_version": "2",
            "plugin_type": plugin_type, "namespace": id, "capabilities": [],
        }))
        .unwrap();
        LoadedPlugin {
            info: manifest.clone().into(),
            source: PluginSource::File { path: id.into() },
            wasm_bytes: wat::parse_str(GUEST).unwrap(),
            manifest: Some(manifest),
        }
    }

    async fn instance(plugin_type: &str) -> PluginInstance {
        let registry = Arc::new(PluginRegistry::new());
        registry.register(guest("g", plugin_type)).await;
        let executor = PluginExecutor::new(
            Arc::new(WasmRuntime::new().unwrap()),
            registry,
            crate::test_support::memory_pool().await,
            DatabaseBackend::Sqlite,
            reqwest::Client::new(),
            Arc::new(LexiconCatalog::detached()),
        );
        executor
            .instantiate("g", "test", HashMap::new(), serde_json::Value::Null)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_guest_growing_past_its_ceiling_is_refused_and_classified_as_memory() {
        let mut inst = instance("library").await;
        let pages = (LIBRARY_MEMORY_CEILING / PAGE + 1) as i32;
        let grow = inst
            .instance
            .get_typed_func::<i32, ()>(&mut inst.store, "grow_or_trap")
            .unwrap();
        let err = grow.call_async(&mut inst.store, pages).await.unwrap_err();
        let classified = classify_error(inst.store.data(), err);
        match classified {
            ExecutionError::MemoryLimit { requested, ceiling } => {
                assert_eq!(ceiling, LIBRARY_MEMORY_CEILING);
                assert_eq!(requested, (pages as usize + 1) * PAGE);
            }
            other => panic!("{other}"),
        }
        assert!(matches!(
            classified.script_error_type(),
            crate::error::ScriptErrorType::Memory
        ));
        assert_eq!(inst.memory.size(&inst.store), 1);
    }

    #[tokio::test]
    async fn a_guest_under_its_ceiling_grows_freely() {
        let mut inst = instance("library").await;
        let grow = inst
            .instance
            .get_typed_func::<i32, i32>(&mut inst.store, "grow")
            .unwrap();
        assert_eq!(grow.call_async(&mut inst.store, 100).await.unwrap(), 1);
        assert_eq!(inst.memory.size(&inst.store), 101);
        assert_eq!(inst.store.data().limiter.refused(), None);
    }

    /// An ordinary trap on a store that never refused anything stays a trap.
    #[tokio::test]
    async fn an_ordinary_trap_is_not_a_memory_error() {
        let mut inst = instance("library").await;
        let trap = inst
            .instance
            .get_typed_func::<(), ()>(&mut inst.store, "trap")
            .unwrap();
        let err = trap.call_async(&mut inst.store, ()).await.unwrap_err();
        let classified = classify_error(inst.store.data(), err);
        assert!(
            matches!(classified, ExecutionError::Trap(_)),
            "{classified}"
        );
        assert!(matches!(
            classified.script_error_type(),
            crate::error::ScriptErrorType::Runtime
        ));
    }

    /// A guest that survives a refused growth and then runs out of time
    /// timed out; the refusal does not outrank the interrupt.
    #[tokio::test]
    async fn a_refusal_survived_and_then_a_deadline_reports_timeout() {
        let mut inst = instance("library").await;
        inst.store.set_fuel(u64::MAX).unwrap();
        let grow = inst
            .instance
            .get_typed_func::<i32, i32>(&mut inst.store, "grow")
            .unwrap();
        assert_eq!(grow.call_async(&mut inst.store, 9000).await.unwrap(), -1);
        assert!(inst.store.data().limiter.refused().is_some());

        inst.arm_guest_deadline(std::time::Duration::from_millis(200));
        let spin = inst
            .instance
            .get_typed_func::<(), ()>(&mut inst.store, "spin")
            .unwrap();
        let err = spin.call_async(&mut inst.store, ()).await.unwrap_err();
        let classified = classify_error(inst.store.data(), err);
        assert!(
            matches!(classified, ExecutionError::Timeout),
            "{classified}"
        );
    }

    /// A refusal classifies the call it happened in and no later one.
    #[tokio::test]
    async fn a_refusal_does_not_carry_into_the_next_call() {
        let mut inst = instance("library").await;
        let err = inst
            .call_no_input_function::<serde_json::Value>("grow_too_much_then_trap")
            .await
            .unwrap_err();
        assert!(matches!(err, ExecutionError::MemoryLimit { .. }), "{err}");
        let err = inst
            .call_no_input_function::<serde_json::Value>("just_trap")
            .await
            .unwrap_err();
        assert!(matches!(err, ExecutionError::Trap(_)), "{err}");
    }

    /// The ceiling is the store's: a module declaring a second memory is
    /// refused at instantiation rather than given a second ceiling.
    #[tokio::test]
    async fn a_module_with_two_memories_is_refused_at_instantiation() {
        let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "id": "two", "name": "two", "version": "1.0.0", "api_version": "2",
            "plugin_type": "library", "namespace": "two", "capabilities": [],
        }))
        .unwrap();
        let registry = Arc::new(PluginRegistry::new());
        registry
            .register(LoadedPlugin {
                info: manifest.clone().into(),
                source: PluginSource::File { path: "two".into() },
                wasm_bytes: wat::parse_str(
                    r#"(module
                        (memory (export "memory") 1)
                        (memory 1)
                        (func (export "alloc") (param i32) (result i32) i32.const 1024)
                        (func (export "dealloc") (param i32 i32))
                    )"#,
                )
                .unwrap(),
                manifest: Some(manifest),
            })
            .await;
        let executor = PluginExecutor::new(
            Arc::new(WasmRuntime::new().unwrap()),
            registry,
            crate::test_support::memory_pool().await,
            DatabaseBackend::Sqlite,
            reqwest::Client::new(),
            Arc::new(LexiconCatalog::detached()),
        );
        let err = executor
            .instantiate("two", "test", HashMap::new(), serde_json::Value::Null)
            .await
            .err()
            .expect("a second memory was allowed");
        assert!(matches!(err, ExecutionError::Instantiation(_)), "{err}");
    }

    #[tokio::test]
    async fn ceilings_follow_the_plugin_type_and_the_script_limit() {
        let inst = instance("library").await;
        assert_eq!(inst.store.data().limiter.ceiling(), LIBRARY_MEMORY_CEILING);
        let inst = instance("auth").await;
        assert_eq!(inst.store.data().limiter.ceiling(), LIBRARY_MEMORY_CEILING);
        let mut inst = instance("interpreter").await;
        assert_eq!(inst.store.data().limiter.ceiling(), 128 * 1024 * 1024);
        inst.set_script_memory_limit(16 * 1024 * 1024);
        assert_eq!(inst.store.data().limiter.ceiling(), 32 * 1024 * 1024);
        let grow = inst
            .instance
            .get_typed_func::<i32, i32>(&mut inst.store, "grow")
            .unwrap();
        assert_eq!(grow.call_async(&mut inst.store, 600).await.unwrap(), -1);
        assert_eq!(inst.store.data().limiter.refused(), Some(601 * PAGE));
    }
}

#[cfg(test)]
mod cache {
    use super::*;
    use crate::plugin::{LoadedPlugin, PluginManifest, PluginSource};

    /// The executor's own path: two instantiations, one compile.
    #[tokio::test]
    async fn a_second_instantiation_does_not_recompile() {
        let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "id": "p", "name": "p", "version": "1.0.0", "api_version": "2",
            "plugin_type": "library", "namespace": "p", "capabilities": [],
        }))
        .unwrap();
        let plugin = LoadedPlugin {
            info: manifest.clone().into(),
            source: PluginSource::File { path: "p".into() },
            wasm_bytes: wat::parse_str(
                r#"(module
                    (memory (export "memory") 1)
                    (func (export "alloc") (param i32) (result i32) i32.const 1024)
                    (func (export "dealloc") (param i32 i32))
                )"#,
            )
            .unwrap(),
            manifest: Some(manifest),
        };
        let runtime = Arc::new(WasmRuntime::new().unwrap().without_shared_modules());
        let registry = Arc::new(PluginRegistry::new().with_runtime(runtime.clone()));
        registry.register(plugin).await;
        let executor = PluginExecutor::new(
            runtime.clone(),
            registry,
            crate::test_support::memory_pool().await,
            DatabaseBackend::Sqlite,
            reqwest::Client::new(),
            Arc::new(LexiconCatalog::detached()),
        );
        for _ in 0..3 {
            executor
                .instantiate("p", "test", HashMap::new(), serde_json::Value::Null)
                .await
                .unwrap();
        }
        assert_eq!(runtime.compile_count(), 1);
    }
}

/// Fuel by plugin type: libraries and auth plugins keep `DEFAULT_FUEL` at
/// instantiation and before every call; an interpreter is given the whole
/// range and bounded by its epoch deadline instead.
#[cfg(test)]
mod fuel {
    use super::*;
    use crate::plugin::runtime::DEFAULT_FUEL;
    use crate::plugin::{LoadedPlugin, PluginManifest, PluginSource};

    /// `spin` never returns; `small` fits comfortably inside `DEFAULT_FUEL`;
    /// `work` runs well past it. Both loops return an empty pointer.
    const GUEST: &str = r#"(module
        (memory (export "memory") 1)
        (func (export "alloc") (param i32) (result i32) i32.const 1024)
        (func (export "dealloc") (param i32 i32))
        (func (export "spin") (result i64) (loop br 0) i64.const 0)
        (func (export "small") (result i64) (local i32)
            (loop
                local.get 0
                i32.const 1
                i32.add
                local.tee 0
                i32.const 100000
                i32.lt_u
                br_if 0)
            i64.const 0)
        (func (export "work") (result i64) (local i32)
            (loop
                local.get 0
                i32.const 1
                i32.add
                local.tee 0
                i32.const 20000000
                i32.lt_u
                br_if 0)
            i64.const 0)
    )"#;

    async fn instance(plugin_type: &str) -> PluginInstance {
        let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "id": "g", "name": "g", "version": "1.0.0", "api_version": "2",
            "plugin_type": plugin_type, "namespace": "g", "capabilities": [],
        }))
        .unwrap();
        let registry = Arc::new(PluginRegistry::new());
        registry
            .register(LoadedPlugin {
                info: manifest.clone().into(),
                source: PluginSource::File { path: "g".into() },
                wasm_bytes: wat::parse_str(GUEST).unwrap(),
                manifest: Some(manifest),
            })
            .await;
        PluginExecutor::new(
            Arc::new(WasmRuntime::new().unwrap()),
            registry,
            crate::test_support::memory_pool().await,
            DatabaseBackend::Sqlite,
            reqwest::Client::new(),
            Arc::new(LexiconCatalog::detached()),
        )
        .instantiate("g", "test", HashMap::new(), serde_json::Value::Null)
        .await
        .unwrap()
    }

    /// wasmtime 49 charges a synthesized startup function at instantiation;
    /// on this host it is one memory image page and a little.
    const STARTUP_ALLOWANCE: u64 = 20_000;

    async fn is_given_default_fuel(plugin_type: &str) {
        let mut inst = instance(plugin_type).await;
        let after_instantiate = inst.store.get_fuel().unwrap();
        assert!(
            DEFAULT_FUEL - after_instantiate <= STARTUP_ALLOWANCE,
            "{after_instantiate}"
        );

        // Spend nearly everything, then confirm the next call starts from
        // the default rather than from what was left.
        inst.store.set_fuel(1_000).unwrap();
        let err = inst
            .call_no_input_function::<serde_json::Value>("small")
            .await
            .unwrap_err();
        assert!(!matches!(err, ExecutionError::Timeout), "{err}");
        let spent = DEFAULT_FUEL - inst.store.get_fuel().unwrap();
        assert!(spent > 1_000 && spent < DEFAULT_FUEL / 10, "{spent}");

        // Past the default budget, whether the guest would return or not.
        for export in ["work", "spin"] {
            let err = inst
                .call_no_input_function::<serde_json::Value>(export)
                .await
                .unwrap_err();
            assert!(matches!(err, ExecutionError::Timeout), "{export}: {err}");
        }
    }

    #[tokio::test]
    async fn a_library_is_given_default_fuel_at_instantiation_and_each_call() {
        is_given_default_fuel("library").await;
    }

    #[tokio::test]
    async fn an_auth_plugin_is_given_exactly_what_a_library_is() {
        is_given_default_fuel("auth").await;
    }

    #[tokio::test]
    async fn an_interpreter_is_given_the_whole_range_and_runs_past_default_fuel() {
        let mut inst = instance("interpreter").await;
        let after_instantiate = inst.store.get_fuel().unwrap();
        assert!(
            u64::MAX - after_instantiate <= STARTUP_ALLOWANCE,
            "{after_instantiate}"
        );

        let err = inst
            .call_no_input_function::<serde_json::Value>("work")
            .await
            .unwrap_err();
        assert!(!matches!(err, ExecutionError::Timeout), "{err}");
        let spent = u64::MAX - inst.store.get_fuel().unwrap();
        assert!(spent > DEFAULT_FUEL, "{spent}");
    }
}

/// Fuel, compile time and instantiation and call latency for two fixtures.
/// The numbers are inputs to sizing rather than assertions, so the test is
/// ignored and prints:
/// `cargo test --lib plugin::executor::baseline -- --ignored --nocapture`.
/// `instantiate` is timed against a module the cache already holds, so it
/// is the cost a cache hit leaves; run 0's `instantiate+call` compiles
/// inside `instantiate` and is the cost a miss pays.
#[cfg(test)]
mod baseline {
    use super::*;
    use crate::plugin::runtime::DEFAULT_FUEL;
    use crate::plugin::{LoadedPlugin, PluginManifest, PluginSource, loader};
    use std::time::{Duration, Instant};

    const TEST_LIBRARY: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/test_library/target/wasm32-unknown-unknown/release/test_library.wasm"
    );
    const SDK_HTTP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sdk_http");

    fn test_library_plugin() -> LoadedPlugin {
        let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "id": "testlib", "name": "testlib", "version": "1.0.0", "api_version": "2",
            "plugin_type": "library", "namespace": "testlib",
            "capabilities": ["library:call", "database:read", "database:write"],
        }))
        .unwrap();
        LoadedPlugin {
            info: manifest.clone().into(),
            source: PluginSource::File {
                path: "tests/fixtures/test_library".into(),
            },
            wasm_bytes: std::fs::read(TEST_LIBRARY).expect("test_library fixture not built"),
            manifest: Some(manifest),
        }
    }

    async fn executor(registry: Arc<PluginRegistry>) -> PluginExecutor {
        crate::db::install_drivers();
        let db = sqlx::AnyPool::connect("sqlite::memory:").await.unwrap();
        PluginExecutor::new(
            Arc::new(WasmRuntime::new().unwrap()),
            registry,
            db,
            DatabaseBackend::Sqlite,
            reqwest::Client::new(),
            Arc::new(LexiconCatalog::detached()),
        )
    }

    struct Sample {
        compile: Duration,
        instantiate_fuel: u64,
        call_fuel: u64,
        instantiate_and_call: Duration,
        instantiate_only: Duration,
        call_only: Duration,
    }

    /// One pass: compile the module on its own for timing; take the
    /// executor's path through one call (a compile on run 0, a cache hit
    /// after); then, with the module cached, time `instantiate` and a call
    /// separately.
    async fn sample(
        executor: &PluginExecutor,
        id: &str,
        wasm: &[u8],
        function: &str,
        args: &[serde_json::Value],
    ) -> Sample {
        let started = Instant::now();
        executor.runtime.compile(wasm).unwrap();
        let compile = started.elapsed();

        let started = Instant::now();
        let mut inst = executor
            .instantiate(id, "baseline", HashMap::new(), serde_json::Value::Null)
            .await
            .unwrap();
        let instantiate_fuel = DEFAULT_FUEL - inst.store.get_fuel().unwrap();
        inst.call_library_function(function, args, &LibraryCallContext::default())
            .await
            .unwrap();
        let instantiate_and_call = started.elapsed();
        let call_fuel = DEFAULT_FUEL - inst.store.get_fuel().unwrap();

        let started = Instant::now();
        let mut inst = executor
            .instantiate(id, "baseline", HashMap::new(), serde_json::Value::Null)
            .await
            .unwrap();
        let instantiate_only = started.elapsed();
        let started = Instant::now();
        inst.call_library_function(function, args, &LibraryCallContext::default())
            .await
            .unwrap();
        let call_only = started.elapsed();

        Sample {
            compile,
            instantiate_fuel,
            call_fuel,
            instantiate_and_call,
            instantiate_only,
            call_only,
        }
    }

    fn print(label: &str, samples: &[Sample]) {
        for (i, s) in samples.iter().enumerate() {
            println!(
                "{label} run {i}: compile {:.1} ms | instantiate fuel {} | call fuel {} | instantiate+call {:.1} ms | instantiate (cached module) {:.2} ms | call {:.2} ms",
                s.compile.as_secs_f64() * 1000.0,
                s.instantiate_fuel,
                s.call_fuel,
                s.instantiate_and_call.as_secs_f64() * 1000.0,
                s.instantiate_only.as_secs_f64() * 1000.0,
                s.call_only.as_secs_f64() * 1000.0,
            );
        }
    }

    #[tokio::test]
    #[ignore]
    async fn fixture_fuel_and_latency() {
        let registry = Arc::new(PluginRegistry::new());
        let test_library = test_library_plugin();
        let test_library_wasm = test_library.wasm_bytes.clone();
        registry.register(test_library).await;
        let http = loader::load_from_file(std::path::Path::new(SDK_HTTP))
            .await
            .expect("sdk_http fixture not built");
        let http_wasm = http.wasm_bytes.clone();
        registry.register(http).await;
        let executor = executor(registry).await;

        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/hello"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("hi"))
            .mount(&server)
            .await;

        let mut samples = Vec::new();
        for _ in 0..3 {
            samples.push(
                sample(
                    &executor,
                    "testlib",
                    &test_library_wasm,
                    "echo",
                    &[serde_json::json!({"a": [1, 2]})],
                )
                .await,
            );
        }
        print("test_library echo", &samples);

        let url = serde_json::json!(format!("{}/hello", server.uri()));
        let mut samples = Vec::new();
        for _ in 0..3 {
            samples.push(
                sample(
                    &executor,
                    "sdk_http",
                    &http_wasm,
                    "get",
                    std::slice::from_ref(&url),
                )
                .await,
            );
        }
        print("sdk_http get", &samples);

        // The disk tier: one runtime writes, a fresh one reads.
        let dir =
            std::env::temp_dir().join(format!("happyview-baseline-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (label, plugin) in [
            (
                "test_library",
                executor.registry.get("testlib").await.unwrap(),
            ),
            ("sdk_http", executor.registry.get("sdk_http").await.unwrap()),
        ] {
            let writer = WasmRuntime::with_cache_dir(Some(dir.clone())).unwrap();
            writer.module_for(&plugin).await.unwrap();
            for i in 0..3 {
                let reader = WasmRuntime::with_cache_dir(Some(dir.clone())).unwrap();
                let started = Instant::now();
                reader.module_for(&plugin).await.unwrap();
                println!(
                    "{label} disk load run {i}: {:.1} ms (compiles {})",
                    started.elapsed().as_secs_f64() * 1000.0,
                    reader.compile_count()
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod panics {
    use super::*;
    use crate::plugin::{LoadedPlugin, PluginManifest, PluginSource, ScriptErrorKind};
    use happyview_plugin_sdk::wire::PanicReport;

    /// An interpreter whose `execute` reports a panic at `src/lib.rs:7:5`
    /// and then traps, as a Rust guest's hook and abort do.
    pub(super) const PANICKING: &str = r#"(module
        (import "env" "host_script_panic" (func $panic (param i32 i32)))
        (memory (export "memory") 1)
        (data (i32.const 32768) "{\"message\":\"boom\",\"file\":\"src/lib.rs\",\"line\":7,\"column\":5}")
        (func (export "alloc") (param i32) (result i32) i32.const 1024)
        (func (export "dealloc") (param i32 i32))
        (func (export "execute") (param i32 i32) (result i64)
            i32.const 32768
            i32.const 58
            call $panic
            unreachable)
    )"#;

    /// The same trap with no report first: an ordinary crash.
    const TRAPPING: &str = r#"(module
        (memory (export "memory") 1)
        (func (export "alloc") (param i32) (result i32) i32.const 1024)
        (func (export "dealloc") (param i32 i32))
        (func (export "execute") (param i32 i32) (result i64) unreachable)
    )"#;

    pub(super) fn interpreter(wat: &str) -> LoadedPlugin {
        let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "id": "guest", "name": "guest", "version": "1.0.0", "api_version": "2",
            "plugin_type": "interpreter", "language_id": "guest", "capabilities": [],
        }))
        .unwrap();
        LoadedPlugin {
            info: manifest.clone().into(),
            source: PluginSource::File {
                path: "guest".into(),
            },
            wasm_bytes: wat::parse_str(wat).unwrap(),
            manifest: Some(manifest),
        }
    }

    pub(super) async fn executor_with(plugin: LoadedPlugin) -> PluginExecutor {
        let registry = Arc::new(PluginRegistry::new());
        registry.register(plugin).await;
        PluginExecutor::new(
            Arc::new(WasmRuntime::new().unwrap()),
            registry,
            crate::test_support::memory_pool().await,
            DatabaseBackend::Sqlite,
            reqwest::Client::new(),
            Arc::new(LexiconCatalog::detached()),
        )
    }

    pub(super) fn input(instructions: Option<u32>) -> ScriptExecuteInput {
        ScriptExecuteInput {
            source: String::new(),
            kind: ScriptKind::XrpcQuery,
            input: serde_json::json!({}),
            context: crate::plugin::ScriptExecuteContext {
                trigger: "xrpc.query:app.test.q".into(),
                ..Default::default()
            },
            libraries: Vec::new(),
            limits: crate::plugin::ScriptExecuteLimits {
                instructions,
                memory_bytes: 16 * 1024 * 1024,
            },
            removed_globals: Vec::new(),
            chunk_name: crate::plugin::DEFAULT_CHUNK_NAME.to_string(),
            artifact: None,
        }
    }

    #[tokio::test]
    async fn a_reported_panic_is_a_runtime_error_at_its_line() {
        let executor = executor_with(interpreter(PANICKING)).await;
        let output = executor
            .execute_script("guest", &input(Some(1_000_000)))
            .await
            .expect("a reported panic is an answer, not a dispatch failure");
        assert_eq!(
            output,
            ScriptExecuteOutput::Error {
                kind: ScriptErrorKind::Runtime,
                message: "boom".into(),
                line: Some(7),
                raw: "panicked at src/lib.rs:7:5: boom".into(),
                chunk: Some("src/lib.rs".into()),
            }
        );
    }

    #[tokio::test]
    async fn a_trap_without_a_report_stays_a_trap() {
        let executor = executor_with(interpreter(TRAPPING)).await;
        let err = executor
            .execute_script("guest", &input(Some(1_000_000)))
            .await
            .expect_err("a bare trap is the interpreter failing");
        assert!(matches!(err, ExecutionError::Trap(_)), "{err}");
    }

    /// A report of a 1 MiB message, well-formed, followed by the trap.
    const OVERSIZED: &str = r#"(module
        (import "env" "host_script_panic" (func $panic (param i32 i32)))
        (memory (export "memory") 18)
        (data (i32.const 65536) "{\"message\":\"")
        (data (i32.const 1114124) "\",\"file\":\"src/lib.rs\",\"line\":7,\"column\":5}")
        (func (export "alloc") (param i32) (result i32) i32.const 1024)
        (func (export "dealloc") (param i32 i32))
        (func (export "execute") (param i32 i32) (result i64)
            i32.const 65548
            i32.const 97
            i32.const 1048576
            memory.fill
            i32.const 65536
            i32.const 1048630
            call $panic
            unreachable)
    )"#;

    #[tokio::test]
    async fn a_report_past_the_cap_is_dropped() {
        let executor = executor_with(interpreter(OVERSIZED)).await;
        let err = executor
            .execute_script("guest", &input(Some(1_000_000)))
            .await
            .expect_err("an unread report leaves the plain trap");
        assert!(matches!(err, ExecutionError::Trap(_)), "{err}");
    }

    #[test]
    fn a_relative_location_is_placed_under_the_source_root() {
        let report = PanicReport {
            message: "boom".into(),
            file: "src/lib.rs".into(),
            line: 3,
            column: 1,
        };
        let ScriptExecuteOutput::Error {
            message,
            line,
            chunk,
            raw,
            ..
        } = panic_output(
            report.clone(),
            Some(std::path::Path::new("modules/registry")),
        )
        else {
            panic!("expected an error");
        };
        assert_eq!(message, "boom");
        assert_eq!(line, Some(3));
        assert_eq!(chunk.as_deref(), Some("modules/registry/src/lib.rs"));
        assert_eq!(raw, "panicked at modules/registry/src/lib.rs:3:1: boom");
    }

    /// A dependency's location is absolute on the build machine; what reaches
    /// an API caller starts at the crate's own directory.
    #[test]
    fn a_dependency_location_keeps_no_build_machine_path() {
        let at = |file: &str| {
            panic_output(
                PanicReport {
                    message: "boom".into(),
                    file: file.into(),
                    line: 3,
                    column: 1,
                },
                Some(std::path::Path::new("modules/registry")),
            )
        };
        let ScriptExecuteOutput::Error { chunk, raw, .. } = at(
            "/Users/dev/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/serde_json-1.0.140/src/value/mod.rs",
        ) else {
            panic!("expected an error");
        };
        assert_eq!(
            chunk.as_deref(),
            Some("serde_json-1.0.140/src/value/mod.rs")
        );
        assert_eq!(
            raw,
            "panicked at serde_json-1.0.140/src/value/mod.rs:3:1: boom"
        );

        let ScriptExecuteOutput::Error { chunk, .. } =
            at(r"C:\Users\dev\.cargo\registry\src\index.crates.io-1\tokio-1.40.0-rc.1\src\lib.rs")
        else {
            panic!("expected an error");
        };
        assert_eq!(chunk.as_deref(), Some("tokio-1.40.0-rc.1/src/lib.rs"));

        // A git checkout or a path dependency names no version.
        let ScriptExecuteOutput::Error {
            message,
            line,
            chunk,
            raw,
            ..
        } = at("/Users/dev/.cargo/git/checkouts/common-3f2a/abc123/src/lib.rs")
        else {
            panic!("expected an error");
        };
        assert_eq!(message, "boom");
        assert_eq!(line, None);
        assert_eq!(chunk, None);
        assert_eq!(raw, "panicked: boom");
        assert!(!raw.contains("/Users/dev"));
    }
}

#[cfg(test)]
mod compiled {
    use super::panics::{executor_with, input, interpreter};
    use super::*;

    /// `execute` counts to two million, then answers `null`. Roughly ten
    /// million fuel: far past a thousand-instruction budget, well inside a
    /// million-instruction one.
    const COUNTING: &str = r#"(module
        (memory (export "memory") 1)
        (data (i32.const 32768) "{\"ok\":{\"status\":\"returned\",\"value\":null,\"value_kind\":\"other\"}}")
        (func (export "alloc") (param i32) (result i32) i32.const 1024)
        (func (export "dealloc") (param i32 i32))
        (func (export "execute") (param i32 i32) (result i64) (local i32)
            (loop
                local.get 2
                i32.const 1
                i32.add
                local.tee 2
                i32.const 2000000
                i32.lt_u
                br_if 0)
            i64.const 32768
            i64.const 32
            i64.shl
            i64.const 62
            i64.or)
    )"#;

    fn returned_null() -> ScriptExecuteOutput {
        ScriptExecuteOutput::Returned {
            value: serde_json::Value::Null,
            value_kind: crate::plugin::ScriptValueKind::Other,
        }
    }

    #[tokio::test]
    async fn a_module_spending_past_its_instruction_budget_times_out() {
        let executor = executor_with(interpreter(COUNTING)).await;
        let module = Arc::new(interpreter(COUNTING));
        let err = executor
            .execute_compiled_as(module, std::path::Path::new("m"), &input(Some(1_000)), None)
            .await
            .expect_err("100,000 fuel cannot count to two million");
        assert!(matches!(err, ExecutionError::Timeout), "{err}");
    }

    #[tokio::test]
    async fn a_module_inside_its_budget_answers() {
        let executor = executor_with(interpreter(COUNTING)).await;
        let module = Arc::new(interpreter(COUNTING));
        let output = executor
            .execute_compiled_as(
                module,
                std::path::Path::new("m"),
                &input(Some(1_000_000)),
                None,
            )
            .await
            .unwrap();
        assert_eq!(output, returned_null());
    }

    #[tokio::test]
    async fn a_job_module_has_no_fuel_budget() {
        let executor = executor_with(interpreter(COUNTING)).await;
        let module = Arc::new(interpreter(COUNTING));
        let mut job = input(None);
        job.kind = ScriptKind::Job;
        let output = executor
            .execute_compiled_as(module, std::path::Path::new("m"), &job, None)
            .await
            .unwrap();
        assert_eq!(output, returned_null());
    }

    /// The instruction budget is the interpreter's own to enforce; fuel stays
    /// out of an interpreter store whatever the budget says.
    #[tokio::test]
    async fn an_interpreter_is_not_charged_fuel_for_its_budget() {
        let executor = executor_with(interpreter(COUNTING)).await;
        let output = executor
            .execute_script("guest", &input(Some(1_000)))
            .await
            .unwrap();
        assert_eq!(output, returned_null());
    }

    /// Reports a panic, then spins instead of trapping.
    const PANIC_THEN_SPIN: &str = r#"(module
        (import "env" "host_script_panic" (func $panic (param i32 i32)))
        (memory (export "memory") 1)
        (data (i32.const 32768) "{\"message\":\"boom\",\"file\":\"src/lib.rs\",\"line\":7,\"column\":5}")
        (func (export "alloc") (param i32) (result i32) i32.const 1024)
        (func (export "dealloc") (param i32 i32))
        (func (export "execute") (param i32 i32) (result i64)
            i32.const 32768
            i32.const 58
            call $panic
            (loop br 0)
            unreachable)
    )"#;

    #[tokio::test]
    async fn a_reported_panic_does_not_hide_a_later_timeout() {
        let executor = executor_with(interpreter(PANIC_THEN_SPIN)).await;
        let module = Arc::new(interpreter(PANIC_THEN_SPIN));
        let err = executor
            .execute_compiled_as(module, std::path::Path::new("m"), &input(Some(1_000)), None)
            .await
            .expect_err("a guest that never stops runs out of fuel");
        assert!(matches!(err, ExecutionError::Timeout), "{err}");
    }

    /// `alloc` counts to two million before answering, roughly ten million
    /// fuel; `execute` answers `null` at once.
    const COSTLY_ALLOC: &str = r#"(module
        (memory (export "memory") 1)
        (data (i32.const 32768) "{\"ok\":{\"status\":\"returned\",\"value\":null,\"value_kind\":\"other\"}}")
        (func (export "alloc") (param i32) (result i32) (local i32)
            (loop
                local.get 1
                i32.const 1
                i32.add
                local.tee 1
                i32.const 2000000
                i32.lt_u
                br_if 0)
            i32.const 1024)
        (func (export "dealloc") (param i32 i32))
        (func (export "execute") (param i32 i32) (result i64)
            i64.const 32768
            i64.const 32
            i64.shl
            i64.const 62
            i64.or)
    )"#;

    /// Placing the input is the host's work: a one-instruction budget, which
    /// `alloc` overspends at any plausible fuel rate, still reaches `execute`.
    #[tokio::test]
    async fn placing_the_input_is_not_charged_to_the_budget() {
        let executor = executor_with(interpreter(COSTLY_ALLOC)).await;
        let module = Arc::new(interpreter(COSTLY_ALLOC));
        let output = executor
            .execute_compiled_as(module, std::path::Path::new("m"), &input(Some(1)), None)
            .await
            .unwrap();
        assert_eq!(output, returned_null());
    }

    #[tokio::test]
    async fn a_module_panic_is_placed_in_its_source_tree() {
        let executor = executor_with(interpreter(super::panics::PANICKING)).await;
        let module = Arc::new(interpreter(super::panics::PANICKING));
        let output = executor
            .execute_compiled_as(
                module,
                std::path::Path::new("modules/registry"),
                &input(Some(1_000_000)),
                None,
            )
            .await
            .unwrap();
        let ScriptExecuteOutput::Error {
            message,
            line,
            chunk,
            raw,
            ..
        } = output
        else {
            panic!("expected an error, got {output:?}");
        };
        assert_eq!(message, "boom");
        assert_eq!(line, Some(7));
        assert_eq!(chunk.as_deref(), Some("modules/registry/src/lib.rs"));
        assert_eq!(raw, "panicked at modules/registry/src/lib.rs:7:5: boom");
    }
}

#[cfg(test)]
mod job_deadline_tests {
    use super::{ScriptKind, TEST_FILE_GUEST_SECONDS, guest_deadline};
    use std::time::Duration;

    #[test]
    fn each_kind_gets_its_deadline_and_only_a_test_run_bounds_a_job() {
        let wall = Some(Duration::from_secs(7));
        let file = Some(Duration::from_secs(TEST_FILE_GUEST_SECONDS));
        let table = [
            (ScriptKind::Job, false, None),
            (ScriptKind::Job, true, file),
            (ScriptKind::Test, false, file),
            (ScriptKind::Test, true, file),
            (ScriptKind::XrpcQuery, false, wall),
            (ScriptKind::XrpcQuery, true, wall),
            (ScriptKind::XrpcProcedure, false, wall),
            (ScriptKind::XrpcProcedure, true, wall),
            (ScriptKind::RecordEvent, false, wall),
            (ScriptKind::RecordEvent, true, wall),
            (ScriptKind::Label, false, wall),
            (ScriptKind::Label, true, wall),
        ];
        assert_eq!(table.len(), ScriptKind::ALL.len() * 2);
        for (kind, in_test_run, expected) in table {
            assert_eq!(
                guest_deadline(kind, in_test_run, Duration::from_secs(7)),
                expected,
                "{kind:?}, in a test run: {in_test_run}"
            );
        }
    }

    /// Only a test run bounds a job: a served job runs as long as it needs
    /// and stops through `should_stop`, which no test run would ever set.
    #[tokio::test]
    async fn only_a_test_run_bounds_a_job() {
        let mut state =
            crate::test_support::test_state_with_pool(crate::test_support::memory_pool().await);
        assert!(!state.plugin_executor().in_test_run());
        state.test = Some(std::sync::Arc::new(crate::testing::TestHarness::new(
            "a.test.lua",
        )));
        assert!(state.plugin_executor().in_test_run());
    }
}

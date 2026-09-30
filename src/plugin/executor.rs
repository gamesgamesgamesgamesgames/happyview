// src/plugin/executor.rs

use crate::db::DatabaseBackend;
use crate::lexicon::LexiconRegistry;
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
use crate::plugin::runtime::{
    GuestDeadline, MemoryLimiter, UNBOUNDED_TICKS, WasmRuntime, fuel_for, memory_ceiling,
    script_memory_ceiling,
};
use crate::plugin::secrets::load_plugin_secrets;
use crate::plugin::{
    ExternalProfile, LoadedPlugin, PluginInfo, PluginRegistry, ScriptExecuteInput,
    ScriptExecuteOutput, ScriptKind, ScriptValidateInput, ScriptValidateOutput, TokenSet,
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

    #[error("Timeout (fuel exhausted)")]
    Timeout,

    #[error(
        "Memory limit exceeded: the plugin asked for {requested} bytes against a {ceiling}-byte ceiling"
    )]
    MemoryLimit { requested: usize, ceiling: usize },

    #[error("Missing export: {0}")]
    MissingExport(String),

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

    /// Size the store's memory ceiling from the Lua-level limit a script
    /// runs under.
    pub fn set_script_memory_limit(&mut self, script_memory_bytes: usize) {
        self.store.data_mut().limiter =
            MemoryLimiter::new(script_memory_ceiling(script_memory_bytes));
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
            .set_fuel(fuel_for(self.store.data().plugin_type))
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
        self.store
            .set_fuel(fuel_for(self.store.data().plugin_type))
            .map_err(ExecutionError::Trap)?;

        let (input_ptr, input_len) = write_to_guest(&mut self.store, &input_bytes)
            .await
            .map_err(|_| ExecutionError::MemoryAllocation)?;

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

/// Classify a wasmtime error as Timeout, MemoryLimit or Trap. A refused
/// memory growth is read off the store rather than the error, because the
/// guest's reaction to it is an ordinary trap; an interrupt is read first,
/// because a guest that survived a refusal and then ran out of time timed
/// out.
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

/// Factory for creating plugin instances
#[derive(Clone)]
pub struct PluginExecutor {
    runtime: Arc<WasmRuntime>,
    registry: Arc<PluginRegistry>,
    db: sqlx::AnyPool,
    db_backend: DatabaseBackend,
    http_client: reqwest::Client,
    lexicons: Arc<LexiconRegistry>,
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

impl PluginExecutor {
    pub fn new(
        runtime: Arc<WasmRuntime>,
        registry: Arc<PluginRegistry>,
        db: sqlx::AnyPool,
        db_backend: DatabaseBackend,
        http_client: reqwest::Client,
        lexicons: Arc<LexiconRegistry>,
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
        let mut inst = self.instantiate_library(lib_id, ctx, caller, depth).await?;
        inst.call_library_function(function, args, ctx).await
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
        // The guard list is the host's, whatever the caller sent: see
        // `host_removed_globals`.
        let input = &ScriptExecuteInput {
            removed_globals: host_removed_globals(),
            ..input.clone()
        };
        let mut inst = self
            .instantiate_interpreter(plugin_id, Some(input), caller)
            .await?;
        inst.set_script_memory_limit(
            usize::try_from(input.limits.memory_bytes).unwrap_or(usize::MAX),
        );
        // Running long is what a job is for, and `should_stop` is its stop;
        // every other kind is bounded by the operator's wall clock, re-armed
        // here so a run starts with a whole budget rather than what
        // instantiation left.
        match input.kind {
            ScriptKind::Job => inst.lift_guest_deadline(),
            _ => inst.arm_guest_deadline(self.script_wall_clock()),
        }
        inst.call_execute(input).await
    }

    /// Ask interpreter `plugin_id` whether `source` is a script it can run.
    /// Validation reads no run, so it carries no context and no session.
    pub async fn validate_script(
        &self,
        plugin_id: &str,
        source: &str,
    ) -> Result<ScriptValidateOutput, ExecutionError> {
        let mut inst = self.instantiate_interpreter(plugin_id, None, None).await?;
        inst.call_validate(&ScriptValidateInput {
            source: source.to_string(),
            removed_globals: host_removed_globals(),
        })
        .await
    }

    /// A library's API surface, computed once per registration.
    pub async fn api_surface(&self, lib_id: &str) -> Result<Arc<ApiSurface>, ExecutionError> {
        if let Some(cached) = self.registry.cached_api_surface(lib_id).await {
            return Ok(cached);
        }
        let mut inst = self
            .instantiate_library(lib_id, &LibraryCallContext::default(), None, 0)
            .await?;
        let surface = Arc::new(inst.call_get_api_surface().await?);
        self.registry
            .cache_api_surface(lib_id, surface.clone())
            .await;
        Ok(surface)
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
    /// anywhere else, plus the run the four `script:host` imports act on.
    ///
    /// No plugin secrets are loaded. An interpreter has no configuration of
    /// its own — `ctx.env` is the script's and travels in the input — and a
    /// secrets query per script run would be on the hottest path there is.
    async fn instantiate_interpreter(
        &self,
        plugin_id: &str,
        input: Option<&ScriptExecuteInput>,
        caller: Option<Arc<CallerSession>>,
    ) -> Result<PluginInstance, ExecutionError> {
        let plugin = self
            .registry
            .get(plugin_id)
            .await
            .ok_or_else(|| ExecutionError::PluginNotFound(plugin_id.to_string()))?;
        if plugin.plugin_type() != PluginType::Interpreter {
            return Err(ExecutionError::NotAnInterpreter(plugin_id.to_string()));
        }
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
        let mut inst = self
            .instantiate(plugin_id, &scope, HashMap::new(), serde_json::Value::Null)
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
        });
        Ok(inst)
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
        // Get plugin from registry
        let plugin = self
            .registry
            .get(plugin_id)
            .await
            .ok_or_else(|| ExecutionError::PluginNotFound(plugin_id.to_string()))?;

        let module = self
            .runtime
            .module_for(&plugin)
            .map_err(ExecutionError::Instantiation)?;

        let linker = self.build_linker(&module)?;
        let state = self.build_state(&plugin, scope, secrets, config).await?;

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
            _lexicons: std::sync::Arc<crate::lexicon::LexiconRegistry>,
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
            Arc::new(LexiconRegistry::new()),
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
            Arc::new(LexiconRegistry::new()),
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
            Arc::new(LexiconRegistry::new()),
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
            Arc::new(LexiconRegistry::new()),
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
    /// the linker holds are the names the wrapper recorded, `host_log`
    /// excepted.
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
            if name == "host_log" {
                continue;
            }
            assert!(
                wrapped.contains(name.as_str()),
                "{name} bypasses the wrapper"
            );
        }
    }

    fn bindings_test_state(runtime: &WasmRuntime) -> PluginState {
        sqlx::any::install_default_drivers();
        PluginState {
            plugin_id: "p".into(),
            scope: "s".into(),
            secrets: HashMap::new(),
            config: serde_json::Value::Null,
            db: None,
            db_backend: DatabaseBackend::Sqlite,
            http_client: reqwest::Client::new(),
            lexicons: Arc::new(LexiconRegistry::new()),
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
        }
    }
}

/// The interpreter path against the `interpreter_echo` fixture, for the two
/// properties that need the instance itself rather than `execute_script`:
/// what `instantiate` bounds on its own, and what the four `script:host`
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
            Arc::new(LexiconRegistry::new()),
        )
        .with_app_state(state)
        .instantiate("echo", "test", HashMap::new(), serde_json::Value::Null)
        .await
        .unwrap()
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

    /// The same four imports on an instance with no run refuse rather than
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
            Arc::new(LexiconRegistry::new()),
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
            Arc::new(LexiconRegistry::new()),
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
        let runtime = Arc::new(WasmRuntime::new().unwrap());
        let registry = Arc::new(PluginRegistry::new().with_runtime(runtime.clone()));
        registry.register(plugin).await;
        let executor = PluginExecutor::new(
            runtime.clone(),
            registry,
            crate::test_support::memory_pool().await,
            DatabaseBackend::Sqlite,
            reqwest::Client::new(),
            Arc::new(LexiconRegistry::new()),
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
            Arc::new(LexiconRegistry::new()),
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
        sqlx::any::install_default_drivers();
        let db = sqlx::AnyPool::connect("sqlite::memory:").await.unwrap();
        PluginExecutor::new(
            Arc::new(WasmRuntime::new().unwrap()),
            registry,
            db,
            DatabaseBackend::Sqlite,
            reqwest::Client::new(),
            Arc::new(LexiconRegistry::new()),
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
            writer.module_for(&plugin).unwrap();
            for i in 0..3 {
                let reader = WasmRuntime::with_cache_dir(Some(dir.clone())).unwrap();
                let started = Instant::now();
                reader.module_for(&plugin).unwrap();
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

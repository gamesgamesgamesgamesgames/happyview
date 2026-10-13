pub mod attestation;
pub mod caller;
pub mod capabilities;
pub mod config;
pub mod encryption;
pub mod executor;
pub mod graph;
pub mod host;
pub mod library;
pub mod loader;
pub mod memory;
pub mod official_registry;
pub mod prepared;
pub mod registry_source;
mod runtime;
pub mod secrets;
mod types;

pub use executor::{ExecutionError, PluginExecutor, PluginInstance};
pub use memory::{MemoryError, PluginEnvelopeError, PluginResponse};
pub use runtime::{
    DEFAULT_SCRIPT_MEMORY_BYTES, EPOCH_TICK, GuestDeadline, LIBRARY_MEMORY_CEILING, MemoryLimiter,
    UNBOUNDED_TICKS, WasmRuntime, fuel_for, memory_ceiling, script_memory_ceiling,
};
pub use types::*;

/// The interpreter contract, under the host's names: what `execute` and
/// `validate` take and return, and what the four `script:host` imports
/// receive. Defined in the SDK the interpreter builds against, so the two
/// sides cannot drift.
pub use happyview_plugin_sdk::wire::{
    ChunkRole, DEFAULT_CHUNK_NAME, ExecuteContext as ScriptExecuteContext,
    ExecuteInput as ScriptExecuteInput, ExecuteLimits as ScriptExecuteLimits,
    ExecuteOutput as ScriptExecuteOutput, ImportRef, JobProgressRequest, JobShouldStopRequest,
    JobWaitRequest, LibraryRef as ScriptLibraryRef, ModuleResolve, PreparedArtifact,
    ResolvedModule, ScriptErrorKind, ScriptJob, ScriptKind, ScriptLogRequest, ScriptSpace,
    ScriptValueKind, ValidateError as ScriptValidateError, ValidateInput as ScriptValidateInput,
    ValidateOutput as ScriptValidateOutput,
};

use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use graph::{GraphError, PluginNode};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Registry of loaded plugins
pub struct PluginRegistry {
    plugins: RwLock<HashMap<String, Arc<LoadedPlugin>>>,
    db: Option<sqlx::AnyPool>,
    db_backend: DatabaseBackend,
    api_surfaces: RwLock<HashMap<String, Arc<library::ApiSurface>>>,
    /// The runtime whose compiled-module cache is keyed by the ids this
    /// registry hands out, so a registration that replaces an id also drops
    /// the module compiled for the outgoing one.
    runtime: Option<Arc<WasmRuntime>>,
    /// One stamp per installed interpreter, computed when it registers so a
    /// run never hashes a module. Each is held beside the exact module it
    /// describes, so a run that instantiated a build since replaced is told
    /// it has no stamp rather than handed its successor's.
    stamps: std::sync::RwLock<HashMap<String, (Arc<LoadedPlugin>, prepared::ArtifactStamp)>>,
    /// Dashboard scripts' artifacts. Here rather than with the scripts
    /// because what invalidates an entry is an interpreter changing, and
    /// that happens only in `register` and `remove`.
    prepared: prepared::PreparedCache,
    /// Bumped whenever an interpreter registers or leaves, so a project
    /// snapshot whose artifacts it made can be rebuilt.
    interpreter_generation: tokio::sync::watch::Sender<u64>,
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self {
            plugins: RwLock::new(HashMap::new()),
            db: None,
            db_backend: DatabaseBackend::Sqlite,
            api_surfaces: RwLock::new(HashMap::new()),
            runtime: None,
            stamps: std::sync::RwLock::new(HashMap::new()),
            prepared: prepared::PreparedCache::default(),
            interpreter_generation: tokio::sync::watch::channel(0).0,
        }
    }
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a registry backed by a database for persistence
    pub fn with_db(db: sqlx::AnyPool, db_backend: DatabaseBackend) -> Self {
        Self {
            plugins: RwLock::new(HashMap::new()),
            db: Some(db),
            db_backend,
            api_surfaces: RwLock::new(HashMap::new()),
            runtime: None,
            stamps: std::sync::RwLock::new(HashMap::new()),
            prepared: prepared::PreparedCache::default(),
            interpreter_generation: tokio::sync::watch::channel(0).0,
        }
    }

    /// Evict a plugin's compiled module from `runtime` whenever its id is
    /// re-registered or removed.
    pub fn with_runtime(mut self, runtime: Arc<WasmRuntime>) -> Self {
        self.runtime = Some(runtime);
        self
    }

    fn evict_module(&self, id: &str) {
        if let Some(runtime) = &self.runtime {
            runtime.evict(id);
        }
    }

    pub fn prepared(&self) -> &prepared::PreparedCache {
        &self.prepared
    }

    /// The stamp of whichever build is registered under `id` now. A run
    /// stamping or checking an artifact wants `artifact_stamp_for`, which
    /// answers for the module it actually instantiated.
    pub fn artifact_stamp(&self, id: &str) -> Option<prepared::ArtifactStamp> {
        self.stamps
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .map(|(_, stamp)| stamp.clone())
    }

    /// The stamp for this exact module, or `None` once it has been replaced
    /// or removed. Compared by identity, as the module cache is, so an
    /// artifact one build returned can never be labelled as another's.
    pub fn artifact_stamp_for(
        &self,
        plugin: &Arc<LoadedPlugin>,
    ) -> Option<prepared::ArtifactStamp> {
        self.stamps
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&plugin.info.id)
            .filter(|(current, _)| Arc::ptr_eq(current, plugin))
            .map(|(_, stamp)| stamp.clone())
    }

    /// Stamp `artifact` for `plugin` and cache it against `source`, unless
    /// `plugin` has been superseded since it prepared, in which case nothing
    /// is stored and `None` comes back. The check and the store happen under
    /// the lock a re-registration evicts under, so an entry from an outgoing
    /// build cannot land after its eviction and outlive it.
    pub fn store_prepared(
        &self,
        plugin: &Arc<LoadedPlugin>,
        source: &str,
        artifact: Option<PreparedArtifact>,
    ) -> prepared::Cached {
        let stamps = self.stamps.read().unwrap_or_else(|e| e.into_inner());
        let (_, stamp) = stamps
            .get(&plugin.info.id)
            .filter(|(current, _)| Arc::ptr_eq(current, plugin))?;
        let artifact = artifact.map(|mut artifact| {
            stamp.apply(&mut artifact);
            artifact
        });
        self.prepared.store(&plugin.info.id, source, artifact)
    }

    pub fn interpreter_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.interpreter_generation.subscribe()
    }

    /// Every artifact an outgoing build made is now unusable: drop the
    /// cached ones, restamp, and tell whoever holds the rest. Callers hold
    /// the `plugins` write lock, so the stamp always describes the module
    /// that lock leaves registered, however registrations interleave.
    fn interpreter_changed(&self, id: &str, incoming: Option<&Arc<LoadedPlugin>>) {
        let mut stamps = self.stamps.write().unwrap_or_else(|e| e.into_inner());
        match incoming {
            Some(plugin) => {
                stamps.insert(
                    id.to_string(),
                    (plugin.clone(), prepared::ArtifactStamp::of(plugin)),
                );
            }
            None => {
                stamps.remove(id);
            }
        }
        self.prepared.evict_interpreter(id);
        drop(stamps);
        // `send_modify` rather than `send`: a registry nobody is watching
        // must still count the change, and `send` refuses with no receivers.
        self.interpreter_generation.send_modify(|g| *g += 1);
    }

    /// The wasm hashes of every installed plugin, as the module cache keys
    /// them.
    pub async fn live_wasm_sha256s(&self) -> std::collections::HashSet<String> {
        use sha2::{Digest, Sha256};
        self.plugins
            .read()
            .await
            .values()
            .map(|p| hex::encode(Sha256::digest(&p.wasm_bytes)))
            .collect()
    }

    pub async fn register(&self, plugin: LoadedPlugin) {
        let id = plugin.info.id.clone();
        self.invalidate_api_surface(&id).await;
        self.evict_module(&id);

        // Persist to database if configured
        if let Some(db) = &self.db
            && let Err(e) = self.persist_plugin(db, &plugin).await
        {
            tracing::error!(plugin_id = %id, error = %e, "Failed to persist plugin to database");
        }

        // Insert before restamping, so whoever the signal wakes finds the
        // new build registered; an interpreter replaced by something else
        // leaves exactly as a removed one does.
        let plugin = Arc::new(plugin);
        let mut plugins = self.plugins.write().await;
        let previous = plugins.insert(id.clone(), plugin.clone());
        if plugin.plugin_type() == PluginType::Interpreter {
            self.interpreter_changed(&id, Some(&plugin));
        } else if previous.is_some_and(|p| p.plugin_type() == PluginType::Interpreter) {
            self.interpreter_changed(&id, None);
        }
    }

    pub async fn cached_api_surface(&self, id: &str) -> Option<Arc<library::ApiSurface>> {
        self.api_surfaces.read().await.get(id).cloned()
    }

    pub async fn cache_api_surface(&self, id: &str, surface: Arc<library::ApiSurface>) {
        self.api_surfaces
            .write()
            .await
            .insert(id.to_string(), surface);
    }

    pub async fn invalidate_api_surface(&self, id: &str) {
        self.api_surfaces.write().await.remove(id);
    }

    async fn persist_plugin(
        &self,
        db: &sqlx::AnyPool,
        plugin: &LoadedPlugin,
    ) -> Result<(), sqlx::Error> {
        let (source, url, sha256) = match &plugin.source {
            PluginSource::File { path } => ("file", Some(path.display().to_string()), None),
            PluginSource::Url { url, sha256 } => ("url", Some(url.clone()), sha256.clone()),
        };

        // Serialize manifest to JSON if present
        let manifest_json = plugin
            .manifest
            .as_ref()
            .and_then(|m| serde_json::to_string(m).ok());

        let now = now_rfc3339();
        let sql = adapt_sql(
            "INSERT INTO happyview_plugins (id, source, url, sha256, enabled, loaded_at, api_version, manifest)
             VALUES (?, ?, ?, ?, TRUE, ?, ?, ?)
             ON CONFLICT (id) DO UPDATE SET
                source = excluded.source,
                url = excluded.url,
                sha256 = excluded.sha256,
                loaded_at = excluded.loaded_at,
                api_version = excluded.api_version,
                manifest = excluded.manifest",
            self.db_backend,
        );

        crate::db::query(&sql)
            .bind(&plugin.info.id)
            .bind(source)
            .bind(url)
            .bind(sha256)
            .bind(&now)
            .bind(&plugin.info.api_version)
            .bind(manifest_json)
            .execute(db)
            .await?;

        Ok(())
    }

    pub async fn get(&self, id: &str) -> Option<Arc<LoadedPlugin>> {
        self.plugins.read().await.get(id).cloned()
    }

    pub async fn list(&self) -> Vec<Arc<LoadedPlugin>> {
        self.plugins.read().await.values().cloned().collect()
    }

    pub async fn remove(&self, id: &str) -> Option<Arc<LoadedPlugin>> {
        self.invalidate_api_surface(id).await;
        self.evict_module(id);
        let mut plugins = self.plugins.write().await;
        let removed = plugins.remove(id);
        if removed
            .as_ref()
            .is_some_and(|p| p.plugin_type() == PluginType::Interpreter)
        {
            self.interpreter_changed(id, None);
        }
        removed
    }

    /// Load all plugins from the database
    pub async fn load_from_db(&self, http: &reqwest::Client) -> Result<usize, String> {
        let Some(db) = &self.db else {
            return Err("No database configured".into());
        };

        let sql = adapt_sql(
            "SELECT id, source, url, sha256 FROM happyview_plugins WHERE enabled = true",
            self.db_backend,
        );

        let rows: Vec<(String, String, Option<String>, Option<String>)> = crate::db::query_as(&sql)
            .fetch_all(db)
            .await
            .map_err(|e| format!("Failed to load plugins from DB: {}", e))?;

        let mut plugins = Vec::new();

        for (id, source, url, sha256) in rows {
            // Skip if already loaded
            if self.plugins.read().await.contains_key(&id) {
                continue;
            }

            match source.as_str() {
                "url" => {
                    if let Some(url) = url {
                        // Load via manifest
                        match loader::fetch_manifest(http, &url).await {
                            Ok(preview) => {
                                match loader::load_from_manifest(http, &preview, sha256.as_deref())
                                    .await
                                {
                                    Ok(plugin) => {
                                        tracing::info!(plugin_id = %id, "Loaded plugin from DB");
                                        plugins.push(plugin);
                                    }
                                    Err(e) => {
                                        tracing::error!(plugin_id = %id, error = %e, "Failed to load plugin WASM");
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::error!(plugin_id = %id, error = %e, "Failed to fetch plugin manifest");
                            }
                        }
                    }
                }
                "file" => {
                    if let Some(path) = url {
                        let path = std::path::Path::new(&path);
                        match loader::load_from_file(path).await {
                            Ok(plugin) => {
                                tracing::info!(plugin_id = %id, "Loaded plugin from DB (file)");
                                plugins.push(plugin);
                            }
                            Err(e) => {
                                tracing::error!(plugin_id = %id, error = %e, "Failed to load plugin from DB");
                            }
                        }
                    }
                }
                _ => {
                    tracing::warn!(plugin_id = %id, source = %source, "Unknown plugin source type");
                }
            }
        }

        let count = plugins.len();
        let failures = self.register_all(plugins).await;
        for (id, err) in &failures {
            tracing::error!(plugin_id = %id, error = %err, "Failed to install plugin from DB");
        }
        Ok(count - failures.len())
    }

    /// Snapshot of the graph for validation.
    pub async fn nodes(&self) -> Vec<PluginNode> {
        self.plugins
            .read()
            .await
            .values()
            .map(|p| PluginNode::from(p.as_ref()))
            .collect()
    }

    /// Validate against the dependency graph, then register (and persist).
    /// This is what the admin API and boot loading go through; `register`
    /// stays as the unchecked primitive for tests and internal callers.
    pub async fn install(&self, plugin: LoadedPlugin) -> Result<(), GraphError> {
        // A library's namespace is how other plugins reach it
        // (`get_library_by_namespace`), so two installed plugins claiming the
        // same one would make that lookup ambiguous. Reinstalling the same id
        // under its own namespace is fine — that's an upgrade, not a clash.
        if let Some(namespace) = plugin.namespace() {
            let existing = self.plugins.read().await;
            if let Some(other) = existing
                .values()
                .find(|p| p.info.id != plugin.info.id && p.namespace() == Some(namespace))
            {
                return Err(GraphError::NamespaceTaken {
                    namespace: namespace.to_string(),
                    by: other.info.id.clone(),
                });
            }
        }

        // A language resolves to one interpreter: `scripts.script_type` holds
        // the id, so two claiming it would make that lookup ambiguous. The
        // loader cannot see the other installed plugins, which is why the
        // check is here.
        if let Some(language_id) = plugin.language_id() {
            let existing = self.plugins.read().await;
            if let Some(other) = existing
                .values()
                .find(|p| p.info.id != plugin.info.id && p.language_id() == Some(language_id))
            {
                return Err(GraphError::LanguageIdTaken {
                    language_id: language_id.to_string(),
                    by: other.info.id.clone(),
                });
            }
        }

        // A project file's extension is the only thing that picks its
        // interpreter, so a second claim would make the choice depend on
        // which plugin happened to load first. The loader cannot see the
        // other installed plugins, which is why the check is here.
        {
            let existing = self.plugins.read().await;
            for extension in plugin.extensions() {
                if let Some(other) = existing
                    .values()
                    .find(|p| p.info.id != plugin.info.id && p.extensions().contains(&extension))
                {
                    return Err(GraphError::ExtensionTaken {
                        extension: extension.to_string(),
                        by: other.info.id.clone(),
                    });
                }
            }
        }

        let candidate = PluginNode::from(&plugin);
        let installed = self.nodes().await;
        graph::validate_install(&installed, &candidate)?;
        self.register(plugin).await;
        Ok(())
    }

    /// Remove a plugin. Refused with `HasDependents` unless `force`, in which
    /// case dependents are removed too. Returns the ids removed, dependents
    /// first; empty when `id` was not installed.
    pub async fn uninstall(&self, id: &str, force: bool) -> Result<Vec<String>, GraphError> {
        if self.get(id).await.is_none() {
            return Ok(Vec::new());
        }
        let installed = self.nodes().await;
        let mut ids = if force {
            graph::transitive_dependents(&installed, id)
        } else {
            graph::validate_remove(&installed, id)?;
            Vec::new()
        };
        ids.push(id.to_string());
        for id in &ids {
            self.remove(id).await;
        }
        Ok(ids)
    }

    /// Install a batch in dependency order. Plugins whose dependencies are
    /// unmet, or that sit on a cycle, are skipped and reported by id.
    pub async fn register_all(&self, plugins: Vec<LoadedPlugin>) -> Vec<(String, GraphError)> {
        let mut failures = Vec::new();
        let mut pending: HashMap<String, LoadedPlugin> = plugins
            .into_iter()
            .map(|p| (p.info.id.clone(), p))
            .collect();

        let mut nodes: Vec<PluginNode> = self
            .nodes()
            .await
            .into_iter()
            .filter(|n| !pending.contains_key(&n.id))
            .collect();
        nodes.extend(pending.values().map(PluginNode::from));
        let order = match graph::load_order(&nodes) {
            Ok(order) => order,
            Err(GraphError::Cycle(cycle)) => {
                // Drop the cycle members, then order what is left.
                for id in &cycle {
                    if pending.remove(id).is_some() {
                        failures.push((id.clone(), GraphError::Cycle(cycle.clone())));
                    }
                }
                let remaining: Vec<PluginNode> = nodes
                    .iter()
                    .filter(|n| !cycle.contains(&n.id))
                    .cloned()
                    .collect();
                graph::load_order(&remaining).unwrap_or_default()
            }
            Err(other) => {
                for (id, _) in pending.drain() {
                    failures.push((id, other.clone()));
                }
                Vec::new()
            }
        };

        for id in order {
            let Some(plugin) = pending.remove(&id) else {
                continue;
            };
            if let Err(e) = self.install(plugin).await {
                failures.push((id, e));
            }
        }
        failures.sort_by(|a, b| a.0.cmp(&b.0));
        failures
    }

    pub async fn list_by_type(&self, plugin_type: PluginType) -> Vec<Arc<LoadedPlugin>> {
        self.plugins
            .read()
            .await
            .values()
            .filter(|p| p.plugin_type() == plugin_type)
            .cloned()
            .collect()
    }

    pub async fn get_library_by_namespace(&self, namespace: &str) -> Option<Arc<LoadedPlugin>> {
        self.plugins
            .read()
            .await
            .values()
            .find(|p| p.namespace() == Some(namespace))
            .cloned()
    }

    /// The interpreter that runs `language_id`. `install` refuses a second
    /// plugin claiming a language, so at most one answers; and only an
    /// interpreter carries a `language_id` at all, so a library declaring one
    /// is not reachable through here.
    pub async fn get_interpreter_by_language(
        &self,
        language_id: &str,
    ) -> Option<Arc<LoadedPlugin>> {
        self.plugins
            .read()
            .await
            .values()
            .find(|p| p.language_id() == Some(language_id))
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::memory_pool;

    fn echo_like_interpreter(version: &str) -> LoadedPlugin {
        let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "id": "interp", "name": "interp", "version": version, "api_version": "2",
            "plugin_type": "interpreter", "language_id": "interp", "capabilities": [],
        }))
        .unwrap();
        LoadedPlugin {
            info: manifest.clone().into(),
            source: PluginSource::File {
                path: "interp".into(),
            },
            wasm_bytes: version.as_bytes().to_vec(),
            manifest: Some(manifest),
        }
    }

    fn library_named(id: &str) -> LoadedPlugin {
        let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "version": "1.0.0", "api_version": "2",
            "plugin_type": "library", "capabilities": [],
        }))
        .unwrap();
        LoadedPlugin {
            info: manifest.clone().into(),
            source: PluginSource::File { path: id.into() },
            wasm_bytes: vec![],
            manifest: Some(manifest),
        }
    }

    fn some_artifact() -> PreparedArtifact {
        PreparedArtifact {
            interpreter: "forged".into(),
            version: "forged".into(),
            engine: "e".into(),
            format: "f".into(),
            bytes: vec![1],
            source_map: None,
        }
    }

    /// Registering an interpreter is what changes the build its artifacts
    /// must match, so the stamp, the cache and the change signal all move
    /// together there and nowhere else.
    #[tokio::test]
    async fn registering_an_interpreter_restamps_evicts_and_signals() {
        let registry = PluginRegistry::new();
        let mut changes = registry.interpreter_changes();
        assert_eq!(registry.artifact_stamp("interp"), None);

        registry.register(echo_like_interpreter("1.0.0")).await;
        let first = registry
            .artifact_stamp("interp")
            .expect("stamped on register");
        assert!(changes.has_changed().unwrap());
        changes.mark_unchanged();

        registry.prepared().store("interp", "src", None);
        registry.register(echo_like_interpreter("1.0.1")).await;
        let second = registry.artifact_stamp("interp").unwrap();
        assert_ne!(first, second);
        assert_eq!(registry.prepared().lookup("interp", "src"), None);
        assert!(changes.has_changed().unwrap());
        changes.mark_unchanged();

        registry.prepared().store("interp", "src", None);
        registry.remove("interp").await;
        assert_eq!(registry.artifact_stamp("interp"), None);
        assert_eq!(registry.prepared().lookup("interp", "src"), None);
        assert!(changes.has_changed().unwrap());
    }

    #[tokio::test]
    async fn a_library_registration_is_not_an_interpreter_change() {
        let registry = PluginRegistry::new();
        let changes = registry.interpreter_changes();
        registry.register(library_named("lib")).await;
        assert!(!changes.has_changed().unwrap());
        assert_eq!(registry.artifact_stamp("lib"), None);
    }

    /// A run holds the module it instantiated, and a re-registration can
    /// land while it is in flight. Its stamp has to be the one for that
    /// module or nothing, never the successor's, or one build's bytecode
    /// would be labelled as loadable by another.
    #[tokio::test]
    async fn a_superseded_module_has_no_stamp() {
        let registry = PluginRegistry::new();
        registry.register(echo_like_interpreter("1.0.0")).await;
        let old = registry.get("interp").await.unwrap();
        assert_eq!(
            registry.artifact_stamp_for(&old),
            registry.artifact_stamp("interp")
        );

        registry.register(echo_like_interpreter("1.0.1")).await;
        let new = registry.get("interp").await.unwrap();
        assert_eq!(registry.artifact_stamp_for(&old), None);
        assert_eq!(
            registry.artifact_stamp_for(&new),
            Some(prepared::ArtifactStamp::of(&new))
        );

        registry.remove("interp").await;
        assert_eq!(registry.artifact_stamp_for(&new), None);
    }

    /// A prepare that finishes after its interpreter was replaced must not
    /// fill the cache: the eviction has already run, so an entry stored now
    /// would outlive the build that made it.
    #[tokio::test]
    async fn a_superseded_module_cannot_fill_the_cache() {
        let registry = PluginRegistry::new();
        registry.register(echo_like_interpreter("1.0.0")).await;
        let old = registry.get("interp").await.unwrap();
        registry.register(echo_like_interpreter("1.0.1")).await;

        assert_eq!(
            registry.store_prepared(&old, "src", Some(some_artifact())),
            None
        );
        assert_eq!(registry.store_prepared(&old, "none", None), None);
        assert_eq!(registry.prepared().lookup("interp", "src"), None);
        assert_eq!(registry.prepared().lookup("interp", "none"), None);

        let new = registry.get("interp").await.unwrap();
        let stored = registry
            .store_prepared(&new, "src", Some(some_artifact()))
            .expect("the current build stores");
        assert!(prepared::ArtifactStamp::of(&new).matches(&stored));
        assert_eq!(
            registry.prepared().lookup("interp", "src"),
            Some(Some(stored))
        );
    }

    /// An interpreter id taken over by a library leaves the same way a
    /// removed interpreter does, or its stamp would outlive it.
    #[tokio::test]
    async fn an_interpreter_replaced_by_a_library_leaves_as_if_removed() {
        let registry = PluginRegistry::new();
        registry.register(echo_like_interpreter("1.0.0")).await;
        let mut changes = registry.interpreter_changes();
        changes.mark_unchanged();
        registry.prepared().store("interp", "src", None);

        registry.register(library_named("interp")).await;
        assert_eq!(registry.artifact_stamp("interp"), None);
        assert_eq!(registry.prepared().lookup("interp", "src"), None);
        assert!(changes.has_changed().unwrap());
    }

    /// Re-registering an id, or removing it, drops the module the runtime
    /// compiled for it; the next instantiation compiles the new bytes.
    #[tokio::test]
    async fn re_registration_evicts_the_compiled_module() {
        let runtime = Arc::new(WasmRuntime::without_ticker().unwrap());
        let registry = PluginRegistry::new().with_runtime(runtime.clone());
        let plugin = |value: i32| {
            let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
                "id": "p", "name": "p", "version": "1.0.0", "api_version": "2",
                "plugin_type": "library", "namespace": "p", "capabilities": [],
            }))
            .unwrap();
            LoadedPlugin {
                info: manifest.clone().into(),
                source: PluginSource::File { path: "p".into() },
                wasm_bytes: wat::parse_str(format!(
                    r#"(module (func (export "answer") (result i32) i32.const {value}))"#
                ))
                .unwrap(),
                manifest: Some(manifest),
            }
        };
        let answer = |module: &wasmtime::Module| {
            let mut store = wasmtime::Store::new(runtime.engine(), ());
            store.set_fuel(1_000_000).unwrap();
            store.set_epoch_deadline(UNBOUNDED_TICKS);
            let instance = wasmtime::Instance::new(&mut store, module, &[]).unwrap();
            instance
                .get_typed_func::<(), i32>(&mut store, "answer")
                .unwrap()
                .call(&mut store, ())
                .unwrap()
        };

        registry.register(plugin(1)).await;
        assert_eq!(
            answer(
                &runtime
                    .module_for(&registry.get("p").await.unwrap())
                    .unwrap()
            ),
            1
        );
        assert_eq!(runtime.compile_count(), 1);

        registry.register(plugin(2)).await;
        assert_eq!(
            answer(
                &runtime
                    .module_for(&registry.get("p").await.unwrap())
                    .unwrap()
            ),
            2
        );
        assert_eq!(runtime.compile_count(), 2);

        registry.remove("p").await;
        registry.register(plugin(2)).await;
        runtime
            .module_for(&registry.get("p").await.unwrap())
            .unwrap();
        assert_eq!(runtime.compile_count(), 3);
    }

    /// `enabled` is BOOLEAN on Postgres and INTEGER on SQLite, so the persist
    /// statement has to use a literal both backends accept.
    #[tokio::test]
    async fn register_persists_an_enabled_row() {
        let pool = memory_pool().await;
        crate::db::query(
            "CREATE TABLE happyview_plugins (
                id TEXT PRIMARY KEY, source TEXT NOT NULL, url TEXT, sha256 TEXT,
                enabled INTEGER NOT NULL DEFAULT 1, loaded_at TEXT,
                api_version TEXT NOT NULL, manifest TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        let registry = PluginRegistry::with_db(pool.clone(), DatabaseBackend::Sqlite);
        registry
            .register(LoadedPlugin {
                info: PluginInfo {
                    id: "p".into(),
                    name: "p".into(),
                    version: "1.0.0".into(),
                    api_version: "2".into(),
                    icon_url: None,
                    required_secrets: vec![],
                    auth_type: "openid".into(),
                    config_schema: None,
                },
                source: PluginSource::Url {
                    url: "https://example.test/p.wasm".into(),
                    sha256: None,
                },
                wasm_bytes: vec![],
                manifest: None,
            })
            .await;

        let (enabled,): (i64,) =
            crate::db::query_as("SELECT enabled FROM happyview_plugins WHERE id = 'p'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(enabled, 1);
    }

    /// A language resolves to one interpreter, so a second one claiming an
    /// installed `language_id` is refused naming the incumbent; the same
    /// interpreter reinstalled keeps its own.
    #[tokio::test]
    async fn installing_a_second_interpreter_for_one_language_is_refused() {
        let interpreter = |id: &str, language_id: &str| {
            let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
                "id": id, "name": id, "version": "1.0.0", "api_version": "2",
                "plugin_type": "interpreter", "language_id": language_id,
                "capabilities": [],
            }))
            .unwrap();
            LoadedPlugin {
                info: manifest.clone().into(),
                source: PluginSource::File { path: id.into() },
                wasm_bytes: vec![],
                manifest: Some(manifest),
            }
        };

        let registry = PluginRegistry::new();
        registry.install(interpreter("lua", "lua")).await.unwrap();

        let err = registry
            .install(interpreter("lua-fork", "lua"))
            .await
            .expect_err("two interpreters claimed one language");
        assert_eq!(
            err,
            GraphError::LanguageIdTaken {
                language_id: "lua".into(),
                by: "lua".into(),
            }
        );
        assert!(err.to_string().contains("lua"), "{err}");

        // A different language installs beside it, and reinstalling the
        // incumbent is an upgrade rather than a clash.
        registry
            .install(interpreter("lua-fork", "moon"))
            .await
            .unwrap();
        registry.install(interpreter("lua", "lua")).await.unwrap();

        // A library never claims a language, whatever its manifest says.
        let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
            "id": "lib", "name": "lib", "version": "1.0.0", "api_version": "2",
            "plugin_type": "library", "language_id": "lua", "capabilities": [],
        }))
        .unwrap();
        registry
            .install(LoadedPlugin {
                info: manifest.clone().into(),
                source: PluginSource::File { path: "lib".into() },
                wasm_bytes: vec![],
                manifest: Some(manifest),
            })
            .await
            .unwrap();
    }

    /// A project file's extension picks the interpreter that runs it, so two
    /// installed interpreters claiming one extension would make a file's
    /// language a matter of install order.
    #[tokio::test]
    async fn installing_a_second_interpreter_for_one_extension_is_refused() {
        let interpreter = |id: &str, language_id: &str, extra: serde_json::Value| {
            let mut json = serde_json::json!({
                "id": id, "name": id, "version": "1.0.0", "api_version": "2",
                "plugin_type": "interpreter", "language_id": language_id,
                "capabilities": [],
            });
            json.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            let manifest: PluginManifest = serde_json::from_value(json).unwrap();
            LoadedPlugin {
                info: manifest.clone().into(),
                source: PluginSource::File { path: id.into() },
                wasm_bytes: vec![],
                manifest: Some(manifest),
            }
        };

        let registry = PluginRegistry::new();
        registry
            .install(interpreter(
                "lua",
                "lua",
                serde_json::json!({"file_extension": "lua"}),
            ))
            .await
            .unwrap();

        // The incumbent claims `lua` only through its legacy field, which is
        // still a claim.
        let err = registry
            .install(interpreter(
                "moon",
                "moon",
                serde_json::json!({"extensions": ["moon", "lua"]}),
            ))
            .await
            .expect_err("two interpreters claimed one extension");
        assert_eq!(
            err,
            GraphError::ExtensionTaken {
                extension: "lua".into(),
                by: "lua".into(),
            }
        );
        assert!(err.to_string().contains(".lua"), "{err}");

        registry
            .install(interpreter(
                "moon",
                "moon",
                serde_json::json!({"extensions": ["moon"]}),
            ))
            .await
            .unwrap();
        // Reinstalling the incumbent with a wider claim is an upgrade.
        registry
            .install(interpreter(
                "lua",
                "lua",
                serde_json::json!({"extensions": ["lua", "luau"]}),
            ))
            .await
            .unwrap();
    }

    /// A script's `script_type` reaches its interpreter through this lookup,
    /// so a language nothing claims answers `None` rather than falling to
    /// whichever plugin happens to declare the field.
    #[tokio::test]
    async fn an_interpreter_is_found_by_the_language_it_claims() {
        let plugin = |id: &str, plugin_type: &str, language_id: &str| {
            let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
                "id": id, "name": id, "version": "1.0.0", "api_version": "2",
                "plugin_type": plugin_type, "language_id": language_id,
                "namespace": id, "capabilities": [],
            }))
            .unwrap();
            LoadedPlugin {
                info: manifest.clone().into(),
                source: PluginSource::File { path: id.into() },
                wasm_bytes: vec![],
                manifest: Some(manifest),
            }
        };

        let registry = PluginRegistry::new();
        registry
            .install(plugin("happyview-lua", "interpreter", "lua"))
            .await
            .unwrap();
        registry
            .install(plugin("stray", "library", "moon"))
            .await
            .unwrap();

        let found = registry
            .get_interpreter_by_language("lua")
            .await
            .expect("the installed interpreter claims lua");
        assert_eq!(found.info.id, "happyview-lua");

        assert!(
            registry
                .get_interpreter_by_language("python")
                .await
                .is_none(),
            "no interpreter claims python"
        );
        assert!(
            registry.get_interpreter_by_language("moon").await.is_none(),
            "a library's language_id is not a claim"
        );
    }
}

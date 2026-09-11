pub mod attestation;
pub mod capabilities;
pub mod encryption;
pub mod executor;
pub mod graph;
pub mod host;
pub mod library;
pub mod loader;
pub mod memory;
pub mod official_registry;
mod runtime;
pub mod secrets;
mod types;

pub use executor::{ExecutionError, PluginExecutor, PluginInstance};
pub use memory::{MemoryError, PluginEnvelopeError, PluginResponse};
pub use runtime::WasmRuntime;
pub use types::*;

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
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self {
            plugins: RwLock::new(HashMap::new()),
            db: None,
            db_backend: DatabaseBackend::Sqlite,
            api_surfaces: RwLock::new(HashMap::new()),
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
        }
    }

    pub async fn register(&self, plugin: LoadedPlugin) {
        let id = plugin.info.id.clone();
        self.invalidate_api_surface(&id).await;

        // Persist to database if configured
        if let Some(db) = &self.db
            && let Err(e) = self.persist_plugin(db, &plugin).await
        {
            tracing::error!(plugin_id = %id, error = %e, "Failed to persist plugin to database");
        }

        self.plugins.write().await.insert(id, Arc::new(plugin));
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
             VALUES (?, ?, ?, ?, 1, ?, ?, ?)
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
        self.plugins.write().await.remove(id)
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
}

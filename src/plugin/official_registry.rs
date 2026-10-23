//! The plugin catalogue cache.
//!
//! Filled from an AT Protocol registry (see
//! [`registry_source`](super::registry_source)), which describes whatever
//! anyone has published. When the registry cannot be read the previous cache
//! stays, so a network blip does not empty the catalogue.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use tokio::sync::RwLock;

/// The account HappyProto publishes its own plugins from.
pub const OFFICIAL_PUBLISHER_DID: &str = "did:plc:mo7vq4jbioxwafy5zkcc3eux";

/// One version of a catalogue entry, for the update preview and detail page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseEntry {
    pub version: String,
    pub name: String,
    pub published_at: Option<String>,
    /// The version's changelog, as markdown.
    pub body: Option<String>,
    /// The lifecycle state: `active`, `deprecated`, `unavailable` or
    /// `retired`.
    pub state: String,
    /// Why the publisher set that state, when they said.
    pub reason: Option<String>,
    /// The registry saw this version's records change after publication.
    pub suspect: bool,
    /// Whether this version may be installed: its lifecycle allows it, the
    /// registry holds its bytes and it isn't suspect. A version that may not
    /// is still listed, so an operator can see it exists.
    pub installable: bool,
    /// Whether the registry holds the bytes: `pending`, `available` or
    /// `deletedAtSource`.
    pub availability: String,
    /// The `software.release.version` record.
    pub version_uri: String,
    /// The `software.release.manifest` record's CID.
    pub manifest_cid: String,
    /// The module's blob CID, which its bytes are fetched by and checked
    /// against.
    pub package_cid: String,
    /// The plugin manifest file's blob CID.
    pub manifest_blob_cid: String,
}

/// Where one of a project's channels points.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelEntry {
    pub version: String,
    pub state: String,
    /// Whether the version may be installed: its state allows it and the
    /// registry holds its bytes.
    pub installable: bool,
    pub availability: String,
}

/// One plugin in the catalogue.
#[derive(Debug, Clone, Serialize)]
pub struct CatalogueEntry {
    /// The project name, which is the plugin's id.
    pub id: String,
    /// The registry account that published it.
    pub publisher_did: Option<String>,
    /// Published by [`OFFICIAL_PUBLISHER_DID`]. The catalogue lists every
    /// publisher; this is what the dashboard badges.
    pub official: bool,
    pub name: String,
    pub description: Option<String>,
    pub icon_url: Option<String>,
    /// Independent of `icon_url`: an entry may carry either, both, or neither.
    pub banner_url: Option<String>,
    /// `auth`, `library` or `interpreter`.
    pub plugin_type: String,
    /// Every channel the project publishes, by name.
    pub channels: BTreeMap<String, ChannelEntry>,
    /// The version the `latest` channel points at, if it has one.
    pub latest_version: Option<String>,
    /// Every version, newest first.
    pub releases: Vec<ReleaseEntry>,
    /// The publisher's long-form prose, from the latest version's artifact,
    /// with that version's changelog.
    pub sections: PluginSections,
    /// The project's own lifecycle state.
    pub lifecycle_state: String,
    /// Where the registry serves the latest version's manifest, by CID. Kept
    /// for callers that work from URLs: the loader cannot install a registry
    /// entry from it. Empty when there is no latest version.
    pub manifest_url: String,
    /// Where the registry serves the latest version's module, by CID. Kept
    /// for callers that work from URLs, and not installable from by the
    /// loader. Empty when there is no latest version.
    pub wasm_url: String,
}

/// The four long-form fields a plugin version's artifact carries. CommonMark
/// written by a publisher and shown to an operator, so a renderer strips
/// embedded HTML and refuses a link scheme it does not recognise.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct PluginSections {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security: Option<String>,
}

/// Cache state stored on `AppState` behind an `Arc<RwLock<_>>`.
#[derive(Debug, Default)]
pub struct OfficialRegistryState {
    /// The catalogue: one project per name.
    pub plugins: HashMap<String, CatalogueEntry>,
    /// Every project an installed plugin follows, by publisher DID and name.
    /// Separate from `plugins` because a name there belongs to one publisher,
    /// and an installed plugin's project may be another's that shares it.
    pub installed: HashMap<(String, String), CatalogueEntry>,
    pub last_refreshed_at: Option<String>,
}

impl OfficialRegistryState {
    /// The freshest entry read for `did`'s project `name`.
    pub fn project(&self, did: &str, name: &str) -> Option<&CatalogueEntry> {
        self.installed
            .get(&(did.to_owned(), name.to_owned()))
            .or_else(|| {
                self.plugins
                    .get(name)
                    .filter(|e| e.publisher_did.as_deref() == Some(did))
            })
    }
}

pub type SharedRegistry = Arc<RwLock<OfficialRegistryState>>;

#[derive(Debug, Clone)]
pub struct RegistryConfig {
    /// The registry to read the catalogue from. `None` when the catalogue is
    /// off.
    pub registry: Option<super::registry_source::RegistrySource>,
    /// The whole catalogue is off: no refresh task, no request. For an
    /// instance with no outbound network.
    pub disabled: bool,
}

impl RegistryConfig {
    /// Apply `PLUGIN_REGISTRY_URL`: a URL points the catalogue at that
    /// registry, and `off` or an empty value turns the catalogue off, so no
    /// request is ever made. Read here rather than in [`Self::production`]
    /// because `production` backs test fixtures, which should not depend on
    /// the environment.
    pub fn with_registry_from_env(mut self) -> Self {
        let Some(configured) = crate::config::vars::var("PLUGIN_REGISTRY_URL") else {
            return self;
        };
        let configured = configured.trim();
        if configured.is_empty() || configured.eq_ignore_ascii_case("off") {
            tracing::info!(
                "official_registry: PLUGIN_REGISTRY_URL is off or empty, the plugin catalogue is off"
            );
            self.disabled = true;
            self.registry = None;
            return self;
        }
        self.registry = Some(super::registry_source::RegistrySource {
            base_url: configured.to_string(),
            ..super::registry_source::RegistrySource::production()
        });
        self
    }

    pub fn production() -> Self {
        Self {
            registry: Some(super::registry_source::RegistrySource::production()),
            disabled: false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error(transparent)]
    Registry(#[from] super::registry_source::SourceError),
    #[error("the plugin catalogue is turned off (PLUGIN_REGISTRY_URL=off)")]
    Disabled,
}

// The official publisher owns its slugs, and every publisher keeps its own.
// Anyone can publish any slug to the registry, and the cache holds one entry
// per id, so the entry under an installed plugin's id may come from someone
// other than whoever published what is installed. Four places hold the line:
// an official entry is never displaced from the cache by another publisher's;
// an on-demand check asks the publisher the cache names for the id (the
// official one when it names none); an install records the publisher of the
// catalogue entry it was installed from (`publisher_for_install`); and an
// update is offered only from an entry by that same publisher
// (`effective_publisher`). An install that recorded none may be updated only
// by the official publisher.

/// Who an entry speaks for: [`OFFICIAL_PUBLISHER_DID`] for an official entry,
/// and otherwise the registry account that published it.
pub fn effective_publisher(plugin: &CatalogueEntry) -> Option<&str> {
    if plugin.official {
        Some(OFFICIAL_PUBLISHER_DID)
    } else {
        plugin.publisher_did.as_deref()
    }
}

/// The publisher of the catalogue entry an install came from: the entry under
/// the installed plugin's own id, and only when its manifest URL is exactly
/// the one installed. Looked up by id because two packages can name the same
/// artifact, and a stranger's entry pointing at the official module must not
/// lend its DID to the official install. `None` for any URL the entry does
/// not offer, such as one typed by hand.
pub fn publisher_for_install(
    state: &OfficialRegistryState,
    plugin_id: &str,
    url: &str,
) -> Option<String> {
    state
        .plugins
        .get(plugin_id)
        .filter(|p| p.manifest_url == url)
        .and_then(|p| effective_publisher(p).map(str::to_owned))
}

/// Put a freshly read entry in the cache unless it would displace an official
/// entry with another publisher's, and answer whichever entry the cache now
/// holds for that id.
async fn hold(state: &SharedRegistry, plugin: CatalogueEntry) -> CatalogueEntry {
    let mut guard = state.write().await;
    if let Some(existing) = guard.plugins.get(&plugin.id)
        && existing.official
        && !plugin.official
    {
        return existing.clone();
    }
    guard.plugins.insert(plugin.id.clone(), plugin.clone());
    guard.last_refreshed_at = Some(crate::db::now_rfc3339());
    plugin
}

/// Rebuild the cache from the registry, along with the projects `installed`
/// (publisher DID, name) plugins follow. Those the listing doesn't carry, or
/// carries under a name another publisher holds, are each read on their own;
/// one that can't be read keeps its previous entry. On error, the previous
/// cache is retained and the error is returned; a listing cut off at the page
/// cap is one, since it would otherwise replace the catalogue with part of
/// it.
pub async fn refresh_full(
    client: &reqwest::Client,
    config: &RegistryConfig,
    state: &SharedRegistry,
    installed: &[(String, String)],
) -> Result<(), RegistryError> {
    if config.disabled {
        return Ok(());
    }
    let Some(source) = &config.registry else {
        return Ok(());
    };

    let listed = super::registry_source::fetch_listing(client, source).await?;
    tracing::info!(
        count = listed.len(),
        "official_registry: catalogue read from the registry"
    );

    let key = |e: &CatalogueEntry| (e.publisher_did.clone().unwrap_or_default(), e.id.clone());
    let mut followed: HashMap<(String, String), CatalogueEntry> = listed
        .iter()
        .filter(|e| installed.contains(&key(e)))
        .map(|e| (key(e), e.clone()))
        .collect();
    let previous = state.read().await.installed.clone();
    for project in installed {
        if followed.contains_key(project) {
            continue;
        }
        let (did, name) = project;
        match super::registry_source::fetch_project(client, source, did, name).await {
            Ok(Some(entry)) => {
                followed.insert(project.clone(), entry);
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(plugin = %name, publisher = %did, error = %e, "official_registry: installed project unreadable");
                if let Some(entry) = previous.get(project) {
                    followed.insert(project.clone(), entry.clone());
                }
            }
        }
    }

    let catalogue = super::registry_source::one_per_name(listed);
    let mut guard = state.write().await;
    guard.plugins = catalogue.into_iter().map(|p| (p.id.clone(), p)).collect();
    guard.installed = followed;
    guard.last_refreshed_at = Some(crate::db::now_rfc3339());
    Ok(())
}

/// The registry projects installed plugins follow, as (publisher DID, name).
pub async fn installed_projects(
    db: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
) -> Result<Vec<(String, String)>, sqlx::Error> {
    let sql = crate::db::adapt_sql(
        "SELECT DISTINCT registry_did, registry_name FROM happyview_plugins
         WHERE registry_did IS NOT NULL AND registry_name IS NOT NULL",
        backend,
    );
    crate::db::query_as(&sql).fetch_all(db).await
}

/// `refresh_full`, for the projects this instance's plugins follow.
pub async fn refresh_for_installs(
    client: &reqwest::Client,
    config: &RegistryConfig,
    state: &SharedRegistry,
    db: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
) -> Result<(), RegistryError> {
    // Without the list, the catalogue still refreshes; installed projects
    // shadowed by another publisher's name just go stale until next time.
    let installed = installed_projects(db, backend).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "official_registry: installed projects unreadable");
        Vec::new()
    });
    refresh_full(client, config, state, &installed).await?;
    if let Err(e) = attach_url_installs(client, config, state, db, backend).await {
        tracing::warn!(error = %e, "official_registry: url installs not matched to registry projects");
    }
    Ok(())
}

/// Attach plugins installed from a URL to the registry project of the same
/// name, so their updates come from the registry. Each plugin is tried once:
/// a match records the project, and no match flags the plugin unresolved,
/// which keeps it out of later passes. The installed bytes are never
/// touched, and nothing is unloaded.
///
/// A project that can't be read at the moment is left for the next refresh
/// instead of being flagged, since "not found" is the only answer that means
/// the registry has no such project.
async fn attach_url_installs(
    client: &reqwest::Client,
    config: &RegistryConfig,
    state: &SharedRegistry,
    db: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
) -> Result<(), sqlx::Error> {
    let Some(source) = &config.registry else {
        return Ok(());
    };
    let sql = crate::db::adapt_sql(
        "SELECT id, publisher_did, manifest FROM happyview_plugins
         WHERE source = 'url' AND enabled = TRUE
           AND registry_did IS NULL AND registry_unresolved = 0",
        backend,
    );
    let candidates: Vec<(String, Option<String>, Option<String>)> =
        crate::db::query_as(&sql).fetch_all(db).await?;

    for (id, publisher, manifest) in candidates {
        // The publisher a URL install recorded; one that recorded none was
        // offered by the official publisher.
        let did = publisher.unwrap_or_else(|| OFFICIAL_PUBLISHER_DID.to_owned());
        let cached = state.read().await.project(&did, &id).cloned();
        let entry = match cached {
            Some(entry) => Some(entry),
            None => match super::registry_source::fetch_project(client, source, &did, &id).await {
                Ok(entry) => entry,
                Err(e) => {
                    tracing::warn!(plugin = %id, publisher = %did, error = %e, "official_registry: project unreadable, will retry");
                    continue;
                }
            },
        };

        let Some(entry) = entry else {
            tracing::warn!(plugin = %id, publisher = %did, "official_registry: no registry project matches this plugin; it keeps updating from its url");
            let sql = crate::db::adapt_sql(
                "UPDATE happyview_plugins SET registry_unresolved = 1
                 WHERE id = ? AND registry_did IS NULL",
                backend,
            );
            crate::db::query(&sql).bind(&id).execute(db).await?;
            continue;
        };

        let installed_version = manifest
            .and_then(|m| serde_json::from_str::<serde_json::Value>(&m).ok())
            .and_then(|m| m.get("version").and_then(|v| v.as_str()).map(str::to_owned));
        let version_uri = installed_version.and_then(|v| {
            entry
                .releases
                .iter()
                .find(|r| r.version == v)
                .map(|r| r.version_uri.clone())
        });
        let sql = crate::db::adapt_sql(
            "UPDATE happyview_plugins
             SET registry_did = ?, registry_name = ?, registry_channel = 'latest', version_uri = ?
             WHERE id = ? AND registry_did IS NULL",
            backend,
        );
        crate::db::query(&sql)
            .bind(&did)
            .bind(&id)
            .bind(version_uri)
            .bind(&id)
            .execute(db)
            .await?;
        tracing::info!(plugin = %id, publisher = %did, "official_registry: plugin attached to its registry project");
        // Tracked from now on, so the next listing judges it by its own
        // project even when another publisher holds the name in the catalogue.
        state.write().await.installed.insert((did, id), entry);
    }
    Ok(())
}

/// Refresh just one plugin's cache entry, from the publisher the cache names
/// for this id, or the official publisher when it names none. The entry is
/// removed only when the registry was read and does not have it.
///
/// A check costs a `getProject` and a `listVersions`, and runs only on an
/// operator's request.
pub async fn refresh_plugin(
    client: &reqwest::Client,
    config: &RegistryConfig,
    state: &SharedRegistry,
    plugin_id: &str,
) -> Result<Option<CatalogueEntry>, RegistryError> {
    if config.disabled {
        return Err(RegistryError::Disabled);
    }
    let Some(source) = &config.registry else {
        return Err(RegistryError::Disabled);
    };

    let publisher = state
        .read()
        .await
        .plugins
        .get(plugin_id)
        .and_then(|p| p.publisher_did.clone())
        .unwrap_or_else(|| OFFICIAL_PUBLISHER_DID.to_owned());

    match super::registry_source::fetch_project(client, source, &publisher, plugin_id).await? {
        Some(plugin) => Ok(Some(hold(state, plugin).await)),
        None => {
            state.write().await.plugins.remove(plugin_id);
            Ok(None)
        }
    }
}

/// Read one project fresh, by its publisher and name, for a plugin installed
/// from it. The cache keeps it unless that would displace an official entry
/// with another publisher's; either way the project as read is answered,
/// since it is what the installed plugin follows. When the registry was read
/// and does not have it, a cached entry for that same project is dropped.
pub async fn refresh_project(
    client: &reqwest::Client,
    config: &RegistryConfig,
    state: &SharedRegistry,
    did: &str,
    name: &str,
) -> Result<Option<CatalogueEntry>, RegistryError> {
    let source = match &config.registry {
        Some(source) if !config.disabled => source,
        _ => return Err(RegistryError::Disabled),
    };
    match super::registry_source::fetch_project(client, source, did, name).await? {
        Some(project) => {
            hold(state, project.clone()).await;
            state
                .write()
                .await
                .installed
                .insert((did.to_owned(), name.to_owned()), project.clone());
            Ok(Some(project))
        }
        None => {
            let mut guard = state.write().await;
            guard.installed.remove(&(did.to_owned(), name.to_owned()));
            if guard
                .plugins
                .get(name)
                .is_some_and(|p| p.publisher_did.as_deref() == Some(did))
            {
                guard.plugins.remove(name);
            }
            Ok(None)
        }
    }
}

/// Background task: run `refresh_full` on startup, then every 15 minutes.
/// Spawns nothing, and answers `None`, when the catalogue is off.
pub fn spawn_refresh_task(
    client: reqwest::Client,
    config: RegistryConfig,
    state: SharedRegistry,
    db: sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
) -> Option<tokio::task::JoinHandle<()>> {
    if config.disabled {
        tracing::info!(
            "official_registry: the plugin catalogue is off (PLUGIN_REGISTRY_URL=off or empty)"
        );
        return None;
    }
    Some(tokio::spawn(async move {
        loop {
            match refresh_for_installs(&client, &config, &state, &db, backend).await {
                Ok(()) => tracing::info!("official_registry: cache refreshed"),
                Err(e) => tracing::warn!(error = %e, "official_registry: refresh failed"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(15 * 60)).await;
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const STRANGER: &str = "did:plc:someoneelse";

    fn registry_at(server: &MockServer) -> RegistryConfig {
        RegistryConfig {
            registry: Some(super::super::registry_source::RegistrySource {
                base_url: server.uri(),
                limit: 100,
            }),
            disabled: false,
        }
    }

    fn cached(id: &str, publisher: Option<&str>, official: bool) -> CatalogueEntry {
        CatalogueEntry {
            id: id.into(),
            publisher_did: publisher.map(str::to_owned),
            official,
            name: id.into(),
            description: None,
            icon_url: None,
            plugin_type: "auth".into(),
            channels: Default::default(),
            latest_version: Some("1.0.0".into()),
            lifecycle_state: "active".into(),
            manifest_url: "https://example.com/m".into(),
            wasm_url: "https://example.com/w".into(),
            releases: vec![],
            sections: Default::default(),
            banner_url: None,
        }
    }

    fn cache_of(entries: Vec<CatalogueEntry>) -> SharedRegistry {
        Arc::new(RwLock::new(OfficialRegistryState {
            plugins: entries.into_iter().map(|e| (e.id.clone(), e)).collect(),
            installed: HashMap::new(),
            last_refreshed_at: Some("2026-04-12T00:00:00Z".into()),
        }))
    }

    fn cached_official_lua() -> SharedRegistry {
        cache_of(vec![cached("lua", Some(OFFICIAL_PUBLISHER_DID), true)])
    }

    /// An operator can point the catalogue at their own registry, or turn it
    /// off. Serial, because it moves process environment.
    #[test]
    #[serial]
    fn plugin_registry_url_redirects_or_disables_the_registry() {
        // SAFETY: `#[serial]` keeps this off every other env reader.
        unsafe {
            std::env::remove_var("PLUGIN_REGISTRY_URL");
        }
        let unset = RegistryConfig::production().with_registry_from_env();
        assert_eq!(
            unset.registry.map(|r| r.base_url).as_deref(),
            Some("https://api.happyproto.at"),
            "unset should leave the default registry"
        );
        assert!(!unset.disabled);

        unsafe {
            std::env::set_var("PLUGIN_REGISTRY_URL", "  https://registry.example.com  ");
        }
        let custom = RegistryConfig::production().with_registry_from_env();
        assert_eq!(
            custom.registry.map(|r| r.base_url).as_deref(),
            Some("https://registry.example.com"),
            "a configured url should be used, trimmed"
        );
        assert!(!custom.disabled, "a configured url leaves the catalogue on");

        for off in ["", "   ", "off", " OFF "] {
            unsafe {
                std::env::set_var("PLUGIN_REGISTRY_URL", off);
            }
            let config = RegistryConfig::production().with_registry_from_env();
            assert!(config.disabled, "{off:?} should turn the catalogue off");
            assert!(
                config.registry.is_none(),
                "{off:?} should drop the registry"
            );
        }

        unsafe {
            std::env::remove_var("PLUGIN_REGISTRY_URL");
        }
    }

    /// A wiremock that fails verification on any request.
    async fn unreachable_sources(disabled: bool) -> (MockServer, RegistryConfig) {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let mut config = registry_at(&server);
        config.disabled = disabled;
        (server, config)
    }

    #[tokio::test]
    async fn an_off_catalogue_refreshes_nothing_and_checks_refuse() {
        let (server, off) = unreachable_sources(true).await;
        let state: SharedRegistry = Arc::new(RwLock::new(OfficialRegistryState::default()));
        refresh_full(&reqwest::Client::new(), &off, &state, &[])
            .await
            .unwrap();
        assert!(state.read().await.last_refreshed_at.is_none());
        assert!(matches!(
            refresh_plugin(&reqwest::Client::new(), &off, &state, "lua").await,
            Err(RegistryError::Disabled)
        ));
        server.verify().await;
    }

    fn registry_package(did: &str, name: &str, latest: &str) -> serde_json::Value {
        serde_json::json!({
            "uri": format!("at://{did}/software.release.project/{name}"),
            "did": did,
            "name": name,
            "publication": { "name": format!("{name} by {did}") },
            "pluginType": "interpreter",
            "channels": { "latest": {
                "version": latest,
                "uri": format!("at://{did}/software.release.version/{name}:{latest}"),
                "cid": "bafyversion",
                "state": "active",
                "availability": "available",
            } },
            "lifecycle": { "state": "active" },
        })
    }

    fn registry_release(did: &str, name: &str, version: &str) -> serde_json::Value {
        serde_json::json!({
            "uri": format!("at://{did}/software.release.version/{name}:{version}"),
            "did": did,
            "name": name,
            "version": version,
            "availability": "available",
            "lifecycle": { "state": "active", "installable": true, "events": [] },
        })
    }

    async fn list_versions_answers(server: &MockServer, did: &str, name: &str, version: &str) {
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.listVersions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "versions": [registry_release(did, name, version)]
            })))
            .mount(server)
            .await;
    }

    async fn get_project_answers(server: &MockServer, actor: &str, package: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.getProject"))
            .and(query_param("actor", actor))
            .respond_with(ResponseTemplate::new(200).set_body_json(package))
            .expect(1..)
            .mount(server)
            .await;
    }

    async fn get_project_not_found(server: &MockServer, actor: &str) {
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.getProject"))
            .and(query_param("actor", actor))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "ProjectNotFound", "message": "no such project"
            })))
            .expect(1..)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn refresh_full_fills_the_cache_from_the_registry() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.searchProjects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "projects": [
                    registry_package(STRANGER, "lua", "9.0.0"),
                    registry_package(OFFICIAL_PUBLISHER_DID, "lua", "1.1.0"),
                    registry_package(STRANGER, "extra", "0.1.0"),
                ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.listVersions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "versions": [] })),
            )
            .mount(&server)
            .await;

        let state: SharedRegistry = Arc::new(RwLock::new(OfficialRegistryState::default()));
        refresh_full(&reqwest::Client::new(), &registry_at(&server), &state, &[])
            .await
            .unwrap();
        let guard = state.read().await;
        assert_eq!(guard.plugins.len(), 2);
        assert!(
            guard.plugins["lua"].official,
            "the official lua holds the id"
        );
        assert_eq!(
            guard.plugins["lua"].latest_version.as_deref(),
            Some("1.1.0")
        );
        assert!(!guard.plugins["extra"].official);
        assert!(guard.last_refreshed_at.is_some());
    }

    #[tokio::test]
    async fn refresh_full_keeps_the_previous_cache_when_the_registry_is_unreachable() {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let state = cached_official_lua();
        let result =
            refresh_full(&reqwest::Client::new(), &registry_at(&server), &state, &[]).await;
        assert!(
            matches!(result, Err(RegistryError::Registry(_))),
            "{result:?}"
        );

        let guard = state.read().await;
        assert_eq!(guard.plugins.len(), 1);
        assert!(guard.plugins.contains_key("lua"));
        assert_eq!(
            guard.last_refreshed_at.as_deref(),
            Some("2026-04-12T00:00:00Z")
        );
    }

    /// A listing cut off at the page cap is not the catalogue: replacing the
    /// cache with it would drop every project past the cap, so the previous
    /// cache stays and the caller is told.
    #[tokio::test]
    async fn refresh_full_keeps_the_previous_cache_when_the_listing_is_cut_off() {
        let server = MockServer::start().await;
        let pages = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = pages.clone();
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.searchProjects"))
            .respond_with(move |_: &wiremock::Request| {
                let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "projects": [registry_package(STRANGER, &format!("p{n}"), "1.0.0")],
                    "cursor": format!("page{n}"),
                }))
            })
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.listVersions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "versions": [] })),
            )
            .expect(0)
            .mount(&server)
            .await;

        let state = cached_official_lua();
        let result =
            refresh_full(&reqwest::Client::new(), &registry_at(&server), &state, &[]).await;
        assert!(
            matches!(
                result,
                Err(RegistryError::Registry(
                    super::super::registry_source::SourceError::Truncated { .. }
                ))
            ),
            "{result:?}"
        );

        let guard = state.read().await;
        assert_eq!(guard.plugins.len(), 1);
        assert!(guard.plugins.contains_key("lua"));
        assert_eq!(
            guard.last_refreshed_at.as_deref(),
            Some("2026-04-12T00:00:00Z")
        );
        drop(guard);
        server.verify().await;
    }

    /// With no cached entry, a check asks for the official publisher's
    /// package.
    #[tokio::test]
    async fn a_check_with_no_recorded_publisher_asks_for_the_official_one() {
        let server = MockServer::start().await;
        get_project_answers(
            &server,
            OFFICIAL_PUBLISHER_DID,
            registry_package(OFFICIAL_PUBLISHER_DID, "lua", "1.2.0"),
        )
        .await;
        list_versions_answers(&server, OFFICIAL_PUBLISHER_DID, "lua", "1.2.0").await;

        let state = cache_of(vec![]);
        let answered = refresh_plugin(
            &reqwest::Client::new(),
            &registry_at(&server),
            &state,
            "lua",
        )
        .await
        .unwrap()
        .unwrap();
        assert!(answered.official);
        assert_eq!(
            state.read().await.plugins["lua"].latest_version.as_deref(),
            Some("1.2.0")
        );
        server.verify().await;
    }

    /// A cached entry names the publisher whose package the cache holds under
    /// the id, and the check reads that publisher's project.
    #[tokio::test]
    async fn a_check_reads_the_project_of_the_publisher_the_cache_names() {
        let server = MockServer::start().await;
        get_project_answers(
            &server,
            STRANGER,
            registry_package(STRANGER, "extra", "2.0.0"),
        )
        .await;
        list_versions_answers(&server, STRANGER, "extra", "2.0.0").await;

        let state = cache_of(vec![cached("extra", Some(STRANGER), false)]);
        let answered = refresh_plugin(
            &reqwest::Client::new(),
            &registry_at(&server),
            &state,
            "extra",
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(answered.publisher_did.as_deref(), Some(STRANGER));
        assert_eq!(answered.latest_version.as_deref(), Some("2.0.0"));
        server.verify().await;
    }

    /// A check on an id the cache holds as official asks the official
    /// publisher, and a registry that answers with someone else's package
    /// regardless does not get it cached or returned.
    #[tokio::test]
    async fn a_check_never_replaces_an_official_entry_with_a_strangers() {
        let registry = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.getProject"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(registry_package(STRANGER, "lua", "9.0.0")),
            )
            .mount(&registry)
            .await;
        list_versions_answers(&registry, STRANGER, "lua", "9.0.0").await;

        let state = cached_official_lua();
        let answered = refresh_plugin(
            &reqwest::Client::new(),
            &registry_at(&registry),
            &state,
            "lua",
        )
        .await
        .unwrap()
        .unwrap();
        assert!(answered.official, "the check answered the stranger's entry");
        assert!(state.read().await.plugins["lua"].official);
    }

    /// A registry that has no such project empties the cached entry.
    #[tokio::test]
    async fn a_check_that_finds_nothing_drops_the_cached_entry() {
        let server = MockServer::start().await;
        get_project_not_found(&server, OFFICIAL_PUBLISHER_DID).await;

        let state = cached_official_lua();
        let answered = refresh_plugin(
            &reqwest::Client::new(),
            &registry_at(&server),
            &state,
            "lua",
        )
        .await
        .unwrap();
        assert!(answered.is_none());
        assert!(!state.read().await.plugins.contains_key("lua"));
    }

    /// A check that cannot read the registry leaves the cached entry alone:
    /// the registry may still hold it.
    #[tokio::test]
    async fn a_check_that_cannot_read_the_registry_keeps_the_cached_entry() {
        let registry = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(503))
            .mount(&registry)
            .await;

        let state = cached_official_lua();
        let result = refresh_plugin(
            &reqwest::Client::new(),
            &registry_at(&registry),
            &state,
            "lua",
        )
        .await;
        assert!(
            matches!(result, Err(RegistryError::Registry(_))),
            "{result:?}"
        );
        assert!(state.read().await.plugins.contains_key("lua"));
    }

    #[test]
    fn an_install_url_names_the_publisher_of_the_entry_offering_it() {
        let mut state = OfficialRegistryState::default();
        let mut official = cached("lua", Some(OFFICIAL_PUBLISHER_DID), true);
        official.manifest_url = "https://a.example/lua/manifest.json".into();
        let mut theirs = cached("extra", Some(STRANGER), false);
        theirs.manifest_url = "https://b.example/extra/manifest.json".into();
        state.plugins.insert(official.id.clone(), official);
        state.plugins.insert(theirs.id.clone(), theirs);

        assert_eq!(
            publisher_for_install(&state, "lua", "https://a.example/lua/manifest.json").as_deref(),
            Some(OFFICIAL_PUBLISHER_DID)
        );
        assert_eq!(
            publisher_for_install(&state, "extra", "https://b.example/extra/manifest.json")
                .as_deref(),
            Some(STRANGER)
        );
        assert!(
            publisher_for_install(&state, "lua", "https://typed.example/manifest.json").is_none(),
            "a URL the entry does not offer records no publisher"
        );
    }

    /// Two packages can name the same artifact; installing `lua` from that
    /// URL still records `lua`'s own publisher, however the cache iterates.
    #[test]
    fn an_install_takes_the_publisher_of_its_own_id_when_two_entries_share_a_url() {
        let url = "https://mirror.example/lua-v1.0.0/manifest.json";
        let mut official = cached("lua", Some(OFFICIAL_PUBLISHER_DID), true);
        official.manifest_url = url.into();
        for impostors in [1, 8, 64] {
            let mut state = OfficialRegistryState::default();
            state.plugins.insert("lua".into(), official.clone());
            for n in 0..impostors {
                let id = format!("notlua{n}");
                state.plugins.insert(
                    id.clone(),
                    CatalogueEntry {
                        id,
                        publisher_did: Some(STRANGER.into()),
                        official: false,
                        ..official.clone()
                    },
                );
            }
            assert_eq!(
                publisher_for_install(&state, "lua", url).as_deref(),
                Some(OFFICIAL_PUBLISHER_DID)
            );
            assert_eq!(
                publisher_for_install(&state, "notlua0", url).as_deref(),
                Some(STRANGER)
            );
        }
    }

    /// An instance with no outbound network sets `PLUGIN_REGISTRY_URL=off`,
    /// and boot must then start no refresh loop and ask the registry nothing.
    #[tokio::test]
    async fn an_off_catalogue_spawns_no_refresh_task() {
        let (server, off) = unreachable_sources(true).await;
        let state: SharedRegistry = Arc::new(RwLock::new(OfficialRegistryState::default()));

        let handle = spawn_refresh_task(
            reqwest::Client::new(),
            off,
            state.clone(),
            crate::test_support::memory_pool().await,
            crate::db::DatabaseBackend::Sqlite,
        );
        assert!(handle.is_none(), "an off catalogue should spawn nothing");

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        server.verify().await;
        assert!(state.read().await.last_refreshed_at.is_none());
    }
}

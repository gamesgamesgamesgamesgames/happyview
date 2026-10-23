//! The plugin catalogue, read from the HappyProto registry.
//!
//! The registry is an AppView over `software.release.*` records: a project,
//! its versions, its channels and its lifecycle statuses, each version
//! carrying an `at.happyproto.plugin.v1` artifact that names its module and
//! manifest by blob CID. Bytes are fetched by CID from the registry's own
//! copy (`getArtifact?cid=`), never from a URL a publisher gave, so whatever
//! is fetched can be checked against the CID that named it.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::official_registry::{CatalogueEntry, ChannelEntry, PluginSections, ReleaseEntry};

/// How many versions one `listVersions` page asks for: the registry's own
/// ceiling, so a project's whole history is usually one request.
const VERSIONS_PER_PAGE: u32 = 100;

/// The channel a project's current version is published on.
const LATEST: &str = "latest";

/// Where to look for projects.
#[derive(Debug, Clone)]
pub struct RegistrySource {
    /// An instance serving `at.happyproto.registry.*`, without a trailing
    /// slash, e.g. `https://api.happyproto.at`.
    pub base_url: String,
    /// How many projects one `searchProjects` page asks for. The registry
    /// clamps it to 1–100.
    pub limit: u32,
}

impl RegistrySource {
    pub fn production() -> Self {
        Self {
            base_url: "https://api.happyproto.at".into(),
            limit: 100,
        }
    }

    fn base(&self) -> &str {
        self.base_url.trim_end_matches('/')
    }

    /// Where the registry serves the bytes a CID names.
    pub fn artifact_url(&self, cid: &str) -> String {
        format!(
            "{}/xrpc/at.happyproto.registry.getArtifact?cid={}",
            self.base(),
            urlencoding::encode(cid)
        )
    }

    /// Every project the registry lists, following its cursors to the end.
    pub async fn search_projects(
        &self,
        client: &reqwest::Client,
    ) -> Result<Vec<ProjectView>, SourceError> {
        #[derive(Deserialize)]
        struct Page {
            #[serde(default)]
            projects: Vec<ProjectView>,
            #[serde(default)]
            cursor: Option<String>,
        }
        let mut projects = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut url = format!(
                "{}/xrpc/at.happyproto.registry.searchProjects?limit={}",
                self.base(),
                self.limit
            );
            if let Some(c) = &cursor {
                url.push_str(&format!("&cursor={}", urlencoding::encode(c)));
            }
            let page: Page = get(client, "searchProjects", url).await?;
            projects.extend(page.projects);
            match next_cursor(cursor.as_deref(), page.cursor) {
                Some(next) => cursor = Some(next),
                None => return Ok(projects),
            }
        }
    }

    /// One project, or `None` when the registry knows neither it nor its
    /// publisher.
    pub async fn get_project(
        &self,
        client: &reqwest::Client,
        did: &str,
        name: &str,
    ) -> Result<Option<ProjectView>, SourceError> {
        get_opt(
            client,
            "getProject",
            format!(
                "{}/xrpc/at.happyproto.registry.getProject?actor={}&name={}",
                self.base(),
                urlencoding::encode(did),
                urlencoding::encode(name),
            ),
            &["ProjectNotFound", "ActorNotFound"],
        )
        .await
    }

    /// Every version of a project, newest indexed first, following cursors.
    pub async fn list_versions(
        &self,
        client: &reqwest::Client,
        did: &str,
        name: &str,
    ) -> Result<Vec<VersionView>, SourceError> {
        #[derive(Deserialize)]
        struct Page {
            #[serde(default)]
            versions: Vec<VersionView>,
            #[serde(default)]
            cursor: Option<String>,
        }
        let mut versions = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut url = format!(
                "{}/xrpc/at.happyproto.registry.listVersions?did={}&name={}&limit={VERSIONS_PER_PAGE}",
                self.base(),
                urlencoding::encode(did),
                urlencoding::encode(name),
            );
            if let Some(c) = &cursor {
                url.push_str(&format!("&cursor={}", urlencoding::encode(c)));
            }
            let page: Page = get(client, "listVersions", url).await?;
            versions.extend(page.versions);
            match next_cursor(cursor.as_deref(), page.cursor) {
                Some(next) => cursor = Some(next),
                None => return Ok(versions),
            }
        }
    }

    /// One version by its version string, or `None` when there is no such
    /// version.
    pub async fn get_version(
        &self,
        client: &reqwest::Client,
        did: &str,
        name: &str,
        version: &str,
    ) -> Result<Option<VersionView>, SourceError> {
        get_opt(
            client,
            "getVersion",
            format!(
                "{}/xrpc/at.happyproto.registry.getVersion?did={}&name={}&version={}",
                self.base(),
                urlencoding::encode(did),
                urlencoding::encode(name),
                urlencoding::encode(version),
            ),
            &["VersionNotFound", "ProjectNotFound"],
        )
        .await
    }

    /// The version a channel points at, or `None` when the channel or the
    /// version it names does not exist.
    pub async fn resolve_channel(
        &self,
        client: &reqwest::Client,
        did: &str,
        name: &str,
        channel: &str,
    ) -> Result<Option<VersionView>, SourceError> {
        get_opt(
            client,
            "resolveChannel",
            format!(
                "{}/xrpc/at.happyproto.registry.resolveChannel?did={}&name={}&channel={}",
                self.base(),
                urlencoding::encode(did),
                urlencoding::encode(name),
                urlencoding::encode(channel),
            ),
            &["ChannelNotFound", "VersionNotFound", "ProjectNotFound"],
        )
        .await
    }
}

/// The cursor to ask for next, or `None` to stop. A registry that answers
/// the cursor it was given would otherwise be asked for the same page
/// forever.
fn next_cursor(asked: Option<&str>, answered: Option<String>) -> Option<String> {
    answered.filter(|c| !c.is_empty() && Some(c.as_str()) != asked)
}

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("registry request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("registry returned status {status} for {method}")]
    Status { method: &'static str, status: u16 },
    #[error("registry answered {method} with {name}: {message}")]
    Xrpc {
        method: &'static str,
        name: String,
        message: String,
    },
}

// ---------------------------------------------------------------------------
// The wire shapes. Only the fields a consumer reads; every view carries more.
// Fields default where the registry may omit them, so one sparse record does
// not fail a whole listing.
// ---------------------------------------------------------------------------

/// A `software.release.project`, as the registry describes it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectView {
    pub uri: String,
    pub did: String,
    #[serde(default)]
    pub handle: Option<String>,
    pub name: String,
    #[serde(default)]
    pub publication: Publication,
    #[serde(default)]
    pub plugin_type: String,
    #[serde(default)]
    pub channels: BTreeMap<String, ChannelView>,
    #[serde(default)]
    pub banner: Option<ImageView>,
    #[serde(default)]
    pub lifecycle: ProjectLifecycle,
    #[serde(default)]
    pub indexed_at: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Publication {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// A CID, served by `getArtifact`.
    #[serde(default)]
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelView {
    pub version: String,
    #[serde(default)]
    pub uri: String,
    #[serde(default)]
    pub cid: String,
    #[serde(default = "active")]
    pub state: String,
    #[serde(default)]
    pub availability: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImageView {
    pub cid: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectLifecycle {
    #[serde(default = "active")]
    pub state: String,
}

impl Default for ProjectLifecycle {
    fn default() -> Self {
        Self { state: active() }
    }
}

fn active() -> String {
    "active".into()
}

/// One published version of a project.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionView {
    pub uri: String,
    #[serde(default)]
    pub cid: String,
    pub did: String,
    pub name: String,
    pub version: String,
    /// The `software.release.manifest` record, not the plugin's own manifest
    /// file: that one is `artifact.manifest`.
    #[serde(default)]
    pub manifest: StrongRef,
    #[serde(default)]
    pub artifact: PluginArtifact,
    #[serde(default)]
    pub changelog: Option<Changelog>,
    #[serde(default)]
    pub availability: String,
    #[serde(default)]
    pub suspect: bool,
    #[serde(default)]
    pub lifecycle: VersionLifecycle,
    #[serde(default)]
    pub indexed_at: Option<String>,
}

impl VersionView {
    /// Whether this version may be installed: its lifecycle allows it and the
    /// registry holds its bytes. One the registry has not copied yet, or has
    /// lost at the source, has nothing to fetch.
    pub fn installable(&self) -> bool {
        self.lifecycle.installable && self.availability == "available"
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct StrongRef {
    #[serde(default)]
    pub uri: String,
    #[serde(default)]
    pub cid: String,
}

/// An `at.happyproto.plugin.v1`, as published.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginArtifact {
    #[serde(default)]
    pub package: BlobRef,
    #[serde(default)]
    pub manifest: BlobRef,
    #[serde(default)]
    pub plugin_type: Option<String>,
    #[serde(default)]
    pub sections: PluginSections,
}

/// A blob reference; only its CID is read.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BlobRef {
    #[serde(default, rename = "ref")]
    pub link: CidLink,
}

impl BlobRef {
    pub fn cid(&self) -> &str {
        &self.link.link
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CidLink {
    #[serde(default, rename = "$link")]
    pub link: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Changelog {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub markdown: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionLifecycle {
    #[serde(default = "active")]
    pub state: String,
    #[serde(default = "yes")]
    pub installable: bool,
    #[serde(default)]
    pub superseded_by: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

impl Default for VersionLifecycle {
    fn default() -> Self {
        Self {
            state: active(),
            installable: true,
            superseded_by: None,
            reason: None,
        }
    }
}

fn yes() -> bool {
    true
}

/// Whether a lifecycle state still allows an install. A channel carries only
/// its state, not the version's own `installable`, so this is the table both
/// are derived from.
fn state_installable(state: &str) -> bool {
    matches!(state, "active" | "deprecated")
}

// ---------------------------------------------------------------------------
// Requests.
// ---------------------------------------------------------------------------

/// The registry's Lua queries answer `{ error, message }` with 200, so a body
/// carrying a string `error` is an error whatever the status.
async fn get_value(
    client: &reqwest::Client,
    method: &'static str,
    url: String,
) -> Result<Result<serde_json::Value, (String, String)>, SourceError> {
    let response = client.get(&url).send().await?;
    let status = response.status();
    let body: Option<serde_json::Value> = response.json().await.ok();
    if let Some(name) = body
        .as_ref()
        .and_then(|b| b.get("error"))
        .and_then(|e| e.as_str())
    {
        let message = body
            .as_ref()
            .and_then(|b| b.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or_default();
        return Ok(Err((name.to_owned(), message.to_owned())));
    }
    match body {
        Some(value) if status.is_success() => Ok(Ok(value)),
        _ => Err(SourceError::Status {
            method,
            status: status.as_u16(),
        }),
    }
}

fn decode<T: for<'de> Deserialize<'de>>(
    method: &'static str,
    value: serde_json::Value,
) -> Result<T, SourceError> {
    serde_json::from_value(value).map_err(|e| SourceError::Xrpc {
        method,
        name: "Unreadable".into(),
        message: e.to_string(),
    })
}

async fn get<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    method: &'static str,
    url: String,
) -> Result<T, SourceError> {
    match get_value(client, method, url).await? {
        Ok(value) => decode(method, value),
        Err((name, message)) => Err(SourceError::Xrpc {
            method,
            name,
            message,
        }),
    }
}

/// `get`, with the errors named in `not_found` answered as `None`.
async fn get_opt<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    method: &'static str,
    url: String,
    not_found: &[&str],
) -> Result<Option<T>, SourceError> {
    match get_value(client, method, url).await? {
        Ok(value) => decode(method, value).map(Some),
        Err((name, _)) if not_found.contains(&name.as_str()) => Ok(None),
        Err((name, message)) => Err(SourceError::Xrpc {
            method,
            name,
            message,
        }),
    }
}

// ---------------------------------------------------------------------------
// Catalogue entries.
// ---------------------------------------------------------------------------

/// Read the catalogue: one entry per project, keyed by project name. A
/// project whose versions cannot be read is left out with a line saying so.
pub async fn fetch_catalogue(
    client: &reqwest::Client,
    source: &RegistrySource,
) -> Result<Vec<CatalogueEntry>, SourceError> {
    let projects = source.search_projects(client).await?;

    // Keyed by name, as the cache is. Anyone can publish any name, so the
    // official publisher's project wins a collision; otherwise the first
    // listed stays.
    let mut entries: Vec<CatalogueEntry> = Vec::new();
    for project in projects {
        let Some(entry) = entry_for(client, source, project).await else {
            continue;
        };
        match entries.iter_mut().find(|e| e.id == entry.id) {
            Some(existing) if entry.official && !existing.official => *existing = entry,
            Some(_) => tracing::warn!(
                plugin = %entry.id,
                publisher = ?entry.publisher_did,
                "registry_source: another publisher already holds this name; not listed"
            ),
            None => entries.push(entry),
        }
    }
    Ok(entries)
}

/// One project, read fresh, for an on-demand check. `None` when the registry
/// does not have it.
pub async fn fetch_project(
    client: &reqwest::Client,
    source: &RegistrySource,
    did: &str,
    name: &str,
) -> Result<Option<CatalogueEntry>, SourceError> {
    let Some(project) = source.get_project(client, did, name).await? else {
        return Ok(None);
    };
    let versions = source
        .list_versions(client, &project.did, &project.name)
        .await?;
    let latest = latest_version_view(client, source, &project, &versions).await;
    Ok(Some(build_entry(source, project, versions, latest)))
}

async fn entry_for(
    client: &reqwest::Client,
    source: &RegistrySource,
    project: ProjectView,
) -> Option<CatalogueEntry> {
    let versions = match source
        .list_versions(client, &project.did, &project.name)
        .await
    {
        Ok(versions) => versions,
        Err(e) => {
            tracing::warn!(
                plugin = %project.name,
                publisher = %project.did,
                error = %e,
                "registry_source: versions unreadable"
            );
            return None;
        }
    };
    let latest = latest_version_view(client, source, &project, &versions).await;
    Some(build_entry(source, project, versions, latest))
}

/// The version the `latest` channel points at. Usually already in the
/// listing; one indexed too long ago to be on its pages is asked for by
/// channel instead.
async fn latest_version_view(
    client: &reqwest::Client,
    source: &RegistrySource,
    project: &ProjectView,
    versions: &[VersionView],
) -> Option<VersionView> {
    let channel = project.channels.get(LATEST)?;
    if let Some(found) = versions
        .iter()
        .find(|v| v.uri == channel.uri || v.version == channel.version)
    {
        return Some(found.clone());
    }
    match source
        .resolve_channel(client, &project.did, &project.name, LATEST)
        .await
    {
        Ok(found) => found,
        Err(e) => {
            tracing::warn!(
                plugin = %project.name,
                publisher = %project.did,
                error = %e,
                "registry_source: latest channel unreadable"
            );
            None
        }
    }
}

fn build_entry(
    source: &RegistrySource,
    project: ProjectView,
    versions: Vec<VersionView>,
    latest: Option<VersionView>,
) -> CatalogueEntry {
    let channels = project
        .channels
        .iter()
        .map(|(name, c)| {
            (
                name.clone(),
                ChannelEntry {
                    version: c.version.clone(),
                    state: c.state.clone(),
                    installable: state_installable(&c.state) && c.availability == "available",
                    availability: c.availability.clone(),
                },
            )
        })
        .collect();

    let sections = latest
        .as_ref()
        .map(|v| PluginSections {
            changelog: v.changelog.as_ref().and_then(|c| c.markdown.clone()),
            ..v.artifact.sections.clone()
        })
        .unwrap_or_default();

    let releases = versions
        .iter()
        .map(|v| ReleaseEntry {
            version: v.version.clone(),
            name: format!("{}-v{}", project.name, v.version),
            published_at: v.indexed_at.clone(),
            body: v.changelog.as_ref().and_then(|c| c.markdown.clone()),
            state: v.lifecycle.state.clone(),
            installable: v.installable(),
            availability: v.availability.clone(),
            version_uri: v.uri.clone(),
            manifest_cid: v.manifest.cid.clone(),
            package_cid: v.artifact.package.cid().to_owned(),
            manifest_blob_cid: v.artifact.manifest.cid().to_owned(),
        })
        .collect();

    let cid_url = |cid: &str| (!cid.is_empty()).then(|| source.artifact_url(cid));

    CatalogueEntry {
        official: project.did == super::official_registry::OFFICIAL_PUBLISHER_DID,
        publisher_did: Some(project.did.clone()),
        name: project
            .publication
            .name
            .clone()
            .unwrap_or_else(|| project.name.clone()),
        description: project.publication.description.clone(),
        icon_url: project.publication.icon.as_deref().and_then(cid_url),
        banner_url: project.banner.as_ref().and_then(|b| cid_url(&b.cid)),
        plugin_type: project.plugin_type.clone(),
        latest_version: project.channels.get(LATEST).map(|c| c.version.clone()),
        channels,
        releases,
        sections,
        lifecycle_state: project.lifecycle.state.clone(),
        manifest_url: latest
            .as_ref()
            .and_then(|v| cid_url(v.artifact.manifest.cid()))
            .unwrap_or_default(),
        wasm_url: latest
            .as_ref()
            .and_then(|v| cid_url(v.artifact.package.cid()))
            .unwrap_or_default(),
        id: project.name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use wiremock::matchers::{method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const HP: &str = super::super::official_registry::OFFICIAL_PUBLISHER_DID;
    const STRANGER: &str = "did:plc:someoneelse";

    fn source(server: &MockServer) -> RegistrySource {
        RegistrySource {
            base_url: server.uri(),
            limit: 100,
        }
    }

    fn version_uri(did: &str, name: &str, version: &str) -> String {
        format!("at://{did}/software.release.version/{name}:{version}")
    }

    fn channel(
        did: &str,
        name: &str,
        version: &str,
        state: &str,
        availability: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "version": version,
            "uri": version_uri(did, name, version),
            "cid": format!("bafyversion{version}"),
            "state": state,
            "availability": availability,
        })
    }

    fn project(did: &str, name: &str, channels: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "uri": format!("at://{did}/software.release.project/{name}"),
            "cid": "bafyproject",
            "did": did,
            "name": name,
            "sidecar": { "uri": format!("at://{did}/at.happyproto.registry.project/{name}") },
            "publication": {
                "uri": format!("at://{did}/site.standard.publication/{name}"),
                "cid": "bafypublication",
                "name": format!("{name} plugin"),
                "description": format!("The {name} plugin"),
                "url": "https://example.com",
                "icon": format!("bafkreiicon{name}"),
            },
            "pluginType": "interpreter",
            "keywords": [],
            "authors": [],
            "security": [],
            "channels": channels,
            "banner": { "cid": format!("bafkreibanner{name}"), "width": 1200, "height": 400 },
            "lifecycle": { "state": "active" },
            "indexedAt": "2026-10-01T00:00:00Z",
        })
    }

    fn version(
        did: &str,
        name: &str,
        version: &str,
        state: &str,
        installable: bool,
        availability: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "uri": version_uri(did, name, version),
            "cid": format!("bafyversion{version}"),
            "did": did,
            "name": name,
            "version": version,
            "manifest": {
                "uri": format!("at://{did}/software.release.manifest/{name}:{version}"),
                "cid": format!("bafymanifestrecord{version}"),
            },
            "artifact": {
                "$type": "at.happyproto.plugin.v1",
                "package": { "$type": "blob", "ref": { "$link": format!("bafkreipackage{version}") }, "mimeType": "application/wasm", "size": 1 },
                "manifest": { "$type": "blob", "ref": { "$link": format!("bafkreimanifest{version}") }, "mimeType": "application/json", "size": 1 },
                "pluginType": "interpreter",
                "apiVersion": "2",
                "capabilities": [],
                "license": "MIT",
                "sections": { "description": format!("# {name} {version}"), "security": "Report privately." },
            },
            "changelog": {
                "uri": format!("at://{did}/site.standard.document/{name}-{version}"),
                "title": version,
                "markdown": format!("- {version} notes"),
                "fetchedAt": "2026-10-02T00:00:00Z",
            },
            "availability": availability,
            "suspect": false,
            "lifecycle": { "state": state, "installable": installable, "events": [] },
            "indexedAt": format!("2026-10-01T00:00:00Z"),
        })
    }

    async fn mount_projects(server: &MockServer, projects: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.searchProjects"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "projects": projects })),
            )
            .mount(server)
            .await;
    }

    async fn mount_versions(
        server: &MockServer,
        did: &str,
        name: &str,
        versions: serde_json::Value,
    ) {
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.listVersions"))
            .and(query_param("did", did))
            .and(query_param("name", name))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "versions": versions })),
            )
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn the_catalogue_is_built_from_projects_and_their_versions() {
        let server = MockServer::start().await;
        mount_projects(
            &server,
            serde_json::json!([
                project(HP, "lua", serde_json::json!({ "latest": channel(HP, "lua", "1.2.0", "active", "available") })),
                project(STRANGER, "extra", serde_json::json!({ "latest": channel(STRANGER, "extra", "9.0.0", "active", "available") })),
            ]),
        )
        .await;
        mount_versions(
            &server,
            HP,
            "lua",
            serde_json::json!([
                version(HP, "lua", "1.2.0", "active", true, "available"),
                version(HP, "lua", "1.1.0", "active", true, "available"),
            ]),
        )
        .await;
        mount_versions(
            &server,
            STRANGER,
            "extra",
            serde_json::json!([version(
                STRANGER,
                "extra",
                "9.0.0",
                "active",
                true,
                "available"
            )]),
        )
        .await;

        let src = source(&server);
        let mut entries = fetch_catalogue(&reqwest::Client::new(), &src)
            .await
            .unwrap();
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(
            entries
                .iter()
                .map(|e| (e.id.as_str(), e.official, e.publisher_did.as_deref()))
                .collect::<Vec<_>>(),
            [("extra", false, Some(STRANGER)), ("lua", true, Some(HP))]
        );

        let lua = &entries[1];
        assert_eq!(lua.name, "lua plugin");
        assert_eq!(lua.description.as_deref(), Some("The lua plugin"));
        assert_eq!(lua.icon_url, Some(src.artifact_url("bafkreiiconlua")));
        assert_eq!(lua.banner_url, Some(src.artifact_url("bafkreibannerlua")));
        assert_eq!(lua.plugin_type, "interpreter");
        assert_eq!(lua.latest_version.as_deref(), Some("1.2.0"));
        assert_eq!(lua.lifecycle_state, "active");
        assert_eq!(
            lua.releases
                .iter()
                .map(|r| r.version.as_str())
                .collect::<Vec<_>>(),
            ["1.2.0", "1.1.0"],
            "newest first, as listed"
        );
        assert_eq!(
            lua.sections.description.as_deref(),
            Some("# lua 1.2.0"),
            "sections come from the latest channel's artifact"
        );
        assert_eq!(lua.sections.security.as_deref(), Some("Report privately."));
        assert_eq!(lua.sections.changelog.as_deref(), Some("- 1.2.0 notes"));
    }

    #[tokio::test]
    async fn channels_carry_their_version_state_and_whether_they_install() {
        let server = MockServer::start().await;
        mount_projects(
            &server,
            serde_json::json!([project(
                HP,
                "lua",
                serde_json::json!({
                    "latest": channel(HP, "lua", "1.2.0", "deprecated", "available"),
                    "beta": channel(HP, "lua", "1.3.0-beta.1", "active", "pending"),
                    "old": channel(HP, "lua", "0.9.0", "retired", "available"),
                })
            )]),
        )
        .await;
        mount_versions(
            &server,
            HP,
            "lua",
            serde_json::json!([version(HP, "lua", "1.2.0", "deprecated", true, "available")]),
        )
        .await;

        let entries = fetch_catalogue(&reqwest::Client::new(), &source(&server))
            .await
            .unwrap();
        let channels = &entries[0].channels;
        assert_eq!(
            channels.keys().map(String::as_str).collect::<Vec<_>>(),
            ["beta", "latest", "old"]
        );
        let latest = &channels["latest"];
        assert_eq!(latest.version, "1.2.0");
        assert_eq!(latest.state, "deprecated");
        assert_eq!(latest.availability, "available");
        assert!(latest.installable, "a deprecated version still installs");
        assert!(
            !channels["beta"].installable,
            "a version the registry has not copied yet has nothing to fetch"
        );
        assert!(
            !channels["old"].installable,
            "a retired version does not install"
        );
    }

    #[tokio::test]
    async fn an_official_project_wins_a_name_collision_and_otherwise_the_first_stays() {
        let server = MockServer::start().await;
        let third = "did:plc:third";
        // The stranger is listed first, so a first-wins rule alone would keep it.
        mount_projects(
            &server,
            serde_json::json!([
                project(STRANGER, "lua", serde_json::json!({})),
                project(HP, "lua", serde_json::json!({})),
                project(STRANGER, "extra", serde_json::json!({})),
                project(third, "extra", serde_json::json!({})),
            ]),
        )
        .await;
        for (did, name) in [
            (STRANGER, "lua"),
            (HP, "lua"),
            (STRANGER, "extra"),
            (third, "extra"),
        ] {
            mount_versions(&server, did, name, serde_json::json!([])).await;
        }

        let mut entries = fetch_catalogue(&reqwest::Client::new(), &source(&server))
            .await
            .unwrap();
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(
            entries
                .iter()
                .map(|e| (e.id.as_str(), e.publisher_did.as_deref()))
                .collect::<Vec<_>>(),
            [("extra", Some(STRANGER)), ("lua", Some(HP))]
        );
        assert!(entries[1].official);
    }

    #[tokio::test]
    async fn search_follows_cursors_to_the_last_page() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.searchProjects"))
            .and(query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "projects": [project(HP, "lua", serde_json::json!({}))],
                "cursor": "page2",
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.searchProjects"))
            .and(query_param("cursor", "page2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "projects": [project(HP, "extra", serde_json::json!({}))],
            })))
            .mount(&server)
            .await;

        let projects = source(&server)
            .search_projects(&reqwest::Client::new())
            .await
            .unwrap();
        assert_eq!(
            projects.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["lua", "extra"]
        );
    }

    #[tokio::test]
    async fn a_200_carrying_an_error_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.searchProjects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": "NotConfigured", "message": "no identity"
            })))
            .mount(&server)
            .await;
        let err = fetch_catalogue(&reqwest::Client::new(), &source(&server))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("NotConfigured"), "{err}");
    }

    #[tokio::test]
    async fn a_missing_project_version_or_channel_is_none() {
        let server = MockServer::start().await;
        for (xrpc, error) in [
            ("getProject", "ProjectNotFound"),
            ("getVersion", "VersionNotFound"),
            ("resolveChannel", "ChannelNotFound"),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("/xrpc/at.happyproto.registry.{xrpc}")))
                .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                    "error": error, "message": "not here"
                })))
                .mount(&server)
                .await;
        }
        let client = reqwest::Client::new();
        let src = source(&server);
        assert!(
            src.get_project(&client, HP, "gone")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            fetch_project(&client, &src, HP, "gone")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            src.get_version(&client, HP, "lua", "9.9.9")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            src.resolve_channel(&client, HP, "lua", "nightly")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn fetch_project_reads_one_project_and_its_latest_version() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.getProject"))
            .and(query_param("actor", HP))
            .and(query_param("name", "lua"))
            .respond_with(ResponseTemplate::new(200).set_body_json(project(
                HP,
                "lua",
                serde_json::json!({ "latest": channel(HP, "lua", "1.0.0", "active", "available") }),
            )))
            .mount(&server)
            .await;
        // The listing's first page does not reach the latest version, so it
        // is asked for by channel.
        mount_versions(
            &server,
            HP,
            "lua",
            serde_json::json!([version(
                HP,
                "lua",
                "2.0.0-beta.1",
                "active",
                true,
                "available"
            )]),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.resolveChannel"))
            .and(query_param("channel", "latest"))
            .respond_with(ResponseTemplate::new(200).set_body_json(version(
                HP,
                "lua",
                "1.0.0",
                "active",
                true,
                "available",
            )))
            .expect(1)
            .mount(&server)
            .await;

        let lua = fetch_project(&reqwest::Client::new(), &source(&server), HP, "lua")
            .await
            .unwrap()
            .unwrap();
        assert!(lua.official);
        assert_eq!(lua.latest_version.as_deref(), Some("1.0.0"));
        assert_eq!(lua.sections.description.as_deref(), Some("# lua 1.0.0"));
        server.verify().await;
    }

    #[tokio::test]
    async fn release_entries_carry_lifecycle_availability_and_cids() {
        let server = MockServer::start().await;
        mount_projects(
            &server,
            serde_json::json!([project(
                HP,
                "lua",
                serde_json::json!({ "latest": channel(HP, "lua", "1.2.0", "active", "available") })
            )]),
        )
        .await;
        mount_versions(
            &server,
            HP,
            "lua",
            serde_json::json!([
                version(HP, "lua", "1.3.0", "active", true, "pending"),
                version(HP, "lua", "1.2.0", "active", true, "available"),
                version(HP, "lua", "1.1.0", "deprecated", true, "available"),
                version(HP, "lua", "1.0.0", "retired", false, "available"),
                version(HP, "lua", "0.9.0", "active", true, "deletedAtSource"),
            ]),
        )
        .await;

        let entries = fetch_catalogue(&reqwest::Client::new(), &source(&server))
            .await
            .unwrap();
        let releases = &entries[0].releases;
        assert_eq!(
            releases
                .iter()
                .map(|r| (
                    r.version.as_str(),
                    r.state.as_str(),
                    r.availability.as_str(),
                    r.installable
                ))
                .collect::<Vec<_>>(),
            [
                ("1.3.0", "active", "pending", false),
                ("1.2.0", "active", "available", true),
                ("1.1.0", "deprecated", "available", true),
                ("1.0.0", "retired", "available", false),
                ("0.9.0", "active", "deletedAtSource", false),
            ],
            "every version is listed; only one the registry holds and the lifecycle allows installs"
        );
        let r = &releases[1];
        assert_eq!(r.version_uri, version_uri(HP, "lua", "1.2.0"));
        assert_eq!(r.manifest_cid, "bafymanifestrecord1.2.0");
        assert_eq!(r.package_cid, "bafkreipackage1.2.0");
        assert_eq!(r.manifest_blob_cid, "bafkreimanifest1.2.0");
        assert_eq!(r.published_at.as_deref(), Some("2026-10-01T00:00:00Z"));
        assert_eq!(r.body.as_deref(), Some("- 1.2.0 notes"));
    }

    #[test]
    fn an_artifact_url_names_the_cid_on_the_registry() {
        let src = RegistrySource {
            base_url: "https://reg.example/".into(),
            limit: 100,
        };
        assert_eq!(
            src.artifact_url("bafkreiabc"),
            "https://reg.example/xrpc/at.happyproto.registry.getArtifact?cid=bafkreiabc"
        );
    }

    #[test]
    fn a_repeated_or_empty_cursor_ends_paging() {
        assert_eq!(next_cursor(None, Some("a".into())).as_deref(), Some("a"));
        assert_eq!(next_cursor(Some("a"), Some("a".into())), None);
        assert_eq!(next_cursor(Some("a"), Some(String::new())), None);
        assert_eq!(next_cursor(Some("a"), None), None);
    }
}

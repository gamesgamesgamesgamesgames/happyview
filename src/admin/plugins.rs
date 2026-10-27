use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};
use crate::plugin::capabilities;
use crate::plugin::encryption::{decrypt, encrypt};
use crate::plugin::graph::GraphError;
use crate::plugin::loader;
use crate::plugin::official_registry::{
    CatalogueEntry, OFFICIAL_PUBLISHER_DID, OfficialRegistryState, ReleaseEntry,
};
use crate::plugin::registry_source::{LATEST, RegistrySource, TRACKS, VersionView};
use crate::plugin::{LoadedPlugin, PluginSource, PluginType, RegistryInstall};

/// If the reload request provides a new URL, use it and clear the old sha256
/// (the new version has its own hash). Otherwise keep the stored values.
fn resolve_reload_url(
    current: (String, Option<String>),
    new_url: Option<String>,
) -> (String, Option<String>) {
    match new_url {
        Some(url) => (url, None),
        None => current,
    }
}

/// Whose updates an installed plugin may be offered (see `official_registry`).
enum InstalledPublisher {
    /// The publisher of the catalogue entry or registry project it was
    /// installed from.
    Recorded(String),
    /// Installed from a URL the catalogue did not offer, or before the
    /// publisher was recorded. Nothing ties it to a publisher, so only the
    /// official one may update it.
    Unrecorded,
}

impl InstalledPublisher {
    fn of(recorded: Option<String>) -> Self {
        match recorded {
            Some(did) => Self::Recorded(did),
            None => Self::Unrecorded,
        }
    }

    /// Whether a release by `publisher` may replace this install.
    fn accepts(&self, publisher: Option<&str>) -> bool {
        match self {
            Self::Recorded(did) => publisher == Some(did.as_str()),
            Self::Unrecorded => publisher == Some(OFFICIAL_PUBLISHER_DID),
        }
    }

    fn may_be_updated_by(&self, entry: &CatalogueEntry) -> bool {
        self.accepts(crate::plugin::official_registry::effective_publisher(entry))
    }
}

/// What a plugin's row says about where it came from.
#[derive(Debug, Default, Clone)]
struct InstalledRow {
    publisher_did: Option<String>,
    registry_did: Option<String>,
    registry_name: Option<String>,
    registry_channel: Option<String>,
    version_uri: Option<String>,
    unresolved: bool,
    /// The module was installed by its CID from the registry. A plugin
    /// attached to its project after a URL install has none until its first
    /// registry install.
    installed_by_cid: bool,
}

impl InstalledRow {
    /// The registry project this plugin belongs to, when it has one.
    fn project(&self) -> Option<(&str, &str)> {
        Some((
            self.registry_did.as_deref()?,
            self.registry_name.as_deref()?,
        ))
    }

    /// How updates are judged for this plugin.
    fn follows(&self) -> Follows<'_> {
        let channel = self.registry_channel.as_deref();
        match self.project() {
            Some(_) if self.installed_by_cid => Follows::Registry(channel),
            Some(_) => Follows::Attached(channel),
            None => Follows::Nothing,
        }
    }
}

/// Whether `entry` describes the project this plugin was installed from: for
/// a registry install, only that publisher's project of that name; for any
/// other plugin, whatever the catalogue holds under its id.
fn speaks_for(row: &InstalledRow, entry: &CatalogueEntry) -> bool {
    match row.project() {
        Some((did, name)) => entry.publisher_did.as_deref() == Some(did) && entry.id == name,
        None => true,
    }
}

/// What an installed plugin's updates are judged against.
#[derive(Debug, Clone, Copy)]
enum Follows<'a> {
    /// A registry install, following a channel, or pinned (`None`). The
    /// channel is the publisher's say: whatever version it names other than
    /// the installed one is the update, even an older one.
    Registry(Option<&'a str>),
    /// A URL install attached to its registry project, following a channel,
    /// until its first install by CID. Its version was chosen by hand, not
    /// by the channel, so only a newer version is offered: a prerelease
    /// installed ahead of the channel isn't "updated" back to an older one.
    Attached(Option<&'a str>),
    /// A URL install with no registry project. Nothing names its updates,
    /// so none are offered; adding it from the registry makes it follow a
    /// channel.
    Nothing,
}

type InstalledRowTuple = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
    Option<String>,
);

const INSTALLED_ROW_SELECT: &str = "SELECT id, publisher_did, registry_did, registry_name,
        registry_channel, version_uri, CAST(registry_unresolved AS BIGINT), package_cid
     FROM happyview_plugins";

fn installed_row_from(row: InstalledRowTuple) -> (String, InstalledRow) {
    (
        row.0,
        InstalledRow {
            publisher_did: row.1,
            registry_did: row.2,
            registry_name: row.3,
            registry_channel: row.4,
            version_uri: row.5,
            unresolved: row.6 != 0,
            installed_by_cid: row.7.is_some_and(|c| !c.is_empty()),
        },
    )
}

/// Every plugin row, by id. A plugin registered without a row (file plugins
/// on an instance that doesn't persist them) is absent.
async fn installed_rows(state: &AppState) -> Result<HashMap<String, InstalledRow>, AppError> {
    let sql = adapt_sql(INSTALLED_ROW_SELECT, state.db_backend);
    let rows: Vec<InstalledRowTuple> = crate::db::query_as(&sql)
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to read plugin rows: {e}")))?;
    Ok(rows.into_iter().map(installed_row_from).collect())
}

async fn installed_row(state: &AppState, plugin_id: &str) -> Result<InstalledRow, AppError> {
    let sql = adapt_sql(
        &format!("{INSTALLED_ROW_SELECT} WHERE id = ?"),
        state.db_backend,
    );
    let row: Option<InstalledRowTuple> = crate::db::query_as(&sql)
        .bind(plugin_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to read plugin row: {e}")))?;
    Ok(row.map(|r| installed_row_from(r).1).unwrap_or_default())
}

async fn set_publisher(
    state: &AppState,
    plugin_id: &str,
    publisher: Option<&str>,
) -> Result<(), AppError> {
    let sql = adapt_sql(
        "UPDATE happyview_plugins SET publisher_did = ? WHERE id = ?",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(publisher)
        .bind(plugin_id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to record plugin publisher: {e}")))?;
    Ok(())
}

/// A plugin installed from a plain URL is no registry project's, whatever
/// it was before: keeping the old project would offer it that project's
/// updates and reload it through the registry.
async fn forget_registry_project(state: &AppState, plugin_id: &str) -> Result<(), AppError> {
    let sql = adapt_sql(
        "UPDATE happyview_plugins SET registry_did = NULL, registry_name = NULL,
             registry_channel = NULL, version_uri = NULL, registry_unresolved = 0
         WHERE id = ?",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(plugin_id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to clear the plugin's project: {e}")))?;
    Ok(())
}

#[derive(Debug, Default)]
struct UpdateInfo {
    update_available: bool,
    latest_version: Option<String>,
    latest_version_installable: Option<bool>,
    latest_state: Option<String>,
    pending_releases: Vec<ReleaseEntry>,
}

/// Whether `version` was published on `channel`'s track: `latest` carries
/// releases, and a prerelease channel carries its own prereleases, named
/// after it (`1.2.0-beta.1` on `beta`).
fn on_track(channel: &str, version: &semver::Version) -> bool {
    if channel == LATEST {
        version.pre.is_empty()
    } else {
        version.pre.as_str().split('.').next() == Some(channel)
    }
}

/// What the catalogue offers an installed plugin. A plugin with no registry
/// project is offered nothing, and a pinned one is shown where `latest`
/// stands but offered nothing. Otherwise an update is offered when the
/// followed channel points at a version that may be installed and that, as
/// `follows` says, differs from or is newer than the installed one.
fn compute_update_info(
    installed_version: &str,
    installed: &InstalledPublisher,
    follows: Follows<'_>,
    cache_entry: Option<&CatalogueEntry>,
) -> UpdateInfo {
    let followed = match follows {
        Follows::Registry(channel) | Follows::Attached(channel) => channel,
        Follows::Nothing => return UpdateInfo::default(),
    };
    let Some(entry) = cache_entry.filter(|e| installed.may_be_updated_by(e)) else {
        return UpdateInfo::default();
    };
    let channel = followed.unwrap_or(LATEST);
    let Some(head) = entry.channels.get(channel) else {
        return UpdateInfo::default();
    };
    // The channel says whether its version may be installed, suspect
    // included; the release, when the catalogue lists it, carries its state.
    let release = entry.releases.iter().find(|r| r.version == head.version);
    let installable = head.installable && release.is_none_or(|r| r.installable);
    let state = release.map_or_else(|| head.state.clone(), |r| r.state.clone());

    let pending_releases = match (followed, semver::Version::parse(installed_version)) {
        (Some(channel), Ok(installed)) => {
            let head_version = semver::Version::parse(&head.version).ok();
            entry
                .releases
                .iter()
                .filter(|r| r.installable)
                .filter(|r| {
                    semver::Version::parse(&r.version).is_ok_and(|v| {
                        v > installed
                            && on_track(channel, &v)
                            && head_version.as_ref().is_none_or(|head| &v <= head)
                    })
                })
                .cloned()
                .collect()
        }
        _ => Vec::new(),
    };

    let moved = match follows {
        Follows::Registry(_) => head.version != installed_version,
        Follows::Nothing => false,
        Follows::Attached(_) => match (
            semver::Version::parse(&head.version),
            semver::Version::parse(installed_version),
        ) {
            (Ok(head), Ok(installed)) => head > installed,
            _ => false,
        },
    };

    UpdateInfo {
        update_available: followed.is_some() && installable && moved,
        latest_version: Some(head.version.clone()),
        latest_version_installable: Some(installable),
        latest_state: Some(state),
        pending_releases,
    }
}

/// Where an installed plugin stands in the registry, from its row and the
/// catalogue entry for its project. `None` for a plugin with no project
/// that was never flagged unresolved.
fn registry_status(
    row: &InstalledRow,
    installed_version: &str,
    entry: Option<&CatalogueEntry>,
) -> Option<super::types::PluginRegistryStatus> {
    if row.project().is_none() && !row.unresolved {
        return None;
    }
    let entry = entry.filter(|e| row.project().is_some() && speaks_for(row, e));
    let installed = entry.and_then(|e| {
        e.releases
            .iter()
            .find(|r| row.version_uri.as_deref() == Some(r.version_uri.as_str()))
            .or_else(|| e.releases.iter().find(|r| r.version == installed_version))
    });
    let channel_state = entry.and_then(|e| {
        let head = e.channels.get(row.registry_channel.as_deref()?)?;
        let release = e.releases.iter().find(|r| r.version == head.version);
        Some(release.map_or_else(|| head.state.clone(), |r| r.state.clone()))
    });
    Some(super::types::PluginRegistryStatus {
        did: row.registry_did.clone(),
        name: row.registry_name.clone(),
        channel: row.registry_channel.clone(),
        version_uri: row.version_uri.clone(),
        installed_state: installed.map(|r| r.state.clone()),
        installed_reason: installed.and_then(|r| r.reason.clone()),
        channel_state,
        unresolved: row.unresolved,
    })
}

/// The catalogue entry that speaks for an installed plugin: its own
/// project's for a registry install, whoever holds the name in the
/// catalogue; for any other plugin, the entry under its id.
fn entry_for<'a>(
    row: &InstalledRow,
    plugin_id: &str,
    cache: &'a OfficialRegistryState,
) -> Option<&'a CatalogueEntry> {
    match row.project() {
        Some((did, name)) => cache.project(did, name),
        None => cache.plugins.get(plugin_id),
    }
}

use super::auth::UserAuth;
use super::permissions::Permission;
use super::types::{
    AddPluginBody, ListPluginsQuery, PluginAllowedHostsResponse, PluginPreviewResponse,
    PluginSecretsResponse, PluginSummary, PluginsListResponse, PreviewPluginBody, PreviewRegistry,
    RegistrySelector, RemovePluginQuery, UpdatePluginAllowedHostsBody, UpdatePluginSecretsBody,
};

/// The type/namespace/dependency/host/capability fields shared by every
/// `PluginSummary` and `PluginPreviewResponse` response.
struct CapabilityFields {
    plugin_type: String,
    namespace: Option<String>,
    dependencies: Vec<crate::plugin::PluginDependency>,
    allowed_hosts: Vec<String>,
    capabilities: capabilities::CapabilityReport,
}

/// What a surface shows about a plugin before it shows what it does.
/// Gathered here so every summary reads the same source.
struct PresentationFields {
    description: Option<String>,
    icon_url: Option<String>,
    banner_url: Option<String>,
}

fn presentation_fields(p: &LoadedPlugin) -> PresentationFields {
    PresentationFields {
        description: p.description().map(String::from),
        icon_url: p.icon_url().map(String::from),
        banner_url: p.banner_url().map(String::from),
    }
}

fn capability_fields(p: &LoadedPlugin) -> CapabilityFields {
    let report = capabilities::report(p).unwrap_or_else(|e| capabilities::CapabilityReport {
        declared: Vec::new(),
        required_by_imports: Vec::new(),
        undeclared: vec![e],
    });
    CapabilityFields {
        plugin_type: p.plugin_type().as_str().to_string(),
        namespace: p.namespace().map(String::from),
        dependencies: p.dependencies().to_vec(),
        allowed_hosts: p.allowed_hosts().to_vec(),
        capabilities: report,
    }
}

fn required_secrets(p: &LoadedPlugin) -> Vec<super::types::SecretDefinition> {
    match &p.manifest {
        Some(manifest) => manifest
            .required_secrets
            .iter()
            .map(|s| super::types::SecretDefinition {
                key: s.key.clone(),
                name: s.name.clone(),
                description: s.description.clone(),
            })
            .collect(),
        // No manifest (the loader always attaches one; this only covers a
        // plugin registered directly, bypassing it) - fall back to minimal
        // definitions from the keys.
        None => p
            .info
            .required_secrets
            .iter()
            .map(|key| super::types::SecretDefinition {
                key: key.clone(),
                name: key.clone(),
                description: None,
            })
            .collect(),
    }
}

/// The plugins with a stored configuration.
async fn configured_plugins(state: &AppState) -> HashSet<String> {
    let sql = adapt_sql(
        "SELECT plugin_id FROM happyview_plugin_configs WHERE config IS NOT NULL",
        state.db_backend,
    );
    crate::db::query_scalar::<String>(&sql)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect()
}

/// Whether a plugin's secrets are configured: it needs none, or has a
/// stored configuration.
fn secrets_configured(p: &LoadedPlugin, configured: &HashSet<String>) -> bool {
    required_secrets(p).is_empty() || configured.contains(&p.info.id)
}

/// A plugin's summary, judged against `entry`, the catalogue entry under its
/// id (or a fresher read of it).
fn summarize(
    p: &LoadedPlugin,
    row: &InstalledRow,
    entry: Option<&CatalogueEntry>,
    secrets_configured: bool,
    loaded_at: Option<String>,
) -> PluginSummary {
    let (source, url, sha256) = match &p.source {
        PluginSource::File { path } => ("file".to_string(), Some(path.display().to_string()), None),
        PluginSource::Url { url, sha256, .. } => {
            ("url".to_string(), Some(url.clone()), sha256.clone())
        }
    };
    let entry = entry.filter(|e| speaks_for(row, e));
    let update = compute_update_info(
        &p.info.version,
        &InstalledPublisher::of(row.publisher_did.clone()),
        row.follows(),
        entry,
    );
    let cap_fields = capability_fields(p);
    let pres = presentation_fields(p);
    PluginSummary {
        id: p.info.id.clone(),
        name: p.info.name.clone(),
        version: p.info.version.clone(),
        description: pres.description,
        icon_url: pres.icon_url,
        banner_url: pres.banner_url,
        source,
        url,
        sha256,
        enabled: true, // Currently all loaded plugins are enabled
        auth_type: p.info.auth_type.clone(),
        required_secrets: required_secrets(p),
        secrets_configured,
        loaded_at,
        update_available: update.update_available,
        latest_version: update.latest_version,
        pending_releases: update.pending_releases,
        latest_version_installable: update.latest_version_installable,
        latest_state: update.latest_state,
        registry: registry_status(row, &p.info.version, entry),
        plugin_type: cap_fields.plugin_type,
        namespace: cap_fields.namespace,
        dependencies: cap_fields.dependencies,
        allowed_hosts: cap_fields.allowed_hosts,
        capabilities: cap_fields.capabilities,
    }
}

/// `summarize`, reading the plugin's row and the cached catalogue entry.
async fn summary_of(
    state: &AppState,
    p: &LoadedPlugin,
    loaded_at: Option<String>,
) -> Result<PluginSummary, AppError> {
    let row = installed_row(state, &p.info.id).await?;
    let configured = secrets_configured(p, &configured_plugins(state).await);
    let guard = state.official_registry.read().await;
    Ok(summarize(
        p,
        &row,
        entry_for(&row, &p.info.id, &guard),
        configured,
        loaded_at,
    ))
}

/// GET /admin/plugins - list all loaded plugins, optionally filtered by
/// `?type=library|interpreter|auth`.
pub(super) async fn list(
    State(state): State<AppState>,
    auth: UserAuth,
    Query(q): Query<ListPluginsQuery>,
) -> Result<Json<PluginsListResponse>, AppError> {
    auth.require(Permission::PluginsRead).await?;

    let plugins = if let Some(type_str) = &q.plugin_type {
        let t = PluginType::parse_str(type_str)
            .ok_or_else(|| AppError::BadRequest(format!("Unknown plugin type '{}'", type_str)))?;
        state.plugin_registry.list_by_type(t).await
    } else {
        state.plugin_registry.list().await
    };

    let configured = configured_plugins(&state).await;

    let rows = installed_rows(&state).await?;
    let official_guard = state.official_registry.read().await;

    let summaries: Vec<PluginSummary> = plugins
        .into_iter()
        .map(|p| {
            let row = rows.get(&p.info.id).cloned().unwrap_or_default();
            summarize(
                &p,
                &row,
                entry_for(&row, &p.info.id, &official_guard),
                secrets_configured(&p, &configured),
                None, // Would need to track this in registry
            )
        })
        .collect();

    Ok(Json(PluginsListResponse {
        plugins: summaries,
        encryption_configured: state.config.token_encryption_key.is_some(),
    }))
}

// ---------------------------------------------------------------------------
// Installing from the registry.
// ---------------------------------------------------------------------------

/// The registry installs resolve against: the one the catalogue reads.
fn registry_source(state: &AppState) -> Result<&RegistrySource, AppError> {
    let config = &state.official_registry_config;
    match &config.registry {
        Some(source) if !config.disabled => Ok(source),
        _ => Err(AppError::BadRequest(
            "the plugin registry is turned off (PLUGIN_REGISTRY_URL=off)".into(),
        )),
    }
}

fn check_selector(selector: &RegistrySelector) -> Result<(), AppError> {
    match (&selector.channel, &selector.version) {
        (Some(_), Some(_)) => Err(AppError::BadRequest(
            "name a channel or a version, not both".into(),
        )),
        (Some(channel), None) if !TRACKS.contains(&channel.as_str()) => Err(AppError::BadRequest(
            format!("channel '{channel}' must be one of {}", TRACKS.join(", ")),
        )),
        _ => Ok(()),
    }
}

/// Why a version may not be installed, or `None` when it may. Says what an
/// operator would act on: the publisher's state and reason, the registry
/// not holding the files, or the version being suspect.
fn not_installable_because(view: &VersionView) -> Option<String> {
    if view.installable() {
        return None;
    }
    let mut why = Vec::new();
    if !view.lifecycle.installable {
        why.push(match &view.lifecycle.reason {
            Some(reason) => format!("its publisher marked it {}: {reason}", view.lifecycle.state),
            None => format!("its publisher marked it {}", view.lifecycle.state),
        });
    }
    if view.availability != "available" {
        why.push(format!(
            "the registry doesn't hold its files ({})",
            view.availability
        ));
    }
    if view.suspect {
        why.push(
            "the registry flagged it as suspect because its records changed after it was published"
                .into(),
        );
    }
    Some(format!(
        "{} {} can't be installed: {}",
        view.name,
        view.version,
        why.join("; ")
    ))
}

/// A registry version, resolved, with its artifacts fetched and checked
/// against their CIDs.
struct ResolvedRelease {
    view: VersionView,
    channel: Option<String>,
    preview: loader::PluginPreview,
}

/// Resolve `did`'s project `name` at `version`, or else at `channel`'s
/// current version (`latest` by default), refusing a version that may not be
/// installed, and fetch and verify its artifacts. Nothing is installed.
async fn resolve_release(
    state: &AppState,
    did: &str,
    name: &str,
    selector: &RegistrySelector,
) -> Result<ResolvedRelease, AppError> {
    check_selector(selector)?;
    let source = registry_source(state)?;
    let unreadable = |e| AppError::BadGateway(format!("the registry could not be read: {e}"));
    let (view, channel) = match &selector.version {
        Some(version) => (
            source
                .get_version(&state.http, did, name, version)
                .await
                .map_err(unreadable)?
                .ok_or_else(|| {
                    AppError::NotFound(format!(
                        "{did} has published no version {version} of {name}"
                    ))
                })?,
            None,
        ),
        None => {
            let channel = selector.channel.as_deref().unwrap_or(LATEST);
            (
                source
                    .resolve_channel(&state.http, did, name, channel)
                    .await
                    .map_err(unreadable)?
                    .ok_or_else(|| {
                        AppError::NotFound(format!(
                            "{did} has published no {name} on the {channel} channel"
                        ))
                    })?,
                Some(channel.to_string()),
            )
        }
    };
    // The publisher recorded for the install is the registry's answer, so it
    // must be the project that was asked for.
    if view.did != did || view.name != name {
        return Err(AppError::BadGateway(format!(
            "the registry answered for {}'s {} when asked for {did}'s {name}",
            view.did, view.name
        )));
    }
    // The caller reviewed one version; a channel that moved since names
    // another, whose capabilities and notes nobody saw.
    if let Some(expected) = &selector.expected_version
        && expected != &view.version
    {
        return Err(AppError::Conflict(format!(
            "{name} resolved to {} where {expected} was previewed; the channel moved, so \
             preview it again",
            view.version
        )));
    }
    if let Some(why) = not_installable_because(&view) {
        return Err(AppError::BadRequest(why));
    }

    let install =
        crate::plugin::registry_source::registry_install_for(source, &view, channel.clone());
    let preview = loader::fetch_registry_artifacts(&state.http, &install)
        .await
        .map_err(|e| AppError::BadRequest(format!("Failed to fetch the plugin: {e}")))?;
    // The project name is the plugin's id; a manifest naming another plugin
    // would install under an id the project doesn't own.
    if preview.manifest.id != name {
        return Err(AppError::BadRequest(format!(
            "{name} {}'s manifest names the plugin '{}', not '{name}'",
            view.version, preview.manifest.id
        )));
    }
    Ok(ResolvedRelease {
        view,
        channel,
        preview,
    })
}

/// A URL or a registry project, exactly one.
enum Origin<'a> {
    Url(&'a str),
    Registry(&'a super::types::RegistryProjectRef),
}

fn origin<'a>(
    url: &'a Option<String>,
    registry: &'a Option<super::types::RegistryProjectRef>,
) -> Result<Origin<'a>, AppError> {
    match (url, registry) {
        (Some(url), None) => Ok(Origin::Url(url)),
        (None, Some(project)) => Ok(Origin::Registry(project)),
        _ => Err(AppError::BadRequest(
            "name a plugin by its url or by its registry project, one of the two".into(),
        )),
    }
}

/// POST /admin/plugins/preview - preview a plugin from a URL or a registry
/// project, fetching and checking everything an install would.
pub(super) async fn preview(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<PreviewPluginBody>,
) -> Result<Json<PluginPreviewResponse>, AppError> {
    auth.require(Permission::PluginsCreate).await?;

    let (preview, registry) = match origin(&body.url, &body.registry)? {
        Origin::Url(url) => (
            loader::fetch_manifest(&state.http, url)
                .await
                .map_err(|e| AppError::BadRequest(format!("Failed to fetch manifest: {}", e)))?,
            None,
        ),
        Origin::Registry(project) => {
            let resolved =
                resolve_release(&state, &project.did, &project.name, &project.selector()).await?;
            let registry = PreviewRegistry {
                did: resolved.view.did.clone(),
                name: resolved.view.name.clone(),
                channel: resolved.channel.clone(),
                version: resolved.view.version.clone(),
                version_uri: resolved.view.uri.clone(),
                state: resolved.view.lifecycle.state.clone(),
            };
            (resolved.preview, Some(registry))
        }
    };

    // Download the WASM too, so the capability report and sha256 reflect
    // what would actually be installed. A `CapabilityMismatch` or
    // `UnknownImport` here is surfaced verbatim: it's the reason the
    // plugin cannot be installed.
    let plugin = loader::load_from_manifest(&state.http, &preview, None)
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let cap_fields = capability_fields(&plugin);
    let pres = presentation_fields(&plugin);
    let sha256 = hex::encode(Sha256::digest(&plugin.wasm_bytes));

    Ok(Json(PluginPreviewResponse {
        id: preview.manifest.id,
        name: preview.manifest.name,
        version: preview.manifest.version,
        description: pres.description,
        icon_url: pres.icon_url,
        banner_url: pres.banner_url,
        auth_type: preview.manifest.auth_type,
        required_secrets: preview
            .manifest
            .required_secrets
            .into_iter()
            .map(|s| super::types::SecretDefinition {
                key: s.key,
                name: s.name,
                description: s.description,
            })
            .collect(),
        manifest_url: preview.manifest_url,
        wasm_url: preview.wasm_url,
        plugin_type: cap_fields.plugin_type,
        namespace: cap_fields.namespace,
        dependencies: cap_fields.dependencies,
        allowed_hosts: cap_fields.allowed_hosts,
        capabilities: cap_fields.capabilities,
        sha256,
        registry,
    }))
}

/// If `accepted` was supplied, every capability `plugin` would actually be
/// granted must be covered by it — used identically by `add` and `reload` so
/// a reload can't silently bring in an import the operator never consented
/// to. Returns the sorted, deduped capability names (for the event log
/// detail) when a check was performed, or `None` when `accepted` was omitted
/// (consent checking is opt-in on the wire).
fn check_capability_consent(
    plugin: &LoadedPlugin,
    accepted: &Option<Vec<String>>,
) -> Result<Option<Vec<String>>, AppError> {
    let Some(accepted) = accepted else {
        return Ok(None);
    };
    let effective = capabilities::effective_set(plugin).map_err(AppError::BadRequest)?;
    let missing: Vec<&str> = effective
        .iter()
        .map(|c| c.as_str())
        .filter(|name| !accepted.iter().any(|a| a == name))
        .collect();
    if !missing.is_empty() {
        return Err(AppError::BadRequest(format!(
            "capability consent missing: {}",
            missing.join(", ")
        )));
    }
    let mut names: Vec<String> = effective.iter().map(|c| c.as_str().to_string()).collect();
    names.sort();
    Ok(Some(names))
}

/// The registry release a loaded plugin was fetched from, if it was.
fn registry_install_of(p: &LoadedPlugin) -> Option<&RegistryInstall> {
    match &p.source {
        PluginSource::Url { registry, .. } => registry.as_deref(),
        PluginSource::File { .. } => None,
    }
}

fn registry_detail(install: &RegistryInstall) -> serde_json::Value {
    serde_json::json!({
        "did": install.did,
        "name": install.name,
        "channel": install.channel,
        "version": install.version,
    })
}

/// Read a just-installed registry plugin's project into the cache, so the
/// listing can show its state before the next full refresh even when another
/// publisher holds its name in the catalogue. The install has happened; a
/// registry that can't be read now only delays that.
async fn track_installed_project(state: &AppState, name: &str, did: Option<&str>) {
    let Some(did) = did else { return };
    if let Err(e) = crate::plugin::official_registry::refresh_project(
        &state.http,
        &state.official_registry_config,
        &state.official_registry,
        did,
        name,
    )
    .await
    {
        tracing::warn!(plugin = %name, publisher = %did, error = %e, "installed project not cached");
    }
}

/// POST /admin/plugins - add a plugin from a URL or a registry project
pub(super) async fn add(
    State(state): State<AppState>,
    auth: UserAuth,
    Json(body): Json<AddPluginBody>,
) -> Result<Json<PluginSummary>, AppError> {
    auth.require(Permission::PluginsCreate).await?;

    let origin = origin(&body.url, &body.registry)?;
    if matches!(origin, Origin::Registry(_)) && body.sha256.is_some() {
        return Err(AppError::BadRequest(
            "sha256 checks a plugin added from a url; a registry plugin is checked against \
             the CIDs its version names"
                .into(),
        ));
    }
    let (plugin, mut detail) = match origin {
        Origin::Url(url) => {
            let preview = loader::fetch_manifest(&state.http, url)
                .await
                .map_err(|e| AppError::BadRequest(format!("Failed to fetch manifest: {}", e)))?;
            let plugin = loader::load_from_manifest(&state.http, &preview, body.sha256.as_deref())
                .await
                .map_err(|e| AppError::BadRequest(format!("Failed to load plugin: {}", e)))?;
            (plugin, serde_json::json!({ "url": url }))
        }
        Origin::Registry(project) => {
            let resolved =
                resolve_release(&state, &project.did, &project.name, &project.selector()).await?;
            let plugin = loader::load_from_manifest(&state.http, &resolved.preview, None)
                .await
                .map_err(|e| AppError::BadRequest(format!("Failed to load plugin: {}", e)))?;
            let detail = match &resolved.preview.registry {
                Some(install) => serde_json::json!({ "registry": registry_detail(install) }),
                None => serde_json::json!({}),
            };
            (plugin, detail)
        }
    };

    // If the caller told us which capabilities they consent to, every
    // capability the plugin would actually be granted must be covered.
    let consented_capabilities = check_capability_consent(&plugin, &body.accepted_capabilities)?;

    let plugin_id = plugin.info.id.clone();
    let registry_publisher = registry_install_of(&plugin).map(|r| r.did.clone());

    // Validate against the dependency graph and register (this also
    // persists to DB).
    state
        .plugin_registry
        .install(plugin)
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    match origin {
        Origin::Url(_) => {
            if let Err(e) = forget_registry_project(&state, &plugin_id).await {
                tracing::warn!(plugin = %plugin_id, error = ?e, "plugin installed from a url still names its registry project");
            }
            // A URL is no publisher's offer, so nothing is recorded: the
            // plugin is matched to the official project of its name at the
            // next refresh, or to none. The plugin is installed whatever
            // happens here.
            if let Err(e) = set_publisher(&state, &plugin_id, None).await {
                tracing::warn!(plugin = %plugin_id, error = ?e, "plugin installed from a url keeps an earlier publisher");
            }
        }
        Origin::Registry(_) => {
            // The registry's answer, checked against the request in
            // `resolve_release`, is whose project this is.
            if let Err(e) = set_publisher(&state, &plugin_id, registry_publisher.as_deref()).await {
                tracing::warn!(plugin = %plugin_id, error = ?e, "plugin installed without a recorded publisher");
            }
            track_installed_project(&state, &plugin_id, registry_publisher.as_deref()).await;
        }
    }

    let installed =
        state.plugin_registry.get(&plugin_id).await.ok_or_else(|| {
            AppError::Internal(format!("'{plugin_id}' vanished as it was installed"))
        })?;
    let mut summary = summary_of(&state, &installed, Some(now_rfc3339())).await?;
    if let Origin::Url(url) = origin {
        summary.url = Some(url.to_string());
        summary.sha256 = body.sha256.clone();
    }

    if let Some(caps) = consented_capabilities {
        detail["capabilities"] = serde_json::json!(caps);
    }

    log_event(
        &state.db,
        EventLog {
            event_type: "plugin.added".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(plugin_id),
            detail,
        },
        state.db_backend,
    )
    .await;

    Ok(Json(summary))
}

/// DELETE /admin/plugins/{id} - remove a plugin
pub(super) async fn remove(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(plugin_id): Path<String>,
    Query(q): Query<RemovePluginQuery>,
) -> Result<axum::response::Response, AppError> {
    auth.require(Permission::PluginsDelete).await?;

    // Validate against the dependency graph (unless forced) and remove from
    // the registry. `removed` is dependents-first: for a forced cascade,
    // everything that depended on `plugin_id` comes out before it does.
    let removed = match state.plugin_registry.uninstall(&plugin_id, q.force).await {
        Ok(ids) if ids.is_empty() => {
            return Err(AppError::NotFound(format!(
                "Plugin '{}' not found",
                plugin_id
            )));
        }
        Ok(ids) => ids,
        Err(GraphError::HasDependents { dependents, .. }) => {
            return Ok((
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": format!(
                        "plugin '{plugin_id}' is required by: {}",
                        dependents.join(", ")
                    ),
                    "dependents": dependents,
                })),
            )
                .into_response());
        }
        Err(e) => return Err(AppError::BadRequest(e.to_string())),
    };

    // Remove each id from the database and log one event per id.
    for id in &removed {
        let sql = adapt_sql(
            "DELETE FROM happyview_plugins WHERE id = ?",
            state.db_backend,
        );
        crate::db::query(&sql)
            .bind(id)
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("Failed to delete plugin: {}", e)))?;

        log_event(
            &state.db,
            EventLog {
                event_type: "plugin.removed".to_string(),
                severity: Severity::Info,
                actor_did: Some(auth.did.clone()),
                subject: Some(id.clone()),
                detail: serde_json::json!({}),
            },
            state.db_backend,
        )
        .await;
    }

    if removed == [plugin_id] {
        Ok(StatusCode::NO_CONTENT.into_response())
    } else {
        Ok((
            StatusCode::OK,
            Json(serde_json::json!({ "removed": removed })),
        )
            .into_response())
    }
}

/// POST /admin/plugins/{id}/reload - reload a plugin from its source. A
/// registry plugin reloads from the registry: by default the version it has,
/// fetched again by CID; with `registry`, its channel's current version,
/// another channel's, or a pinned version.
pub(super) async fn reload(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(plugin_id): Path<String>,
    body: Option<Json<super::types::ReloadPluginBody>>,
) -> Result<Json<PluginSummary>, AppError> {
    auth.require(Permission::PluginsCreate).await?;

    // Get current plugin to find its source
    let current = state
        .plugin_registry
        .get(&plugin_id)
        .await
        .ok_or_else(|| AppError::NotFound(format!("Plugin '{}' not found", plugin_id)))?;

    let (url, sha256) = match &current.source {
        PluginSource::Url { url, sha256, .. } => (url.clone(), sha256.clone()),
        PluginSource::File { .. } => {
            return Err(AppError::BadRequest(
                "Cannot reload file-based plugins via API".into(),
            ));
        }
    };

    let body = body.map(|Json(b)| b);
    let accepted_capabilities = body.as_ref().and_then(|b| b.accepted_capabilities.clone());
    let selector = body.as_ref().and_then(|b| b.registry.clone());
    let new_url = body.as_ref().and_then(|b| b.url.clone());
    let row = installed_row(&state, &plugin_id).await?;
    let installed_release = registry_install_of(&current).cloned();

    let reloaded = match (selector, new_url, installed_release) {
        (Some(_), Some(_), _) => {
            return Err(AppError::BadRequest(
                "name a url or a registry version, not both".into(),
            ));
        }
        (Some(selector), None, installed_release) => {
            let Some((did, name)) = row.project() else {
                return Err(AppError::BadRequest(format!(
                    "'{plugin_id}' wasn't installed from the registry; add it from the registry \
                     to follow a channel"
                )));
            };
            // A recorded install stays with its publisher, as a URL reload
            // does: the project must be the recorded publisher's.
            if !InstalledPublisher::of(row.publisher_did.clone()).accepts(Some(did)) {
                return Err(AppError::Conflict(format!(
                    "'{plugin_id}' was installed from {}'s package, and {did}'s {name} is \
                     not that publisher's. Remove the plugin and add it again to install \
                     another publisher's.",
                    row.publisher_did
                        .as_deref()
                        .unwrap_or("an unrecorded publisher")
                )));
            }
            // Naming neither keeps what it has: the channel it follows, or
            // the version it is pinned to.
            let selector = match (&selector.channel, &selector.version) {
                (None, None) => match &row.registry_channel {
                    Some(channel) => RegistrySelector {
                        channel: Some(channel.clone()),
                        version: None,
                        expected_version: selector.expected_version,
                    },
                    None => RegistrySelector {
                        channel: None,
                        version: Some(
                            installed_release
                                .map(|r| r.version)
                                .unwrap_or_else(|| current.info.version.clone()),
                        ),
                        expected_version: selector.expected_version,
                    },
                },
                _ => selector,
            };
            let resolved = resolve_release(&state, did, name, &selector).await?;
            Reloaded::Registry(Box::new(resolved.preview))
        }
        (None, Some(_), Some(_)) => {
            return Err(AppError::BadRequest(format!(
                "'{plugin_id}' is installed from the registry; reload it by channel or \
                 version, or remove it and add it from the url"
            )));
        }
        (None, None, Some(installed_release)) => Reloaded::Registry(Box::new(
            loader::fetch_registry_artifacts(&state.http, &installed_release)
                .await
                .map_err(|e| AppError::BadRequest(format!("Failed to reload plugin: {e}")))?,
        )),
        (None, new_url, None) => {
            let installed_url = url.clone();
            let (url, sha256) = resolve_reload_url((url, sha256), new_url);
            // A recorded install stays with its publisher, and a URL is no
            // publisher's offer, so it can't be moved to a new one. An
            // unrecorded install has no publisher to hold to.
            if url != installed_url
                && let Some(recorded) = &row.publisher_did
            {
                return Err(AppError::Conflict(format!(
                    "'{plugin_id}' was installed from {recorded}'s package, and a URL can't be \
                     checked against that publisher. Update it from the registry, or remove the \
                     plugin and add it again from the URL."
                )));
            }
            let preview = loader::fetch_manifest(&state.http, &url)
                .await
                .map_err(|e| AppError::BadRequest(format!("Failed to fetch manifest: {}", e)))?;
            Reloaded::Url {
                changed: url != installed_url,
                url,
                sha256,
                preview: Box::new(preview),
            }
        }
    };

    let (preview, sha256) = match &reloaded {
        Reloaded::Registry(preview) => (&**preview, None),
        Reloaded::Url {
            preview, sha256, ..
        } => (&**preview, sha256.clone()),
    };
    let plugin = loader::load_from_manifest(&state.http, preview, sha256.as_deref())
        .await
        .map_err(|e| AppError::BadRequest(format!("Failed to reload plugin: {}", e)))?;

    // Same consent check `add` performs, so a reloaded version can't bring in
    // a capability the operator never agreed to. Checked before the old
    // plugin is removed, so a refusal here leaves the currently-installed
    // version untouched rather than uninstalling it and then failing to
    // reinstall.
    let consented_capabilities = check_capability_consent(&plugin, &accepted_capabilities)?;
    let registry_publisher = registry_install_of(&plugin).map(|r| r.did.clone());
    let mut detail = match registry_install_of(&plugin) {
        Some(install) => serde_json::json!({ "registry": registry_detail(install) }),
        None => serde_json::json!({ "url": plugin_url(&reloaded) }),
    };

    // Remove old plugin
    state.plugin_registry.remove(&plugin_id).await;

    if let Reloaded::Url { url, .. } = &reloaded {
        // Persist the (possibly new) URL so restarts pick it up
        let persist_sql = adapt_sql(
            "UPDATE happyview_plugins SET url = ?, sha256 = NULL WHERE id = ?",
            state.db_backend,
        );
        crate::db::query(&persist_sql)
            .bind(url)
            .bind(&plugin_id)
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("Failed to persist plugin URL: {}", e)))?;
    }

    // Validate against the dependency graph and register the reloaded plugin
    // (which also persists it, registry columns included).
    state
        .plugin_registry
        .install(plugin)
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    match &reloaded {
        // A new URL is no publisher's and no registry project's (the guard
        // above let it through only for an unrecorded install); reloading
        // the same URL keeps both.
        Reloaded::Url { changed: true, .. } => {
            set_publisher(&state, &plugin_id, None).await?;
            forget_registry_project(&state, &plugin_id).await?;
        }
        Reloaded::Url { .. } => {}
        // The project's publisher, which the check above held to the
        // recorded one. An unrecorded install the official publisher
        // updated is the official publisher's from now on.
        Reloaded::Registry(_) => {
            set_publisher(&state, &plugin_id, registry_publisher.as_deref()).await?;
            track_installed_project(&state, &plugin_id, registry_publisher.as_deref()).await;
        }
    }

    if let Some(caps) = consented_capabilities {
        detail["capabilities"] = serde_json::json!(caps);
    }

    log_event(
        &state.db,
        EventLog {
            event_type: "plugin.reloaded".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(plugin_id.clone()),
            detail,
        },
        state.db_backend,
    )
    .await;

    let reinstalled =
        state.plugin_registry.get(&plugin_id).await.ok_or_else(|| {
            AppError::Internal(format!("'{plugin_id}' vanished as it was reloaded"))
        })?;
    let mut summary = summary_of(&state, &reinstalled, Some(now_rfc3339())).await?;
    if let Reloaded::Url { url, sha256, .. } = &reloaded {
        summary.url = Some(url.clone());
        summary.sha256 = sha256.clone();
    }
    Ok(Json(summary))
}

/// What a reload fetched: a registry release, verified by CID, or a URL's
/// manifest.
enum Reloaded {
    Registry(Box<loader::PluginPreview>),
    Url {
        url: String,
        sha256: Option<String>,
        /// The URL differs from the one installed.
        changed: bool,
        preview: Box<loader::PluginPreview>,
    },
}

fn plugin_url(reloaded: &Reloaded) -> &str {
    match reloaded {
        Reloaded::Registry(preview) => &preview.manifest_url,
        Reloaded::Url { url, .. } => url,
    }
}

/// GET /admin/plugins/{id}/secrets - get plugin secrets (values masked)
pub(super) async fn get_secrets(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(plugin_id): Path<String>,
) -> Result<Json<PluginSecretsResponse>, AppError> {
    auth.require(Permission::PluginsRead).await?;

    let encryption_key = state
        .config
        .token_encryption_key
        .as_ref()
        .ok_or_else(|| AppError::Internal("Encryption key not configured".into()))?;

    // Verify plugin exists
    state
        .plugin_registry
        .get(&plugin_id)
        .await
        .ok_or_else(|| AppError::NotFound(format!("Plugin '{}' not found", plugin_id)))?;

    // Get secrets from plugin_configs table
    let sql = adapt_sql(
        "SELECT config FROM happyview_plugin_configs WHERE plugin_id = ?",
        state.db_backend,
    );

    let row: Option<(String,)> = crate::db::query_as(&sql)
        .bind(&plugin_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to fetch config: {}", e)))?;

    let secrets: HashMap<String, String> = match row {
        Some((config_json,)) => {
            let config: serde_json::Value = serde_json::from_str(&config_json)
                .map_err(|e| AppError::Internal(format!("Invalid config JSON: {}", e)))?;

            // Extract and decrypt secrets, then mask for display
            if let Some(secrets_obj) = config.get("secrets").and_then(|s| s.as_object()) {
                secrets_obj
                    .iter()
                    .filter_map(|(k, v)| {
                        v.as_str().and_then(|encrypted_b64| {
                            // Decode base64 and decrypt
                            let encrypted = BASE64.decode(encrypted_b64).ok()?;
                            let decrypted = decrypt(encryption_key, &encrypted).ok()?;
                            let val = String::from_utf8(decrypted).ok()?;

                            // Mask the value for display
                            let masked = if val.len() > 8 {
                                format!("********{}", &val[val.len() - 4..])
                            } else {
                                "********".to_string()
                            };
                            Some((k.clone(), masked))
                        })
                    })
                    .collect()
            } else {
                HashMap::new()
            }
        }
        None => HashMap::new(),
    };

    Ok(Json(PluginSecretsResponse { plugin_id, secrets }))
}

/// PUT /admin/plugins/{id}/secrets - update plugin secrets
pub(super) async fn update_secrets(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(plugin_id): Path<String>,
    Json(body): Json<UpdatePluginSecretsBody>,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::PluginsCreate).await?;

    let encryption_key = state
        .config
        .token_encryption_key
        .as_ref()
        .ok_or_else(|| AppError::Internal("Encryption key not configured".into()))?;

    // Verify plugin exists
    state
        .plugin_registry
        .get(&plugin_id)
        .await
        .ok_or_else(|| AppError::NotFound(format!("Plugin '{}' not found", plugin_id)))?;

    // Get existing config or create new one
    let sql = adapt_sql(
        "SELECT config FROM happyview_plugin_configs WHERE plugin_id = ?",
        state.db_backend,
    );

    let row: Option<(String,)> = crate::db::query_as(&sql)
        .bind(&plugin_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to fetch config: {}", e)))?;

    let mut config: serde_json::Value = match row {
        Some((config_json,)) => serde_json::from_str(&config_json)
            .map_err(|e| AppError::Internal(format!("Invalid config JSON: {}", e)))?,
        None => serde_json::json!({}),
    };

    // Get existing encrypted secrets to preserve unchanged values
    let existing_secrets: HashMap<String, String> = config
        .get("secrets")
        .and_then(|s| s.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    // Merge secrets: if new value starts with "********", keep existing encrypted value
    // Otherwise, encrypt the new value
    let mut merged_secrets = serde_json::Map::new();
    for (key, value) in body.secrets {
        if value.starts_with("********") {
            // Keep existing encrypted value if present
            if let Some(existing) = existing_secrets.get(&key) {
                merged_secrets.insert(key, serde_json::Value::String(existing.clone()));
            }
        } else if !value.is_empty() {
            // Encrypt new value and store as base64
            let encrypted = encrypt(encryption_key, value.as_bytes())
                .map_err(|e| AppError::Internal(format!("Encryption failed: {}", e)))?;
            let encoded = BASE64.encode(&encrypted);
            merged_secrets.insert(key, serde_json::Value::String(encoded));
        }
        // Empty values are not stored (allows clearing a secret)
    }

    // Update config with merged secrets
    config["secrets"] = serde_json::Value::Object(merged_secrets);

    let config_json = serde_json::to_string(&config)
        .map_err(|e| AppError::Internal(format!("Failed to serialize config: {}", e)))?;

    // Upsert into plugin_configs
    let sql = adapt_sql(
        "INSERT INTO happyview_plugin_configs (plugin_id, config, updated_at) VALUES (?, ?, ?)
         ON CONFLICT (plugin_id) DO UPDATE SET config = EXCLUDED.config, updated_at = EXCLUDED.updated_at",
        state.db_backend,
    );

    crate::db::query(&sql)
        .bind(&plugin_id)
        .bind(&config_json)
        .bind(now_rfc3339())
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to update secrets: {}", e)))?;

    log_event(
        &state.db,
        EventLog {
            event_type: "plugin.secrets_updated".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(plugin_id),
            detail: serde_json::json!({}),
        },
        state.db_backend,
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

/// A plugin's `allowed_hosts` only means anything under
/// `network:request:defined` — the operator has nowhere else to point it,
/// since `network:request` takes its list from the manifest and
/// `network:request:unrestricted` needs none. Both allowed-hosts endpoints
/// refuse a plugin that hasn't declared it, the same way they refuse one
/// that doesn't exist.
fn require_network_request_defined(plugin: &LoadedPlugin) -> Result<(), AppError> {
    if plugin
        .declared_capabilities()
        .contains(&capabilities::PluginCapability::NetworkRequestDefined)
    {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!(
            "plugin '{}' does not declare network:request:defined",
            plugin.info.id
        )))
    }
}

/// GET /admin/plugins/{id}/allowed-hosts - operator-configured hosts for a
/// `network:request:defined` plugin
pub(super) async fn get_allowed_hosts(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(plugin_id): Path<String>,
) -> Result<Json<PluginAllowedHostsResponse>, AppError> {
    auth.require(Permission::PluginsRead).await?;

    let plugin = state
        .plugin_registry
        .get(&plugin_id)
        .await
        .ok_or_else(|| AppError::NotFound(format!("Plugin '{}' not found", plugin_id)))?;
    require_network_request_defined(&plugin)?;

    let hosts = crate::plugin::config::load_allowed_hosts(&state.db, state.db_backend, &plugin_id)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to fetch config: {}", e)))?;

    Ok(Json(PluginAllowedHostsResponse { hosts }))
}

/// PUT /admin/plugins/{id}/allowed-hosts - replace the operator-configured
/// hosts for a `network:request:defined` plugin, returning the stored list
pub(super) async fn update_allowed_hosts(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(plugin_id): Path<String>,
    Json(body): Json<UpdatePluginAllowedHostsBody>,
) -> Result<Json<PluginAllowedHostsResponse>, AppError> {
    auth.require(Permission::PluginsCreate).await?;

    let plugin = state
        .plugin_registry
        .get(&plugin_id)
        .await
        .ok_or_else(|| AppError::NotFound(format!("Plugin '{}' not found", plugin_id)))?;
    require_network_request_defined(&plugin)?;

    crate::plugin::config::store_allowed_hosts(
        &state.db,
        state.db_backend,
        &plugin_id,
        &body.hosts,
    )
    .await?;

    log_event(
        &state.db,
        EventLog {
            event_type: "plugin.allowed_hosts_updated".to_string(),
            severity: Severity::Info,
            actor_did: Some(auth.did.clone()),
            subject: Some(plugin_id),
            detail: serde_json::json!({ "allowed_hosts": &body.hosts }),
        },
        state.db_backend,
    )
    .await;

    Ok(Json(PluginAllowedHostsResponse { hosts: body.hosts }))
}

/// POST /admin/plugins/{id}/check-update — read one plugin's project from the
/// registry now. A registry plugin's own project is read, whoever holds its
/// name in the catalogue; any other plugin's is the catalogue entry's.
pub(super) async fn check_update(
    State(state): State<AppState>,
    auth: UserAuth,
    Path(plugin_id): Path<String>,
) -> Result<Json<PluginSummary>, AppError> {
    auth.require(Permission::PluginsCreate).await?;

    let row = installed_row(&state, &plugin_id).await?;
    let fresh = match row.project() {
        Some((did, name)) => {
            crate::plugin::official_registry::refresh_project(
                &state.http,
                &state.official_registry_config,
                &state.official_registry,
                did,
                name,
            )
            .await
        }
        None => {
            crate::plugin::official_registry::refresh_plugin(
                &state.http,
                &state.official_registry_config,
                &state.official_registry,
                &plugin_id,
            )
            .await
        }
    }
    .map_err(|e| AppError::BadRequest(format!("Update check failed: {}", e)))?;

    let current = state
        .plugin_registry
        .get(&plugin_id)
        .await
        .ok_or_else(|| AppError::NotFound(format!("Plugin '{}' not found", plugin_id)))?;
    let configured = secrets_configured(&current, &configured_plugins(&state).await);
    Ok(Json(summarize(
        &current,
        &row,
        fresh.as_ref(),
        configured,
        None,
    )))
}

/// GET /admin/plugins/official — list plugins from the official registry cache
pub(super) async fn list_official(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<super::types::OfficialPluginsListResponse>, AppError> {
    auth.require(Permission::PluginsRead).await?;

    let guard = state.official_registry.read().await;
    let plugins = guard
        .plugins
        .values()
        .map(|p| super::types::OfficialPluginSummary {
            id: p.id.clone(),
            publisher_did: p.publisher_did.clone(),
            name: p.name.clone(),
            description: p.description.clone(),
            icon_url: p.icon_url.clone(),
            plugin_type: p.plugin_type.clone(),
            latest_version: p.latest_version.clone(),
            channels: p.channels.clone(),
            lifecycle_state: p.lifecycle_state.clone(),
            official: p.official,
            banner_url: p.banner_url.clone(),
            releases: p.releases.clone(),
            sections: p.sections.clone(),
        })
        .collect::<Vec<_>>();

    Ok(Json(super::types::OfficialPluginsListResponse {
        plugins,
        last_refreshed_at: guard.last_refreshed_at.clone(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::official_registry::{CatalogueEntry, ChannelEntry, ReleaseEntry};

    fn release(version: &str) -> ReleaseEntry {
        ReleaseEntry {
            version: version.into(),
            name: format!("v{version}"),
            published_at: Some("2026-04-10T00:00:00Z".into()),
            body: Some("notes".into()),
            state: "active".into(),
            reason: None,
            suspect: false,
            installable: true,
            availability: "available".into(),
            version_uri: format!("at://did:plc:x/software.release.version/steam.{version}"),
            manifest_cid: String::new(),
            package_cid: String::new(),
            manifest_blob_cid: String::new(),
        }
    }

    fn channel(version: &str) -> ChannelEntry {
        ChannelEntry {
            version: version.into(),
            state: "active".into(),
            installable: true,
            availability: "available".into(),
        }
    }

    /// An official entry whose `latest` points at the first of `versions`.
    fn entry(versions: &[&str]) -> CatalogueEntry {
        CatalogueEntry {
            id: "steam".into(),
            publisher_did: Some(OFFICIAL_PUBLISHER_DID.into()),
            official: true,
            name: "steam".into(),
            description: None,
            icon_url: None,
            plugin_type: "auth".into(),
            channels: [("latest".to_string(), channel(versions[0]))]
                .into_iter()
                .collect(),
            latest_version: Some(versions[0].into()),
            lifecycle_state: "active".into(),
            releases: versions.iter().map(|v| release(v)).collect(),
            sections: Default::default(),
            banner_url: None,
        }
    }

    fn unrecorded() -> InstalledPublisher {
        InstalledPublisher::Unrecorded
    }

    fn latest(
        installed: &str,
        publisher: &InstalledPublisher,
        entry: Option<&CatalogueEntry>,
    ) -> UpdateInfo {
        compute_update_info(installed, publisher, Follows::Registry(Some(LATEST)), entry)
    }

    #[test]
    fn compute_update_info_flags_update_when_behind() {
        let cached = entry(&["1.2.0", "1.1.0", "1.0.0"]);
        let info = latest("1.0.0", &unrecorded(), Some(&cached));
        assert!(info.update_available);
        assert_eq!(info.latest_version.as_deref(), Some("1.2.0"));
        assert_eq!(info.latest_version_installable, Some(true));
        assert_eq!(info.latest_state.as_deref(), Some("active"));
        assert_eq!(info.pending_releases.len(), 2);
        assert_eq!(info.pending_releases[0].version, "1.2.0");
        assert_eq!(info.pending_releases[1].version, "1.1.0");
    }

    #[test]
    fn compute_update_info_no_update_when_current() {
        let cached = entry(&["1.2.0"]);
        let info = latest("1.2.0", &unrecorded(), Some(&cached));
        assert!(!info.update_available);
        assert_eq!(info.latest_version.as_deref(), Some("1.2.0"));
        assert!(info.pending_releases.is_empty());
    }

    /// A channel moved to a version that may not be installed: shown, with
    /// its state, and not offered. Releases in between that may be
    /// installed are still listed.
    #[test]
    fn a_channel_on_a_version_that_is_not_installable_is_not_offered() {
        for (state, availability, suspect) in [
            ("unavailable", "available", false),
            ("retired", "available", false),
            ("active", "deletedAtSource", false),
            ("active", "available", true),
        ] {
            let mut cached = entry(&["1.2.0", "1.1.0", "1.0.0"]);
            let head = cached.channels.get_mut("latest").unwrap();
            head.state = state.into();
            head.availability = availability.into();
            head.installable =
                !matches!(state, "unavailable" | "retired") && availability == "available";
            cached.releases[0].state = state.into();
            cached.releases[0].suspect = suspect;
            cached.releases[0].installable = head.installable && !suspect;

            let info = latest("1.0.0", &unrecorded(), Some(&cached));
            assert!(!info.update_available, "{state}/{availability}/{suspect}");
            assert_eq!(info.latest_version.as_deref(), Some("1.2.0"));
            assert_eq!(info.latest_version_installable, Some(false));
            assert_eq!(info.latest_state.as_deref(), Some(state));
            let pending: Vec<&str> = info
                .pending_releases
                .iter()
                .map(|r| r.version.as_str())
                .collect();
            assert_eq!(pending, vec!["1.1.0"]);
        }
    }

    /// Each channel offers its own track: `latest` its releases, a
    /// prerelease channel its own prereleases, never past where it points.
    #[test]
    fn pending_releases_follow_the_channels_track() {
        let mut cached = entry(&[
            "2.0.0-beta.2",
            "2.0.0-beta.1",
            "1.9.0-alpha.1",
            "1.1.0",
            "1.0.0",
        ]);
        cached.channels = [
            ("latest".to_string(), channel("1.1.0")),
            ("beta".to_string(), channel("2.0.0-beta.1")),
        ]
        .into_iter()
        .collect();
        let versions = |info: UpdateInfo| -> Vec<String> {
            info.pending_releases
                .into_iter()
                .map(|r| r.version)
                .collect()
        };

        assert_eq!(
            versions(latest("1.0.0", &unrecorded(), Some(&cached))),
            vec!["1.1.0"]
        );
        let beta = compute_update_info(
            "1.0.0",
            &unrecorded(),
            Follows::Registry(Some("beta")),
            Some(&cached),
        );
        assert!(beta.update_available);
        assert_eq!(beta.latest_version.as_deref(), Some("2.0.0-beta.1"));
        assert_eq!(versions(beta), vec!["2.0.0-beta.1"]);
        assert!(
            !compute_update_info(
                "1.0.0",
                &unrecorded(),
                Follows::Registry(Some("alpha")),
                Some(&cached),
            )
            .update_available,
            "a channel the project doesn't publish offers nothing"
        );
    }

    /// A pinned install is shown where `latest` stands and offered nothing.
    #[test]
    fn a_pinned_install_is_offered_nothing() {
        let cached = entry(&["1.2.0", "1.0.0"]);
        let info = compute_update_info(
            "1.0.0",
            &unrecorded(),
            Follows::Registry(None),
            Some(&cached),
        );
        assert!(!info.update_available);
        assert_eq!(info.latest_version.as_deref(), Some("1.2.0"));
        assert!(info.pending_releases.is_empty());
    }

    fn stranger(versions: &[&str]) -> CatalogueEntry {
        CatalogueEntry {
            official: false,
            publisher_did: Some("did:plc:someoneelse".into()),
            ..entry(versions)
        }
    }

    /// An install is offered only its own publisher's releases: an official
    /// install never sees a stranger's entry under its id, and a stranger's
    /// install never sees the official one.
    #[test]
    fn an_install_is_only_updated_by_its_own_publisher() {
        let official = InstalledPublisher::Recorded(OFFICIAL_PUBLISHER_DID.into());
        let theirs = InstalledPublisher::Recorded("did:plc:someoneelse".into());
        let third = InstalledPublisher::Recorded("did:plc:third".into());

        assert!(latest("1.0.0", &official, Some(&entry(&["1.2.0"]))).update_available);
        let info = latest("1.0.0", &official, Some(&stranger(&["9.0.0"])));
        assert!(!info.update_available);
        assert!(info.latest_version.is_none());

        assert!(latest("1.0.0", &theirs, Some(&stranger(&["9.0.0"]))).update_available);
        assert!(!latest("1.0.0", &theirs, Some(&entry(&["1.2.0"]))).update_available);
        assert!(!latest("1.0.0", &third, Some(&stranger(&["9.0.0"]))).update_available);
    }

    /// A row that recorded no publisher is tied to none, so only the
    /// official publisher's entry is offered to it.
    #[test]
    fn an_unrecorded_install_is_only_updated_by_the_official_publisher() {
        assert!(!latest("1.0.0", &unrecorded(), Some(&stranger(&["9.0.0"]))).update_available);
        assert!(latest("1.0.0", &unrecorded(), Some(&entry(&["1.2.0"]))).update_available);
    }

    #[test]
    fn compute_update_info_no_cache_entry() {
        let info = latest("1.2.0", &unrecorded(), None);
        assert!(!info.update_available);
        assert!(info.latest_version.is_none());
        assert!(info.pending_releases.is_empty());
    }

    fn attached(installed: &str, entry: &CatalogueEntry) -> UpdateInfo {
        compute_update_info(
            installed,
            &unrecorded(),
            Follows::Attached(Some(LATEST)),
            Some(entry),
        )
    }

    /// A URL install with no registry project is offered nothing, whatever
    /// the catalogue holds under its id.
    #[test]
    fn a_url_install_with_no_project_is_offered_nothing() {
        let cached = entry(&["9.0.0", "1.0.0"]);
        let info = compute_update_info("1.0.0", &unrecorded(), Follows::Nothing, Some(&cached));
        assert!(!info.update_available);
        assert!(info.latest_version.is_none());
        assert!(info.pending_releases.is_empty());
        let row = InstalledRow::default();
        assert!(matches!(row.follows(), Follows::Nothing));
    }

    /// An attached install whose version isn't semver can't be placed
    /// against the channel, so nothing is offered.
    #[test]
    fn compute_update_info_handles_malformed_installed_version() {
        let cached = entry(&["1.2.0"]);
        let info = attached("not-semver", &cached);
        assert!(!info.update_available);
        assert_eq!(info.latest_version.as_deref(), Some("1.2.0"));
        assert!(info.pending_releases.is_empty());
    }

    /// A URL install attached to its project is offered only a newer
    /// version until it is first installed by CID: one installed ahead of
    /// its channel isn't offered the older version. Once installed by CID it
    /// follows the channel wherever it goes.
    #[test]
    fn an_attached_install_ahead_of_its_channel_is_offered_nothing() {
        let cached = entry(&["0.4.0"]);
        let info = attached("0.5.0-alpha.3", &cached);
        assert!(!info.update_available);
        assert!(info.pending_releases.is_empty());
        assert!(attached("0.3.0", &cached).update_available);

        let mut row = registry_row(Some(LATEST));
        assert!(matches!(row.follows(), Follows::Registry(Some(LATEST))));
        row.installed_by_cid = false;
        assert!(matches!(row.follows(), Follows::Attached(Some(LATEST))));
    }

    /// A registry install follows its channel wherever the publisher points
    /// it, even back to an older version.
    #[test]
    fn a_registry_install_is_offered_whatever_its_channel_names() {
        let cached = entry(&["0.4.0"]);
        assert!(latest("0.5.0-alpha.3", &unrecorded(), Some(&cached)).update_available);
        assert!(latest("not-semver", &unrecorded(), Some(&cached)).update_available);
    }

    fn registry_row(channel: Option<&str>) -> InstalledRow {
        InstalledRow {
            publisher_did: Some(OFFICIAL_PUBLISHER_DID.into()),
            registry_did: Some(OFFICIAL_PUBLISHER_DID.into()),
            registry_name: Some("steam".into()),
            registry_channel: channel.map(str::to_owned),
            version_uri: Some("at://did:plc:x/software.release.version/steam.1.0.0".into()),
            unresolved: false,
            installed_by_cid: true,
        }
    }

    /// The installed version's own state and reason, and where the followed
    /// channel stands, from the project's catalogue entry.
    #[test]
    fn registry_status_reads_the_installed_versions_state() {
        let mut cached = entry(&["1.2.0", "1.0.0"]);
        cached.releases[1].state = "retired".into();
        cached.releases[1].reason = Some("leaks tokens".into());
        cached.channels.get_mut("latest").unwrap().state = "deprecated".into();
        cached.releases[0].state = "deprecated".into();

        let status =
            registry_status(&registry_row(Some("latest")), "1.0.0", Some(&cached)).unwrap();
        assert_eq!(status.installed_state.as_deref(), Some("retired"));
        assert_eq!(status.installed_reason.as_deref(), Some("leaks tokens"));
        assert_eq!(status.channel_state.as_deref(), Some("deprecated"));
        assert_eq!(status.channel.as_deref(), Some("latest"));

        // Another publisher's project under the same name says nothing
        // about this one.
        let status = registry_status(
            &registry_row(Some("latest")),
            "1.0.0",
            Some(&stranger(&["1.0.0"])),
        )
        .unwrap();
        assert!(status.installed_state.is_none());
        assert!(status.channel_state.is_none());
    }

    /// A plugin with no project is reported only when it was flagged
    /// unresolved.
    #[test]
    fn registry_status_is_absent_for_a_plain_install() {
        assert!(registry_status(&InstalledRow::default(), "1.0.0", None).is_none());
        let flagged = InstalledRow {
            unresolved: true,
            ..Default::default()
        };
        let status = registry_status(&flagged, "1.0.0", None).unwrap();
        assert!(status.unresolved);
        assert!(status.did.is_none());
    }

    #[test]
    fn a_selector_names_one_known_channel_or_a_version() {
        let ok = |channel: Option<&str>, version: Option<&str>| {
            check_selector(&RegistrySelector {
                channel: channel.map(str::to_owned),
                version: version.map(str::to_owned),
                expected_version: None,
            })
            .is_ok()
        };
        assert!(ok(None, None));
        assert!(ok(Some("beta"), None));
        assert!(ok(None, Some("1.0.0")));
        assert!(!ok(Some("latest"), Some("1.0.0")));
        assert!(!ok(Some("stable"), None));
    }

    #[test]
    fn resolve_reload_url_uses_override_and_clears_sha() {
        let current = ("https://old".to_string(), Some("deadbeef".to_string()));
        let (url, sha) = resolve_reload_url(current, Some("https://new".into()));
        assert_eq!(url, "https://new");
        assert_eq!(sha, None);
    }

    #[test]
    fn resolve_reload_url_keeps_current_without_a_new_url() {
        let current = ("https://old".to_string(), Some("deadbeef".to_string()));
        let (url, sha) = resolve_reload_url(current, None);
        assert_eq!(url, "https://old");
        assert_eq!(sha.as_deref(), Some("deadbeef"));
    }
}

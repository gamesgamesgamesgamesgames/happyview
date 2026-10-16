//! The plugin catalogue cache.
//!
//! Filled from two sources on every refresh: an AT Protocol registry where
//! one is configured (see [`registry_source`](super::registry_source)), which
//! describes whatever anyone has published, and a walk of [`OFFICIAL_REPO`]'s
//! GitHub releases, which is official by construction. The registry's entry
//! is preferred for any id it answers, and a walk entry holds any id the
//! registry gave only to another publisher.

use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// The repo the GitHub walk reads. Every plugin it finds is official.
pub const OFFICIAL_REPO: &str = "happyproto/plugins";

/// The account HappyProto publishes its own plugins from.
pub const OFFICIAL_PUBLISHER_DID: &str = "did:plc:mo7vq4jbioxwafy5zkcc3eux";

/// A release entry for the update preview UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseEntry {
    pub version: String,
    pub name: String,
    pub published_at: String,
    pub body: String,
}

/// One plugin in the catalogue.
#[derive(Debug, Clone, Serialize)]
pub struct OfficialPlugin {
    pub id: String,
    /// The registry account that published it; `None` for an entry from the
    /// GitHub walk.
    pub publisher_did: Option<String>,
    /// Published by [`OFFICIAL_PUBLISHER_DID`], or found by the GitHub walk of
    /// [`OFFICIAL_REPO`]. The catalogue lists every publisher; this is what
    /// the dashboard badges.
    pub official: bool,
    pub name: String,
    pub description: Option<String>,
    pub icon_url: Option<String>,
    pub latest_version: String,
    pub manifest_url: String,
    pub wasm_url: String,
    pub releases: Vec<ReleaseEntry>,
    /// The publisher's long-form prose. Empty for a GitHub-sourced entry,
    /// which has no profile record to carry it.
    pub sections: PluginSections,
}

/// The four long-form fields of `at.happyproto.plugin.profile`. CommonMark
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
    pub plugins: HashMap<String, OfficialPlugin>,
    pub last_refreshed_at: Option<String>,
}

pub type SharedRegistry = Arc<RwLock<OfficialRegistryState>>;

/// Raw GitHub release payload (subset of fields we use).
#[derive(Debug, Clone, Deserialize)]
pub struct GithubRelease {
    pub tag_name: String,
    pub name: Option<String>,
    pub published_at: String,
    pub body: Option<String>,
    pub html_url: String,
}

/// Parse a monorepo tag like `steam-v1.2.0` into `("steam", "1.2.0")`.
/// Returns `None` for tags we don't recognize.
pub fn parse_tag(tag: &str) -> Option<(String, Version)> {
    let (id, version) = tag.rsplit_once("-v")?;
    let parsed = Version::parse(version).ok()?;
    Some((id.to_string(), parsed))
}

/// Group releases by plugin id, filter out unparseable tags, sort each
/// group newest-first.
pub fn group_releases(
    releases: Vec<GithubRelease>,
) -> HashMap<String, Vec<(Version, GithubRelease)>> {
    let mut grouped: HashMap<String, Vec<(Version, GithubRelease)>> = HashMap::new();
    for release in releases {
        let Some((id, version)) = parse_tag(&release.tag_name) else {
            continue;
        };
        grouped.entry(id).or_default().push((version, release));
    }
    for entries in grouped.values_mut() {
        entries.sort_by(|a, b| b.0.cmp(&a.0));
    }
    grouped
}

/// Convert a grouped release entry list into serializable `ReleaseEntry`s
/// for the cache / UI. The first entry is the latest.
pub fn to_release_entries(entries: &[(Version, GithubRelease)]) -> Vec<ReleaseEntry> {
    entries
        .iter()
        .map(|(version, release)| ReleaseEntry {
            version: version.to_string(),
            name: release
                .name
                .clone()
                .unwrap_or_else(|| release.tag_name.clone()),
            published_at: release.published_at.clone(),
            body: release.body.clone().unwrap_or_default(),
        })
        .collect()
}

use crate::plugin::loader;

#[derive(Debug, Clone)]
pub struct RegistryConfig {
    /// The registry to read the catalogue from. `None` leaves only the GitHub
    /// fallback, which is what a test wanting deterministic output passes.
    pub registry: Option<super::registry_source::RegistrySource>,
    /// Base URL for the GitHub REST API, e.g. `https://api.github.com`.
    pub api_base: String,
    /// Base URL for release asset downloads, e.g.
    /// `https://github.com/happyproto/plugins/releases/download`.
    pub release_base: String,
    /// The whole catalogue is off: no refresh task, no request to either
    /// source. For an instance with no outbound network.
    pub disabled: bool,
}

impl RegistryConfig {
    /// Apply `PLUGIN_GITHUB_API_BASE`, which redirects the GitHub fallback.
    /// A stack that should reach nothing live points this at a closed port and
    /// sets `PLUGIN_REGISTRY_URL` empty; the catalogue is then empty, which is
    /// what an offline stack should show rather than whatever the internet
    /// happened to answer.
    pub fn with_github_base_from_env(mut self) -> Self {
        let Some(configured) = crate::config::vars::var("PLUGIN_GITHUB_API_BASE") else {
            return self;
        };
        let configured = configured.trim();
        if !configured.is_empty() {
            self.api_base = configured.trim_end_matches('/').to_string();
        }
        self
    }

    /// Apply `PLUGIN_REGISTRY_URL`: a URL points the catalogue at that
    /// registry, an empty value turns the registry off, leaving only the
    /// GitHub fallback, and `off` turns the whole catalogue off, so neither
    /// source is ever asked. Read here rather than in [`Self::production`]
    /// because `production` backs test fixtures, which should not depend on
    /// the environment.
    pub fn with_registry_from_env(mut self) -> Self {
        let Some(configured) = crate::config::vars::var("PLUGIN_REGISTRY_URL") else {
            return self;
        };
        let configured = configured.trim();
        if configured.eq_ignore_ascii_case("off") {
            tracing::info!(
                "official_registry: PLUGIN_REGISTRY_URL=off, the plugin catalogue is off"
            );
            self.disabled = true;
            self.registry = None;
            return self;
        }
        self.registry = if configured.is_empty() {
            tracing::info!(
                "official_registry: PLUGIN_REGISTRY_URL is empty, reading the catalogue \
                 from {OFFICIAL_REPO} only"
            );
            None
        } else {
            Some(super::registry_source::RegistrySource {
                base_url: configured.to_string(),
                ..super::registry_source::RegistrySource::production()
            })
        };
        self
    }

    pub fn production() -> Self {
        Self {
            registry: Some(super::registry_source::RegistrySource::production()),
            api_base: "https://api.github.com".into(),
            release_base: format!("https://github.com/{}/releases/download", OFFICIAL_REPO),
            disabled: false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("GitHub API request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("GitHub API returned status {0}")]
    Status(u16),
    #[error(transparent)]
    Registry(#[from] super::registry_source::SourceError),
    #[error("the plugin catalogue is turned off (PLUGIN_REGISTRY_URL=off)")]
    Disabled,
}

async fn fetch_releases(
    client: &reqwest::Client,
    config: &RegistryConfig,
) -> Result<Vec<GithubRelease>, RegistryError> {
    let url = format!(
        "{}/repos/{}/releases?per_page=100",
        config.api_base, OFFICIAL_REPO
    );
    let response = client
        .get(&url)
        .header("User-Agent", "happyview")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(RegistryError::Status(response.status().as_u16()));
    }
    let releases: Vec<GithubRelease> = response.json().await?;
    Ok(releases)
}

async fn build_official_plugin(
    client: &reqwest::Client,
    config: &RegistryConfig,
    id: &str,
    entries: &[(Version, GithubRelease)],
) -> OfficialPlugin {
    let (latest_version, _) = entries
        .first()
        .map(|(v, _)| (v.to_string(), ()))
        .expect("entries non-empty");
    let tag = format!("{}-v{}", id, latest_version);
    let manifest_url = format!("{}/{}/manifest.json", config.release_base, tag);

    // Try to enrich with manifest fields. On failure, fall back to id.
    let (name, description, icon_url, wasm_url) =
        match loader::fetch_manifest(client, &manifest_url).await {
            Ok(preview) => (
                preview.manifest.name,
                preview.manifest.description,
                preview.manifest.icon_url,
                preview.wasm_url,
            ),
            Err(e) => {
                tracing::warn!(
                    plugin = id,
                    error = %e,
                    "official_registry: failed to fetch manifest, using fallback metadata"
                );
                (
                    id.to_string(),
                    None,
                    None,
                    format!("{}/{}/{}.wasm", config.release_base, tag, id),
                )
            }
        };

    OfficialPlugin {
        id: id.to_string(),
        publisher_did: None,
        official: true,
        name,
        description,
        icon_url,
        latest_version,
        manifest_url,
        wasm_url,
        releases: to_release_entries(entries),
        // A GitHub release carries no profile record, so this source has no
        // prose to offer; only a registry-sourced entry does.
        sections: PluginSections::default(),
    }
}

// The official publisher owns its slugs, and every publisher keeps its own.
// Anyone can publish any slug to the registry, and the cache holds one entry
// per id, so the entry under an installed plugin's id may come from someone
// other than whoever published what is installed. Four places hold the line:
// an official entry, from either source, is never displaced from the cache by
// another publisher's; an on-demand check asks the official sources before
// anyone else; an install records the publisher of the catalogue entry it was
// installed from (`publisher_for_install`); and an update is offered only
// from an entry by that same publisher (`effective_publisher`). An install
// that recorded none falls back to its URL: one from [`OFFICIAL_REPO`]'s
// releases is offered only official entries (`is_official_manifest_url`).

/// Who an entry speaks for: [`OFFICIAL_PUBLISHER_DID`] for an official entry,
/// including a GitHub-walk one that names no DID, and otherwise the registry
/// account that published it.
pub fn effective_publisher(plugin: &OfficialPlugin) -> Option<&str> {
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
/// lend its DID to the official install. A URL under [`OFFICIAL_REPO`]'s
/// releases is the official artifact whichever entry offered it, or none, so
/// it records [`OFFICIAL_PUBLISHER_DID`] outright. `None` for any other URL
/// the entry does not offer, such as one typed by hand.
pub fn publisher_for_install(
    config: &RegistryConfig,
    state: &OfficialRegistryState,
    plugin_id: &str,
    url: &str,
) -> Option<String> {
    if is_official_manifest_url(config, url) {
        return Some(OFFICIAL_PUBLISHER_DID.to_owned());
    }
    state
        .plugins
        .get(plugin_id)
        .filter(|p| p.manifest_url == url)
        .and_then(|p| effective_publisher(p).map(str::to_owned))
}

/// Whether a manifest URL lies under [`OFFICIAL_REPO`]'s releases. Decides
/// the publisher an install of such a URL records, and is the whole rule for
/// an install that recorded none. Scheme, host and path are compared
/// case-insensitively, as GitHub resolves them. A URL whose path the parser
/// rewrites (a `..` or `.` segment in any spelling, a backslash) answers
/// `false` rather than being judged by its normalised form, since that is
/// not the text that was stored. `false` is not the cautious answer for an
/// unrecorded install: it lifts the official-only restriction and lets any
/// publisher's entry under that id be offered. It is accepted because only a
/// hand-typed URL reaches it, and an install from the catalogue records its
/// publisher instead.
pub fn is_official_manifest_url(config: &RegistryConfig, url: &str) -> bool {
    let (Ok(base), Ok(candidate)) = (
        reqwest::Url::parse(&config.release_base),
        reqwest::Url::parse(url),
    ) else {
        return false;
    };
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let raw_path = after_scheme
        .find('/')
        .map_or("", |i| &after_scheme[i..])
        .split(['?', '#'])
        .next()
        .unwrap_or_default();
    if raw_path != candidate.path() {
        return false;
    }
    let base_path = base.path().trim_end_matches('/').to_ascii_lowercase();
    base.scheme() == candidate.scheme()
        && base.host_str().map(str::to_ascii_lowercase)
            == candidate.host_str().map(str::to_ascii_lowercase)
        && base.port_or_known_default() == candidate.port_or_known_default()
        && candidate
            .path()
            .to_ascii_lowercase()
            .strip_prefix(&base_path)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Put a freshly read entry in the cache unless it would displace an official
/// entry with another publisher's, and answer whichever entry the cache now
/// holds for that id.
async fn hold(state: &SharedRegistry, plugin: OfficialPlugin) -> OfficialPlugin {
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

/// Every plugin the GitHub walk finds, each built from its newest release.
async fn github_walk(
    client: &reqwest::Client,
    config: &RegistryConfig,
) -> Result<HashMap<String, OfficialPlugin>, RegistryError> {
    let releases = fetch_releases(client, config).await?;
    let mut plugins = HashMap::new();
    for (id, entries) in group_releases(releases) {
        if entries.is_empty() {
            continue;
        }
        let plugin = build_official_plugin(client, config, &id, &entries).await;
        plugins.insert(id, plugin);
    }
    Ok(plugins)
}

/// Rebuild the cache from both sources. On error, the previous cache is
/// retained and the error is returned.
pub async fn refresh_full(
    client: &reqwest::Client,
    config: &RegistryConfig,
    state: &SharedRegistry,
) -> Result<(), RegistryError> {
    if config.disabled {
        return Ok(());
    }

    // The registry describes whatever anyone published and is preferred for
    // every id it answers. The GitHub walk is read too, because it is official
    // by construction: its entries take any id the registry gave only to
    // another publisher, which covers an official package that is not
    // published to the registry yet, failed to read, or sits past the page.
    let mut plugins: HashMap<String, OfficialPlugin> = HashMap::new();
    if let Some(source) = &config.registry {
        match super::registry_source::fetch_catalogue(client, source).await {
            Ok(listed) => {
                tracing::info!(
                    count = listed.len(),
                    "official_registry: catalogue read from the registry"
                );
                plugins.extend(listed.into_iter().map(|p| (p.id.clone(), p)));
            }
            Err(e) => tracing::warn!(
                error = %e,
                "official_registry: registry unreadable, reading {OFFICIAL_REPO} only"
            ),
        }
    }

    match github_walk(client, config).await {
        Ok(walked) => {
            for (id, plugin) in walked {
                if plugins.get(&id).is_none_or(|existing| !existing.official) {
                    plugins.insert(id, plugin);
                }
            }
        }
        // Nothing to show at all: keep the previous cache rather than empty
        // it.
        Err(e) if plugins.is_empty() => return Err(e),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "official_registry: {OFFICIAL_REPO} unreadable, keeping its cached entries"
            );
            // This refresh cannot see the official entries the walk would
            // have supplied, so the cached ones stay rather than leaving their
            // ids to whoever else published them.
            let guard = state.read().await;
            for (id, cached) in guard.plugins.iter().filter(|(_, p)| p.official) {
                if plugins.get(id).is_none_or(|existing| !existing.official) {
                    plugins.insert(id.clone(), cached.clone());
                }
            }
        }
    }

    let mut guard = state.write().await;
    guard.plugins = plugins;
    guard.last_refreshed_at = Some(crate::db::now_rfc3339());
    Ok(())
}

/// Refresh just one plugin's cache entry. Official sources are asked first:
/// the registry for the official publisher's package, then the GitHub walk,
/// and only then the other publisher the cache names for this id, if any.
/// The entry is removed only when every source was read and none has it.
///
/// A check therefore costs up to three requests rather than one: a
/// `getPackage` for the official publisher, the GitHub releases listing, and
/// a second `getPackage` for another publisher, plus a manifest download when
/// the walk answers. It runs only on an operator's request.
pub async fn refresh_plugin(
    client: &reqwest::Client,
    config: &RegistryConfig,
    state: &SharedRegistry,
    plugin_id: &str,
) -> Result<Option<OfficialPlugin>, RegistryError> {
    if config.disabled {
        return Err(RegistryError::Disabled);
    }

    let other_publisher = state
        .read()
        .await
        .plugins
        .get(plugin_id)
        .and_then(|p| p.publisher_did.clone())
        .filter(|did| did != OFFICIAL_PUBLISHER_DID);
    let mut unread: Option<RegistryError> = None;

    if let Some(source) = &config.registry {
        match super::registry_source::fetch_package(
            client,
            source,
            OFFICIAL_PUBLISHER_DID,
            plugin_id,
        )
        .await
        {
            Ok(Some(plugin)) => return Ok(Some(hold(state, plugin).await)),
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(
                    plugin = plugin_id,
                    error = %e,
                    "official_registry: registry unreadable, falling back to {OFFICIAL_REPO}"
                );
                unread = Some(e.into());
            }
        }
    }

    match fetch_releases(client, config).await {
        Ok(releases) => {
            if let Some(entries) = group_releases(releases).remove(plugin_id) {
                let plugin = build_official_plugin(client, config, plugin_id, &entries).await;
                return Ok(Some(hold(state, plugin).await));
            }
        }
        Err(e) => unread = Some(e),
    }

    if let (Some(source), Some(did)) = (&config.registry, other_publisher) {
        match super::registry_source::fetch_package(client, source, &did, plugin_id).await {
            Ok(Some(plugin)) => return Ok(Some(hold(state, plugin).await)),
            Ok(None) => {}
            Err(e) => unread = Some(e.into()),
        }
    }

    // A source that could not be read may still hold the plugin, so its cached
    // entry stays.
    if let Some(e) = unread {
        return Err(e);
    }
    state.write().await.plugins.remove(plugin_id);
    Ok(None)
}

/// Background task: run `refresh_full` on startup, then every 15 minutes.
/// Spawns nothing, and answers `None`, when the catalogue is off.
pub fn spawn_refresh_task(
    client: reqwest::Client,
    config: RegistryConfig,
    state: SharedRegistry,
) -> Option<tokio::task::JoinHandle<()>> {
    if config.disabled {
        tracing::info!("official_registry: the plugin catalogue is off (PLUGIN_REGISTRY_URL=off)");
        return None;
    }
    Some(tokio::spawn(async move {
        loop {
            match refresh_full(&client, &config, &state).await {
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

    fn make_release(tag: &str) -> GithubRelease {
        GithubRelease {
            tag_name: tag.to_string(),
            name: Some(tag.to_string()),
            published_at: "2026-04-13T00:00:00Z".to_string(),
            body: Some(format!("body for {tag}")),
            html_url: format!("https://example.com/{tag}"),
        }
    }

    #[test]
    fn parse_tag_happy_path() {
        let (id, version) = parse_tag("steam-v1.2.0").unwrap();
        assert_eq!(id, "steam");
        assert_eq!(version, Version::parse("1.2.0").unwrap());
    }

    #[test]
    fn parse_tag_prerelease() {
        let (id, version) = parse_tag("xbox-v2.0.0-beta.1").unwrap();
        assert_eq!(id, "xbox");
        assert_eq!(version, Version::parse("2.0.0-beta.1").unwrap());
    }

    #[test]
    fn parse_tag_rejects_malformed() {
        assert!(parse_tag("not-a-tag").is_none());
        assert!(parse_tag("steam-v").is_none());
        assert!(parse_tag("steam-vNOT_SEMVER").is_none());
    }

    #[test]
    fn group_releases_sorts_newest_first() {
        let releases = vec![
            make_release("steam-v1.0.0"),
            make_release("steam-v1.2.0"),
            make_release("steam-v1.1.0"),
            make_release("xbox-v0.1.0"),
            make_release("garbage-tag"),
        ];
        let grouped = group_releases(releases);
        assert_eq!(grouped.len(), 2);
        let steam = grouped.get("steam").unwrap();
        assert_eq!(steam.len(), 3);
        assert_eq!(steam[0].0.to_string(), "1.2.0");
        assert_eq!(steam[1].0.to_string(), "1.1.0");
        assert_eq!(steam[2].0.to_string(), "1.0.0");
    }

    #[test]
    fn to_release_entries_preserves_order() {
        let grouped = group_releases(vec![
            make_release("steam-v1.1.0"),
            make_release("steam-v1.2.0"),
        ]);
        let entries = to_release_entries(grouped.get("steam").unwrap());
        assert_eq!(entries[0].version, "1.2.0");
        assert_eq!(entries[1].version, "1.1.0");
        assert_eq!(entries[0].body, "body for steam-v1.2.0");
    }

    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn gh_release_json(tag: &str, body: &str) -> serde_json::Value {
        serde_json::json!({
            "tag_name": tag,
            "name": tag,
            "published_at": "2026-04-10T00:00:00Z",
            "body": body,
            "html_url": format!("https://example.com/{tag}"),
        })
    }

    fn manifest_json(id: &str, version: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "name": id.to_string() + " Plugin",
            "version": version,
            "api_version": "2",
            "description": format!("The {id} plugin"),
            "icon_url": format!("https://example.com/{id}.png"),
            "wasm_file": format!("{id}.wasm"),
            "required_secrets": [],
            "auth_type": "oauth2",
        })
    }

    /// The GitHub fallback is redirectable too, so a stack that should reach
    /// nothing live can close both sources. Serial, because it moves process
    /// environment.
    #[test]
    #[serial]
    fn plugin_github_api_base_redirects_the_fallback() {
        // SAFETY: `#[serial]` keeps this off every other env reader.
        unsafe {
            std::env::remove_var("PLUGIN_GITHUB_API_BASE");
        }
        assert_eq!(
            RegistryConfig::production()
                .with_github_base_from_env()
                .api_base,
            "https://api.github.com",
            "unset should leave the default"
        );

        unsafe {
            std::env::set_var("PLUGIN_GITHUB_API_BASE", "  http://127.0.0.1:9/  ");
        }
        assert_eq!(
            RegistryConfig::production()
                .with_github_base_from_env()
                .api_base,
            "http://127.0.0.1:9",
            "a configured base should be used, trimmed of space and trailing slash"
        );

        unsafe {
            std::env::remove_var("PLUGIN_GITHUB_API_BASE");
        }
    }

    /// An operator can point the catalogue at their own registry, or turn it
    /// off entirely and keep only the GitHub fallback. Serial, because it
    /// moves process environment.
    #[test]
    #[serial]
    fn plugin_registry_url_redirects_or_disables_the_registry() {
        // SAFETY: `#[serial]` keeps this off every other env reader.
        unsafe {
            std::env::remove_var("PLUGIN_REGISTRY_URL");
        }
        assert_eq!(
            RegistryConfig::production()
                .with_registry_from_env()
                .registry
                .map(|r| r.base_url)
                .as_deref(),
            Some("https://api.happyproto.at"),
            "unset should leave the default registry"
        );

        unsafe {
            std::env::set_var("PLUGIN_REGISTRY_URL", "  https://registry.example.com  ");
        }
        assert_eq!(
            RegistryConfig::production()
                .with_registry_from_env()
                .registry
                .map(|r| r.base_url)
                .as_deref(),
            Some("https://registry.example.com"),
            "a configured url should be used, trimmed"
        );

        assert!(
            !RegistryConfig::production()
                .with_registry_from_env()
                .disabled,
            "a configured url leaves the catalogue on"
        );

        unsafe {
            std::env::set_var("PLUGIN_REGISTRY_URL", "");
        }
        let empty = RegistryConfig::production().with_registry_from_env();
        assert!(
            empty.registry.is_none(),
            "an empty value should leave only the GitHub fallback"
        );
        assert!(
            !empty.disabled,
            "an empty value keeps the GitHub fallback on"
        );

        for off in ["off", " OFF "] {
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

    #[tokio::test]
    async fn refresh_full_populates_cache_from_mock_github() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/repos/happyproto/plugins/releases"))
            .and(query_param("per_page", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                gh_release_json("steam-v1.2.0", "- steam 1.2.0 notes"),
                gh_release_json("steam-v1.1.0", "- steam 1.1.0 notes"),
                gh_release_json("xbox-v0.1.0", "- xbox 0.1.0 notes"),
                gh_release_json("bogus-tag", "ignored"),
            ])))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/download/steam-v1.2.0/manifest.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(manifest_json("steam", "1.2.0")))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/download/xbox-v0.1.0/manifest.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(manifest_json("xbox", "0.1.0")))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let state: SharedRegistry = Arc::new(RwLock::new(OfficialRegistryState::default()));

        let config = RegistryConfig {
            // The GitHub fallback, deterministically.
            registry: None,
            api_base: server.uri(),
            release_base: format!("{}/download", server.uri()),
            disabled: false,
        };

        refresh_full(&client, &config, &state).await.unwrap();

        let guard = state.read().await;
        assert_eq!(guard.plugins.len(), 2);

        let steam = guard.plugins.get("steam").unwrap();
        assert_eq!(steam.latest_version, "1.2.0");
        assert_eq!(steam.releases.len(), 2);
        assert_eq!(steam.releases[0].version, "1.2.0");
        assert_eq!(steam.releases[1].version, "1.1.0");
        assert_eq!(steam.name, "steam Plugin");
        assert_eq!(steam.description.as_deref(), Some("The steam plugin"));
        assert!(steam.manifest_url.contains("steam-v1.2.0"));
        assert!(steam.wasm_url.ends_with("steam.wasm"));

        assert!(guard.last_refreshed_at.is_some());
    }

    #[tokio::test]
    async fn refresh_full_retains_previous_cache_on_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/happyproto/plugins/releases"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let state: SharedRegistry = Arc::new(RwLock::new(OfficialRegistryState {
            plugins: HashMap::from([(
                "steam".to_string(),
                OfficialPlugin {
                    id: "steam".into(),
                    publisher_did: None,
                    official: true,
                    name: "steam Plugin".into(),
                    description: None,
                    icon_url: None,
                    latest_version: "1.0.0".into(),
                    manifest_url: "https://example.com/m".into(),
                    wasm_url: "https://example.com/w".into(),
                    releases: vec![],
                    sections: Default::default(),
                },
            )]),
            last_refreshed_at: Some("2026-04-12T00:00:00Z".into()),
        }));

        let config = RegistryConfig {
            // The GitHub fallback, deterministically.
            registry: None,
            api_base: server.uri(),
            release_base: format!("{}/download", server.uri()),
            disabled: false,
        };

        let result = refresh_full(&reqwest::Client::new(), &config, &state).await;
        assert!(result.is_err());

        let guard = state.read().await;
        assert_eq!(guard.plugins.len(), 1);
        assert!(guard.plugins.contains_key("steam"));
    }

    /// A wiremock that fails verification on any request, as both sources.
    async fn unreachable_sources(disabled: bool) -> (MockServer, RegistryConfig) {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let config = RegistryConfig {
            registry: Some(super::super::registry_source::RegistrySource {
                base_url: server.uri(),
                limit: 100,
            }),
            api_base: server.uri(),
            release_base: format!("{}/download", server.uri()),
            disabled,
        };
        (server, config)
    }

    #[tokio::test]
    async fn an_off_catalogue_refreshes_nothing_and_checks_refuse() {
        let (server, off) = unreachable_sources(true).await;
        let state: SharedRegistry = Arc::new(RwLock::new(OfficialRegistryState::default()));
        refresh_full(&reqwest::Client::new(), &off, &state)
            .await
            .unwrap();
        assert!(state.read().await.last_refreshed_at.is_none());
        assert!(matches!(
            refresh_plugin(&reqwest::Client::new(), &off, &state, "lua").await,
            Err(RegistryError::Disabled)
        ));
        server.verify().await;
    }

    const STRANGER: &str = "did:plc:someoneelse";

    fn registry_package(did: &str, slug: &str, latest: &str) -> serde_json::Value {
        serde_json::json!({
            "uri": format!("at://{did}/at.happyproto.plugin.profile/{slug}"),
            "did": did,
            "status": "active",
            "latestVersion": latest,
            "record": { "name": format!("{slug} by {did}") },
        })
    }

    fn registry_release(slug: &str, version: &str) -> serde_json::Value {
        serde_json::json!({
            "status": "active",
            "record": {
                "version": version,
                "artifacts": { "package": { "url": format!("https://elsewhere.example/{slug}-v{version}/{slug}.wasm") } },
            },
        })
    }

    /// A registry where a stranger has published `lua` and the official
    /// publisher nothing.
    async fn stranger_holds_lua(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.searchPackages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "packages": [registry_package(STRANGER, "lua", "9.0.0")]
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.getPackage"))
            .and(query_param("actor", STRANGER))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(registry_package(STRANGER, "lua", "9.0.0")),
            )
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.getPackage"))
            .and(query_param("actor", OFFICIAL_PUBLISHER_DID))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "PackageNotFound", "message": "no such package"
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.listReleases"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "releases": [registry_release("lua", "9.0.0")]
            })))
            .mount(server)
            .await;
    }

    async fn github_lists(server: &MockServer, status: u16, tags: &[&str]) {
        Mock::given(method("GET"))
            .and(path("/repos/happyproto/plugins/releases"))
            .respond_with(
                ResponseTemplate::new(status).set_body_json(serde_json::Value::Array(
                    tags.iter().map(|t| gh_release_json(t, "")).collect(),
                )),
            )
            .mount(server)
            .await;
    }

    fn both_sources(registry: &MockServer, gh: &MockServer) -> RegistryConfig {
        RegistryConfig {
            registry: Some(super::super::registry_source::RegistrySource {
                base_url: registry.uri(),
                limit: 100,
            }),
            api_base: gh.uri(),
            release_base: format!("{}/download", gh.uri()),
            disabled: false,
        }
    }

    fn cached_official_lua() -> SharedRegistry {
        Arc::new(RwLock::new(OfficialRegistryState {
            plugins: HashMap::from([(
                "lua".to_string(),
                OfficialPlugin {
                    id: "lua".into(),
                    publisher_did: None,
                    official: true,
                    name: "lua".into(),
                    description: None,
                    icon_url: None,
                    latest_version: "1.0.0".into(),
                    manifest_url: "https://example.com/m".into(),
                    wasm_url: "https://example.com/w".into(),
                    releases: vec![],
                    sections: Default::default(),
                },
            )]),
            last_refreshed_at: None,
        }))
    }

    /// The official publisher has put nothing on the registry yet, a stranger
    /// has published `lua`, and the GitHub walk still has the official one.
    /// The walk's entry holds the id.
    #[tokio::test]
    async fn the_walk_holds_an_official_id_a_stranger_published_to_the_registry() {
        let registry = MockServer::start().await;
        stranger_holds_lua(&registry).await;
        let gh = MockServer::start().await;
        github_lists(&gh, 200, &["lua-v1.1.0"]).await;

        let state: SharedRegistry = Arc::new(RwLock::new(OfficialRegistryState::default()));
        refresh_full(
            &reqwest::Client::new(),
            &both_sources(&registry, &gh),
            &state,
        )
        .await
        .unwrap();
        let guard = state.read().await;
        let lua = &guard.plugins["lua"];
        assert!(lua.official);
        assert_eq!(lua.latest_version, "1.1.0");
    }

    /// The same, with the walk unreadable this time: the cached official
    /// entry stays rather than the stranger's taking its place.
    #[tokio::test]
    async fn an_unreadable_walk_keeps_the_cached_official_entry() {
        let registry = MockServer::start().await;
        stranger_holds_lua(&registry).await;
        let gh = MockServer::start().await;
        github_lists(&gh, 500, &[]).await;

        let state = cached_official_lua();
        refresh_full(
            &reqwest::Client::new(),
            &both_sources(&registry, &gh),
            &state,
        )
        .await
        .unwrap();
        let guard = state.read().await;
        assert!(guard.plugins["lua"].official);
        assert_eq!(guard.plugins["lua"].latest_version, "1.0.0");
    }

    /// A check on an id the cache holds as official asks the official
    /// publisher, and a registry that answers with someone else's package
    /// regardless does not get it cached or returned.
    #[tokio::test]
    async fn a_check_never_replaces_an_official_entry_with_a_strangers() {
        let registry = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.getPackage"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(registry_package(STRANGER, "lua", "9.0.0")),
            )
            .mount(&registry)
            .await;
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.listReleases"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "releases": [registry_release("lua", "9.0.0")]
            })))
            .mount(&registry)
            .await;
        let gh = MockServer::start().await;
        github_lists(&gh, 200, &[]).await;

        let state = cached_official_lua();
        let answered = refresh_plugin(
            &reqwest::Client::new(),
            &both_sources(&registry, &gh),
            &state,
            "lua",
        )
        .await
        .unwrap()
        .unwrap();
        assert!(answered.official, "the check answered the stranger's entry");
        assert!(state.read().await.plugins["lua"].official);
    }

    /// A check that cannot read a source leaves the cached entry alone: the
    /// unread source may still hold it.
    #[tokio::test]
    async fn a_check_that_cannot_read_the_registry_keeps_the_cached_entry() {
        let registry = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(503))
            .mount(&registry)
            .await;
        let gh = MockServer::start().await;
        github_lists(&gh, 200, &[]).await;

        let state = cached_official_lua();
        let result = refresh_plugin(
            &reqwest::Client::new(),
            &both_sources(&registry, &gh),
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
    fn an_official_manifest_url_is_one_under_the_release_base() {
        let config = RegistryConfig::production();
        let official = |url: &str| is_official_manifest_url(&config, url);
        assert!(official(
            "https://github.com/happyproto/plugins/releases/download/lua-v1.0.0/manifest.json"
        ));
        assert!(
            official(
                "HTTPS://GitHub.COM/HappyProto/plugins/releases/download/lua-v1.0.0/manifest.json"
            ),
            "scheme, host and path compare case-insensitively"
        );
        assert!(!official(
            "https://github.com/happyproto/plugins/releases/downloadx/manifest.json"
        ));
        assert!(!official(
            "https://github.com/happyproto/plugins/releases/download/../../../evil/plugins/manifest.json"
        ));
        assert!(!official(
            "https://github.com/happyproto/plugins/releases/download/%2E%2E/x/manifest.json"
        ));
        // Each of these normalises back under the base, so only refusing a
        // path the parser rewrote rejects them.
        assert!(!official(
            "https://github.com/happyproto/plugins/releases/download/a/../b/manifest.json"
        ));
        assert!(!official(
            "https://github.com/happyproto/plugins/releases/download/a/%2e%2E/b/manifest.json"
        ));
        assert!(!official(
            "https://github.com/happyproto/plugins/releases/download/a/.%2e/b/manifest.json"
        ));
        assert!(!official(
            "https://github.com/happyproto/plugins/releases/download/a/%2e./b/manifest.json"
        ));
        assert!(!official(
            "https://github.com/happyproto/plugins/releases/download/a\\..\\b/manifest.json"
        ));
        assert!(!official(
            "https://github.com/happyproto/plugins/releases/download/./a/manifest.json"
        ));
        assert!(!official(
            "https://github.com.evil.example/happyproto/plugins/releases/download/a/manifest.json"
        ));
        assert!(!official(
            "http://github.com/happyproto/plugins/releases/download/a/manifest.json"
        ));
        assert!(!official("https://example.com/manifest.json"));
        assert!(!official("not a url"));
    }

    /// A stranger's package can name the official artifact under another id;
    /// installing `lua` from that URL still records `lua`'s own publisher,
    /// however the cache happens to iterate.
    #[test]
    fn an_install_takes_the_publisher_of_its_own_id_when_two_entries_share_a_url() {
        // Not under the official release base, so the entry decides.
        let url = "https://mirror.example/lua-v1.0.0/manifest.json";
        let official = OfficialPlugin {
            id: "lua".into(),
            publisher_did: None,
            official: true,
            name: "lua".into(),
            description: None,
            icon_url: None,
            latest_version: "1.0.0".into(),
            manifest_url: url.into(),
            wasm_url: url.replace("manifest.json", "lua.wasm"),
            releases: vec![],
            sections: Default::default(),
        };
        // Many impostors, so a lookup that ignored the id would almost surely
        // reach one before `lua` in a HashMap's order.
        for impostors in [1, 8, 64] {
            let mut state = OfficialRegistryState::default();
            state.plugins.insert("lua".into(), official.clone());
            for n in 0..impostors {
                let id = format!("notlua{n}");
                state.plugins.insert(
                    id.clone(),
                    OfficialPlugin {
                        id,
                        publisher_did: Some(STRANGER.into()),
                        official: false,
                        ..official.clone()
                    },
                );
            }
            assert_eq!(
                publisher_for_install(&RegistryConfig::production(), &state, "lua", url).as_deref(),
                Some(OFFICIAL_PUBLISHER_DID)
            );
            assert_eq!(
                publisher_for_install(&RegistryConfig::production(), &state, "notlua0", url)
                    .as_deref(),
                Some(STRANGER)
            );
        }
    }

    /// A stranger's package can name the official artifact under the very id
    /// the official plugin uses, while the walk holds nothing for it. The
    /// install is still the official module and records the official DID.
    #[test]
    fn an_official_artifact_records_the_official_publisher_whoever_offered_it() {
        let config = RegistryConfig::production();
        let url = format!("{}/lua-v1.0.0/manifest.json", config.release_base);
        let mut state = OfficialRegistryState::default();
        state.plugins.insert(
            "lua".into(),
            OfficialPlugin {
                id: "lua".into(),
                publisher_did: Some(STRANGER.into()),
                official: false,
                name: "lua".into(),
                description: None,
                icon_url: None,
                latest_version: "1.0.0".into(),
                manifest_url: url.clone(),
                wasm_url: url.replace("manifest.json", "lua.wasm"),
                releases: vec![],
                sections: Default::default(),
            },
        );
        assert_eq!(
            publisher_for_install(&config, &state, "lua", &url).as_deref(),
            Some(OFFICIAL_PUBLISHER_DID)
        );
        assert_eq!(
            publisher_for_install(&config, &OfficialRegistryState::default(), "lua", &url)
                .as_deref(),
            Some(OFFICIAL_PUBLISHER_DID),
            "an official URL typed by hand is still the official artifact"
        );
    }

    #[test]
    fn an_install_url_names_the_publisher_of_the_entry_offering_it() {
        let mut state = OfficialRegistryState::default();
        let walked = OfficialPlugin {
            id: "lua".into(),
            publisher_did: None,
            official: true,
            name: "lua".into(),
            description: None,
            icon_url: None,
            latest_version: "1.0.0".into(),
            manifest_url: "https://a.example/lua/manifest.json".into(),
            wasm_url: "https://a.example/lua/lua.wasm".into(),
            releases: vec![],
            sections: Default::default(),
        };
        let registry = OfficialPlugin {
            id: "extra".into(),
            publisher_did: Some(STRANGER.into()),
            official: false,
            manifest_url: "https://b.example/extra/manifest.json".into(),
            ..walked.clone()
        };
        state.plugins.insert(walked.id.clone(), walked);
        state.plugins.insert(registry.id.clone(), registry);

        assert_eq!(
            publisher_for_install(
                &RegistryConfig::production(),
                &state,
                "lua",
                "https://a.example/lua/manifest.json"
            )
            .as_deref(),
            Some(OFFICIAL_PUBLISHER_DID)
        );
        assert_eq!(
            publisher_for_install(
                &RegistryConfig::production(),
                &state,
                "extra",
                "https://b.example/extra/manifest.json"
            )
            .as_deref(),
            Some(STRANGER)
        );
        assert!(
            publisher_for_install(
                &RegistryConfig::production(),
                &state,
                "lua",
                "https://typed.example/manifest.json"
            )
            .is_none()
        );
    }

    /// An instance with no outbound network sets `PLUGIN_REGISTRY_URL=off`,
    /// and boot must then start no refresh loop and ask neither source
    /// anything. Both sources point at a mock that fails verification on any
    /// request.
    #[tokio::test]
    async fn an_off_catalogue_spawns_no_refresh_task() {
        let (server, off) = unreachable_sources(true).await;
        let state: SharedRegistry = Arc::new(RwLock::new(OfficialRegistryState::default()));

        let handle = spawn_refresh_task(reqwest::Client::new(), off, state.clone());
        assert!(handle.is_none(), "an off catalogue should spawn nothing");

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        server.verify().await;
        assert!(state.read().await.last_refreshed_at.is_none());
    }
}

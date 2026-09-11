//! Cache of plugins discovered from the official `happyview-plugins` repo.

use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub const OFFICIAL_REPO: &str = "gamesgamesgamesgamesgames/happyview-plugins";

/// A release entry for the update preview UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseEntry {
    pub version: String,
    pub name: String,
    pub published_at: String,
    pub body: String,
}

/// One plugin discovered in the official repo.
#[derive(Debug, Clone, Serialize)]
pub struct OfficialPlugin {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub icon_url: Option<String>,
    pub latest_version: String,
    pub manifest_url: String,
    pub wasm_url: String,
    pub releases: Vec<ReleaseEntry>,
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
    /// Base URL for the GitHub REST API, e.g. `https://api.github.com`.
    pub api_base: String,
    /// Base URL for release asset downloads, e.g.
    /// `https://github.com/gamesgamesgamesgamesgames/happyview-plugins/releases/download`.
    pub release_base: String,
}

impl RegistryConfig {
    pub fn production() -> Self {
        Self {
            api_base: "https://api.github.com".into(),
            release_base: format!("https://github.com/{}/releases/download", OFFICIAL_REPO),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("GitHub API request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("GitHub API returned status {0}")]
    Status(u16),
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
        name,
        description,
        icon_url,
        latest_version,
        manifest_url,
        wasm_url,
        releases: to_release_entries(entries),
    }
}

/// Fetch all releases and rebuild the cache atomically. On error, the
/// previous cache is retained and the error is returned.
pub async fn refresh_full(
    client: &reqwest::Client,
    config: &RegistryConfig,
    state: &SharedRegistry,
) -> Result<(), RegistryError> {
    let releases = fetch_releases(client, config).await?;
    let grouped = group_releases(releases);

    let mut plugins = HashMap::new();
    for (id, entries) in grouped {
        if entries.is_empty() {
            continue;
        }
        let plugin = build_official_plugin(client, config, &id, &entries).await;
        plugins.insert(id, plugin);
    }

    let mut guard = state.write().await;
    guard.plugins = plugins;
    guard.last_refreshed_at = Some(crate::db::now_rfc3339());
    Ok(())
}

/// Refresh just one plugin's cache entry from a fresh GitHub fetch.
/// Falls back to removing the entry if the plugin has no releases.
pub async fn refresh_plugin(
    client: &reqwest::Client,
    config: &RegistryConfig,
    state: &SharedRegistry,
    plugin_id: &str,
) -> Result<Option<OfficialPlugin>, RegistryError> {
    let releases = fetch_releases(client, config).await?;
    let mut grouped = group_releases(releases);

    let Some(entries) = grouped.remove(plugin_id) else {
        let mut guard = state.write().await;
        guard.plugins.remove(plugin_id);
        return Ok(None);
    };

    let plugin = build_official_plugin(client, config, plugin_id, &entries).await;
    let mut guard = state.write().await;
    guard.plugins.insert(plugin_id.to_string(), plugin.clone());
    guard.last_refreshed_at = Some(crate::db::now_rfc3339());
    Ok(Some(plugin))
}

/// Background task: run `refresh_full` on startup, then every 15 minutes.
pub fn spawn_refresh_task(client: reqwest::Client, config: RegistryConfig, state: SharedRegistry) {
    tokio::spawn(async move {
        loop {
            match refresh_full(&client, &config, &state).await {
                Ok(()) => tracing::info!("official_registry: cache refreshed"),
                Err(e) => tracing::warn!(error = %e, "official_registry: refresh failed"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(15 * 60)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn refresh_full_populates_cache_from_mock_github() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path(
                "/repos/gamesgamesgamesgamesgames/happyview-plugins/releases",
            ))
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
            api_base: server.uri(),
            release_base: format!("{}/download", server.uri()),
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
            .and(path(
                "/repos/gamesgamesgamesgamesgames/happyview-plugins/releases",
            ))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let state: SharedRegistry = Arc::new(RwLock::new(OfficialRegistryState {
            plugins: HashMap::from([(
                "steam".to_string(),
                OfficialPlugin {
                    id: "steam".into(),
                    name: "steam Plugin".into(),
                    description: None,
                    icon_url: None,
                    latest_version: "1.0.0".into(),
                    manifest_url: "https://example.com/m".into(),
                    wasm_url: "https://example.com/w".into(),
                    releases: vec![],
                },
            )]),
            last_refreshed_at: Some("2026-04-12T00:00:00Z".into()),
        }));

        let config = RegistryConfig {
            api_base: server.uri(),
            release_base: format!("{}/download", server.uri()),
        };

        let result = refresh_full(&reqwest::Client::new(), &config, &state).await;
        assert!(result.is_err());

        let guard = state.read().await;
        assert_eq!(guard.plugins.len(), 1);
        assert!(guard.plugins.contains_key("steam"));
    }
}

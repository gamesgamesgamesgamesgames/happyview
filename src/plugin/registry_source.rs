//! The plugin catalogue, read from an AT Protocol registry.
//!
//! [`official_registry`](super::official_registry) enumerates one GitHub
//! org's releases, which only ever describes plugins that org publishes. A
//! registry is an AppView over `at.happyproto.plugin.*` records, so it
//! describes whatever anyone has published, and it is the surface the
//! HappyProto registry exists to serve.
//!
//! What this module does *not* change is installation. A registry answers
//! where a release's module lives (`artifacts.package.url`), and the existing
//! loader already derives a manifest from a sibling `.wasm` URL, so a
//! registry-sourced entry installs through exactly the path a GitHub-sourced
//! one does. A release that carries only a blob and no URL has no manifest to
//! derive and is skipped here rather than offered and then failing at install;
//! serving those needs a reconstructed manifest, which is its own change.

use serde::Deserialize;

use super::official_registry::{OfficialPlugin, ReleaseEntry};

/// Where to look for packages, and under whose authority.
#[derive(Debug, Clone)]
pub struct RegistrySource {
    /// An instance serving `at.happyproto.registry.*`, without a trailing
    /// slash, e.g. `https://api.happyproto.at`.
    pub base_url: String,
    /// How many packages one refresh will read. The registry pages, and a
    /// refresh that walked every page would make the cost of a cold cache
    /// depend on how big the registry has grown.
    pub limit: u32,
}

impl RegistrySource {
    pub fn production() -> Self {
        Self {
            base_url: "https://api.happyproto.at".into(),
            limit: 100,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("registry request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("registry returned status {status} for {method}")]
    Status { method: &'static str, status: u16 },
}

// ---------------------------------------------------------------------------
// The wire shapes, as the registry's lexicons describe them. Only the fields
// a catalogue entry needs: a package view and a release view both carry more.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SearchPackages {
    #[serde(default)]
    packages: Vec<PackageView>,
}

#[derive(Debug, Deserialize)]
struct PackageView {
    uri: String,
    did: String,
    #[serde(default)]
    record: ProfileRecord,
    #[serde(default, rename = "latestVersion")]
    latest_version: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ProfileRecord {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReleaseView {
    #[serde(default)]
    record: ReleaseRecord,
    #[serde(default, rename = "indexedAt")]
    indexed_at: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ReleaseRecord {
    #[serde(default)]
    artifacts: Artifacts,
}

#[derive(Debug, Default, Deserialize)]
struct Artifacts {
    #[serde(default)]
    package: Option<Artifact>,
    #[serde(default)]
    icon: Option<Artifact>,
}

#[derive(Debug, Deserialize)]
struct Artifact {
    #[serde(default)]
    url: Option<String>,
}

/// The slug a package's profile URI ends with, which is the plugin's id.
///
/// Taken from the URI rather than from a field: a profile record carries no
/// slug of its own, the record key is the slug, and the URI is where the
/// registry puts it.
fn slug_of(uri: &str) -> Option<&str> {
    let slug = uri.rsplit('/').next()?;
    (!slug.is_empty() && slug != uri).then_some(slug)
}

async fn get<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    method: &'static str,
    url: String,
) -> Result<T, SourceError> {
    let response = client.get(&url).send().await?;
    let status = response.status();
    if !status.is_success() {
        return Err(SourceError::Status {
            method,
            status: status.as_u16(),
        });
    }
    Ok(response.json().await?)
}

/// Read the catalogue. Answers one entry per package that has an installable
/// release; a package whose latest release carries no module URL is left out,
/// with a line saying which and why.
pub async fn fetch_catalogue(
    client: &reqwest::Client,
    source: &RegistrySource,
) -> Result<Vec<OfficialPlugin>, SourceError> {
    let base = source.base_url.trim_end_matches('/');
    let listing: SearchPackages = get(
        client,
        "searchPackages",
        format!(
            "{base}/xrpc/at.happyproto.registry.searchPackages?limit={}",
            source.limit
        ),
    )
    .await?;

    let mut plugins = Vec::new();
    for package in listing.packages {
        let Some(slug) = slug_of(&package.uri).map(str::to_owned) else {
            tracing::warn!(uri = %package.uri, "registry_source: package uri has no slug");
            continue;
        };
        // No active release means nothing to install. A package can be indexed
        // before its first release is, and a withdrawn one keeps its profile.
        let Some(version) = package.latest_version.clone() else {
            continue;
        };

        let release: ReleaseView = match get(
            client,
            "getRelease",
            format!(
                "{base}/xrpc/at.happyproto.registry.getRelease?did={}&slug={}&version={}",
                urlencoding::encode(&package.did),
                urlencoding::encode(&slug),
                urlencoding::encode(&version),
            ),
        )
        .await
        {
            Ok(view) => view,
            Err(e) => {
                tracing::warn!(plugin = %slug, error = %e, "registry_source: release unreadable");
                continue;
            }
        };

        let Some(wasm_url) = release
            .record
            .artifacts
            .package
            .as_ref()
            .and_then(|a| a.url.clone())
        else {
            tracing::info!(
                plugin = %slug,
                version = %version,
                "registry_source: skipping a release whose module has no url, \
                 so no manifest can be derived for it"
            );
            continue;
        };

        // The loader derives this from the module's own directory; naming it
        // here keeps the entry self-describing and matches what the GitHub
        // source puts in the same field.
        let manifest_url = match wasm_url.rsplit_once('/') {
            Some((dir, _)) => format!("{dir}/manifest.json"),
            None => continue,
        };

        plugins.push(OfficialPlugin {
            id: slug.clone(),
            name: package.record.name.clone().unwrap_or_else(|| slug.clone()),
            description: package.record.description.clone(),
            icon_url: release
                .record
                .artifacts
                .icon
                .as_ref()
                .and_then(|a| a.url.clone()),
            latest_version: version.clone(),
            manifest_url,
            wasm_url,
            releases: vec![ReleaseEntry {
                version,
                name: package.record.name.clone().unwrap_or_else(|| slug.clone()),
                published_at: release.indexed_at.unwrap_or_default(),
                body: String::new(),
            }],
        });
    }

    Ok(plugins)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slug_is_the_last_segment_of_the_profile_uri() {
        assert_eq!(
            slug_of("at://did:plc:abc/at.happyproto.plugin.profile/happyview-http"),
            Some("happyview-http")
        );
        assert_eq!(slug_of(""), None);
        assert_eq!(slug_of("no-slashes-here"), None);
        assert_eq!(slug_of("at://did:plc:abc/collection/"), None);
    }

    /// The registry's own field names, so a rename on either side is a test
    /// failure rather than a silently empty catalogue.
    #[test]
    fn a_package_view_deserialises_from_the_registrys_shape() {
        let listing: SearchPackages = serde_json::from_value(serde_json::json!({
            "packages": [{
                "uri": "at://did:plc:abc/at.happyproto.plugin.profile/demo",
                "cid": "bafy",
                "did": "did:plc:abc",
                "record": { "name": "Demo", "description": "A demo." },
                "status": "active",
                "latestVersion": "1.2.3",
                "indexedAt": "2026-01-01T00:00:00.000000+00:00"
            }]
        }))
        .expect("the listing should parse");

        let package = &listing.packages[0];
        assert_eq!(package.did, "did:plc:abc");
        assert_eq!(package.latest_version.as_deref(), Some("1.2.3"));
        assert_eq!(package.record.name.as_deref(), Some("Demo"));
    }

    /// A package with no active release is not installable, and neither is a
    /// release whose module is a blob with no url: both are left out rather
    /// than offered and then failing at install.
    #[test]
    fn a_release_view_deserialises_and_distinguishes_a_blob_only_module() {
        let with_url: ReleaseView = serde_json::from_value(serde_json::json!({
            "uri": "at://did:plc:abc/at.happyproto.plugin.release/demo:1.0.0",
            "record": {
                "version": "1.0.0",
                "artifacts": { "package": { "url": "https://example.com/d/m.wasm" } }
            },
            "indexedAt": "2026-01-01T00:00:00.000000+00:00"
        }))
        .expect("the release should parse");
        assert_eq!(
            with_url.record.artifacts.package.and_then(|a| a.url),
            Some("https://example.com/d/m.wasm".into())
        );

        let blob_only: ReleaseView = serde_json::from_value(serde_json::json!({
            "record": {
                "version": "1.0.0",
                "artifacts": { "package": { "checksum": "bafkrei" } }
            }
        }))
        .expect("a blob-only release should still parse");
        assert!(
            blob_only
                .record
                .artifacts
                .package
                .and_then(|a| a.url)
                .is_none(),
            "a blob-only module has no url to install from"
        );
    }
}

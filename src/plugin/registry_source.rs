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

use super::official_registry::{OfficialPlugin, PluginSections, ReleaseEntry};

/// How many of a package's releases one refresh reads. The registry clamps a
/// page to 100, so asking for its ceiling costs the same as asking for less.
const RELEASES_PER_PACKAGE: u32 = 100;

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
    #[error("registry answered {method} with {name}: {message}")]
    Xrpc {
        method: &'static str,
        name: String,
        message: String,
    },
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
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ProfileRecord {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    /// The publisher's long-form prose. Absent for a profile written before
    /// the field existed, which is why every field inside it is optional too.
    #[serde(default)]
    sections: PluginSections,
}

#[derive(Debug, Deserialize)]
struct ListReleases {
    #[serde(default)]
    releases: Vec<ReleaseView>,
}

#[derive(Debug, Deserialize)]
struct ReleaseView {
    #[serde(default)]
    record: ReleaseRecord,
    #[serde(default, rename = "indexedAt")]
    indexed_at: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

impl ReleaseView {
    /// A pending release has not been confirmed and a deleted one has been
    /// withdrawn; neither is offered. Absent reads as active, which is what a
    /// registry that predates the field meant.
    fn is_active(&self) -> bool {
        self.status.as_deref().is_none_or(|s| s == "active")
    }
}

#[derive(Debug, Default, Deserialize)]
struct ReleaseRecord {
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    artifacts: Artifacts,
}

#[derive(Debug, Default, Deserialize)]
struct Artifacts {
    #[serde(default)]
    package: Option<Artifact>,
    #[serde(default)]
    icon: Option<Artifact>,
    /// The module's own `manifest.json`, where the publisher uploaded one.
    /// A release whose module is a blob has no sibling file to derive a
    /// manifest from, so this is the only way such a release is installable.
    #[serde(default)]
    manifest: Option<Artifact>,
}

#[derive(Debug, Deserialize)]
struct Artifact {
    #[serde(default)]
    url: Option<String>,
    /// Set when the publisher uploaded the bytes rather than linking them,
    /// which is what the HappyProto registry's own publisher does. The blob
    /// itself is never read here: `getArtifact` serves it, so only its
    /// presence matters.
    #[serde(default)]
    blob: Option<serde_json::Value>,
}

/// Where an artifact's bytes are served from: the URL the publisher gave, or
/// the registry's own copy when they uploaded a blob instead.
///
/// `getArtifact` answers any kind the copy job holds, and it holds every
/// artifact the release declared, so a blob needs no special case beyond
/// naming its kind.
fn artifact_url(
    base: &str,
    did: &str,
    slug: &str,
    version: &str,
    kind: &str,
    artifact: &Artifact,
) -> Option<String> {
    if let Some(url) = &artifact.url {
        return Some(url.clone());
    }
    artifact.blob.as_ref()?;
    Some(format!(
        "{base}/xrpc/at.happyproto.registry.getArtifact?did={}&slug={}&version={}&type={kind}",
        urlencoding::encode(did),
        urlencoding::encode(slug),
        urlencoding::encode(version),
    ))
}

/// The highest version among `releases`, by semver precedence, or `None`
/// when none of them carries a version that parses.
///
/// A prerelease sorts below the same core version without one, so an
/// `alpha` line never shadows the stable release it leads to — the rule the
/// registry's own ordering follows.
fn highest_version(releases: &[ReleaseView]) -> Option<String> {
    releases
        .iter()
        .filter_map(|r| {
            let raw = r.record.version.as_deref()?;
            Some((semver::Version::parse(raw).ok()?, raw.to_owned()))
        })
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, raw)| raw)
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

/// `get`, with the registry's own not-found as `None`.
async fn get_opt<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    method: &'static str,
    url: String,
) -> Result<Option<T>, SourceError> {
    match get_value(client, method, url).await? {
        Ok(value) => decode(method, value).map(Some),
        Err((name, _)) if name == "PackageNotFound" => Ok(None),
        Err((name, message)) => Err(SourceError::Xrpc {
            method,
            name,
            message,
        }),
    }
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

    // Keyed by slug, as the cache is. Anyone can publish any slug, so the
    // official publisher's package wins a collision; otherwise the first
    // listed stays.
    let mut plugins: Vec<OfficialPlugin> = Vec::new();
    for package in listing.packages {
        let Some(plugin) = entry_for(client, base, package).await else {
            continue;
        };
        match plugins.iter_mut().find(|p| p.id == plugin.id) {
            Some(existing) if plugin.official && !existing.official => *existing = plugin,
            Some(_) => tracing::warn!(
                plugin = %plugin.id,
                publisher = ?plugin.publisher_did,
                "registry_source: another publisher already holds this slug; not listed"
            ),
            None => plugins.push(plugin),
        }
    }

    Ok(plugins)
}

/// One package's catalogue entry, or `None` with a line saying why it is
/// not installable.
async fn entry_for(
    client: &reqwest::Client,
    base: &str,
    package: PackageView,
) -> Option<OfficialPlugin> {
    // `getPackage` still answers a withdrawn package, flagged deleted.
    if package.status.as_deref() == Some("deleted") {
        return None;
    }
    let Some(slug) = slug_of(&package.uri).map(str::to_owned) else {
        tracing::warn!(uri = %package.uri, "registry_source: package uri has no slug");
        return None;
    };
    // Every release rather than just the latest: the update preview lists
    // each version between what is installed and what is current, so a
    // catalogue carrying only the newest would show one entry where the
    // GitHub walk showed several. `RELEASES_PER_PACKAGE` is the registry's
    // own ceiling, so this is one request either way.
    let mut listing: ListReleases = match get(
        client,
        "listReleases",
        format!(
            "{base}/xrpc/at.happyproto.registry.listReleases?did={}&slug={}&limit={RELEASES_PER_PACKAGE}",
            urlencoding::encode(&package.did),
            urlencoding::encode(&slug),
        ),
    )
    .await
    {
        Ok(view) => view,
        Err(e) => {
            tracing::warn!(plugin = %slug, error = %e, "registry_source: releases unreadable");
            return None;
        }
    };
    listing.releases.retain(ReleaseView::is_active);

    // Which release is current is worked out here rather than taken from the
    // package view: `searchPackages` answers no `latestVersion` at all — it
    // skips the per-package lookup that would cost, the same way it skips
    // `handle` — so trusting the field skipped every package the registry
    // served. The releases are already in hand for the update preview, so
    // the highest active one costs nothing extra. A registry that does
    // supply the field is still honoured, as long as it names a release this
    // listing calls active.
    let Some(version) = package
        .latest_version
        .clone()
        .filter(|named| {
            listing
                .releases
                .iter()
                .any(|r| r.record.version.as_deref() == Some(named.as_str()))
        })
        .or_else(|| highest_version(&listing.releases))
    else {
        // No active release means nothing to install. A package can be
        // indexed before its first release is, and a withdrawn one keeps its
        // profile.
        return None;
    };

    // The module comes from the release named current; the others are listed
    // for the preview but are not what gets installed.
    let release = listing
        .releases
        .iter()
        .find(|r| r.record.version.as_deref() == Some(version.as_str()))?;

    let Some(module) = release.record.artifacts.package.as_ref() else {
        tracing::info!(
            plugin = %slug,
            version = %version,
            "registry_source: skipping a release that declares no module"
        );
        return None;
    };
    let wasm_url = artifact_url(base, &package.did, &slug, &version, "package", module)?;

    // A published manifest is the authority. Where the release declares
    // none, the module's own directory answers — but only when the publisher
    // linked the module, because that fallback means "the file beside it".
    // A module served from the registry's copy has no directory of its own:
    // every blob's URL shares one `/xrpc/` path, so the sibling of any of
    // them is the same meaningless `/xrpc/manifest.json`. Such a release is
    // skipped rather than offered with a URL that cannot resolve.
    let manifest_url = match (release.record.artifacts.manifest.as_ref(), &module.url) {
        (Some(a), _) => artifact_url(base, &package.did, &slug, &version, "manifest", a),
        (None, Some(linked)) => linked
            .rsplit_once('/')
            .map(|(dir, _)| format!("{dir}/manifest.json")),
        (None, None) => None,
    };
    let Some(manifest_url) = manifest_url else {
        tracing::info!(
            plugin = %slug,
            version = %version,
            "registry_source: skipping a release whose module is uploaded rather \
             than linked and which publishes no manifest artifact, so nothing \
             names how to load it"
        );
        return None;
    };

    Some(OfficialPlugin {
        id: slug.clone(),
        official: package.did == super::official_registry::OFFICIAL_PUBLISHER_DID,
        publisher_did: Some(package.did.clone()),
        name: package.record.name.clone().unwrap_or_else(|| slug.clone()),
        description: package.record.description.clone(),
        icon_url: release
            .record
            .artifacts
            .icon
            .as_ref()
            .and_then(|a| artifact_url(base, &package.did, &slug, &version, "icon", a)),
        latest_version: version,
        manifest_url,
        wasm_url,
        sections: package.record.sections.clone(),
        releases: listing
            .releases
            .iter()
            .filter_map(|r| {
                Some(ReleaseEntry {
                    version: r.record.version.clone()?,
                    name: format!("{slug}-v{}", r.record.version.as_deref()?),
                    published_at: r.indexed_at.clone().unwrap_or_default(),
                    // Markdown, rendered for an operator deciding whether
                    // to upgrade. Absent is normal: `notes` is optional and
                    // a release published before the field existed has none.
                    body: r.record.notes.clone().unwrap_or_default(),
                })
            })
            .collect(),
    })
}

/// One package, read fresh, for an on-demand update check. `None` when the
/// registry no longer has it or it has no installable release.
pub async fn fetch_package(
    client: &reqwest::Client,
    source: &RegistrySource,
    did: &str,
    slug: &str,
) -> Result<Option<OfficialPlugin>, SourceError> {
    let base = source.base_url.trim_end_matches('/');
    let package: Option<PackageView> = get_opt(
        client,
        "getPackage",
        format!(
            "{base}/xrpc/at.happyproto.registry.getPackage?actor={}&slug={}",
            urlencoding::encode(did),
            urlencoding::encode(slug),
        ),
    )
    .await?;
    Ok(match package {
        Some(package) => entry_for(client, base, package).await,
        None => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const HP: &str = super::super::official_registry::OFFICIAL_PUBLISHER_DID;

    fn source(server: &MockServer) -> RegistrySource {
        RegistrySource {
            base_url: server.uri(),
            limit: 100,
        }
    }

    fn package(did: &str, slug: &str, latest: &str) -> serde_json::Value {
        serde_json::json!({
            "uri": format!("at://{did}/at.happyproto.plugin.profile/{slug}"),
            "did": did,
            "status": "active",
            "latestVersion": latest,
            "record": { "name": format!("{slug} plugin") },
        })
    }

    fn release(slug: &str, version: &str, status: &str) -> serde_json::Value {
        serde_json::json!({
            "uri": format!("at://{HP}/at.happyproto.plugin.release/{slug}:{version}"),
            "status": status,
            "indexedAt": "2026-10-01T00:00:00Z",
            "record": {
                "version": version,
                "artifacts": { "package": { "url": format!("https://example.com/{slug}-v{version}/{slug}.wasm") } },
            },
        })
    }

    async fn mount(server: &MockServer, packages: serde_json::Value, releases: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.searchPackages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "packages": packages })),
            )
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.listReleases"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "releases": releases })),
            )
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn the_catalogue_lists_every_publisher_and_marks_the_official_one() {
        let server = MockServer::start().await;
        mount(
            &server,
            serde_json::json!([
                package(HP, "lua", "1.2.0"),
                package("did:plc:someoneelse", "extra", "9.0.0")
            ]),
            serde_json::json!([
                release("lua", "1.2.0", "active"),
                release("extra", "9.0.0", "active")
            ]),
        )
        .await;
        let mut plugins = fetch_catalogue(&reqwest::Client::new(), &source(&server))
            .await
            .unwrap();
        plugins.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(
            plugins
                .iter()
                .map(|p| (p.id.as_str(), p.official))
                .collect::<Vec<_>>(),
            [("extra", false), ("lua", true)]
        );
        assert_eq!(
            plugins[0].publisher_did.as_deref(),
            Some("did:plc:someoneelse")
        );
    }

    #[tokio::test]
    async fn an_official_package_wins_a_slug_collision() {
        let server = MockServer::start().await;
        // The impostor is listed first, so a first-wins rule would keep it.
        mount(
            &server,
            serde_json::json!([
                package("did:plc:someoneelse", "lua", "9.0.0"),
                package(HP, "lua", "1.2.0")
            ]),
            serde_json::json!([
                release("lua", "9.0.0", "active"),
                release("lua", "1.2.0", "active")
            ]),
        )
        .await;
        let plugins = fetch_catalogue(&reqwest::Client::new(), &source(&server))
            .await
            .unwrap();
        assert_eq!(plugins.len(), 1);
        assert!(plugins[0].official);
        assert_eq!(plugins[0].publisher_did.as_deref(), Some(HP));
    }

    #[tokio::test]
    async fn only_active_releases_are_listed_or_installed() {
        let server = MockServer::start().await;
        mount(
            &server,
            serde_json::json!([package(HP, "lua", "1.2.0")]),
            serde_json::json!([
                release("lua", "1.3.0-beta.1", "pending"),
                release("lua", "1.2.0", "active"),
                release("lua", "1.1.0", "active"),
                release("lua", "1.0.0", "deleted"),
            ]),
        )
        .await;
        let plugins = fetch_catalogue(&reqwest::Client::new(), &source(&server))
            .await
            .unwrap();
        assert_eq!(
            plugins[0]
                .releases
                .iter()
                .map(|r| r.version.as_str())
                .collect::<Vec<_>>(),
            ["1.2.0", "1.1.0"]
        );
        assert_eq!(
            plugins[0].wasm_url,
            "https://example.com/lua-v1.2.0/lua.wasm"
        );
    }

    #[tokio::test]
    async fn a_200_carrying_an_error_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.searchPackages"))
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
    async fn fetch_package_reads_one_package_and_answers_none_when_gone() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.getPackage"))
            .and(query_param("slug", "lua"))
            .respond_with(ResponseTemplate::new(200).set_body_json(package(HP, "lua", "1.2.0")))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.getPackage"))
            .and(query_param("slug", "gone"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "PackageNotFound", "message": "gone"
            })))
            .mount(&server)
            .await;
        mount(
            &server,
            serde_json::json!([]),
            serde_json::json!([release("lua", "1.2.0", "active")]),
        )
        .await;

        let client = reqwest::Client::new();
        let lua = fetch_package(&client, &source(&server), HP, "lua")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lua.latest_version, "1.2.0");
        assert!(lua.official);
        assert!(
            fetch_package(&client, &source(&server), HP, "gone")
                .await
                .unwrap()
                .is_none()
        );
    }

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
                "notes": "## 1.0.0\n\n- first release",
                "artifacts": { "package": { "url": "https://example.com/d/m.wasm" } }
            },
            "indexedAt": "2026-01-01T00:00:00.000000+00:00"
        }))
        .expect("the release should parse");
        assert_eq!(
            with_url.record.notes.as_deref(),
            Some("## 1.0.0\n\n- first release"),
            "notes are markdown and are read verbatim"
        );
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

    /// `searchPackages` answers no `latestVersion` — it skips the per-package
    /// lookup that would cost, exactly as it skips `handle` — so a source
    /// that trusted the field skipped every package the registry served.
    #[test]
    fn the_current_version_is_the_highest_active_release() {
        let releases: Vec<ReleaseView> = serde_json::from_value(serde_json::json!([
            { "record": { "version": "1.0.0" } },
            { "record": { "version": "1.2.0" } },
            { "record": { "version": "1.10.0" } },
            { "record": { "version": "1.3.0" } },
        ]))
        .expect("the releases should parse");

        assert_eq!(
            highest_version(&releases).as_deref(),
            Some("1.10.0"),
            "versions order numerically, not as strings"
        );
        assert_eq!(highest_version(&[]), None, "no releases, no version");
    }

    /// A prerelease never shadows the stable release it leads to, which is
    /// the ordering the registry itself applies.
    #[test]
    fn a_prerelease_sorts_below_the_release_it_leads_to() {
        let mixed: Vec<ReleaseView> = serde_json::from_value(serde_json::json!([
            { "record": { "version": "1.0.0-alpha.2" } },
            { "record": { "version": "1.0.0" } },
            { "record": { "version": "1.0.0-alpha.10" } },
        ]))
        .expect("the releases should parse");
        assert_eq!(highest_version(&mixed).as_deref(), Some("1.0.0"));

        let only_pre: Vec<ReleaseView> = serde_json::from_value(serde_json::json!([
            { "record": { "version": "1.0.0-alpha.2" } },
            { "record": { "version": "1.0.0-alpha.10" } },
        ]))
        .expect("the releases should parse");
        assert_eq!(
            highest_version(&only_pre).as_deref(),
            Some("1.0.0-alpha.10"),
            "alpha.10 is after alpha.2: prerelease identifiers compare numerically"
        );
    }

    /// A blob has no URL of its own, so the registry's own copy is where its
    /// bytes come from. A linked artifact keeps the URL the publisher gave.
    #[test]
    fn an_artifact_resolves_to_its_url_or_to_the_registrys_copy() {
        let linked = Artifact {
            url: Some("https://example.com/d/m.wasm".into()),
            blob: None,
        };
        assert_eq!(
            artifact_url(
                "https://reg.example",
                "did:plc:abc",
                "demo",
                "1.0.0",
                "package",
                &linked
            )
            .as_deref(),
            Some("https://example.com/d/m.wasm"),
        );

        let uploaded = Artifact {
            url: None,
            blob: Some(serde_json::json!({ "$type": "blob" })),
        };
        let url = artifact_url(
            "https://reg.example",
            "did:plc:abc",
            "demo",
            "1.0.0-alpha.1",
            "manifest",
            &uploaded,
        )
        .expect("an uploaded artifact resolves to getArtifact");
        assert!(url.starts_with("https://reg.example/xrpc/at.happyproto.registry.getArtifact?"));
        assert!(url.contains("type=manifest"));
        assert!(
            url.contains("version=1.0.0-alpha.1"),
            "the version is carried as given: {url}"
        );

        let neither = Artifact {
            url: None,
            blob: None,
        };
        assert_eq!(
            artifact_url(
                "https://reg.example",
                "did:plc:abc",
                "demo",
                "1.0.0",
                "package",
                &neither
            ),
            None,
            "an artifact naming no bytes resolves to nothing"
        );
    }

    /// The sibling-manifest fallback means "the file beside the module", so
    /// it only answers for a module the publisher linked. Every uploaded
    /// module is served from the one `/xrpc/` path, where the sibling of any
    /// of them is the same meaningless `/xrpc/manifest.json`.
    #[test]
    fn an_uploaded_module_has_no_sibling_manifest_to_fall_back_on() {
        let uploaded = artifact_url(
            "https://reg.example",
            "did:plc:abc",
            "demo",
            "1.0.0",
            "package",
            &Artifact {
                url: None,
                blob: Some(serde_json::json!({ "$type": "blob" })),
            },
        )
        .expect("an uploaded module resolves to getArtifact");
        let sibling = uploaded
            .rsplit_once('/')
            .map(|(dir, _)| format!("{dir}/manifest.json"));
        assert_eq!(
            sibling.as_deref(),
            Some("https://reg.example/xrpc/manifest.json"),
            "which names no plugin at all, and is why the fallback is not taken"
        );
    }

    /// The prose a detail page shows. Every field is optional, and a profile
    /// written before `sections` existed parses to an empty set rather than
    /// failing the whole catalogue.
    #[test]
    fn a_profiles_sections_survive_the_parse_and_are_each_optional() {
        let listing: SearchPackages = serde_json::from_value(serde_json::json!({
            "packages": [{
                "uri": "at://did:plc:abc/at.happyproto.plugin.profile/demo",
                "did": "did:plc:abc",
                "record": {
                    "name": "Demo",
                    "sections": { "description": "# What it does", "security": "Report privately." }
                }
            }, {
                "uri": "at://did:plc:abc/at.happyproto.plugin.profile/bare",
                "did": "did:plc:abc",
                "record": { "name": "Bare" }
            }]
        }))
        .expect("the listing should parse");

        let sections = &listing.packages[0].record.sections;
        assert_eq!(sections.description.as_deref(), Some("# What it does"));
        assert_eq!(sections.security.as_deref(), Some("Report privately."));
        assert_eq!(sections.installation, None);
        assert_eq!(sections.changelog, None);
        assert_eq!(
            listing.packages[1].record.sections,
            PluginSections::default()
        );
    }
}

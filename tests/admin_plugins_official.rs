mod common;

use serde_json::{Value, json};
use serial_test::serial;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::app::TestApp;
use happyview::plugin::official_registry::{OFFICIAL_PUBLISHER_DID, RegistryConfig};

const OFFICIAL: &str = OFFICIAL_PUBLISHER_DID;
const STRANGER: &str = "did:plc:someoneelse";

// ---------------------------------------------------------------------------
// A registry, modelled: projects with their versions and channels, served
// over the read API by a wiremock server. Change the model, then `serve` it
// again.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Version {
    version: String,
    state: &'static str,
    reason: Option<String>,
    availability: &'static str,
    suspect: bool,
    /// Bytes the registry serves for the module instead of the published
    /// ones, for a registry that serves the wrong bytes.
    served_module: Option<Vec<u8>>,
}

struct Project {
    did: String,
    name: String,
    /// Returned by `searchProjects`; a project that isn't is still found by
    /// `getProject`.
    listed: bool,
    /// `listVersions` fails for it.
    versions_fail: bool,
    /// Newest first, as the registry lists them.
    versions: Vec<Version>,
    channels: Vec<(String, String)>,
}

impl Project {
    fn release(&mut self, version: &str) -> &mut Version {
        self.versions.insert(
            0,
            Version {
                version: version.into(),
                state: "active",
                reason: None,
                availability: "available",
                suspect: false,
                served_module: None,
            },
        );
        &mut self.versions[0]
    }

    fn version(&mut self, version: &str) -> &mut Version {
        self.versions
            .iter_mut()
            .find(|v| v.version == version)
            .expect("no such version in the model")
    }

    fn channel(&mut self, channel: &str, version: &str) -> &mut Self {
        self.channels.retain(|(c, _)| c != channel);
        self.channels.push((channel.into(), version.into()));
        self
    }

    /// Publish `version` and point `latest` at it.
    fn publish(&mut self, version: &str) -> &mut Self {
        self.release(version);
        self.channel("latest", version)
    }
}

struct Registry {
    server: MockServer,
    projects: Vec<Project>,
}

/// A distinct module per project version, so each has its own CID.
fn module_bytes(name: &str, version: &str) -> Vec<u8> {
    let tag: u32 = format!("{name}@{version}")
        .bytes()
        .fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    wat::parse_str(format!(
        "(module (memory (export \"memory\") 1) \
         (global i32 (i32.const {})) \
         (func (export \"alloc\") (param i32) (result i32) i32.const 8) \
         (func (export \"dealloc\") (param i32 i32)))",
        tag as i32
    ))
    .unwrap()
}

fn manifest_bytes(name: &str, version: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "id": name, "name": name, "version": version, "api_version": "2",
        "plugin_type": "library", "capabilities": [], "wasm_file": "plugin.wasm",
    }))
    .unwrap()
}

fn cid(bytes: &[u8]) -> String {
    happyview::cid_verify::raw_cid(bytes).unwrap().to_string()
}

fn version_uri(did: &str, name: &str, version: &str) -> String {
    format!("at://{did}/software.release.version/{name}:{version}")
}

fn version_view(project: &Project, v: &Version) -> Value {
    let (did, name) = (project.did.as_str(), project.name.as_str());
    json!({
        "uri": version_uri(did, name, &v.version),
        "cid": format!("bafyversion{}", v.version),
        "did": did,
        "name": name,
        "version": v.version,
        "manifest": {
            "uri": format!("at://{did}/software.release.manifest/{name}:{}", v.version),
            "cid": format!("bafymanifestrecord{}", v.version),
        },
        "artifact": {
            "$type": "at.happyproto.plugin.v1",
            "package": { "$type": "blob", "ref": { "$link": cid(&module_bytes(name, &v.version)) } },
            "manifest": { "$type": "blob", "ref": { "$link": cid(&manifest_bytes(name, &v.version)) } },
            "pluginType": "library",
        },
        "changelog": { "uri": "at://x/site.standard.document/1", "title": v.version, "markdown": format!("- {}", v.version) },
        "availability": v.availability,
        "suspect": v.suspect,
        "lifecycle": {
            "state": v.state,
            "installable": matches!(v.state, "active" | "deprecated"),
            "reason": v.reason,
            "events": [],
        },
        "indexedAt": "2026-10-01T00:00:00Z",
    })
}

fn project_view(project: &Project) -> Value {
    let channels: serde_json::Map<String, Value> = project
        .channels
        .iter()
        .map(|(channel, version)| {
            let v = project
                .versions
                .iter()
                .find(|v| &v.version == version)
                .unwrap();
            (
                channel.clone(),
                json!({
                    "version": version,
                    "uri": version_uri(&project.did, &project.name, version),
                    "cid": format!("bafyversion{version}"),
                    "state": v.state,
                    "availability": v.availability,
                }),
            )
        })
        .collect();
    json!({
        "uri": format!("at://{}/software.release.project/{}", project.did, project.name),
        "cid": "bafyproject",
        "did": project.did,
        "name": project.name,
        "sidecar": { "uri": "at://x/at.happyproto.registry.project/1" },
        "publication": { "uri": "at://x/site.standard.publication/1", "cid": "bafypub", "name": project.name, "url": "https://example.com" },
        "pluginType": "library",
        "keywords": [],
        "authors": [],
        "security": [],
        "channels": channels,
        "lifecycle": { "state": "active" },
        "indexedAt": "2026-10-01T00:00:00Z",
    })
}

fn xrpc(name: &str) -> String {
    format!("/xrpc/at.happyproto.registry.{name}")
}

fn not_found(error: &str) -> ResponseTemplate {
    ResponseTemplate::new(400).set_body_json(json!({ "error": error, "message": "not found" }))
}

impl Registry {
    async fn start() -> Self {
        Self {
            server: MockServer::start().await,
            projects: Vec::new(),
        }
    }

    fn project(&mut self, did: &str, name: &str) -> &mut Project {
        if let Some(i) = self
            .projects
            .iter()
            .position(|p| p.did == did && p.name == name)
        {
            return &mut self.projects[i];
        }
        self.projects.push(Project {
            did: did.into(),
            name: name.into(),
            listed: true,
            versions_fail: false,
            versions: Vec::new(),
            channels: Vec::new(),
        });
        self.projects.last_mut().unwrap()
    }

    fn config(&self) -> RegistryConfig {
        RegistryConfig {
            registry: Some(happyview::plugin::registry_source::RegistrySource {
                base_url: self.server.uri(),
                limit: 100,
            }),
            disabled: false,
        }
    }

    /// Answer the read API from the model as it now stands.
    async fn serve(&self) {
        let server = &self.server;
        server.reset().await;
        let mut listed = Vec::new();
        for p in &self.projects {
            for v in &p.versions {
                let module = module_bytes(&p.name, &v.version);
                let manifest = manifest_bytes(&p.name, &v.version);
                for (cid, bytes) in [
                    (cid(&module), v.served_module.clone().unwrap_or(module)),
                    (cid(&manifest), manifest),
                ] {
                    Mock::given(method("GET"))
                        .and(path(xrpc("getArtifact")))
                        .and(query_param("cid", cid))
                        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
                        .mount(server)
                        .await;
                }
                Mock::given(method("GET"))
                    .and(path(xrpc("getVersion")))
                    .and(query_param("did", p.did.as_str()))
                    .and(query_param("name", p.name.as_str()))
                    .and(query_param("version", v.version.as_str()))
                    .respond_with(ResponseTemplate::new(200).set_body_json(version_view(p, v)))
                    .mount(server)
                    .await;
            }
            for (channel, version) in &p.channels {
                let v = p.versions.iter().find(|v| &v.version == version).unwrap();
                Mock::given(method("GET"))
                    .and(path(xrpc("resolveChannel")))
                    .and(query_param("did", p.did.as_str()))
                    .and(query_param("name", p.name.as_str()))
                    .and(query_param("channel", channel.as_str()))
                    .respond_with(ResponseTemplate::new(200).set_body_json(version_view(p, v)))
                    .mount(server)
                    .await;
            }
            let versions = if p.versions_fail {
                ResponseTemplate::new(503)
            } else {
                ResponseTemplate::new(200).set_body_json(json!({
                    "versions": p.versions.iter().map(|v| version_view(p, v)).collect::<Vec<_>>(),
                }))
            };
            Mock::given(method("GET"))
                .and(path(xrpc("listVersions")))
                .and(query_param("did", p.did.as_str()))
                .and(query_param("name", p.name.as_str()))
                .respond_with(versions)
                .mount(server)
                .await;
            Mock::given(method("GET"))
                .and(path(xrpc("getProject")))
                .and(query_param("actor", p.did.as_str()))
                .and(query_param("name", p.name.as_str()))
                .respond_with(ResponseTemplate::new(200).set_body_json(project_view(p)))
                .mount(server)
                .await;
            if p.listed {
                listed.push(project_view(p));
            }
        }
        Mock::given(method("GET"))
            .and(path(xrpc("searchProjects")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "projects": listed })))
            .mount(server)
            .await;
        for (name, error) in [
            ("getProject", "ProjectNotFound"),
            ("getVersion", "VersionNotFound"),
            ("resolveChannel", "ChannelNotFound"),
            ("listVersions", "ProjectNotFound"),
        ] {
            Mock::given(method("GET"))
                .and(path(xrpc(name)))
                .respond_with(not_found(error))
                .with_priority(10)
                .mount(server)
                .await;
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers over the admin API.
// ---------------------------------------------------------------------------

async fn app_on(registry: &Registry) -> TestApp {
    TestApp::new_with_persisted_plugins(registry.config()).await
}

/// The periodic refresh: the catalogue, and every project an installed
/// plugin follows.
async fn refresh(app: &TestApp) {
    happyview::plugin::official_registry::refresh_for_installs(
        &app.state.http,
        &app.state.official_registry_config,
        &app.state.official_registry,
        &app.state.db,
        app.state.db_backend,
    )
    .await
    .unwrap();
}

async fn install_from_registry(app: &TestApp, registry: Value) -> (u16, Value) {
    app.post_json_status("/admin/plugins", json!({ "registry": registry }))
        .await
}

async fn summary_of(app: &TestApp, id: &str) -> Value {
    app.get_json("/admin/plugins")
        .await
        .get("plugins")
        .and_then(|p| p.as_array())
        .and_then(|p| p.iter().find(|p| p["id"] == id).cloned())
        .expect("the installed plugin should be listed")
}

/// The registry columns of a plugin's row, by name; `None` when there is no
/// row.
async fn row_of(
    app: &TestApp,
    id: &str,
) -> Option<std::collections::HashMap<&'static str, Option<String>>> {
    let sql = happyview::db::adapt_sql(
        "SELECT publisher_did, registry_did, registry_name, registry_channel, version_uri,
                manifest_cid, manifest_blob_cid, package_cid, manifest_url
         FROM happyview_plugins WHERE id = ?",
        app.state.db_backend,
    );
    #[allow(clippy::type_complexity)]
    let row: Option<(
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = happyview::db::query_as(&sql)
        .bind(id)
        .fetch_optional(&app.state.db)
        .await
        .unwrap();
    row.map(|r| {
        [
            ("publisher_did", r.0),
            ("registry_did", r.1),
            ("registry_name", r.2),
            ("registry_channel", r.3),
            ("version_uri", r.4),
            ("manifest_cid", r.5),
            ("manifest_blob_cid", r.6),
            ("package_cid", r.7),
            ("manifest_url", r.8),
        ]
        .into_iter()
        .collect()
    })
}

async fn recorded_publisher(app: &TestApp, id: &str) -> Option<String> {
    row_of(app, id)
        .await
        .and_then(|r| r["publisher_did"].clone())
}

// ---------------------------------------------------------------------------
// The catalogue.
// ---------------------------------------------------------------------------

/// The catalogue lists every project the registry does, official ones
/// badged, with every version and its notes.
#[tokio::test]
#[serial]
async fn the_catalogue_lists_registry_projects() {
    common::require_db!();
    let mut registry = Registry::start().await;
    registry
        .project(OFFICIAL, "steam")
        .publish("1.1.0")
        .publish("1.2.0");
    registry.project(STRANGER, "extra").publish("0.1.0");
    registry.serve().await;
    let app = app_on(&registry).await;
    refresh(&app).await;

    let body = app.get_json("/admin/plugins/official").await;
    let plugins = body["plugins"].as_array().unwrap();
    assert_eq!(plugins.len(), 2);
    let steam = plugins.iter().find(|p| p["id"] == "steam").unwrap();
    assert_eq!(steam["latest_version"], "1.2.0");
    assert_eq!(steam["official"], true);
    assert_eq!(steam["publisher_did"], OFFICIAL);
    assert_eq!(steam["channels"]["latest"]["version"], "1.2.0");
    let versions: Vec<&str> = steam["releases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["version"].as_str().unwrap())
        .collect();
    assert_eq!(versions, vec!["1.2.0", "1.1.0"]);
    assert_eq!(steam["releases"][0]["body"], "- 1.2.0");
    let extra = plugins.iter().find(|p| p["id"] == "extra").unwrap();
    assert_eq!(extra["official"], false);
    assert_eq!(extra["publisher_did"], STRANGER);
    assert!(body["last_refreshed_at"].is_string());
}

// ---------------------------------------------------------------------------
// Installing by registry identity.
// ---------------------------------------------------------------------------

/// A preview resolves the channel and verifies the artifacts; an install
/// records the project, the channel, the version and the CIDs it was
/// verified against, and its publisher from the registry.
#[tokio::test]
#[serial]
async fn a_plugin_installs_by_its_registry_project_and_channel() {
    common::require_db!();
    let mut registry = Registry::start().await;
    registry.project(STRANGER, "extra").publish("1.0.0");
    registry.serve().await;
    let app = app_on(&registry).await;

    let (status, preview) = app
        .post_json_status(
            "/admin/plugins/preview",
            json!({ "registry": { "did": STRANGER, "name": "extra" } }),
        )
        .await;
    assert_eq!(status, 200, "{preview}");
    assert_eq!(preview["id"], "extra");
    assert_eq!(preview["version"], "1.0.0");
    assert_eq!(preview["registry"]["did"], STRANGER);
    assert_eq!(preview["registry"]["name"], "extra");
    assert_eq!(preview["registry"]["channel"], "latest");
    assert_eq!(preview["registry"]["version"], "1.0.0");
    assert_eq!(
        preview["registry"]["version_uri"],
        version_uri(STRANGER, "extra", "1.0.0")
    );
    assert_eq!(preview["registry"]["state"], "active");
    assert!(
        app.state.plugin_registry.get("extra").await.is_none(),
        "a preview installs nothing"
    );

    let (status, body) =
        install_from_registry(&app, json!({ "did": STRANGER, "name": "extra" })).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["version"], "1.0.0");
    assert_eq!(body["registry"]["channel"], "latest");

    let row = row_of(&app, "extra").await.expect("a row is written");
    assert_eq!(row["publisher_did"].as_deref(), Some(STRANGER));
    assert_eq!(row["registry_did"].as_deref(), Some(STRANGER));
    assert_eq!(row["registry_name"].as_deref(), Some("extra"));
    assert_eq!(row["registry_channel"].as_deref(), Some("latest"));
    assert_eq!(
        row["version_uri"].as_deref(),
        Some(version_uri(STRANGER, "extra", "1.0.0").as_str())
    );
    assert_eq!(
        row["manifest_cid"].as_deref(),
        Some("bafymanifestrecord1.0.0")
    );
    assert_eq!(
        row["package_cid"].as_deref(),
        Some(cid(&module_bytes("extra", "1.0.0")).as_str())
    );
    assert_eq!(
        row["manifest_blob_cid"].as_deref(),
        Some(cid(&manifest_bytes("extra", "1.0.0")).as_str())
    );

    let summary = summary_of(&app, "extra").await;
    assert_eq!(summary["registry"]["did"], STRANGER);
    assert_eq!(summary["registry"]["name"], "extra");
    assert_eq!(summary["registry"]["channel"], "latest");
    assert_eq!(summary["registry"]["unresolved"], false);
}

/// Naming a version pins the install to it: no channel is recorded. Naming
/// both, a channel the registry has no track for, or neither project field
/// is refused.
#[tokio::test]
#[serial]
async fn an_install_by_version_is_pinned_and_a_bad_request_is_refused() {
    common::require_db!();
    let mut registry = Registry::start().await;
    registry
        .project(OFFICIAL, "lua")
        .publish("1.0.0")
        .publish("1.1.0");
    registry.serve().await;
    let app = app_on(&registry).await;

    for bad in [
        json!({ "did": OFFICIAL, "name": "lua", "channel": "latest", "version": "1.0.0" }),
        json!({ "did": OFFICIAL, "name": "lua", "channel": "stable" }),
    ] {
        let (status, body) = install_from_registry(&app, bad.clone()).await;
        assert_eq!(status, 400, "{bad} should be refused: {body}");
    }
    let (status, body) = app
        .post_json_status(
            "/admin/plugins",
            json!({ "url": "https://example.com/m.json", "registry": { "did": OFFICIAL, "name": "lua" } }),
        )
        .await;
    assert_eq!(status, 400, "a url and a registry project at once: {body}");
    let (status, body) = app
        .post_json_status(
            "/admin/plugins",
            json!({ "registry": { "did": OFFICIAL, "name": "lua" }, "sha256": "abc" }),
        )
        .await;
    assert_eq!(
        status, 400,
        "a registry install is checked by CID, not sha256: {body}"
    );
    let (status, body) =
        install_from_registry(&app, json!({ "did": OFFICIAL, "name": "nope" })).await;
    assert_eq!(status, 404, "{body}");
    assert!(row_of(&app, "lua").await.is_none());

    let (status, body) = install_from_registry(
        &app,
        json!({ "did": OFFICIAL, "name": "lua", "version": "1.0.0" }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["version"], "1.0.0");
    let row = row_of(&app, "lua").await.unwrap();
    assert_eq!(row["registry_channel"], None);
    assert_eq!(
        row["version_uri"].as_deref(),
        Some(version_uri(OFFICIAL, "lua", "1.0.0").as_str())
    );

    // Pinned, so a newer version on `latest` is not an update for it.
    refresh(&app).await;
    let summary = summary_of(&app, "lua").await;
    assert_eq!(summary["update_available"], false);
    assert!(summary["registry"]["channel"].is_null());
}

/// The registry serving module bytes whose CID is not the one the version
/// names: the install is refused naming both CIDs, and nothing is written.
#[tokio::test]
#[serial]
async fn bytes_that_do_not_match_their_cid_are_refused_and_nothing_is_written() {
    common::require_db!();
    let mut registry = Registry::start().await;
    let tampered = module_bytes("other", "6.6.6");
    registry
        .project(OFFICIAL, "lua")
        .publish("1.0.0")
        .version("1.0.0")
        .served_module = Some(tampered.clone());
    registry.serve().await;
    let app = app_on(&registry).await;
    let expected = cid(&module_bytes("lua", "1.0.0"));
    let actual = cid(&tampered);

    for endpoint in ["/admin/plugins/preview", "/admin/plugins"] {
        let (status, body) = app
            .post_json_status(
                endpoint,
                json!({ "registry": { "did": OFFICIAL, "name": "lua" } }),
            )
            .await;
        assert_eq!(status, 400, "{endpoint}: {body}");
        let message = body["error"].as_str().unwrap();
        assert!(
            message.contains(&expected) && message.contains(&actual),
            "{endpoint} should name both CIDs: {message}"
        );
    }
    assert!(row_of(&app, "lua").await.is_none(), "no row is written");
    assert!(app.state.plugin_registry.get("lua").await.is_none());
}

/// A manifest whose id isn't the project's name is refused: the project
/// name is the plugin's id.
#[tokio::test]
#[serial]
async fn a_manifest_naming_another_plugin_is_refused() {
    common::require_db!();
    let mut registry = Registry::start().await;
    registry.project(STRANGER, "lua").publish("1.0.0");
    registry.serve().await;
    // The registry's `lua` 1.0.0 answers with `notlua`'s manifest and module.
    let mut view = version_view(&registry.projects[0], &registry.projects[0].versions[0]);
    let manifest = manifest_bytes("notlua", "1.0.0");
    let module = module_bytes("notlua", "1.0.0");
    view["artifact"]["manifest"]["ref"]["$link"] = json!(cid(&manifest));
    view["artifact"]["package"]["ref"]["$link"] = json!(cid(&module));
    Mock::given(method("GET"))
        .and(path(xrpc("resolveChannel")))
        .respond_with(ResponseTemplate::new(200).set_body_json(view))
        .with_priority(1)
        .mount(&registry.server)
        .await;
    for (c, bytes) in [(cid(&manifest), manifest), (cid(&module), module)] {
        Mock::given(method("GET"))
            .and(path(xrpc("getArtifact")))
            .and(query_param("cid", c))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
            .mount(&registry.server)
            .await;
    }
    let app = app_on(&registry).await;

    let (status, body) =
        install_from_registry(&app, json!({ "did": STRANGER, "name": "lua" })).await;
    assert_eq!(status, 400, "{body}");
    assert!(body["error"].as_str().unwrap().contains("notlua"), "{body}");
    assert!(row_of(&app, "lua").await.is_none());
    assert!(app.state.plugin_registry.get("notlua").await.is_none());
}

// ---------------------------------------------------------------------------
// Lifecycle.
// ---------------------------------------------------------------------------

/// A channel pointing at a version that isn't installable is refused on
/// install with its state and reason, and once an installed plugin's channel
/// moves to one, no update is offered and the summary says why.
#[tokio::test]
#[serial]
async fn a_channel_on_a_version_that_is_not_installable_is_refused_and_not_offered() {
    common::require_db!();
    let mut registry = Registry::start().await;
    registry.project(OFFICIAL, "lua").publish("1.0.0");
    let http = registry.project(OFFICIAL, "http");
    http.publish("2.0.0");
    let gone = http.version("2.0.0");
    gone.state = "retired";
    gone.reason = Some("remote code execution".into());
    let suspect = registry.project(OFFICIAL, "db");
    suspect.publish("3.0.0").version("3.0.0").suspect = true;
    registry.serve().await;
    let app = app_on(&registry).await;

    let (status, body) =
        install_from_registry(&app, json!({ "did": OFFICIAL, "name": "http" })).await;
    assert_eq!(status, 400, "{body}");
    let message = body["error"].as_str().unwrap();
    assert!(message.contains("retired"), "{message}");
    assert!(message.contains("remote code execution"), "{message}");
    assert!(row_of(&app, "http").await.is_none());

    let (status, body) = app
        .post_json_status(
            "/admin/plugins/preview",
            json!({ "registry": { "did": OFFICIAL, "name": "db", "version": "3.0.0" } }),
        )
        .await;
    assert_eq!(status, 400, "a suspect version is refused: {body}");
    assert!(
        body["error"].as_str().unwrap().contains("suspect"),
        "{body}"
    );

    let (status, body) =
        install_from_registry(&app, json!({ "did": OFFICIAL, "name": "lua" })).await;
    assert_eq!(status, 200, "{body}");

    // `latest` moves to a version the registry no longer holds.
    let lua = registry.project(OFFICIAL, "lua");
    lua.publish("1.1.0").version("1.1.0").state = "unavailable";
    registry.serve().await;
    refresh(&app).await;

    let summary = summary_of(&app, "lua").await;
    assert_eq!(summary["update_available"], false, "{summary}");
    assert_eq!(summary["latest_version"], "1.1.0");
    assert_eq!(summary["latest_version_installable"], false);
    assert_eq!(summary["latest_state"], "unavailable");
    assert_eq!(summary["registry"]["channel_state"], "unavailable");
    assert_eq!(summary["registry"]["installed_state"], "active");
    assert!(summary["pending_releases"].as_array().unwrap().is_empty());

    // Nor can the update be forced.
    let (status, body) = app
        .post_json_status("/admin/plugins/lua/reload", json!({ "registry": {} }))
        .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(summary_of(&app, "lua").await["version"], "1.0.0");
}

/// An installed version its publisher later retires keeps running, and the
/// listing flags it with the reason.
#[tokio::test]
#[serial]
async fn an_installed_version_that_is_retired_stays_loaded_and_is_flagged() {
    common::require_db!();
    let mut registry = Registry::start().await;
    registry.project(OFFICIAL, "lua").publish("1.0.0");
    registry.serve().await;
    let app = app_on(&registry).await;
    let (status, body) =
        install_from_registry(&app, json!({ "did": OFFICIAL, "name": "lua" })).await;
    assert_eq!(status, 200, "{body}");

    let retired = registry.project(OFFICIAL, "lua").version("1.0.0");
    retired.state = "retired";
    retired.reason = Some("leaks tokens".into());
    registry.serve().await;
    refresh(&app).await;

    let summary = summary_of(&app, "lua").await;
    assert_eq!(summary["version"], "1.0.0");
    assert_eq!(summary["registry"]["installed_state"], "retired");
    assert_eq!(summary["registry"]["installed_reason"], "leaks tokens");
    assert!(app.state.plugin_registry.get("lua").await.is_some());

    let (status, body) = app
        .post_json_status("/admin/plugins/lua/check-update", json!({}))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["registry"]["installed_state"], "retired");
}

// ---------------------------------------------------------------------------
// Updating and switching channel.
// ---------------------------------------------------------------------------

/// When the followed channel moves, the update is offered with the releases
/// in between, and a reload installs the channel's new version.
#[tokio::test]
#[serial]
async fn reload_updates_to_the_channels_new_version() {
    common::require_db!();
    let mut registry = Registry::start().await;
    registry.project(OFFICIAL, "lua").publish("1.0.0");
    registry.serve().await;
    let app = app_on(&registry).await;
    let (status, body) =
        install_from_registry(&app, json!({ "did": OFFICIAL, "name": "lua" })).await;
    assert_eq!(status, 200, "{body}");

    let lua = registry.project(OFFICIAL, "lua");
    lua.publish("1.1.0").publish("1.2.0");
    lua.release("1.3.0-beta.1");
    lua.channel("beta", "1.3.0-beta.1");
    registry.serve().await;
    refresh(&app).await;

    let summary = summary_of(&app, "lua").await;
    assert_eq!(summary["update_available"], true, "{summary}");
    assert_eq!(summary["latest_version"], "1.2.0");
    assert_eq!(summary["latest_version_installable"], true);
    assert_eq!(summary["latest_state"], "active");
    let pending: Vec<&str> = summary["pending_releases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["version"].as_str().unwrap())
        .collect();
    assert_eq!(
        pending,
        vec!["1.2.0", "1.1.0"],
        "only releases on latest's track"
    );

    let (status, body) = app
        .post_json_status("/admin/plugins/lua/reload", json!({ "registry": {} }))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["version"], "1.2.0");
    let row = row_of(&app, "lua").await.unwrap();
    assert_eq!(
        row["version_uri"].as_deref(),
        Some(version_uri(OFFICIAL, "lua", "1.2.0").as_str())
    );
    assert_eq!(row["registry_channel"].as_deref(), Some("latest"));
    assert_eq!(
        row["package_cid"].as_deref(),
        Some(cid(&module_bytes("lua", "1.2.0")).as_str())
    );
    let summary = summary_of(&app, "lua").await;
    assert_eq!(summary["version"], "1.2.0");
    assert_eq!(summary["update_available"], false);
}

/// Switching channel is a reload naming the new channel: it installs that
/// channel's version and follows it from then on. Naming a version pins.
#[tokio::test]
#[serial]
async fn a_reload_switches_channel_or_pins_a_version() {
    common::require_db!();
    let mut registry = Registry::start().await;
    let lua = registry.project(OFFICIAL, "lua");
    lua.publish("1.0.0");
    lua.release("1.1.0-beta.1");
    lua.channel("beta", "1.1.0-beta.1");
    registry.serve().await;
    let app = app_on(&registry).await;
    let (status, body) =
        install_from_registry(&app, json!({ "did": OFFICIAL, "name": "lua" })).await;
    assert_eq!(status, 200, "{body}");

    let (status, body) = app
        .post_json_status(
            "/admin/plugins/lua/reload",
            json!({ "registry": { "channel": "beta" } }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["version"], "1.1.0-beta.1");
    assert_eq!(
        row_of(&app, "lua").await.unwrap()["registry_channel"].as_deref(),
        Some("beta")
    );

    // Following beta, its next prerelease is the update.
    let lua = registry.project(OFFICIAL, "lua");
    lua.release("1.1.0-beta.2");
    lua.channel("beta", "1.1.0-beta.2");
    registry.serve().await;
    refresh(&app).await;
    let summary = summary_of(&app, "lua").await;
    assert_eq!(summary["registry"]["channel"], "beta");
    assert_eq!(summary["update_available"], true);
    assert_eq!(summary["latest_version"], "1.1.0-beta.2");

    let (status, body) = app
        .post_json_status(
            "/admin/plugins/lua/reload",
            json!({ "registry": { "version": "1.0.0" } }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["version"], "1.0.0");
    assert_eq!(row_of(&app, "lua").await.unwrap()["registry_channel"], None);
}

/// A plain reload of a registry install fetches its own version again by
/// CID, rather than treating the registry's artifact URL as a manifest URL.
#[tokio::test]
#[serial]
async fn a_plain_reload_of_a_registry_install_refetches_its_version() {
    common::require_db!();
    let mut registry = Registry::start().await;
    registry.project(OFFICIAL, "lua").publish("1.0.0");
    registry.serve().await;
    let app = app_on(&registry).await;
    let (status, body) =
        install_from_registry(&app, json!({ "did": OFFICIAL, "name": "lua" })).await;
    assert_eq!(status, 200, "{body}");
    registry.project(OFFICIAL, "lua").publish("1.1.0");
    registry.serve().await;

    let (status, body) = app
        .post_json_status("/admin/plugins/lua/reload", json!({}))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["version"], "1.0.0");
    assert_eq!(
        row_of(&app, "lua").await.unwrap()["registry_channel"].as_deref(),
        Some("latest")
    );

    let (status, body) = app
        .post_json_status(
            "/admin/plugins/lua/reload",
            json!({ "url": "https://example.com/lua/manifest.json" }),
        )
        .await;
    assert_eq!(
        status, 400,
        "a registry install is reloaded from the registry: {body}"
    );
}

/// Installing over a registry plugin from a plain URL makes it a URL
/// install: its registry identity goes with it.
#[tokio::test]
#[serial]
async fn a_url_install_over_a_registry_install_clears_its_identity() {
    common::require_db!();
    let mut registry = Registry::start().await;
    registry.project(OFFICIAL, "lua").publish("1.0.0");
    registry.serve().await;
    let app = app_on(&registry).await;
    let (status, body) =
        install_from_registry(&app, json!({ "did": OFFICIAL, "name": "lua" })).await;
    assert_eq!(status, 200, "{body}");

    let url = serve_module(&app, "typed-lua", "lua", "2.0.0").await;
    let (status, body) = app
        .post_json_status("/admin/plugins", json!({ "url": url }))
        .await;
    assert_eq!(status, 200, "{body}");
    let row = row_of(&app, "lua").await.unwrap();
    for column in [
        "registry_did",
        "registry_name",
        "registry_channel",
        "version_uri",
        "manifest_cid",
        "package_cid",
    ] {
        assert_eq!(row[column], None, "{column} should be cleared");
    }
    assert!(summary_of(&app, "lua").await["registry"].is_null());
}

/// The on-demand check reads the installed project itself.
#[tokio::test]
#[serial]
async fn check_update_reads_the_installed_project() {
    common::require_db!();
    let mut registry = Registry::start().await;
    registry.project(STRANGER, "extra").publish("1.0.0");
    registry.serve().await;
    let app = app_on(&registry).await;
    let (status, body) =
        install_from_registry(&app, json!({ "did": STRANGER, "name": "extra" })).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(summary_of(&app, "extra").await["update_available"], false);

    registry.project(STRANGER, "extra").publish("1.1.0");
    registry.serve().await;
    let (status, body) = app
        .post_json_status("/admin/plugins/extra/check-update", json!({}))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["update_available"], true);
    assert_eq!(body["latest_version"], "1.1.0");
    assert_eq!(body["registry"]["channel_state"], "active");
    let summary = summary_of(&app, "extra").await;
    assert_eq!(summary["update_available"], true);
}

// ---------------------------------------------------------------------------
// Two publishers, one name. The official one holds the catalogue id, and an
// install stays with the publisher it came from.
// ---------------------------------------------------------------------------

/// The official project wins the id, and is what an official install is
/// offered; the stranger's newer version never is.
#[tokio::test]
#[serial]
async fn a_strangers_project_is_never_the_update_for_an_official_install() {
    common::require_db!();
    let mut registry = Registry::start().await;
    registry.project(STRANGER, "lua").publish("9.0.0");
    registry.project(OFFICIAL, "lua").publish("1.0.0");
    registry.serve().await;
    let app = app_on(&registry).await;
    let (status, body) =
        install_from_registry(&app, json!({ "did": OFFICIAL, "name": "lua" })).await;
    assert_eq!(status, 200, "{body}");

    registry.project(OFFICIAL, "lua").publish("1.1.0");
    registry.serve().await;
    refresh(&app).await;
    assert_eq!(
        app.state.official_registry.read().await.plugins["lua"]
            .publisher_did
            .as_deref(),
        Some(OFFICIAL),
        "the official project wins the id"
    );
    let summary = summary_of(&app, "lua").await;
    assert_eq!(summary["update_available"], true);
    assert_eq!(summary["latest_version"], "1.1.0");

    let (status, body) = app
        .post_json_status("/admin/plugins/lua/reload", json!({ "registry": {} }))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["version"], "1.1.0");
    assert_eq!(
        recorded_publisher(&app, "lua").await.as_deref(),
        Some(OFFICIAL)
    );
}

/// A registry where the stranger's `lua` is listed and the official one is
/// either not listed (past the page) or listed with unreadable versions.
async fn registry_with_only_a_strangers_lua(official_versions_fail: bool) -> Registry {
    let mut registry = Registry::start().await;
    registry.project(STRANGER, "lua").publish("9.0.0");
    let official = registry.project(OFFICIAL, "lua");
    official.publish("1.0.0");
    if official_versions_fail {
        official.versions_fail = true;
    } else {
        official.listed = false;
    }
    registry.serve().await;
    registry
}

/// Refresh, check the cache holds only the stranger's `lua`, and that the
/// official install is offered nothing from it.
async fn refresh_and_expect_no_offer(app: &TestApp) {
    refresh(app).await;
    assert_eq!(
        app.state.official_registry.read().await.plugins["lua"]
            .publisher_did
            .as_deref(),
        Some(STRANGER),
        "the scenario should leave only the stranger's entry"
    );
    let summary = summary_of(app, "lua").await;
    assert_eq!(summary["update_available"], false);
    assert!(summary["latest_version"].is_null());
    assert!(summary["pending_releases"].as_array().unwrap().is_empty());
}

/// The official project sits past the registry's listing, so the catalogue
/// holds only the stranger's `lua`: an official URL install, and an official
/// registry install, are offered nothing from it.
#[tokio::test]
#[serial]
async fn a_strangers_lua_is_not_offered_when_the_official_one_is_not_listed() {
    common::require_db!();
    let registry = registry_with_only_a_strangers_lua(false).await;
    let app = an_official_lua_install(registry.config()).await;
    refresh_and_expect_no_offer(&app).await;
}

/// The official project is listed but its versions cannot be read, so it
/// yields no entry.
#[tokio::test]
#[serial]
async fn a_strangers_lua_is_not_offered_when_the_official_entry_fails_to_build() {
    common::require_db!();
    let registry = registry_with_only_a_strangers_lua(true).await;
    let app = an_official_lua_install(registry.config()).await;
    refresh_and_expect_no_offer(&app).await;
}

/// A restart: the cache starts empty. The on-demand check for a URL install,
/// which reaches the stranger through the cache, offers nothing either.
#[tokio::test]
#[serial]
async fn a_strangers_lua_is_not_offered_on_a_cold_start() {
    common::require_db!();
    let registry = registry_with_only_a_strangers_lua(false).await;
    let app = an_official_lua_install(registry.config()).await;
    app.state.official_registry.write().await.plugins.clear();
    refresh_and_expect_no_offer(&app).await;

    let (status, body) = app
        .post_json_status("/admin/plugins/lua/check-update", json!({}))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["update_available"], false);
    assert!(body["latest_version"].is_null());
}

/// An official registry install, while the catalogue holds the stranger's
/// `lua`: the listing judges it by its own project, which the refresh reads
/// on its own, and never offers the stranger's version. The on-demand check
/// reads the official project too.
#[tokio::test]
#[serial]
async fn an_official_install_is_judged_by_its_own_project_when_a_stranger_holds_the_name() {
    common::require_db!();
    let mut registry = registry_with_only_a_strangers_lua(false).await;
    let app = app_on(&registry).await;
    let (status, body) =
        install_from_registry(&app, json!({ "did": OFFICIAL, "name": "lua" })).await;
    assert_eq!(status, 200, "{body}");

    registry.project(OFFICIAL, "lua").publish("1.1.0");
    registry.serve().await;
    refresh(&app).await;
    assert_eq!(
        app.state.official_registry.read().await.plugins["lua"]
            .publisher_did
            .as_deref(),
        Some(STRANGER),
        "the catalogue lists only the stranger's lua"
    );
    let summary = summary_of(&app, "lua").await;
    assert_eq!(summary["update_available"], true, "{summary}");
    assert_eq!(summary["latest_version"], "1.1.0");

    registry.project(OFFICIAL, "lua").publish("1.2.0");
    registry.serve().await;
    let (status, body) = app
        .post_json_status("/admin/plugins/lua/check-update", json!({}))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["update_available"], true);
    assert_eq!(body["latest_version"], "1.2.0");

    let (status, body) = app
        .post_json_status("/admin/plugins/lua/reload", json!({ "registry": {} }))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["version"], "1.2.0");
}

/// A stranger's install of `lua`, whose name the official project holds in
/// the catalogue, still shows its own version's state: retired here. Whether
/// the registry lists the stranger's project or not, the refresh reads it.
#[tokio::test]
#[serial]
async fn a_shadowed_strangers_install_shows_its_own_lifecycle() {
    common::require_db!();
    for listed in [true, false] {
        let mut registry = Registry::start().await;
        registry.project(STRANGER, "lua").publish("1.0.0");
        registry.project(OFFICIAL, "lua").publish("2.0.0");
        registry.serve().await;
        let app = app_on(&registry).await;
        let (status, body) =
            install_from_registry(&app, json!({ "did": STRANGER, "name": "lua" })).await;
        assert_eq!(status, 200, "{body}");

        let theirs = registry.project(STRANGER, "lua");
        theirs.listed = listed;
        theirs.version("1.0.0").state = "retired";
        registry.serve().await;
        app.state.official_registry.write().await.installed.clear();
        refresh(&app).await;
        assert_eq!(
            app.state.official_registry.read().await.plugins["lua"]
                .publisher_did
                .as_deref(),
            Some(OFFICIAL),
            "the official project holds the name"
        );

        let summary = summary_of(&app, "lua").await;
        assert_eq!(
            summary["registry"]["installed_state"], "retired",
            "listed: {listed}: {summary}"
        );
        assert_eq!(summary["registry"]["channel_state"], "retired");
        assert_eq!(summary["update_available"], false);
    }
}

/// A stranger's install of a name the official publisher also uses is never
/// offered the official project, and its reload reads its own project.
#[tokio::test]
#[serial]
async fn a_strangers_install_is_never_updated_from_the_official_project() {
    common::require_db!();
    let mut registry = Registry::start().await;
    registry.project(STRANGER, "lua").publish("1.0.0");
    registry.project(OFFICIAL, "lua").publish("2.0.0");
    registry.serve().await;
    let app = app_on(&registry).await;
    let (status, body) =
        install_from_registry(&app, json!({ "did": STRANGER, "name": "lua" })).await;
    assert_eq!(status, 200, "{body}");
    refresh(&app).await;

    // Judged by its own project, not the official 2.0.0 holding the name.
    let summary = summary_of(&app, "lua").await;
    assert_eq!(summary["update_available"], false);
    assert_eq!(summary["latest_version"], "1.0.0");

    registry.project(STRANGER, "lua").publish("1.0.1");
    registry.serve().await;
    let (status, body) = app
        .post_json_status("/admin/plugins/lua/reload", json!({ "registry": {} }))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["version"], "1.0.1", "its own publisher's version");
    assert_eq!(
        recorded_publisher(&app, "lua").await.as_deref(),
        Some(STRANGER)
    );
}

/// A reload's project must be the recorded publisher's: a registry install
/// whose recorded publisher is someone else is refused with 409.
#[tokio::test]
#[serial]
async fn a_registry_reload_from_another_publisher_is_refused() {
    common::require_db!();
    let mut registry = Registry::start().await;
    registry.project(STRANGER, "lua").publish("1.0.0");
    registry.serve().await;
    let app = app_on(&registry).await;
    let (status, body) =
        install_from_registry(&app, json!({ "did": STRANGER, "name": "lua" })).await;
    assert_eq!(status, 200, "{body}");
    // The row says the official publisher installed it; the project it
    // follows is the stranger's.
    let sql = happyview::db::adapt_sql(
        "UPDATE happyview_plugins SET publisher_did = ? WHERE id = ?",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(OFFICIAL)
        .bind("lua")
        .execute(&app.state.db)
        .await
        .unwrap();

    let (status, body) = app
        .post_json_status("/admin/plugins/lua/reload", json!({ "registry": {} }))
        .await;
    assert_eq!(status, 409, "{body}");
}

// ---------------------------------------------------------------------------
// URL installs keep their publisher rules.
// ---------------------------------------------------------------------------

/// Serve a library plugin's manifest and module under `/<dir>/` on the app's
/// mock server, and answer the manifest URL.
async fn serve_module(app: &TestApp, dir: &str, id: &str, version: &str) -> String {
    let manifest = json!({
        "id": id, "name": id, "version": version, "api_version": "2",
        "plugin_type": "library", "capabilities": [], "wasm_file": "plugin.wasm",
    });
    Mock::given(method("GET"))
        .and(path(format!("/{dir}/manifest.json")))
        .respond_with(ResponseTemplate::new(200).set_body_json(manifest))
        .mount(&app.mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/{dir}/plugin.wasm")))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(module_bytes(id, version), "application/wasm"),
        )
        .mount(&app.mock_server)
        .await;
    format!("{}/{dir}/manifest.json", app.mock_server.uri())
}

/// A catalogue entry by `publisher` (official when `None`) offering
/// `manifest_url` as its latest version.
fn catalogue_entry(
    id: &str,
    publisher: Option<&str>,
    manifest_url: &str,
    versions: &[&str],
) -> happyview::plugin::official_registry::CatalogueEntry {
    use happyview::plugin::official_registry::{CatalogueEntry, ChannelEntry, ReleaseEntry};
    CatalogueEntry {
        id: id.into(),
        publisher_did: publisher.map(str::to_owned),
        official: publisher.is_none_or(|p| p == OFFICIAL),
        name: id.into(),
        description: None,
        icon_url: None,
        plugin_type: "library".into(),
        channels: [(
            "latest".to_string(),
            ChannelEntry {
                version: versions[0].into(),
                state: "active".into(),
                installable: true,
                availability: "available".into(),
            },
        )]
        .into_iter()
        .collect(),
        latest_version: Some(versions[0].into()),
        lifecycle_state: "active".into(),
        manifest_url: manifest_url.into(),
        wasm_url: manifest_url.replace("manifest.json", "plugin.wasm"),
        releases: versions
            .iter()
            .map(|v| ReleaseEntry {
                version: (*v).into(),
                name: format!("{id}-v{v}"),
                published_at: Some("2026-10-01T00:00:00Z".into()),
                body: None,
                state: "active".into(),
                reason: None,
                suspect: false,
                installable: true,
                availability: "available".into(),
                version_uri: String::new(),
                manifest_cid: String::new(),
                package_cid: String::new(),
                manifest_blob_cid: String::new(),
            })
            .collect(),
        sections: Default::default(),
        banner_url: None,
    }
}

async fn put_in_catalogue(
    app: &TestApp,
    entry: happyview::plugin::official_registry::CatalogueEntry,
) {
    app.state
        .official_registry
        .write()
        .await
        .plugins
        .insert(entry.id.clone(), entry);
}

async fn install(app: &TestApp, url: &str) {
    let (status, body) = app
        .post_json_status("/admin/plugins", json!({ "url": url }))
        .await;
    assert_eq!(status, 200, "{body}");
}

/// The server derives the publisher from the catalogue entry whose manifest
/// URL was installed: a registry entry's account, the official DID for an
/// official entry, and nothing for a URL the catalogue does not offer. A
/// reload to a new URL derives it again.
#[tokio::test]
#[serial]
async fn an_install_records_the_publisher_of_its_catalogue_entry() {
    common::require_db!();
    let app = TestApp::new_with_persisted_plugins(RegistryConfig::production()).await;

    let extra = serve_module(&app, "extra", "extra", "1.0.0").await;
    put_in_catalogue(
        &app,
        catalogue_entry("extra", Some(STRANGER), &extra, &["1.0.0"]),
    )
    .await;
    install(&app, &extra).await;
    assert_eq!(
        recorded_publisher(&app, "extra").await.as_deref(),
        Some(STRANGER)
    );

    let walked = serve_module(&app, "walked", "walked", "1.0.0").await;
    put_in_catalogue(&app, catalogue_entry("walked", None, &walked, &["1.0.0"])).await;
    install(&app, &walked).await;
    assert_eq!(
        recorded_publisher(&app, "walked").await.as_deref(),
        Some(OFFICIAL)
    );

    let typed = serve_module(&app, "typed", "typed", "1.0.0").await;
    install(&app, &typed).await;
    assert!(recorded_publisher(&app, "typed").await.is_none());

    let typed_official = serve_module(&app, "typed-v2", "typed", "1.1.0").await;
    put_in_catalogue(
        &app,
        catalogue_entry("typed", None, &typed_official, &["1.1.0"]),
    )
    .await;
    let (status, body) = app
        .post_json_status(
            "/admin/plugins/typed/reload",
            json!({ "url": typed_official }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        recorded_publisher(&app, "typed").await.as_deref(),
        Some(OFFICIAL)
    );
}

/// A third party's URL install of a name the official publisher also uses is
/// never offered the official release.
#[tokio::test]
#[serial]
async fn a_third_party_install_is_never_offered_the_official_entry() {
    common::require_db!();
    let app = TestApp::new_with_persisted_plugins(RegistryConfig::production()).await;
    let theirs = serve_module(&app, "their-lua", "lua", "1.0.0").await;
    put_in_catalogue(
        &app,
        catalogue_entry("lua", Some(STRANGER), &theirs, &["1.0.0"]),
    )
    .await;
    install(&app, &theirs).await;

    put_in_catalogue(
        &app,
        catalogue_entry(
            "lua",
            None,
            "https://elsewhere.example/lua/manifest.json",
            &["2.0.0"],
        ),
    )
    .await;
    let summary = summary_of(&app, "lua").await;
    assert_eq!(summary["update_available"], false);
    assert!(summary["latest_version"].is_null());
}

/// Install the official `lua` from a URL an official catalogue entry offers,
/// with stranger entries naming the same artifact under other ids, so only
/// the recorded publisher keeps a stranger's release away from it. Answers
/// the app.
async fn an_official_lua_install(config: RegistryConfig) -> TestApp {
    let app = TestApp::new_with_persisted_plugins(config).await;
    let ours = serve_module(&app, "our-lua", "lua", "1.0.0").await;
    put_in_catalogue(&app, catalogue_entry("lua", None, &ours, &["1.0.0"])).await;
    for n in 0..16 {
        put_in_catalogue(
            &app,
            catalogue_entry(&format!("notlua{n}"), Some(STRANGER), &ours, &["1.0.0"]),
        )
        .await;
    }
    install(&app, &ours).await;
    assert_eq!(
        recorded_publisher(&app, "lua").await.as_deref(),
        Some(OFFICIAL)
    );
    app
}

/// A recorded URL install is never re-bound to another publisher by a
/// reload: a URL the catalogue offers under this id from someone else is
/// refused with 409 and the install is left as it was, while the recorded
/// publisher's own new release reloads.
#[tokio::test]
#[serial]
async fn reload_refuses_a_url_from_another_publisher() {
    common::require_db!();
    let app = an_official_lua_install(RegistryConfig::production()).await;

    let theirs = serve_module(&app, "their-lua-v2", "lua", "2.0.0").await;
    put_in_catalogue(
        &app,
        catalogue_entry("lua", Some(STRANGER), &theirs, &["2.0.0"]),
    )
    .await;
    let (status, body) = app
        .post_json_status("/admin/plugins/lua/reload", json!({ "url": theirs }))
        .await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(
        recorded_publisher(&app, "lua").await.as_deref(),
        Some(OFFICIAL)
    );
    assert_eq!(summary_of(&app, "lua").await["version"], "1.0.0");

    // A hand-typed URL is no publisher's offer either.
    let typed = serve_module(&app, "typed-lua", "lua", "2.0.0").await;
    let (status, _) = app
        .post_json_status("/admin/plugins/lua/reload", json!({ "url": typed }))
        .await;
    assert_eq!(status, 409);

    let ours = serve_module(&app, "our-lua-v2", "lua", "2.0.0").await;
    put_in_catalogue(&app, catalogue_entry("lua", None, &ours, &["2.0.0"])).await;
    let (status, body) = app
        .post_json_status("/admin/plugins/lua/reload", json!({ "url": ours }))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(summary_of(&app, "lua").await["version"], "2.0.0");
    assert_eq!(
        recorded_publisher(&app, "lua").await.as_deref(),
        Some(OFFICIAL)
    );
}

//! Whether a newer HappyView is available.
//!
//! HappyView's own releases are published to the HappyProto registry, as the
//! `happyview` project of [`PUBLISHER_DID`]: one `software.release.version`
//! per release and one channel per line. This module reads the channel for
//! the line the running build is on, and answers whether the version it
//! points at is newer.
//!
//! The 2.x plugin catalogue reads GitHub, not the registry, so this module
//! carries its own small registry reader: one XRPC query,
//! `at.happyproto.registry.resolveChannel`.
//!
//! An instance follows its own line only. A stable 2.x instance follows the
//! `v2` maintenance channel (or `latest`, when it holds a 2.x version), a beta
//! follows `beta` and an alpha follows `alpha`, so a stable instance is never
//! offered a prerelease. Semver alone can't cross lines (`3.0.0-beta.1` sorts
//! after `3.0.0-alpha.67` whatever either contains), so another line is only
//! ever mentioned, never offered: a prerelease hears when its version has been
//! released as stable, and a stable instance hears when a new major is out.
//!
//! The check sends nothing about the instance. Its requests carry the
//! publisher's DID, the project name and a channel name, the same for every
//! instance on a line, and a `User-Agent` without the version.
//!
//! `UPDATE_CHECK=off` turns it off, and so does `PLUGIN_REGISTRY_URL=off`
//! (or empty), the switch 3.x uses to cut an instance off from the registry.
//! `PLUGIN_REGISTRY_URL` set to a URL points the check at that registry.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::db::{DatabaseBackend, adapt_sql};

/// The account HappyView's releases are published by.
pub const PUBLISHER_DID: &str = "did:plc:mo7vq4jbioxwafy5zkcc3eux";

/// The registry asked when `PLUGIN_REGISTRY_URL` is unset.
pub const DEFAULT_REGISTRY_URL: &str = "https://api.happyproto.at";

/// The most a registry answer may be. A channel resolves to one version, a
/// few kilobytes; a larger answer is refused rather than held in memory.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// The registry errors that mean the channel has no version to offer.
const NOT_FOUND: &[&str] = &["ChannelNotFound", "VersionNotFound", "ProjectNotFound"];

/// The registry project HappyView's releases are published as.
pub const PROJECT: &str = "happyview";

/// The stable channel. It moves to each new major when that major ships.
pub const LATEST: &str = "latest";

/// How often the registry is asked again.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// The longest the first check waits after startup. A random wait in this
/// range keeps a fleet restarted at once from asking at once.
pub const STARTUP_JITTER: Duration = Duration::from_secs(10 * 60);

/// The general upgrade guide, shown with every offer. A release that needs
/// more names its own guide in its artifact.
pub const UPGRADE_GUIDE_URL: &str = "https://happyview.dev/docs/guides/upgrading";

/// The settings row the last good answer is kept in, so a restart while the
/// registry is down still knows what it knew.
const SETTING_KEY: &str = "update_check_last";

/// The line a build belongs to, from its version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Stable { major: u64 },
    Beta,
    Alpha,
}

impl Line {
    /// The line `version` is on, or `None` for a build that isn't a release:
    /// an unstamped build reports `0.1.0`, and a prerelease that is neither
    /// alpha nor beta follows no channel.
    pub fn of(version: &str) -> Option<Line> {
        let v = semver::Version::parse(version).ok()?;
        if v.major == 0 {
            return None;
        }
        let pre = v.pre.as_str();
        if pre.is_empty() {
            Some(Line::Stable { major: v.major })
        } else if pre.starts_with("beta.") {
            Some(Line::Beta)
        } else if pre.starts_with("alpha.") {
            Some(Line::Alpha)
        } else {
            None
        }
    }

    /// The channel this line follows first. A stable line has a maintenance
    /// channel of its own, `v<major>`; while a major is current, `latest`
    /// serves too (see [`check_once`]).
    pub fn channel(&self) -> String {
        match self {
            Line::Stable { major } => format!("v{major}"),
            Line::Beta => "beta".into(),
            Line::Alpha => "alpha".into(),
        }
    }
}

/// How urgent an offer is.
///
/// Always [`Severity::Routine`] for now. The `software.release` lexicon has
/// no way yet to say a release fixes a vulnerability; when it does,
/// [`severity`] is the one place that reads it, and the dashboard already
/// renders [`Severity::Security`] as the stronger notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Routine,
    Security,
}

/// The severity of moving from `current` to `offered`.
pub fn severity(_current: &str, _offered: &ServerVersionView) -> Severity {
    Severity::Routine
}

/// A version of HappyView the dashboard can point the operator at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Offer {
    pub version: String,
    pub channel: String,
    pub released_at: Option<String>,
    /// The release notes.
    pub release_url: Option<String>,
    /// The guide to follow: the release's own when it names one, otherwise
    /// [`UPGRADE_GUIDE_URL`].
    pub upgrade_guide_url: String,
    pub severity: Severity,
}

/// What the check knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Not checked yet, or no check has succeeded.
    Unknown,
    /// The running version is the newest on its channel.
    Current,
    /// A newer version is on the channel: see `available`.
    Available,
    /// `UPDATE_CHECK=off` or `PLUGIN_REGISTRY_URL=off`.
    Disabled,
    /// Not a release build, so there is no channel to follow.
    Dev,
}

/// The answer `/admin/update-status` serves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub state: State,
    pub current: String,
    /// The channel followed, when there is one.
    pub channel: Option<String>,
    /// A newer version on the instance's own channel.
    pub available: Option<Offer>,
    /// A version on another line worth knowing about: the stable release of
    /// a prerelease's version, or a new major for a stable instance. Never an
    /// upgrade offer, since lines can't be compared by version.
    pub other_line: Option<Offer>,
    /// When the last successful check finished, as RFC 3339.
    pub checked_at: Option<String>,
    /// Why the last check failed, when it did. What was known before stays.
    pub last_error: Option<String>,
    /// A hosting platform runs this instance and upgrades it, so the
    /// dashboard says so rather than pointing at the self-hosting guide.
    pub managed: bool,
}

impl UpdateStatus {
    pub fn initial(current: &str, enabled: bool, managed: bool) -> Self {
        let line = Line::of(current);
        let state = match (&line, enabled) {
            (None, _) => State::Dev,
            (Some(_), false) => State::Disabled,
            (Some(_), true) => State::Unknown,
        };
        Self {
            state,
            current: current.to_owned(),
            channel: line.filter(|_| enabled).map(|l| l.channel()),
            available: None,
            other_line: None,
            checked_at: None,
            last_error: None,
            managed,
        }
    }
}

pub type SharedUpdateStatus = Arc<RwLock<UpdateStatus>>;

impl UpdateStatus {
    pub fn into_shared(self) -> SharedUpdateStatus {
        Arc::new(RwLock::new(self))
    }
}

/// The registry the check reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrySource {
    pub base_url: String,
}

impl RegistrySource {
    pub fn production() -> Self {
        Self {
            base_url: DEFAULT_REGISTRY_URL.to_owned(),
        }
    }

    fn base(&self) -> &str {
        self.base_url.trim_end_matches('/')
    }
}

/// Whether to check, and where.
#[derive(Debug, Clone)]
pub struct UpdateCheckConfig {
    /// `None` when the check is off.
    pub registry: Option<RegistrySource>,
}

impl UpdateCheckConfig {
    /// Off when `UPDATE_CHECK` is `off`, `false`, `0` or `no`, or when
    /// `PLUGIN_REGISTRY_URL` is `off` or empty. Otherwise it reads the registry
    /// `PLUGIN_REGISTRY_URL` names, or [`DEFAULT_REGISTRY_URL`] when it is unset.
    pub fn from_env() -> Self {
        Self::new(
            std::env::var("UPDATE_CHECK").ok().as_deref(),
            std::env::var("PLUGIN_REGISTRY_URL").ok().as_deref(),
        )
    }

    /// The config `UPDATE_CHECK` and `PLUGIN_REGISTRY_URL` give, each `None`
    /// when unset.
    pub fn new(update_check: Option<&str>, registry_url: Option<&str>) -> Self {
        let switched_off = update_check.is_some_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "off" | "false" | "0" | "no"
            )
        });
        let registry = match registry_url.map(str::trim) {
            _ if switched_off => None,
            None => Some(RegistrySource::production()),
            Some(url) if url.is_empty() || url.eq_ignore_ascii_case("off") => None,
            Some(url) => Some(RegistrySource {
                base_url: url.to_owned(),
            }),
        };
        Self { registry }
    }

    pub fn enabled(&self) -> bool {
        self.registry.is_some()
    }
}

/// A HappyView release, as the registry serves one of its versions. Only
/// what the check reads.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerVersionView {
    pub version: String,
    #[serde(default)]
    pub availability: String,
    #[serde(default)]
    pub suspect: bool,
    #[serde(default)]
    pub lifecycle: VersionLifecycle,
    #[serde(default)]
    pub artifact: ServerArtifact,
}

/// A version's lifecycle status. Only what the check reads.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionLifecycle {
    #[serde(default = "yes")]
    pub installable: bool,
}

impl Default for VersionLifecycle {
    fn default() -> Self {
        Self { installable: true }
    }
}

fn yes() -> bool {
    true
}

impl ServerVersionView {
    /// The same rule a plugin install follows: its lifecycle allows it, the
    /// registry has it, and its records weren't changed after publication.
    fn offerable(&self) -> bool {
        self.lifecycle.installable && self.availability == "available" && !self.suspect
    }

    fn offer(&self, channel: &str, current: &str) -> Offer {
        let artifact = &self.artifact;
        Offer {
            version: self.version.clone(),
            channel: channel.to_owned(),
            released_at: artifact.released_at.clone(),
            release_url: artifact.release_url.clone().filter(|u| is_https(u)),
            upgrade_guide_url: artifact
                .upgrade_guide_url
                .clone()
                .filter(|u| is_https(u))
                .unwrap_or_else(|| UPGRADE_GUIDE_URL.to_owned()),
            severity: severity(current, self),
        }
    }
}

/// An `at.happyproto.server.release.v1`, as published.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerArtifact {
    #[serde(default, rename = "$type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub released_at: Option<String>,
    #[serde(default)]
    pub release_url: Option<String>,
    #[serde(default)]
    pub upgrade_guide_url: Option<String>,
}

/// The dashboard renders these as links, so a publisher's value is used only
/// when it is an https URL.
fn is_https(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| u.scheme() == "https" && u.host_str().is_some())
}

fn parse(version: &str) -> Option<semver::Version> {
    semver::Version::parse(version).ok()
}

/// `version` with its prerelease dropped: `3.0.0-beta.4` is `3.0.0`.
fn release_of(v: &semver::Version) -> semver::Version {
    semver::Version::new(v.major, v.minor, v.patch)
}

/// Why a check failed.
#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    #[error("registry request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("the registry's answer is larger than {limit} bytes")]
    TooLarge { limit: usize },
    #[error("the registry answered {status}")]
    Status { status: u16 },
    #[error("the registry answered {name}: {message}")]
    Xrpc { name: String, message: String },
    #[error("the registry's answer could not be read: {0}")]
    Unreadable(String),
}

/// Read a response body of at most `limit` bytes. A declared length past the
/// limit is refused before reading, and an undeclared one as soon as the
/// bytes read pass it, so nothing past the limit is held.
async fn read_capped(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, CheckError> {
    if response
        .content_length()
        .is_some_and(|declared| declared > limit as u64)
    {
        return Err(CheckError::TooLarge { limit });
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > limit {
            return Err(CheckError::TooLarge { limit });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// The version `channel` of HappyView points at, or `None` when the channel,
/// the version it names or the project doesn't exist.
///
/// The registry's queries can answer `{ error, message }` with any status,
/// so a body carrying a string `error` is an error whatever the status.
async fn resolve_channel(
    client: &reqwest::Client,
    source: &RegistrySource,
    channel: &str,
) -> Result<Option<ServerVersionView>, CheckError> {
    let url = format!(
        "{}/xrpc/at.happyproto.registry.resolveChannel?did={}&name={}&channel={}",
        source.base(),
        urlencoding::encode(PUBLISHER_DID),
        urlencoding::encode(PROJECT),
        urlencoding::encode(channel),
    );
    let response = client.get(&url).send().await?;
    let status = response.status();
    let bytes = read_capped(response, MAX_RESPONSE_BYTES).await?;
    let body: Option<serde_json::Value> = serde_json::from_slice(&bytes).ok();
    if let Some(name) = body
        .as_ref()
        .and_then(|b| b.get("error"))
        .and_then(|e| e.as_str())
    {
        if NOT_FOUND.contains(&name) {
            return Ok(None);
        }
        let message = body
            .as_ref()
            .and_then(|b| b.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or_default();
        return Err(CheckError::Xrpc {
            name: name.to_owned(),
            message: message.to_owned(),
        });
    }
    match body {
        Some(value) if status.is_success() => serde_json::from_value(value)
            .map(Some)
            .map_err(|e| CheckError::Unreadable(e.to_string())),
        _ => Err(CheckError::Status {
            status: status.as_u16(),
        }),
    }
}

async fn resolve(
    client: &reqwest::Client,
    source: &RegistrySource,
    channel: &str,
) -> Result<Option<ServerVersionView>, CheckError> {
    Ok(resolve_channel(client, source, channel)
        .await?
        .filter(ServerVersionView::offerable))
}

/// One check: what `current`'s channel offers, and what is worth knowing
/// about the stable line. Starts from `previous` so `managed` and anything
/// not re-read carry over.
pub async fn check_once(
    client: &reqwest::Client,
    source: &RegistrySource,
    previous: &UpdateStatus,
) -> Result<UpdateStatus, CheckError> {
    let current = previous.current.as_str();
    let mut next = UpdateStatus {
        last_error: None,
        checked_at: Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        available: None,
        other_line: None,
        ..previous.clone()
    };
    let (Some(line), Some(running)) = (Line::of(current), parse(current)) else {
        next.state = State::Dev;
        next.channel = None;
        return Ok(next);
    };

    let latest = resolve(client, source, LATEST).await?;
    let latest_version = latest.as_ref().and_then(|v| parse(&v.version));

    // The channel this line follows. A stable major with no maintenance
    // channel of its own is the current one, and `latest` serves it, as long
    // as `latest` is still on that major.
    let own_channel = line.channel();
    let (channel, own) = match &line {
        Line::Stable { major } => match resolve(client, source, &own_channel).await? {
            Some(v) => (own_channel, Some(v)),
            None => {
                let same_major = latest_version.as_ref().is_some_and(|v| v.major == *major);
                (LATEST.to_owned(), latest.clone().filter(|_| same_major))
            }
        },
        Line::Beta | Line::Alpha => {
            let v = resolve(client, source, &own_channel).await?;
            (own_channel, v)
        }
    };

    next.channel = Some(channel.clone());
    next.available = own
        .filter(|v| parse(&v.version).is_some_and(|offered| offered > running))
        .map(|v| v.offer(&channel, current));
    next.state = if next.available.is_some() {
        State::Available
    } else {
        State::Current
    };

    // The stable line, mentioned when it has something for this instance: a
    // prerelease's own version released, or a newer major.
    if let (Some(stable), Some(stable_version)) = (&latest, &latest_version) {
        let worth_mentioning = match &line {
            Line::Stable { major } => stable_version.major > *major,
            Line::Beta | Line::Alpha => {
                stable_version.pre.is_empty() && *stable_version >= release_of(&running)
            }
        };
        if worth_mentioning {
            next.other_line = Some(stable.offer(LATEST, current));
        }
    }
    Ok(next)
}

/// The last good answer stored by an earlier run, if it was for this very
/// version: one stored before an upgrade describes another build.
pub async fn load_stored(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    current: &str,
) -> Option<UpdateStatus> {
    let sql = adapt_sql(
        "SELECT value FROM happyview_instance_settings WHERE key = ?",
        backend,
    );
    let (raw,): (String,) = crate::db::query_as(&sql)
        .bind(SETTING_KEY)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()?;
    serde_json::from_str::<UpdateStatus>(&raw)
        .ok()
        .filter(|s| s.current == current)
}

/// Keep `status` as the last good answer.
pub async fn store(pool: &sqlx::AnyPool, backend: DatabaseBackend, status: &UpdateStatus) {
    let Ok(raw) = serde_json::to_string(status) else {
        return;
    };
    let sql = adapt_sql(
        "INSERT INTO happyview_instance_settings (key, value, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT (key) DO UPDATE SET value = ?, updated_at = ?",
        backend,
    );
    let now = crate::db::now_rfc3339();
    let result = crate::db::query(&sql)
        .bind(SETTING_KEY)
        .bind(&raw)
        .bind(&now)
        .bind(&raw)
        .bind(&now)
        .execute(pool)
        .await;
    if let Err(e) = result {
        tracing::warn!(error = %e, "update_check: could not store the result");
    }
}

/// The client the check sends with. Its `User-Agent` names HappyView but
/// not the version, which is the one thing the requests would otherwise say
/// about the instance.
pub fn client() -> reqwest::Client {
    crate::http_retry::build_http_client("HappyView")
}

/// Background task: wait a random part of [`STARTUP_JITTER`], check, then
/// check every [`CHECK_INTERVAL`]. A failed check keeps what the last good one
/// found and records why. Spawns nothing when the check is off.
pub fn spawn_check_task(
    config: UpdateCheckConfig,
    status: SharedUpdateStatus,
    db: sqlx::AnyPool,
    backend: DatabaseBackend,
) -> Option<tokio::task::JoinHandle<()>> {
    let Some(source) = config.registry else {
        tracing::info!("update_check: off (UPDATE_CHECK=off or PLUGIN_REGISTRY_URL=off)");
        return None;
    };
    Some(tokio::spawn(async move {
        {
            let mut s = status.write().await;
            if s.state == State::Dev {
                return;
            }
            if let Some(stored) = load_stored(&db, backend, &s.current).await {
                *s = UpdateStatus {
                    managed: s.managed,
                    ..stored
                };
            }
        }
        let jitter = rand::random_range(0..STARTUP_JITTER.as_secs());
        tokio::time::sleep(Duration::from_secs(jitter)).await;
        let client = client();
        loop {
            let previous = status.read().await.clone();
            match check_once(&client, &source, &previous).await {
                Ok(next) => {
                    if let Some(offer) = &next.available {
                        tracing::info!(
                            current = %next.current,
                            available = %offer.version,
                            "update_check: a newer HappyView is available"
                        );
                    }
                    store(&db, backend, &next).await;
                    *status.write().await = next;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "update_check: check failed");
                    status.write().await.last_error = Some(e.to_string());
                }
            }
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn lines_come_from_the_version() {
        assert_eq!(Line::of("2.17.3"), Some(Line::Stable { major: 2 }));
        assert_eq!(Line::of("3.0.0-beta.12"), Some(Line::Beta));
        assert_eq!(Line::of("3.0.0-alpha.67"), Some(Line::Alpha));
        assert_eq!(Line::of("0.1.0"), None, "an unstamped build");
        assert_eq!(Line::of("2.17.0-dev.2"), None);
        assert_eq!(Line::of("not a version"), None);
    }

    #[test]
    fn stable_lines_follow_their_maintenance_channel() {
        assert_eq!(Line::Stable { major: 2 }.channel(), "v2");
        assert_eq!(Line::Beta.channel(), "beta");
        assert_eq!(Line::Alpha.channel(), "alpha");
    }

    #[test]
    fn only_https_links_are_kept() {
        assert!(is_https("https://github.com/x/y/releases/tag/v1.0.0"));
        assert!(!is_https("http://example.com"));
        assert!(!is_https("javascript:alert(1)"));
        assert!(!is_https("not a url"));
    }

    #[test]
    fn the_check_is_off_when_either_switch_is() {
        assert_eq!(
            UpdateCheckConfig::new(None, None).registry,
            Some(RegistrySource::production())
        );
        for off in ["off", "false", "0", "no", " OFF "] {
            assert!(!UpdateCheckConfig::new(Some(off), None).enabled(), "{off}");
        }
        assert!(UpdateCheckConfig::new(Some("on"), None).enabled());
        for off in ["off", "", "  ", "OFF"] {
            assert!(
                !UpdateCheckConfig::new(None, Some(off)).enabled(),
                "{off:?}"
            );
        }
        assert!(!UpdateCheckConfig::new(Some("off"), Some("https://registry.example")).enabled());
    }

    #[test]
    fn a_registry_url_points_the_check_elsewhere() {
        assert_eq!(
            UpdateCheckConfig::new(None, Some(" https://registry.example/ ")).registry,
            Some(RegistrySource {
                base_url: "https://registry.example/".into()
            })
        );
    }

    #[test]
    fn initial_state_follows_build_and_switch() {
        assert_eq!(
            UpdateStatus::initial("2.17.3", true, false).state,
            State::Unknown
        );
        assert_eq!(
            UpdateStatus::initial("2.17.3", false, false).state,
            State::Disabled
        );
        assert_eq!(
            UpdateStatus::initial("0.1.0", true, false).state,
            State::Dev
        );
    }

    fn version(v: &str) -> serde_json::Value {
        serde_json::json!({
            "uri": format!("at://{PUBLISHER_DID}/software.release.version/happyview.{v}"),
            "did": PUBLISHER_DID,
            "name": PROJECT,
            "kind": "server",
            "version": v,
            "availability": "available",
            "suspect": false,
            "lifecycle": { "state": "active", "installable": true },
            "artifact": {
                "$type": "at.happyproto.server.release.v1",
                "releasedAt": "2026-11-01T00:00:00Z",
                "releaseUrl": format!("https://github.com/gamesgamesgamesgamesgames/happyview/releases/tag/v{v}"),
            },
        })
    }

    async fn channel(server: &MockServer, name: &str, body: Option<serde_json::Value>) {
        let response = match body {
            Some(b) => ResponseTemplate::new(200).set_body_json(b),
            None => ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "ChannelNotFound", "message": "no such channel"
            })),
        };
        Mock::given(method("GET"))
            .and(path("/xrpc/at.happyproto.registry.resolveChannel"))
            .and(query_param("did", PUBLISHER_DID))
            .and(query_param("name", PROJECT))
            .and(query_param("channel", name))
            .respond_with(response)
            .mount(server)
            .await;
    }

    async fn run(server: &MockServer, current: &str) -> UpdateStatus {
        let source = RegistrySource {
            base_url: server.uri(),
        };
        check_once(
            &reqwest::Client::new(),
            &source,
            &UpdateStatus::initial(current, true, false),
        )
        .await
        .expect("check")
    }

    #[tokio::test]
    async fn a_stable_instance_is_offered_its_maintenance_line() {
        let server = MockServer::start().await;
        channel(&server, "v2", Some(version("2.17.4"))).await;
        channel(&server, LATEST, Some(version("3.0.0"))).await;

        let status = run(&server, "2.17.3").await;
        assert_eq!(status.state, State::Available);
        assert_eq!(status.channel.as_deref(), Some("v2"));
        let offer = status.available.expect("offer");
        assert_eq!(offer.version, "2.17.4");
        assert_eq!(offer.severity, Severity::Routine);
        assert_eq!(offer.upgrade_guide_url, UPGRADE_GUIDE_URL);
        assert_eq!(
            offer.release_url.as_deref(),
            Some("https://github.com/gamesgamesgamesgamesgames/happyview/releases/tag/v2.17.4")
        );
        let other = status.other_line.expect("the new major is mentioned");
        assert_eq!(other.version, "3.0.0");
    }

    #[tokio::test]
    async fn the_current_major_falls_back_to_latest() {
        let server = MockServer::start().await;
        channel(&server, "v3", None).await;
        channel(&server, LATEST, Some(version("3.1.0"))).await;

        let status = run(&server, "3.0.2").await;
        assert_eq!(status.channel.as_deref(), Some(LATEST));
        assert_eq!(status.available.expect("offer").version, "3.1.0");
        assert!(status.other_line.is_none());
    }

    #[tokio::test]
    async fn latest_on_another_major_is_never_offered_as_an_update() {
        let server = MockServer::start().await;
        channel(&server, "v2", None).await;
        channel(&server, LATEST, Some(version("3.0.0"))).await;

        let status = run(&server, "2.17.3").await;
        assert_eq!(status.state, State::Current);
        assert!(status.available.is_none());
        assert_eq!(status.other_line.expect("mentioned").version, "3.0.0");
    }

    #[tokio::test]
    async fn an_up_to_date_instance_is_current() {
        let server = MockServer::start().await;
        channel(&server, "v2", Some(version("2.17.3"))).await;
        channel(&server, LATEST, Some(version("2.17.3"))).await;

        let status = run(&server, "2.17.3").await;
        assert_eq!(status.state, State::Current);
        assert!(status.available.is_none());
        assert!(status.other_line.is_none());
        assert!(status.checked_at.is_some());
    }

    #[tokio::test]
    async fn a_prerelease_follows_its_own_channel_and_hears_of_its_release() {
        let server = MockServer::start().await;
        channel(&server, "beta", Some(version("3.0.0-beta.13"))).await;
        channel(&server, LATEST, Some(version("3.0.0"))).await;

        let status = run(&server, "3.0.0-beta.12").await;
        assert_eq!(status.channel.as_deref(), Some("beta"));
        assert_eq!(status.available.expect("offer").version, "3.0.0-beta.13");
        let other = status.other_line.expect("3.0.0 is out");
        assert_eq!(
            (other.version.as_str(), other.channel.as_str()),
            ("3.0.0", LATEST)
        );
    }

    #[tokio::test]
    async fn a_prerelease_is_not_told_about_an_older_stable() {
        let server = MockServer::start().await;
        channel(&server, "alpha", Some(version("3.0.0-alpha.67"))).await;
        channel(&server, LATEST, Some(version("2.17.4"))).await;

        let status = run(&server, "3.0.0-alpha.67").await;
        assert_eq!(status.state, State::Current);
        assert!(status.other_line.is_none());
    }

    #[tokio::test]
    async fn a_version_not_offerable_is_not_offered() {
        let server = MockServer::start().await;
        let mut suspect = version("2.17.4");
        suspect["suspect"] = true.into();
        channel(&server, "v2", Some(suspect)).await;
        channel(&server, LATEST, None).await;

        let status = run(&server, "2.17.3").await;
        assert_eq!(status.state, State::Current);
        assert!(status.available.is_none());
    }

    #[tokio::test]
    async fn a_non_https_link_from_the_registry_is_dropped() {
        let server = MockServer::start().await;
        let mut v = version("2.17.4");
        v["artifact"]["releaseUrl"] = "javascript:alert(1)".into();
        v["artifact"]["upgradeGuideUrl"] = "http://example.com/guide".into();
        channel(&server, "v2", Some(v)).await;
        channel(&server, LATEST, None).await;

        let offer = run(&server, "2.17.3").await.available.expect("offer");
        assert_eq!(offer.release_url, None);
        assert_eq!(offer.upgrade_guide_url, UPGRADE_GUIDE_URL);
    }

    #[tokio::test]
    async fn a_failed_check_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let source = RegistrySource {
            base_url: server.uri(),
        };
        let result = check_once(
            &reqwest::Client::new(),
            &source,
            &UpdateStatus::initial("2.17.3", true, false),
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn a_not_found_answer_is_no_version_whatever_the_status() {
        for name in NOT_FOUND {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(serde_json::json!({ "error": name, "message": "" })),
                )
                .mount(&server)
                .await;
            let source = RegistrySource {
                base_url: server.uri(),
            };
            let found = resolve_channel(&reqwest::Client::new(), &source, "v2")
                .await
                .expect(name);
            assert!(found.is_none(), "{name}");
        }
    }

    #[tokio::test]
    async fn any_other_registry_error_fails_the_check() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "error": "InternalServerError", "message": "boom" }),
            ))
            .mount(&server)
            .await;
        let source = RegistrySource {
            base_url: server.uri(),
        };
        let err = resolve_channel(&reqwest::Client::new(), &source, "v2")
            .await
            .expect_err("an error");
        assert!(matches!(err, CheckError::Xrpc { ref name, .. } if name == "InternalServerError"));
    }

    #[tokio::test]
    async fn an_oversized_answer_is_refused() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("x".repeat(MAX_RESPONSE_BYTES + 1)),
            )
            .mount(&server)
            .await;
        let source = RegistrySource {
            base_url: server.uri(),
        };
        let err = resolve_channel(&reqwest::Client::new(), &source, "v2")
            .await
            .expect_err("too large");
        assert!(matches!(err, CheckError::TooLarge { .. }));
    }

    #[tokio::test]
    async fn a_trailing_slash_on_the_registry_url_is_ignored() {
        let server = MockServer::start().await;
        channel(&server, "v2", Some(version("2.17.4"))).await;
        let source = RegistrySource {
            base_url: format!("{}/", server.uri()),
        };
        let found = resolve_channel(&reqwest::Client::new(), &source, "v2")
            .await
            .expect("resolved");
        assert_eq!(found.expect("a version").version, "2.17.4");
    }

    #[tokio::test]
    async fn a_dev_build_never_asks() {
        let server = MockServer::start().await;
        let status = run(&server, "0.1.0").await;
        assert_eq!(status.state, State::Dev);
        assert!(
            server
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty()
        );
    }
}

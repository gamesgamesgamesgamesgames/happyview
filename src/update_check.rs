//! Whether a newer HappyView is available.
//!
//! HappyView's releases are published as `software.release` records in the
//! repo of happyview.dev ([`PUBLISHER_DID`]): a version per release, and a
//! channel record per line pointing at the newest version on it. This module
//! reads them straight from the publisher's PDS, with no registry in between:
//! it resolves the DID to its PDS, reads the channel record for the line the
//! running build is on, and follows it to the version it points at.
//!
//! Each line has a channel of its own, so an instance never compares across
//! lines: a stable 2.x follows `v2`, a 3.x beta `v3-beta`, a 3.x alpha
//! `v3-alpha`. A stable instance is never offered a prerelease. Another line
//! is only ever mentioned, never offered, through `latest`: a stable instance
//! hears when a new major is out, and a prerelease hears when its version has
//! been released as stable.
//!
//! The version a channel names is pinned by CID, and the version record read
//! is checked against that CID, recomputed from its content, so the version
//! offered is the one the channel record vouches for.
//!
//! The check sends nothing about the instance. Its requests name the
//! publisher's DID and a channel, the same for every instance on a line, with
//! a `User-Agent` that leaves out the version. `UPDATE_CHECK=off` turns it off.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;

use crate::cid_verify::{CidCheck, verify_record_cid};
use crate::db::{DatabaseBackend, adapt_sql};

/// The account HappyView's releases are published from: happyview.dev.
pub const PUBLISHER_DID: &str = "did:plc:qneu5uamqs6cug7sjobbaexm";

/// The `software.release.project` HappyView's releases belong to.
pub const PROJECT: &str = "happyview";

/// The channel on the newest stable major.
pub const LATEST: &str = "latest";

const CHANNEL: &str = "software.release.channel";
const VERSION: &str = "software.release.version";

/// How often the publisher is asked again.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// The longest the first check waits after startup. A random wait in this
/// range keeps a fleet restarted at once from asking at once.
pub const STARTUP_JITTER: Duration = Duration::from_secs(10 * 60);

/// The general upgrade guide, shown with every offer.
pub const UPGRADE_GUIDE_URL: &str = "https://happyview.dev/docs/guides/upgrading";

/// The largest record response read. A channel or version record is a few
/// hundred bytes.
const MAX_RECORD_BYTES: usize = 64 * 1024;

/// The largest DID document read. One is a few hundred bytes.
const MAX_DID_DOC_BYTES: usize = 64 * 1024;

/// The longest one whole check may take, every request included.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// The settings row the last good answer is kept in, so a restart while the
/// publisher's PDS is down still knows what it knew.
const SETTING_KEY: &str = "update_check_last";

/// The line a build belongs to, from its version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Stable { major: u64 },
    Beta { major: u64 },
    Alpha { major: u64 },
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
        let major = v.major;
        let pre = v.pre.as_str();
        if pre.is_empty() {
            Some(Line::Stable { major })
        } else if pre.starts_with("beta.") {
            Some(Line::Beta { major })
        } else if pre.starts_with("alpha.") {
            Some(Line::Alpha { major })
        } else {
            None
        }
    }

    /// The channel this line follows.
    pub fn channel(&self) -> String {
        match self {
            Line::Stable { major } => format!("v{major}"),
            Line::Beta { major } => format!("v{major}-beta"),
            Line::Alpha { major } => format!("v{major}-alpha"),
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
pub fn severity(_current: &str, _offered: &Release) -> Severity {
    Severity::Routine
}

/// A version of HappyView the dashboard can point the operator at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Offer {
    pub version: String,
    pub channel: String,
    /// The release notes, from the version's `changelog`.
    pub release_url: Option<String>,
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
    /// `UPDATE_CHECK=off`.
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

/// Whether to check, and how to find the publisher.
#[derive(Debug, Clone)]
pub struct UpdateCheckConfig {
    pub enabled: bool,
    /// The PLC directory the publisher's DID is resolved through
    /// (`PLC_URL`).
    pub plc_url: String,
}

impl UpdateCheckConfig {
    /// Off when `UPDATE_CHECK` is `off`, `false`, `0` or `no`. Nothing else
    /// turns it off: in particular `PLUGIN_REGISTRY_URL=off` doesn't, since
    /// the check never reads the plugin registry.
    pub fn from_env(plc_url: &str) -> Self {
        let switched_off = std::env::var("UPDATE_CHECK").is_ok_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "off" | "false" | "0" | "no"
            )
        });
        Self {
            enabled: !switched_off,
            plc_url: plc_url.to_owned(),
        }
    }
}

/// A HappyView release, as its `software.release.version` record says.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    /// The release notes' URL, when the record names an https one.
    pub changelog: Option<String>,
}

impl Release {
    fn offer(&self, channel: &str, current: &str) -> Offer {
        Offer {
            version: self.version.clone(),
            channel: channel.to_owned(),
            release_url: self.changelog.clone(),
            upgrade_guide_url: UPGRADE_GUIDE_URL.to_owned(),
            severity: severity(current, self),
        }
    }
}

/// Why a check failed.
#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    #[error("could not resolve the publisher's PDS: {0}")]
    Resolve(String),
    #[error("request to the publisher's PDS failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("the publisher's PDS answered HTTP {status} for {what}")]
    Status { what: String, status: u16 },
    #[error("the update check took longer than {0:?}")]
    TimedOut(Duration),
    #[error("the publisher's PDS answered {what} with {name}")]
    Xrpc { what: String, name: String },
    #[error("{0}")]
    Invalid(String),
}

/// The dashboard renders these as links, so a published value is used only
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

/// Read a response body of at most `limit` bytes. A declared length past the
/// limit is refused before reading, and an undeclared one as soon as the
/// bytes read pass it, so nothing past the limit is held. `None` when the
/// body is too large.
async fn read_capped(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Option<Vec<u8>>, reqwest::Error> {
    if response
        .content_length()
        .is_some_and(|declared| declared > limit as u64)
    {
        return Ok(None);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > limit {
            return Ok(None);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Some(body))
}

/// The body of `response`, at most `limit` bytes.
async fn read_body(
    response: reqwest::Response,
    limit: usize,
    what: &str,
) -> Result<Vec<u8>, CheckError> {
    read_capped(response, limit)
        .await?
        .ok_or_else(|| CheckError::Invalid(format!("{what} is larger than {limit} bytes")))
}

/// The publisher's PDS, from its DID document in the PLC directory. The
/// document is read with a size cap, and only an https endpoint is used.
async fn resolve_pds(
    client: &reqwest::Client,
    plc_url: &str,
    https_only: bool,
) -> Result<String, CheckError> {
    let url = format!("{}/{PUBLISHER_DID}", plc_url.trim_end_matches('/'));
    let response = client.get(&url).send().await?;
    let status = response.status();
    let bytes = read_body(response, MAX_DID_DOC_BYTES, "the publisher's DID document").await?;
    if !status.is_success() {
        return Err(CheckError::Resolve(format!(
            "the PLC directory answered HTTP {}",
            status.as_u16()
        )));
    }
    let doc: Value = serde_json::from_slice(&bytes)
        .map_err(|e| CheckError::Resolve(format!("the DID document isn't JSON: {e}")))?;
    if doc["id"].as_str() != Some(PUBLISHER_DID) {
        return Err(CheckError::Resolve(
            "the DID document is for another DID".into(),
        ));
    }
    let full_id = format!("{PUBLISHER_DID}#atproto_pds");
    let endpoint = doc["service"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| {
            let id = s["id"].as_str();
            id == Some("#atproto_pds") || id == Some(full_id.as_str())
        })
        .and_then(|s| s["serviceEndpoint"].as_str())
        .ok_or_else(|| CheckError::Resolve("the DID document names no PDS".into()))?;
    if https_only && !is_https(endpoint) {
        return Err(CheckError::Resolve(format!(
            "the publisher's PDS is not an https URL: {endpoint}"
        )));
    }
    Ok(endpoint.to_owned())
}

/// A record as `com.atproto.repo.getRecord` answers it.
#[derive(Debug, Deserialize)]
struct RecordView {
    #[serde(default)]
    cid: String,
    value: Value,
}

/// One record of the publisher's, or `None` when the repo has no such
/// record.
async fn get_record(
    client: &reqwest::Client,
    pds: &str,
    collection: &str,
    rkey: &str,
) -> Result<Option<RecordView>, CheckError> {
    let what = format!("{collection}/{rkey}");
    let url = format!(
        "{}/xrpc/com.atproto.repo.getRecord?repo={}&collection={}&rkey={}",
        pds.trim_end_matches('/'),
        urlencoding::encode(PUBLISHER_DID),
        urlencoding::encode(collection),
        urlencoding::encode(rkey),
    );
    let response = client.get(&url).send().await?;
    let status = response.status();
    let bytes = read_body(response, MAX_RECORD_BYTES, &what).await?;
    let body: Option<Value> = serde_json::from_slice(&bytes).ok();
    if let Some(name) = body
        .as_ref()
        .and_then(|b| b.get("error"))
        .and_then(Value::as_str)
    {
        return if name == "RecordNotFound" {
            Ok(None)
        } else {
            Err(CheckError::Xrpc {
                what,
                name: name.to_owned(),
            })
        };
    }
    match body {
        _ if !status.is_success() => Err(CheckError::Status {
            what,
            status: status.as_u16(),
        }),
        Some(value) => serde_json::from_value(value)
            .map(Some)
            .map_err(|e| CheckError::Invalid(format!("{what} is unreadable: {e}"))),
        None => Err(CheckError::Invalid(format!(
            "the publisher's PDS answered {what} with something that isn't JSON"
        ))),
    }
}

/// The release `channel` points at, or `None` when there is no such
/// channel. The version is followed by the strongRef in the channel record,
/// and checked against the CID that names it.
async fn resolve(
    client: &reqwest::Client,
    pds: &str,
    channel: &str,
) -> Result<Option<Release>, CheckError> {
    let Some(record) = get_record(client, pds, CHANNEL, &format!("{PROJECT}.{channel}")).await?
    else {
        return Ok(None);
    };
    let reference = &record.value["version"];
    let (Some(uri), Some(cid)) = (reference["uri"].as_str(), reference["cid"].as_str()) else {
        return Err(CheckError::Invalid(format!(
            "the {channel} channel names no version"
        )));
    };
    // at://<did>/software.release.version/<rkey>: only a version of this
    // project in the publisher's own repo counts.
    let rkey = uri
        .strip_prefix(&format!("at://{PUBLISHER_DID}/{VERSION}/"))
        .filter(|rkey| rkey.starts_with(&format!("{PROJECT}.")) && !rkey.contains('/'))
        .ok_or_else(|| {
            CheckError::Invalid(format!(
                "the {channel} channel points outside the publisher's {PROJECT} versions: {uri}"
            ))
        })?;
    let Some(version) = get_record(client, pds, VERSION, rkey).await? else {
        return Err(CheckError::Invalid(format!(
            "the {channel} channel points at {uri}, which doesn't exist"
        )));
    };
    if version.cid != cid || verify_record_cid(cid, &version.value) != CidCheck::Match {
        return Err(CheckError::Invalid(format!(
            "{uri} doesn't match the CID the {channel} channel pins"
        )));
    }
    let number = version.value["version"]
        .as_str()
        .filter(|v| parse(v).is_some())
        .ok_or_else(|| CheckError::Invalid(format!("{uri} has no semver version")))?;
    Ok(Some(Release {
        version: number.to_owned(),
        changelog: version.value["changelog"]
            .as_str()
            .filter(|u| is_https(u))
            .map(str::to_owned),
    }))
}

/// One check: what `current`'s channel offers, and what is worth knowing
/// about the stable line. Starts from `previous` so `managed` carries over.
pub async fn check_once(
    client: &reqwest::Client,
    plc_url: &str,
    previous: &UpdateStatus,
) -> Result<UpdateStatus, CheckError> {
    check_once_with(client, plc_url, previous, true).await
}

/// [`check_once`], with the https requirement on the PDS endpoint as a
/// parameter so tests can serve a PDS over plain http. Always `true` in use.
async fn check_once_with(
    client: &reqwest::Client,
    plc_url: &str,
    previous: &UpdateStatus,
    https_only: bool,
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

    let pds = resolve_pds(client, plc_url, https_only).await?;

    let channel = line.channel();
    let own = resolve(client, &pds, &channel).await?;
    next.channel = Some(channel.clone());
    // Only a newer version on this very line is offered: a channel record
    // pointing at another line's version is not an upgrade, whatever its
    // number.
    next.available = own
        .filter(|r| Line::of(&r.version).as_ref() == Some(&line))
        .filter(|r| parse(&r.version).is_some_and(|offered| offered > running))
        .map(|r| r.offer(&channel, current));
    next.state = if next.available.is_some() {
        State::Available
    } else {
        State::Current
    };

    // The stable line, mentioned when it has something for this instance: a
    // prerelease's own version released, or a newer major. Only ever a
    // stable release. Reading it is a separate question from the instance's
    // own line, so a failure here keeps the answer above and says why.
    match resolve(client, &pds, LATEST).await {
        Ok(Some(stable)) => {
            if let Some(stable_version) = parse(&stable.version).filter(|v| v.pre.is_empty()) {
                let worth_mentioning = match &line {
                    Line::Stable { major } => stable_version.major > *major,
                    Line::Beta { .. } | Line::Alpha { .. } => {
                        stable_version >= release_of(&running)
                    }
                };
                if worth_mentioning {
                    next.other_line = Some(stable.offer(LATEST, current));
                }
            }
        }
        Ok(None) => {}
        Err(e) => next.last_error = Some(format!("reading the latest channel: {e}")),
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
    if !config.enabled {
        tracing::info!("update_check: off (UPDATE_CHECK=off)");
        return None;
    }
    Some(tokio::spawn(async move {
        let (dev, current) = {
            let s = status.read().await;
            (s.state == State::Dev, s.current.clone())
        };
        if dev {
            return;
        }
        // Read before taking the lock, so the dashboard is never kept
        // waiting on the database.
        if let Some(stored) = load_stored(&db, backend, &current).await {
            let mut s = status.write().await;
            *s = UpdateStatus {
                managed: s.managed,
                ..stored
            };
        }
        let jitter = rand::random_range(0..STARTUP_JITTER.as_secs());
        tokio::time::sleep(Duration::from_secs(jitter)).await;
        let client = client();
        loop {
            run_check(&client, &config.plc_url, &status, &db, backend).await;
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    }))
}

/// One pass of the background task: check, within [`CHECK_TIMEOUT`], and
/// either store and serve the new answer or keep the last good one and say
/// why the check failed.
pub async fn run_check(
    client: &reqwest::Client,
    plc_url: &str,
    status: &SharedUpdateStatus,
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
) {
    let previous = status.read().await.clone();
    let result = tokio::time::timeout(CHECK_TIMEOUT, check_once(client, plc_url, &previous))
        .await
        .unwrap_or(Err(CheckError::TimedOut(CHECK_TIMEOUT)));
    match result {
        Ok(next) => {
            if let Some(offer) = &next.available {
                tracing::info!(
                    current = %next.current,
                    available = %offer.version,
                    "update_check: a newer HappyView is available"
                );
            }
            store(db, backend, &next).await;
            *status.write().await = next;
        }
        Err(e) => {
            tracing::warn!(error = %e, "update_check: check failed");
            status.write().await.last_error = Some(e.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn lines_come_from_the_version() {
        assert_eq!(Line::of("2.17.3"), Some(Line::Stable { major: 2 }));
        assert_eq!(Line::of("3.0.0-beta.12"), Some(Line::Beta { major: 3 }));
        assert_eq!(Line::of("3.0.0-alpha.67"), Some(Line::Alpha { major: 3 }));
        assert_eq!(Line::of("0.1.0"), None, "an unstamped build");
        assert_eq!(Line::of("2.17.0-dev.2"), None);
        assert_eq!(Line::of("not a version"), None);
    }

    #[test]
    fn each_line_has_a_channel_of_its_own() {
        assert_eq!(Line::Stable { major: 2 }.channel(), "v2");
        assert_eq!(Line::Beta { major: 3 }.channel(), "v3-beta");
        assert_eq!(Line::Alpha { major: 3 }.channel(), "v3-alpha");
    }

    #[test]
    fn only_https_links_are_kept() {
        assert!(is_https("https://github.com/x/y/releases/tag/v1.0.0"));
        assert!(!is_https("http://example.com"));
        assert!(!is_https("javascript:alert(1)"));
        assert!(!is_https("not a url"));
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

    /// A version record as the release workflow writes it, with its real
    /// CID, so the check's CID verification passes.
    fn version_record(v: &str) -> (Value, String) {
        let value = json!({
            "$type": "software.release.version",
            "project": format!("at://{PUBLISHER_DID}/software.release.project/{PROJECT}"),
            "version": v,
            "changelog": format!("https://github.com/gamesgamesgamesgamesgames/happyview/releases/tag/v{v}"),
            "manifest": {
                "uri": format!("at://{PUBLISHER_DID}/software.release.manifest/3m5abcdefgh22"),
                "cid": "bafyreigbtj4x7ip5legnfznufuopl4sg4knzc2cof6duas4b3q2fy6swua",
            },
        });
        let cid = crate::cid_verify::compute_record_cid(&value)
            .expect("cid")
            .to_string();
        (value, cid)
    }

    struct Publisher {
        server: MockServer,
    }

    impl Publisher {
        /// A PLC directory and the publisher's PDS, on one mock server.
        async fn start() -> Self {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path(format!("/{PUBLISHER_DID}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": PUBLISHER_DID,
                    "service": [{
                        "id": "#atproto_pds",
                        "type": "AtprotoPersonalDataServer",
                        "serviceEndpoint": server.uri(),
                    }],
                })))
                .mount(&server)
                .await;
            // Anything not mounted below is a record the repo doesn't have.
            Mock::given(method("GET"))
                .and(path("/xrpc/com.atproto.repo.getRecord"))
                .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                    "error": "RecordNotFound", "message": "Could not locate record",
                })))
                .with_priority(10)
                .mount(&server)
                .await;
            Self { server }
        }

        async fn record(&self, collection: &str, rkey: &str, value: Value, cid: &str) {
            Mock::given(method("GET"))
                .and(path("/xrpc/com.atproto.repo.getRecord"))
                .and(query_param("repo", PUBLISHER_DID))
                .and(query_param("collection", collection))
                .and(query_param("rkey", rkey))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "uri": format!("at://{PUBLISHER_DID}/{collection}/{rkey}"),
                    "cid": cid,
                    "value": value,
                })))
                .with_priority(1)
                .mount(&self.server)
                .await;
        }

        /// Publishes version `v` and points `channel` at it.
        async fn channel(&self, channel: &str, v: &str) {
            let (value, cid) = version_record(v);
            let rkey = format!("{PROJECT}.{v}");
            self.record(VERSION, &rkey, value, &cid).await;
            self.channel_to(
                channel,
                &format!("at://{PUBLISHER_DID}/{VERSION}/{rkey}"),
                &cid,
            )
            .await;
        }

        async fn channel_to(&self, channel: &str, uri: &str, cid: &str) {
            self.record(
                CHANNEL,
                &format!("{PROJECT}.{channel}"),
                json!({
                    "$type": "software.release.channel",
                    "project": format!("at://{PUBLISHER_DID}/software.release.project/{PROJECT}"),
                    "channel": channel,
                    "version": { "uri": uri, "cid": cid },
                }),
                "bafyreichannel",
            )
            .await;
        }

        async fn check(&self, current: &str) -> Result<UpdateStatus, CheckError> {
            check_once_with(
                &reqwest::Client::new(),
                &self.server.uri(),
                &UpdateStatus::initial(current, true, false),
                false,
            )
            .await
        }
    }

    #[tokio::test]
    async fn a_stable_instance_is_offered_its_own_line() {
        let p = Publisher::start().await;
        p.channel("v2", "2.17.4").await;
        p.channel(LATEST, "3.0.0").await;

        let status = p.check("2.17.3").await.unwrap();
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
        assert_eq!(
            status
                .other_line
                .expect("the new major is mentioned")
                .version,
            "3.0.0"
        );
    }

    #[tokio::test]
    async fn an_up_to_date_instance_is_current() {
        let p = Publisher::start().await;
        p.channel("v2", "2.17.3").await;
        p.channel(LATEST, "2.17.3").await;

        let status = p.check("2.17.3").await.unwrap();
        assert_eq!(status.state, State::Current);
        assert!(status.available.is_none());
        assert!(status.other_line.is_none());
        assert!(status.checked_at.is_some());
    }

    #[tokio::test]
    async fn a_line_with_no_channel_yet_is_current() {
        let p = Publisher::start().await;
        let status = p.check("2.17.3").await.unwrap();
        assert_eq!(status.state, State::Current);
        assert_eq!(status.channel.as_deref(), Some("v2"));
    }

    #[tokio::test]
    async fn a_prerelease_follows_its_own_channel_and_hears_of_its_release() {
        let p = Publisher::start().await;
        p.channel("v3-beta", "3.0.0-beta.13").await;
        p.channel(LATEST, "3.0.0").await;

        let status = p.check("3.0.0-beta.12").await.unwrap();
        assert_eq!(status.channel.as_deref(), Some("v3-beta"));
        assert_eq!(status.available.expect("offer").version, "3.0.0-beta.13");
        let other = status.other_line.expect("3.0.0 is out");
        assert_eq!(
            (other.version.as_str(), other.channel.as_str()),
            ("3.0.0", LATEST)
        );
    }

    #[tokio::test]
    async fn a_prerelease_is_not_told_about_an_older_stable() {
        let p = Publisher::start().await;
        p.channel("v3-alpha", "3.0.0-alpha.67").await;
        p.channel(LATEST, "2.17.4").await;

        let status = p.check("3.0.0-alpha.67").await.unwrap();
        assert_eq!(status.state, State::Current);
        assert!(status.other_line.is_none());
    }

    #[tokio::test]
    async fn a_version_that_does_not_match_its_pinned_cid_fails_the_check() {
        let p = Publisher::start().await;
        let (value, _) = version_record("2.17.4");
        let (_, other_cid) = version_record("2.17.5");
        let rkey = format!("{PROJECT}.2.17.4");
        // The PDS serves a record whose content isn't what the CID names.
        p.record(VERSION, &rkey, value, &other_cid).await;
        p.channel_to(
            "v2",
            &format!("at://{PUBLISHER_DID}/{VERSION}/{rkey}"),
            &other_cid,
        )
        .await;

        let err = p.check("2.17.3").await.unwrap_err();
        assert!(err.to_string().contains("CID"), "{err}");
    }

    #[tokio::test]
    async fn a_channel_pointing_outside_the_publishers_versions_fails_the_check() {
        let p = Publisher::start().await;
        let (_, cid) = version_record("9.9.9");
        p.channel_to(
            "v2",
            &format!("at://did:plc:someoneelse/{VERSION}/{PROJECT}.9.9.9"),
            &cid,
        )
        .await;
        assert!(p.check("2.17.3").await.is_err());
    }

    #[tokio::test]
    async fn a_non_https_changelog_is_dropped() {
        let p = Publisher::start().await;
        let (mut value, _) = version_record("2.17.4");
        value["changelog"] = "javascript:alert(1)".into();
        let cid = crate::cid_verify::compute_record_cid(&value)
            .unwrap()
            .to_string();
        let rkey = format!("{PROJECT}.2.17.4");
        p.record(VERSION, &rkey, value, &cid).await;
        p.channel_to(
            "v2",
            &format!("at://{PUBLISHER_DID}/{VERSION}/{rkey}"),
            &cid,
        )
        .await;

        let offer = p.check("2.17.3").await.unwrap().available.expect("offer");
        assert_eq!(offer.release_url, None);
    }

    #[tokio::test]
    async fn an_unreachable_pds_fails_the_check() {
        let p = Publisher::start().await;
        Mock::given(method("GET"))
            .and(path("/xrpc/com.atproto.repo.getRecord"))
            .respond_with(ResponseTemplate::new(503))
            .with_priority(1)
            .mount(&p.server)
            .await;
        assert!(p.check("2.17.3").await.is_err());
    }

    #[tokio::test]
    async fn an_oversized_record_fails_the_check() {
        let p = Publisher::start().await;
        Mock::given(method("GET"))
            .and(path("/xrpc/com.atproto.repo.getRecord"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("x".repeat(MAX_RECORD_BYTES + 1)),
            )
            .with_priority(1)
            .mount(&p.server)
            .await;
        let err = p.check("2.17.3").await.unwrap_err();
        assert!(err.to_string().contains("larger than"), "{err}");
    }

    /// Serves one chunked response, with no `Content-Length`, of `chunks`
    /// chunks of `chunk_size` bytes each, and answers its URL.
    async fn chunked_server(chunks: usize, chunk_size: usize) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request).await;
            let mut response =
                b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n"
                    .to_vec();
            for _ in 0..chunks {
                response.extend_from_slice(format!("{chunk_size:x}\r\n").as_bytes());
                response.extend(std::iter::repeat_n(b'x', chunk_size));
                response.extend_from_slice(b"\r\n");
            }
            response.extend_from_slice(b"0\r\n\r\n");
            // The reader may hang up once it has seen enough.
            let _ = socket.write_all(&response).await;
            let _ = socket.shutdown().await;
        });
        format!("http://{addr}/")
    }

    #[tokio::test]
    async fn a_body_with_no_length_is_refused_once_it_passes_the_cap() {
        let url = chunked_server(8, 1024).await;
        let response = reqwest::get(&url).await.unwrap();
        assert_eq!(response.content_length(), None, "the length is undeclared");
        assert_eq!(read_capped(response, 4 * 1024).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_body_with_no_length_within_the_cap_is_read() {
        let url = chunked_server(4, 1024).await;
        let response = reqwest::get(&url).await.unwrap();
        assert_eq!(response.content_length(), None, "the length is undeclared");
        let body = read_capped(response, 4 * 1024)
            .await
            .unwrap()
            .expect("read");
        assert_eq!(body.len(), 4 * 1024);
    }

    #[tokio::test]
    async fn a_dev_build_never_asks() {
        let p = Publisher::start().await;
        let status = p.check("0.1.0").await.unwrap();
        assert_eq!(status.state, State::Dev);
        assert!(
            p.server
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_channel_pointing_at_another_lines_version_is_not_offered() {
        let p = Publisher::start().await;
        p.channel("v2", "3.0.0").await;
        p.channel("v3-beta", "3.0.0-alpha.70").await;
        let stable = p.check("2.17.3").await.unwrap();
        assert!(stable.available.is_none(), "{stable:?}");
        let beta = p.check("3.0.0-beta.12").await.unwrap();
        assert!(beta.available.is_none(), "{beta:?}");
    }

    #[tokio::test]
    async fn latest_on_a_prerelease_is_never_mentioned() {
        let p = Publisher::start().await;
        p.channel("v2", "2.17.3").await;
        p.channel(LATEST, "3.0.1-beta.1").await;
        assert!(p.check("2.17.3").await.unwrap().other_line.is_none());
        assert!(p.check("3.0.0-beta.12").await.unwrap().other_line.is_none());
    }

    #[tokio::test]
    async fn a_failing_latest_keeps_the_own_line_answer() {
        let p = Publisher::start().await;
        p.channel("v2", "2.17.4").await;
        Mock::given(method("GET"))
            .and(path("/xrpc/com.atproto.repo.getRecord"))
            .and(query_param("rkey", format!("{PROJECT}.{LATEST}")))
            .respond_with(ResponseTemplate::new(503))
            .with_priority(1)
            .mount(&p.server)
            .await;
        let status = p.check("2.17.3").await.unwrap();
        assert_eq!(status.available.expect("offer").version, "2.17.4");
        assert!(status.other_line.is_none());
        assert!(status.last_error.expect("why").contains("latest"));
    }

    #[tokio::test]
    async fn a_channel_must_point_at_a_happyview_version() {
        let p = Publisher::start().await;
        let (value, cid) = version_record("2.17.4");
        let rkey = "other.2.17.4";
        p.record(VERSION, rkey, value, &cid).await;
        p.channel_to(
            "v2",
            &format!("at://{PUBLISHER_DID}/{VERSION}/{rkey}"),
            &cid,
        )
        .await;
        assert!(p.check("2.17.3").await.is_err());
    }

    #[tokio::test]
    async fn a_pds_that_is_not_https_is_refused() {
        let p = Publisher::start().await;
        p.channel("v2", "2.17.4").await;
        let err = check_once(
            &reqwest::Client::new(),
            &p.server.uri(),
            &UpdateStatus::initial("2.17.3", true, false),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("https"), "{err}");
    }

    #[tokio::test]
    async fn an_oversized_did_document_is_refused() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/{PUBLISHER_DID}")))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("x".repeat(MAX_DID_DOC_BYTES + 1)),
            )
            .mount(&server)
            .await;
        let err = check_once_with(
            &reqwest::Client::new(),
            &server.uri(),
            &UpdateStatus::initial("2.17.3", true, false),
            false,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("larger than"), "{err}");
    }

    #[tokio::test]
    async fn a_record_that_is_not_json_says_so() {
        let p = Publisher::start().await;
        Mock::given(method("GET"))
            .and(path("/xrpc/com.atproto.repo.getRecord"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>"))
            .with_priority(1)
            .mount(&p.server)
            .await;
        let err = p.check("2.17.3").await.unwrap_err();
        assert!(err.to_string().contains("isn't JSON"), "{err}");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn update_check_off_spawns_nothing_and_sends_nothing() {
        // SAFETY: serialised with every other test that touches the environment.
        unsafe { std::env::set_var("UPDATE_CHECK", "off") };
        let config = UpdateCheckConfig::from_env("http://127.0.0.1:9");
        unsafe { std::env::remove_var("UPDATE_CHECK") };
        assert!(!config.enabled);
        let status = Arc::new(RwLock::new(UpdateStatus::initial("2.17.3", false, false)));
        let pool = crate::test_support::migrated_memory_pool().await;
        assert!(spawn_check_task(config, status.clone(), pool, DatabaseBackend::Sqlite).is_none());
        assert_eq!(status.read().await.state, State::Disabled);
    }

    #[tokio::test]
    async fn a_failed_pass_keeps_the_last_good_answer() {
        let p = Publisher::start().await;
        p.channel("v2", "2.17.4").await;
        let pool = crate::test_support::migrated_memory_pool().await;
        let status = Arc::new(RwLock::new(UpdateStatus::initial("2.17.3", true, false)));
        let good = p.check("2.17.3").await.unwrap();
        *status.write().await = good.clone();

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .with_priority(1)
            .mount(&p.server)
            .await;
        run_check(
            &reqwest::Client::new(),
            &p.server.uri(),
            &status,
            &pool,
            DatabaseBackend::Sqlite,
        )
        .await;
        let after = status.read().await.clone();
        assert_eq!(after.available, good.available);
        assert_eq!(after.state, State::Available);
        assert!(after.last_error.is_some());
    }

    #[tokio::test]
    async fn a_stored_answer_is_only_reused_for_the_same_version() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let mut answer = UpdateStatus::initial("2.17.3", true, false);
        answer.state = State::Current;
        store(&pool, DatabaseBackend::Sqlite, &answer).await;
        assert_eq!(
            load_stored(&pool, DatabaseBackend::Sqlite, "2.17.3").await,
            Some(answer.clone())
        );
        assert_eq!(
            load_stored(&pool, DatabaseBackend::Sqlite, "2.17.4").await,
            None
        );
        // A second store replaces the first.
        answer.state = State::Available;
        store(&pool, DatabaseBackend::Sqlite, &answer).await;
        assert_eq!(
            load_stored(&pool, DatabaseBackend::Sqlite, "2.17.3")
                .await
                .unwrap()
                .state,
            State::Available
        );
    }

    #[test]
    #[serial_test::serial]
    fn plugin_registry_off_does_not_turn_the_check_off() {
        // SAFETY: serialised with every other test that touches the environment.
        unsafe {
            std::env::remove_var("UPDATE_CHECK");
            std::env::set_var("PLUGIN_REGISTRY_URL", "off");
        }
        assert!(UpdateCheckConfig::from_env("https://plc.directory").enabled);
        unsafe {
            std::env::set_var("UPDATE_CHECK", "off");
        }
        assert!(!UpdateCheckConfig::from_env("https://plc.directory").enabled);
        unsafe {
            std::env::remove_var("UPDATE_CHECK");
            std::env::remove_var("PLUGIN_REGISTRY_URL");
        }
    }
}

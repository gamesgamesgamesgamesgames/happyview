//! Detecting whether a PDS serves atproto spaces.
//!
//! Detection has two tiers.
//!
//! `community.lexicon.service.describe` is the declarative answer, and where a
//! host serves it, it is authoritative. The reference PDS
//! (`ghcr.io/bluesky-social/atproto:pds-spaces-alpha`) does not serve it, so a
//! descriptor-only detector would report the reference PDS as having no spaces.
//!
//! When there is no descriptor, the detector probes a method and reads the
//! error status. This is the only signal from a host that publishes no method
//! list. Several statuses are ambiguous, so the probe is narrow: see
//! [`PROBE_METHOD`] and [`CONTROL_METHOD`].

use std::time::Duration;

use crate::AppState;
use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::error::AppError;

const DESCRIBE_NSID: &str = "community.lexicon.service.describe";
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The method used to prove the route table carries spaces.
///
/// **Must be auth-gated.** On the reference PDS
/// (`tests/spaces_reference_interop.rs`), a missing route answers `400
/// InvalidRequest`, and so does a present route rejecting missing query
/// params. `com.atproto.simplespace.getSpace` answers 400 when present, so it
/// cannot be used here. Only a `401` distinguishes presence from absence.
const PROBE_METHOD: &str = "com.atproto.space.listSpaces";

/// A method that cannot exist, to catch a host that answers uniformly.
///
/// HappyView returns 401 for every unrouted `/xrpc/` path, because those paths
/// fall through to its SPA fallback. Without this control, the detector would
/// read HappyView as spaces-capable on a build with spaces off.
const CONTROL_METHOD: &str = "com.atproto.space.zzzProbeControlDoesNotExist";

/// Methods a repo host must serve for migration to be worth attempting.
///
/// Each entry is a set of accepted spellings, and one match is enough.
/// Implementations lag renames: ZDS advertises `getRepoState`, and `addMember`
/// predates `putMember`.
const REQUIRED_METHODS: &[&[&str]] = &[
    &["com.atproto.simplespace.createSpace"],
    &[
        "com.atproto.simplespace.putMember",
        "com.atproto.simplespace.addMember",
    ],
    &[
        "com.atproto.simplespace.getSpace",
        "com.atproto.space.getSpace",
    ],
    &["com.atproto.space.createRecord"],
    &["com.atproto.space.applyWrites"],
    &["com.atproto.space.listRecords"],
    &["com.atproto.space.listRepoOps"],
    &[
        "com.atproto.space.getLatestCommit",
        "com.atproto.space.getRepoState",
    ],
    &["com.atproto.space.getDelegationToken"],
];

/// How long an answer is trusted.
///
/// The two TTLs differ. A stale "yes" costs one failed call with an explanatory
/// error. A stale "no" is worse: after an operator enables spaces, every account
/// on that server is told it has none until the entry expires, and the users
/// cannot change that. A negative also records transient failures (a restart, a
/// timeout, a network error), which are not evidence about the server. So a
/// negative is kept only long enough to avoid a probe on every page load.
const POSITIVE_TTL: Duration = Duration::from_secs(60 * 60);
const NEGATIVE_TTL: Duration = Duration::from_secs(5 * 60);

/// Which tier produced an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectionTier {
    /// The server stated its own method list.
    Descriptor,
    /// Inferred from how the server answers an auth-gated method.
    Probe,
    /// The server could not be reached, which is not evidence about it.
    Unreachable,
}

impl DetectionTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Descriptor => "descriptor",
            Self::Probe => "probe",
            Self::Unreachable => "unreachable",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "descriptor" => Some(Self::Descriptor),
            "probe" => Some(Self::Probe),
            "unreachable" => Some(Self::Unreachable),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SpaceSupport {
    pub supported: bool,
    pub pds: String,
    pub tier: DetectionTier,
    /// Required methods the server did not offer. Only the descriptor tier knows
    /// which are missing; an unsupported answer from any other tier lists every
    /// required method.
    pub missing: Vec<String>,
    pub checked_at: String,
}

impl SpaceSupport {
    fn unsupported(pds: &str, tier: DetectionTier, missing: Vec<String>) -> Self {
        Self {
            supported: false,
            pds: pds.to_string(),
            tier,
            missing,
            checked_at: now_rfc3339(),
        }
    }

    fn supported(pds: &str, tier: DetectionTier) -> Self {
        Self {
            supported: true,
            pds: pds.to_string(),
            tier,
            missing: Vec::new(),
            checked_at: now_rfc3339(),
        }
    }
}

fn all_required_names() -> Vec<String> {
    REQUIRED_METHODS
        .iter()
        .map(|spellings| spellings[0].to_string())
        .collect()
}

/// Tier 1: read the server's own method list.
///
/// `None` means "no answer", not "no spaces". Any non-200, an unparseable
/// body, or a missing `methods` array falls through to the probe, because the
/// reference PDS answers a plain `400` here and treating that as a denial would
/// rule it out entirely.
async fn fetch_descriptor(http: &reqwest::Client, pds: &str) -> Option<Vec<String>> {
    let url = format!("{}/xrpc/{DESCRIBE_NSID}", pds.trim_end_matches('/'));
    let resp = http
        .get(&url)
        .header("accept", "application/json")
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }

    let body: serde_json::Value = resp.json().await.ok()?;
    let methods = body.get("methods")?.as_array()?;
    Some(
        methods
            .iter()
            .filter_map(|m| m.get("value")?.as_str().map(str::to_string))
            .collect(),
    )
}

/// The status a method answers with, or `None` when the host is unreachable.
async fn probe_status(http: &reqwest::Client, pds: &str, nsid: &str) -> Option<u16> {
    let url = format!("{}/xrpc/{nsid}", pds.trim_end_matches('/'));
    http.get(&url)
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
        .ok()
        .map(|r| r.status().as_u16())
}

/// Probe a PDS directly, bypassing the cache.
pub async fn probe(http: &reqwest::Client, pds: &str) -> SpaceSupport {
    if let Some(methods) = fetch_descriptor(http, pds).await {
        let missing: Vec<String> = REQUIRED_METHODS
            .iter()
            .filter(|spellings| {
                !spellings
                    .iter()
                    .any(|name| methods.iter().any(|m| m == name))
            })
            .map(|spellings| spellings[0].to_string())
            .collect();

        return if missing.is_empty() {
            SpaceSupport::supported(pds, DetectionTier::Descriptor)
        } else {
            SpaceSupport::unsupported(pds, DetectionTier::Descriptor, missing)
        };
    }

    let (real, control) = tokio::join!(
        probe_status(http, pds, PROBE_METHOD),
        probe_status(http, pds, CONTROL_METHOD)
    );

    let (Some(real), Some(control)) = (real, control) else {
        // Treated as unsupported, which also gives it the short negative TTL.
        return SpaceSupport::unsupported(pds, DetectionTier::Unreachable, all_required_names());
    };

    // A host that answers a method that cannot exist the same way gives no
    // signal, so the probe is inconclusive.
    let conclusive = real != control;
    // Only 401 shows the route exists; see `PROBE_METHOD`.
    let supported = conclusive && real == 401;

    if supported {
        SpaceSupport::supported(pds, DetectionTier::Probe)
    } else {
        SpaceSupport::unsupported(pds, DetectionTier::Probe, all_required_names())
    }
}

fn is_fresh(checked_at: &str, supported: bool) -> bool {
    let Ok(checked) = chrono::DateTime::parse_from_rfc3339(checked_at) else {
        return false;
    };
    let age = chrono::Utc::now().signed_duration_since(checked.with_timezone(&chrono::Utc));
    let Ok(age) = age.to_std() else {
        // A timestamp in the future is a clock problem, not freshness.
        return false;
    };
    age < if supported {
        POSITIVE_TTL
    } else {
        NEGATIVE_TTL
    }
}

async fn read_cache(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    pds: &str,
) -> Result<Option<SpaceSupport>, AppError> {
    let sql = adapt_sql(
        "SELECT supported, tier, missing, checked_at FROM happyview_space_pds_support WHERE pds_endpoint = ?",
        backend,
    );
    let row: Option<(i32, String, String, String)> = crate::db::query_as(&sql)
        .bind(pds)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to read pds support cache: {e}")))?;

    Ok(
        row.map(|(supported, tier, missing, checked_at)| SpaceSupport {
            supported: supported != 0,
            pds: pds.to_string(),
            tier: DetectionTier::parse(&tier).unwrap_or(DetectionTier::Probe),
            missing: serde_json::from_str(&missing).unwrap_or_default(),
            checked_at,
        }),
    )
}

async fn write_cache(
    pool: &sqlx::AnyPool,
    backend: DatabaseBackend,
    support: &SpaceSupport,
) -> Result<(), AppError> {
    let missing = serde_json::to_string(&support.missing).unwrap_or_else(|_| "[]".into());
    let sql = adapt_sql(
        "INSERT INTO happyview_space_pds_support (pds_endpoint, supported, tier, missing, checked_at) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT (pds_endpoint) DO UPDATE SET \
             supported = excluded.supported, \
             tier = excluded.tier, \
             missing = excluded.missing, \
             checked_at = excluded.checked_at",
        backend,
    );
    crate::db::query(&sql)
        .bind(&support.pds)
        .bind(support.supported as i32)
        .bind(support.tier.as_str())
        .bind(&missing)
        .bind(&support.checked_at)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(format!("failed to write pds support cache: {e}")))?;
    Ok(())
}

/// Cached support for a PDS, re-probed once the cached answer has aged out.
///
/// A probe that cannot reach the server is not an error here: it lands as "not
/// supported", which is what an unreachable PDS means for this feature. Callers
/// are never expected to surface a detection failure to a user.
pub async fn get_support(
    state: &AppState,
    pds: &str,
    force: bool,
) -> Result<SpaceSupport, AppError> {
    if !force
        && let Some(cached) = read_cache(&state.db, state.db_backend, pds).await?
        && is_fresh(&cached.checked_at, cached.supported)
    {
        return Ok(cached);
    }

    let fresh = probe(&state.http, pds).await;
    write_cache(&state.db, state.db_backend, &fresh).await?;
    Ok(fresh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn descriptor(methods: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "roles": ["pds"],
            "methods": methods.iter().map(|m| serde_json::json!({
                "$type": "community.lexicon.service.describe#nsid",
                "value": m,
            })).collect::<Vec<_>>(),
        })
    }

    /// Every method the required list asks for, in its canonical spelling.
    fn full_method_list() -> Vec<&'static str> {
        REQUIRED_METHODS.iter().map(|s| s[0]).collect()
    }

    async fn mount_status(server: &MockServer, nsid: &str, status: u16) {
        Mock::given(method("GET"))
            .and(path(format!("/xrpc/{nsid}")))
            .respond_with(ResponseTemplate::new(status))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn a_descriptor_listing_the_space_methods_is_a_confident_yes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/xrpc/{DESCRIBE_NSID}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(descriptor(&full_method_list())))
            .mount(&server)
            .await;

        let got = probe(&reqwest::Client::new(), &server.uri()).await;
        assert!(got.supported);
        assert_eq!(got.tier, DetectionTier::Descriptor);
        assert!(got.missing.is_empty());
    }

    #[tokio::test]
    async fn a_descriptor_omitting_the_space_methods_is_a_confident_no() {
        // The server stated its method list and spaces are not in it, so there
        // is nothing to infer and no probe to run.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/xrpc/{DESCRIBE_NSID}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(descriptor(&["com.atproto.server.describeServer"])),
            )
            .mount(&server)
            .await;

        let got = probe(&reqwest::Client::new(), &server.uri()).await;
        assert!(!got.supported);
        assert_eq!(got.tier, DetectionTier::Descriptor);
        assert!(
            !got.missing.is_empty(),
            "a descriptor knows what is missing"
        );
    }

    #[tokio::test]
    async fn an_alternate_spelling_satisfies_a_required_method() {
        let server = MockServer::start().await;
        let mut methods: Vec<&str> = full_method_list();
        methods.retain(|m| {
            *m != "com.atproto.space.getLatestCommit" && *m != "com.atproto.simplespace.putMember"
        });
        methods.push("com.atproto.space.getRepoState");
        methods.push("com.atproto.simplespace.addMember");

        Mock::given(method("GET"))
            .and(path(format!("/xrpc/{DESCRIBE_NSID}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(descriptor(&methods)))
            .mount(&server)
            .await;

        assert!(
            probe(&reqwest::Client::new(), &server.uri())
                .await
                .supported
        );
    }

    #[tokio::test]
    async fn a_400_descriptor_falls_through_to_the_probe() {
        // The reference PDS answers the descriptor with 400.
        let server = MockServer::start().await;
        mount_status(&server, DESCRIBE_NSID, 400).await;
        mount_status(&server, PROBE_METHOD, 401).await;
        mount_status(&server, CONTROL_METHOD, 404).await;

        let got = probe(&reqwest::Client::new(), &server.uri()).await;
        assert!(
            got.supported,
            "401 on an auth-gated method means the route exists"
        );
        assert_eq!(got.tier, DetectionTier::Probe);
    }

    #[tokio::test]
    async fn a_malformed_descriptor_body_falls_through_rather_than_denying() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/xrpc/{DESCRIBE_NSID}")))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;
        mount_status(&server, PROBE_METHOD, 401).await;
        mount_status(&server, CONTROL_METHOD, 404).await;

        let got = probe(&reqwest::Client::new(), &server.uri()).await;
        assert!(got.supported);
        assert_eq!(got.tier, DetectionTier::Probe);
    }

    #[tokio::test]
    async fn a_probe_matching_its_control_is_inconclusive() {
        // A host answering 401 for every path, as HappyView does. See
        // `CONTROL_METHOD`.
        let server = MockServer::start().await;
        mount_status(&server, DESCRIBE_NSID, 400).await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let got = probe(&reqwest::Client::new(), &server.uri()).await;
        assert!(!got.supported, "indistinguishable responses prove nothing");
        assert_eq!(got.tier, DetectionTier::Probe);
    }

    #[tokio::test]
    async fn a_404_on_the_space_method_is_a_no() {
        let server = MockServer::start().await;
        mount_status(&server, DESCRIBE_NSID, 400).await;
        mount_status(&server, PROBE_METHOD, 404).await;
        mount_status(&server, CONTROL_METHOD, 400).await;

        assert!(
            !probe(&reqwest::Client::new(), &server.uri())
                .await
                .supported
        );
    }

    #[tokio::test]
    async fn a_400_on_the_space_method_is_a_no() {
        // A present route called without its params also answers 400, so 400
        // never shows presence.
        let server = MockServer::start().await;
        mount_status(&server, DESCRIBE_NSID, 400).await;
        mount_status(&server, PROBE_METHOD, 400).await;
        mount_status(&server, CONTROL_METHOD, 404).await;

        assert!(
            !probe(&reqwest::Client::new(), &server.uri())
                .await
                .supported
        );
    }

    #[tokio::test]
    async fn an_unreachable_host_is_unsupported_not_an_error() {
        // An unreachable PDS means "no spaces for now", never a failed request
        // surfacing to a user mid-login.
        let got = probe(&reqwest::Client::new(), "http://127.0.0.1:1").await;
        assert!(!got.supported);
        assert_eq!(got.tier, DetectionTier::Unreachable);
    }

    // -----------------------------------------------------------------------
    // Cache
    // -----------------------------------------------------------------------

    async fn state_with_cached(
        supported: bool,
        tier: DetectionTier,
        age: chrono::Duration,
    ) -> (AppState, String) {
        let pool = crate::test_support::migrated_memory_pool().await;
        let state = crate::test_support::test_state_with_pool(pool);
        let pds = "https://pds.example".to_string();

        let checked_at = (chrono::Utc::now() - age).to_rfc3339();
        write_cache(
            &state.db,
            state.db_backend,
            &SpaceSupport {
                supported,
                pds: pds.clone(),
                tier,
                missing: Vec::new(),
                checked_at,
            },
        )
        .await
        .expect("seed cache");

        (state, pds)
    }

    #[tokio::test]
    async fn a_fresh_positive_is_served_from_cache() {
        let (state, pds) = state_with_cached(
            true,
            DetectionTier::Descriptor,
            chrono::Duration::minutes(10),
        )
        .await;

        // The endpoint is unroutable, so a cache miss would come back
        // Unreachable rather than Descriptor.
        let got = get_support(&state, &pds, false).await.unwrap();
        assert!(got.supported);
        assert_eq!(got.tier, DetectionTier::Descriptor);
    }

    #[tokio::test]
    async fn a_negative_expires_far_sooner_than_a_positive() {
        let ten_minutes = chrono::Duration::minutes(10);

        let (state, pds) = state_with_cached(false, DetectionTier::Descriptor, ten_minutes).await;
        let got = get_support(&state, &pds, false).await.unwrap();
        assert_eq!(
            got.tier,
            DetectionTier::Unreachable,
            "a 10-minute-old negative must be re-probed"
        );

        let (state, pds) = state_with_cached(true, DetectionTier::Descriptor, ten_minutes).await;
        let got = get_support(&state, &pds, false).await.unwrap();
        assert_eq!(
            got.tier,
            DetectionTier::Descriptor,
            "a 10-minute-old positive must still be trusted"
        );
    }

    #[tokio::test]
    async fn force_bypasses_a_fresh_answer() {
        let (state, pds) =
            state_with_cached(true, DetectionTier::Descriptor, chrono::Duration::zero()).await;

        let got = get_support(&state, &pds, true).await.unwrap();
        assert_eq!(got.tier, DetectionTier::Unreachable);
    }

    #[tokio::test]
    async fn one_probe_serves_every_account_on_a_host() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/xrpc/{DESCRIBE_NSID}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(descriptor(&full_method_list())))
            .expect(1)
            .mount(&server)
            .await;

        let pool = crate::test_support::migrated_memory_pool().await;
        let state = crate::test_support::test_state_with_pool(pool);

        assert!(
            get_support(&state, &server.uri(), false)
                .await
                .unwrap()
                .supported
        );
        assert!(
            get_support(&state, &server.uri(), false)
                .await
                .unwrap()
                .supported
        );
        // `expect(1)` is asserted when the server drops.
    }
}

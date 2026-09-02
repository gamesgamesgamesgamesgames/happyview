//! The background reporter.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::AppState;
use crate::telemetry::payload::Snapshot;
use crate::telemetry::{assemble, consent};

const PERIOD: Duration = Duration::from_secs(86_400);
const SEND_TIMEOUT: Duration = Duration::from_secs(15);

static TELEMETRY_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

fn telemetry_client() -> &'static reqwest::Client {
    TELEMETRY_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("failed to build telemetry HTTP client")
    })
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BenchmarkEntry {
    pub p50: f64,
    pub value: f64,
    pub percentile: f64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Benchmarks {
    pub cohort_size: u32,
    pub metrics: BTreeMap<String, BenchmarkEntry>,
}

/// Spread the fleet across the reporting period.
pub fn jitter_offset(instance_id: &str, period: Duration) -> Duration {
    use std::hash::{Hash, Hasher};

    let secs = period.as_secs();
    if secs == 0 {
        return Duration::ZERO;
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    instance_id.hash(&mut hasher);
    Duration::from_secs(hasher.finish() % secs)
}

pub fn parse_benchmarks(body: &str) -> Option<Benchmarks> {
    let parsed: Benchmarks = serde_json::from_str(body).ok()?;
    if parsed.metrics.is_empty() {
        return None;
    }
    Some(parsed)
}

pub async fn send_once(
    http: &reqwest::Client,
    url: &str,
    snapshot: &Snapshot,
) -> Result<Option<Benchmarks>, String> {
    let response = http
        .post(url)
        .timeout(SEND_TIMEOUT)
        .json(snapshot)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !response.status().is_success() {
        return Err(format!("collector returned {}", response.status()));
    }

    let body = response.text().await.map_err(|e| e.to_string())?;
    Ok(parse_benchmarks(&body))
}

/// Assemble and send one snapshot if consent currently permits it.
pub async fn report_once(state: &AppState) -> Result<Option<Benchmarks>, String> {
    let mut current = consent::load(&state.db, state.db_backend).await;
    if !current.mode.reports() {
        return Err("telemetry is disabled".to_string());
    }

    if current.instance_id.is_none() {
        current.instance_id = consent::ensure_instance_id(&state.db, state.db_backend).await;
    }

    let snapshot = assemble::assemble(
        &state.db,
        state.db_backend,
        &state.config.database_url,
        &current,
        &state.telemetry_counters,
    )
    .await
    .ok_or_else(|| "no instance id".to_string())?;

    send_once(
        telemetry_client(),
        &state.config.telemetry_collector_url,
        &snapshot,
    )
    .await
}

/// Background loop. Spawned once from `main`.
pub async fn run_reporter(state: AppState) {
    let seed = consent::load(&state.db, state.db_backend)
        .await
        .instance_id
        .unwrap_or_else(|| state.config.public_url.clone());
    tokio::time::sleep(jitter_offset(&seed, PERIOD)).await;

    tracing::info!("starting telemetry reporter");

    loop {
        let mode = consent::load(&state.db, state.db_backend).await.mode;
        if matches!(mode, consent::TelemetryMode::Auto) {
            match report_once(&state).await {
                Ok(_) => tracing::debug!("telemetry snapshot sent"),
                Err(e) => tracing::warn!(error = %e, "telemetry snapshot failed"),
            }
        }

        tokio::time::sleep(PERIOD).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const DAY: Duration = Duration::from_secs(86_400);

    #[test]
    fn jitter_is_deterministic_for_an_instance() {
        let id = "11111111-2222-3333-4444-555555555555";
        assert_eq!(jitter_offset(id, DAY), jitter_offset(id, DAY));
    }

    #[test]
    fn jitter_stays_inside_the_period() {
        for i in 0..500 {
            let id = format!("instance-{i}");
            assert!(jitter_offset(&id, DAY) < DAY);
        }
    }

    #[test]
    fn jitter_spreads_instances_across_the_period() {
        let mut buckets = std::collections::HashSet::new();
        for i in 0..500 {
            let offset = jitter_offset(&format!("instance-{i}"), DAY);
            buckets.insert(offset.as_secs() / 3600);
        }
        assert!(buckets.len() > 12, "only {} distinct hours", buckets.len());
    }

    #[test]
    fn jitter_handles_a_zero_period_without_dividing_by_zero() {
        assert_eq!(jitter_offset("anything", Duration::ZERO), Duration::ZERO);
    }

    #[tokio::test]
    async fn a_send_failure_is_an_error_value_not_a_panic() {
        // Nothing in the reporter may propagate. Port 1 refuses immediately.
        let http = reqwest::Client::new();
        let snapshot = crate::telemetry::payload::Snapshot {
            schema_version: 1,
            instance_id: "x".into(),
            reported_at: "2026-08-18T00:00:00+00:00".into(),
            report_mode: "auto".into(),
            happyview_version: "0.1.0".into(),
            process_started_at: "2026-08-18T00:00:00+00:00".into(),
            contact: None,
            totals: Default::default(),
            since_start: Default::default(),
            features: Default::default(),
            host: Default::default(),
            lexicons: Default::default(),
        };

        let result = send_once(&http, "http://127.0.0.1:1/v1/snapshot", &snapshot).await;
        assert!(result.is_err());
    }

    #[test]
    fn a_malformed_benchmark_body_parses_to_none_rather_than_failing() {
        assert!(parse_benchmarks("not json").is_none());
        assert!(parse_benchmarks("").is_none());
        assert!(parse_benchmarks(r#"{"cohort_size":3}"#).is_none());
        assert!(parse_benchmarks(r#"{"cohort_size":3,"metrics":{}}"#).is_none());
    }

    #[test]
    fn a_well_formed_benchmark_body_parses() {
        let body = r#"{"cohort_size":42,"metrics":{"records":{"p50":100.0,"value":250.0,"percentile":78.0}}}"#;
        let parsed = parse_benchmarks(body).unwrap();
        assert_eq!(parsed.cohort_size, 42);
        assert_eq!(parsed.metrics.len(), 1);
    }
}

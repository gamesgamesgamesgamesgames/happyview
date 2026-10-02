use std::collections::HashMap;
use std::sync::Arc;

use futures_util::StreamExt;
use serde::Deserialize;
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::AppState;
use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::event_log::{EventLog, Severity, log_event, normalize_rfc3339};
use crate::profile;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// AT Protocol event stream frame header (DAG-CBOR).
#[derive(Deserialize)]
struct FrameHeader {
    op: i64,
    #[serde(default)]
    t: Option<String>,
}

#[derive(Deserialize)]
struct SubscribeLabelsMessage {
    seq: i64,
    labels: Vec<Label>,
}

#[derive(Deserialize)]
struct Label {
    src: String,
    uri: String,
    val: String,
    #[serde(default)]
    neg: bool,
    cts: String,
    exp: Option<String>,
}

// Used for the queryLabels HTTP response.
#[derive(Deserialize)]
struct QueryLabelsResponse {
    labels: Vec<Label>,
}

// ---------------------------------------------------------------------------
// Spawn — manages per-labeler subscription tasks
// ---------------------------------------------------------------------------

pub fn spawn(state: AppState, mut subscriptions_rx: watch::Receiver<()>) {
    tokio::spawn(async move {
        let mut tasks: HashMap<String, JoinHandle<()>> = HashMap::new();

        loop {
            // Read all active subscriptions from the database.
            let active: Vec<(String,)> = crate::db::query_as(
                "SELECT did FROM happyview_labeler_subscriptions WHERE status = 'active'",
            )
            .fetch_all(&state.db)
            .await
            .unwrap_or_default();

            let active_dids: Vec<String> = active.into_iter().map(|(did,)| did).collect();

            // Stop tasks for removed/paused subscriptions.
            let to_remove: Vec<String> = tasks
                .keys()
                .filter(|did| !active_dids.contains(did))
                .cloned()
                .collect();

            for did in to_remove {
                if let Some(handle) = tasks.remove(&did) {
                    tracing::info!(did = %did, "stopping labeler subscription");
                    handle.abort();
                }
            }

            // Start tasks for new subscriptions.
            for did in &active_dids {
                if !tasks.contains_key(did) {
                    tracing::info!(did = %did, "starting labeler subscription");
                    let state = state.clone();
                    let did_clone = did.clone();
                    let handle = tokio::spawn(run_subscription(state, did_clone));
                    tasks.insert(did.clone(), handle);
                }
            }

            // Wait for a signal that subscriptions have changed.
            if subscriptions_rx.changed().await.is_err() {
                tracing::info!("labeler subscriptions channel closed, stopping");
                break;
            }
        }

        // Clean up all tasks on exit.
        for (_, handle) in tasks {
            handle.abort();
        }
    });
}

// ---------------------------------------------------------------------------
// Per-labeler reconnect loop
// ---------------------------------------------------------------------------

async fn run_subscription(state: AppState, did: String) {
    let mut backoff_secs: u64 = 2;
    const MAX_BACKOFF_SECS: u64 = 300; // 5 minutes

    loop {
        match run_subscription_once(&state, &did).await {
            Ok(()) => {
                tracing::info!(did = %did, "labeler subscription ended cleanly, reconnecting");
                backoff_secs = 2; // reset on clean disconnect
            }
            Err(e) => {
                tracing::warn!(did = %did, backoff = backoff_secs, "labeler subscription error: {e}");
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
        tracing::info!(did = %did, "reconnecting to labeler");
        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
    }
}

// ---------------------------------------------------------------------------
// Single connection lifecycle
// ---------------------------------------------------------------------------

async fn run_subscription_once(
    state: &AppState,
    did: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Resolve the labeler's service endpoint from its DID document.
    // Prefers #atproto_labeler, falls back to #atproto_pds.
    let pds_endpoint = profile::resolve_labeler_endpoint(&state.http, &state.config.plc_url, did)
        .await
        .map_err(|e| format!("failed to resolve labeler endpoint for {did}: {e:?}"))?;

    // Convert HTTP URL to WebSocket URL.
    let ws_url = http_to_ws(&pds_endpoint);

    // Read cursor from database.
    let cursor_sql = adapt_sql(
        "SELECT cursor FROM happyview_labeler_subscriptions WHERE did = ?",
        state.db_backend,
    );
    let cursor: Option<(Option<i64>,)> = crate::db::query_as(&cursor_sql)
        .bind(did)
        .fetch_optional(&state.db)
        .await?;

    let cursor_val = cursor.and_then(|(c,)| c).unwrap_or(0);

    let url = format!(
        "{}/xrpc/com.atproto.label.subscribeLabels?cursor={}",
        ws_url.trim_end_matches('/'),
        cursor_val
    );

    tracing::info!(did = %did, url = %url, "connecting to labeler");

    let request = url.into_client_request()?;

    // Manually establish TCP + TLS with HTTP/1.1 ALPN, then do the
    // WebSocket handshake over the established stream. This avoids
    // tokio-tungstenite's default TLS which may negotiate h2 via ALPN.
    let host = request
        .uri()
        .host()
        .ok_or("missing host in WebSocket URL")?
        .to_string();
    let port = request.uri().port_u16().unwrap_or(443);

    let tcp = TcpStream::connect((&*host, port)).await?;

    let _ = rustls::crypto::ring::default_provider().install_default();
    let root_store =
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let mut tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    tls_config.alpn_protocols = vec![b"http/1.1".to_vec()];

    let connector = tokio_rustls::TlsConnector::from(Arc::new(tls_config));
    let domain = rustls::pki_types::ServerName::try_from(host)?;
    let tls_stream = connector.connect(domain, tcp).await?;

    let (ws, _) = tokio_tungstenite::client_async(request, tls_stream).await?;

    tracing::info!(did = %did, "connected to labeler");

    log_event(
        &state.db,
        EventLog {
            event_type: "labeler.connected".to_string(),
            severity: Severity::Info,
            actor_did: None,
            subject: Some(did.to_string()),
            detail: serde_json::json!({ "did": did }),
        },
        state.db_backend,
    )
    .await;

    let (_write, mut read) = ws.split();
    let mut events_since_cursor_save: u64 = 0;
    let mut last_seq: i64 = cursor_val;

    loop {
        let msg = match read.next().await {
            Some(Ok(m)) => m,
            Some(Err(e)) => {
                tracing::warn!(did = %did, "labeler websocket read error: {e}");
                // Persist cursor before disconnecting.
                persist_cursor(&state.db, did, last_seq, state.db_backend).await;
                return Err(e.into());
            }
            None => {
                tracing::info!(did = %did, "labeler websocket stream ended");
                break;
            }
        };

        let bytes = match msg {
            Message::Binary(b) => b.to_vec(),
            Message::Close(_) => {
                tracing::info!(did = %did, "labeler websocket received close frame");
                break;
            }
            _ => continue,
        };

        // AT Protocol event streams use two concatenated DAG-CBOR objects:
        // 1. Frame header: { op: int, t: string? }
        // 2. Frame body: the actual message payload
        let message: SubscribeLabelsMessage = match parse_event_frame(&bytes) {
            Ok(Some(m)) => m,
            Ok(None) => continue, // non-message frame (error, info, etc.)
            Err(e) => {
                tracing::warn!(did = %did, "skipping unparseable labeler message: {e}");
                continue;
            }
        };

        last_seq = message.seq;

        for label in &message.labels {
            apply_label(state, label).await;
        }

        events_since_cursor_save += 1;
        if events_since_cursor_save >= 100 {
            persist_cursor(&state.db, did, last_seq, state.db_backend).await;
            events_since_cursor_save = 0;
        }
    }

    // Persist final cursor on disconnect.
    persist_cursor(&state.db, did, last_seq, state.db_backend).await;

    log_event(
        &state.db,
        EventLog {
            event_type: "labeler.disconnected".to_string(),
            severity: Severity::Warn,
            actor_did: None,
            subject: Some(did.to_string()),
            detail: serde_json::json!({ "did": did, "last_seq": last_seq }),
        },
        state.db_backend,
    )
    .await;

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse an AT Protocol event stream frame (two concatenated DAG-CBOR objects).
/// Returns `Ok(Some(msg))` for label messages, `Ok(None)` for other frame types.
fn parse_event_frame(
    bytes: &[u8],
) -> Result<Option<SubscribeLabelsMessage>, Box<dyn std::error::Error + Send + Sync>> {
    let mut cursor = std::io::Cursor::new(bytes);

    // Decode frame header.
    let header: FrameHeader = ciborium::from_reader(&mut cursor)?;

    // op=1 is a regular message, op=-1 is an error frame.
    if header.op != 1 {
        return Ok(None);
    }

    // Only process #labels messages.
    match header.t.as_deref() {
        Some("#labels") => {}
        _ => return Ok(None),
    }

    // Decode the body (remaining bytes after header).
    let message: SubscribeLabelsMessage = ciborium::from_reader(&mut cursor)?;
    Ok(Some(message))
}

fn http_to_ws(url: &str) -> String {
    let base = url.trim_end_matches('/');
    if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        format!("ws://{base}")
    }
}

/// Persist a label received from a subscribed upstream labeler.
///
/// Before touching the DB we run the trigger-keyed script chain (computed
/// from `label.uri` — `labeler.apply:<nsid>` for at-uri subjects,
/// `labeler.apply:_actor` for bare DIDs). The script can rewrite any field
/// of the label (including `val` or `neg`) or return nil to skip
/// persistence. Failure is fail-open: a dead-lettered script proceeds with
/// the original label, so a buggy script can't permanently break the
/// firehose.
async fn apply_label(state: &AppState, label: &Label) {
    let event = crate::lua::LabelAppliedEvent {
        src: label.src.clone(),
        uri: label.uri.clone(),
        val: label.val.clone(),
        neg: label.neg,
        cts: label.cts.clone(),
        exp: label.exp.clone(),
    };
    let final_label = match crate::lua::run_label_applied_script(state, event).await {
        crate::lua::LabelHookOutcome::Continue(next) => next,
        crate::lua::LabelHookOutcome::Skip => {
            tracing::debug!(
                src = %label.src, uri = %label.uri, val = %label.val,
                "label.applied script skipped persistence"
            );
            return;
        }
    };

    let db = &state.db;
    let backend = state.db_backend;

    if final_label.neg {
        // Negation label — remove it.
        let delete_sql = adapt_sql(
            "DELETE FROM happyview_labels WHERE src = ? AND uri = ? AND val = ?",
            backend,
        );
        if let Err(e) = crate::db::query(&delete_sql)
            .bind(&final_label.src)
            .bind(&final_label.uri)
            .bind(&final_label.val)
            .execute(db)
            .await
        {
            tracing::warn!(
                src = %final_label.src, uri = %final_label.uri, val = %final_label.val,
                "failed to delete negated label: {e}"
            );
        }
    } else {
        // The label lexicon types `exp` as a datetime, so an upstream labeler
        // may send any offset, while every reader compares the column as text
        // against a `+00:00` cutoff — an offset left as sent orders by wall
        // clock, which serves an expired label as live for up to fourteen
        // hours.
        //
        // An expiry that does not parse is stored as NULL rather than
        // verbatim, because where a raw value happens to sort is what would
        // decide the label's fate: `whenever` sorts above every cutoff and
        // reads as permanent, an empty string sorts below every cutoff and
        // reads as expired, and neither is anything the sender said. NULL is
        // the one deterministic reading, and the warn line carries the
        // rejected value so an operator can see a label arrived with an
        // expiry at all. The label itself still persists, since this path is
        // fail-open on the firehose.
        let exp = match final_label.exp.as_deref().map(normalize_rfc3339) {
            Some(Ok(normalised)) => Some(normalised),
            Some(Err(e)) => {
                tracing::warn!(
                    src = %final_label.src, uri = %final_label.uri, val = %final_label.val,
                    "dropping label expiry: {e}"
                );
                None
            }
            None => None,
        };

        // Normal label — upsert. Store timestamps as RFC3339 strings for portability.
        let insert_sql = adapt_sql(
            r#"
            INSERT INTO happyview_labels (src, uri, val, cts, exp)
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT (src, uri, val) DO UPDATE
                SET cts = EXCLUDED.cts,
                    exp = EXCLUDED.exp
            "#,
            backend,
        );

        if let Err(e) = crate::db::query(&insert_sql)
            .bind(&final_label.src)
            .bind(&final_label.uri)
            .bind(&final_label.val)
            .bind(&final_label.cts)
            .bind(&exp)
            .execute(db)
            .await
        {
            tracing::warn!(
                src = %final_label.src, uri = %final_label.uri, val = %final_label.val,
                "failed to upsert label: {e}"
            );
        }
    }
}

async fn persist_cursor(db: &sqlx::AnyPool, did: &str, seq: i64, backend: DatabaseBackend) {
    let now = now_rfc3339();
    let update_sql = adapt_sql(
        "UPDATE happyview_labeler_subscriptions SET cursor = ?, updated_at = ? WHERE did = ?",
        backend,
    );
    if let Err(e) = crate::db::query(&update_sql)
        .bind(seq)
        .bind(&now)
        .bind(did)
        .execute(db)
        .await
    {
        tracing::warn!(did = %did, seq, "failed to persist labeler cursor: {e}");
    }
}

// ---------------------------------------------------------------------------
// Backfill labels for a specific URI
// ---------------------------------------------------------------------------

/// Spawn a background task to backfill labels for a given URI from all active
/// labeler subscriptions. Fire-and-forget.
pub fn backfill_labels_for_uri(state: Arc<AppState>, uri: String) {
    tokio::spawn(async move {
        if let Err(e) = backfill_labels_for_uri_inner(&state, &uri).await {
            tracing::warn!(uri = %uri, "failed to backfill labels: {e}");
        }
    });
}

async fn backfill_labels_for_uri_inner(
    state: &AppState,
    uri: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let subscriptions: Vec<(String,)> = crate::db::query_as(
        "SELECT did FROM happyview_labeler_subscriptions WHERE status = 'active'",
    )
    .fetch_all(&state.db)
    .await?;

    for (labeler_did,) in subscriptions {
        if let Err(e) = backfill_from_labeler(state, &labeler_did, uri).await {
            tracing::warn!(
                labeler = %labeler_did, uri = %uri,
                "failed to backfill labels from labeler: {e}"
            );
        }
    }

    Ok(())
}

async fn backfill_from_labeler(
    state: &AppState,
    labeler_did: &str,
    uri: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let pds_endpoint =
        profile::resolve_pds_endpoint(&state.http, &state.config.plc_url, labeler_did)
            .await
            .map_err(|e| format!("failed to resolve PDS for {labeler_did}: {e:?}"))?;

    let encoded_uri = urlencoding::encode(uri);
    let url = format!(
        "{}/xrpc/com.atproto.label.queryLabels?uriPatterns={}",
        pds_endpoint.trim_end_matches('/'),
        encoded_uri
    );

    let resp = state.http.get(&url).send().await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("queryLabels returned {status}: {body}").into());
    }

    let response: QueryLabelsResponse = resp.json().await?;

    for label in &response.labels {
        apply_label(state, label).await;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Label garbage collection
// ---------------------------------------------------------------------------

/// Rows deleted per statement, matching `event_log::SWEEP_BATCH_SIZE` and
/// `jobs::native::delete_collection::BATCH_SIZE` for the reasons recorded
/// there: one unbounded `DELETE` accumulates every dirtied page in the WAL
/// until it commits, which inflates disk instead of reducing it, and holds the
/// SQLite write lock against Jetstream ingest for the whole transaction.
/// Nothing bounds the selection here — it is every expired label an instance
/// holds, however long it has held them.
const EXPIRY_BATCH_SIZE: i64 = 5000;

/// Delete every label whose expiry has passed, returning how many went.
pub async fn delete_expired_labels(
    db: &sqlx::AnyPool,
    backend: DatabaseBackend,
) -> Result<u64, sqlx::Error> {
    // `exp` is TEXT on both backends, so the cutoff is bound as text rather
    // than read from a SQL clock: Postgres's `NOW()` is a `timestamptz` it
    // refuses to compare against text at all, whatever the table holds, and
    // SQLite's `datetime('now')` is space-separated, where byte 10 sorts below
    // the column's `T` and so retains every expiry falling on the current UTC
    // day.
    //
    // Two forms are collected. The `+00:00` form `normalize_rfc3339` writes
    // orders exactly. A `Z` suffix orders correctly on every digit up to the
    // second, differing from the cutoff only at the offset position, where `Z`
    // sorts above `+`; a `Z` value expiring inside the cutoff's own second
    // therefore survives one more hourly tick, which is an error under a
    // second and in the conservative direction. `Z` is the network's
    // convention and so is most of what an instance holds from before
    // normalisation, which is why skipping it would collect almost nothing.
    //
    // Every other shape is skipped rather than ordered, because a text
    // comparison orders by wall clock and gets those wrong destructively: a
    // negative offset reads *earlier* than the instant it denotes, so a live
    // label would be deleted by up to the offset, as would a same-day future
    // expiry in the space-separated `+00` form
    // `20260318000000_uuid_to_text.sql` wrote for any row it found, or any
    // unparseable text that happens to sort low. Skipping leaves such a row
    // with the behaviour the readers already give it, pending a repair that
    // rewrites it; casting instead would delete nothing at all, since one
    // unparseable row makes the cast throw and takes the statement with it.
    //
    // The `IS NOT NULL` and `exp < ?` terms are kept so the partial index on
    // `exp` still matches.
    let sql = adapt_sql(
        "DELETE FROM happyview_labels WHERE (src, uri, val) IN (
             SELECT src, uri, val FROM happyview_labels
             WHERE exp IS NOT NULL
               AND (exp LIKE '____-__-__T__:__:__+00:00'
                    OR exp LIKE '____-__-__T__:__:__.%+00:00'
                    OR exp LIKE '____-__-__T__:__:__Z'
                    OR exp LIKE '____-__-__T__:__:__.%Z')
               AND exp < ?
             LIMIT ?
         )",
        backend,
    );

    // One cutoff for the whole sweep, so a batch cannot be selected against an
    // instant later than the one the sweep started at.
    let cutoff = now_rfc3339();
    let mut deleted: u64 = 0;

    loop {
        let affected = crate::db::query(&sql)
            .bind(&cutoff)
            .bind(EXPIRY_BATCH_SIZE)
            .execute(db)
            .await?
            .rows_affected();

        deleted += affected;

        // A short batch means the selection is drained. Waiting for exactly
        // zero would keep going against a firehose that can deliver a label
        // already past the cutoff, and the hourly cadence makes finishing on
        // the next pass free.
        if (affected as i64) < EXPIRY_BATCH_SIZE {
            return Ok(deleted);
        }
    }
}

/// Delete every label whose subject is no longer indexed, returning how many
/// went.
///
/// Only record labels can be orphaned, so the sweep is confined to `at://`
/// subjects. An account label's subject is a bare DID, which matches no record
/// URI and so read as orphaned on its first pass — every account-level label an
/// instance held was deleted within the hour. Account labels are retained until
/// they expire or are negated.
pub async fn delete_orphaned_labels(db: &sqlx::AnyPool) -> Result<u64, sqlx::Error> {
    crate::db::query(
        "DELETE FROM happyview_labels WHERE uri LIKE 'at://%' AND NOT EXISTS (SELECT 1 FROM happyview_records WHERE happyview_records.uri = happyview_labels.uri)",
    )
    .execute(db)
    .await
    .map(|r| r.rows_affected())
}

/// Hourly task to clean up expired and orphaned labels.
pub async fn spawn_label_gc(db: sqlx::AnyPool, backend: DatabaseBackend) {
    tracing::info!("starting label garbage collection task");

    let interval = tokio::time::Duration::from_secs(3600); // 1 hour

    loop {
        tokio::time::sleep(interval).await;

        // Each sweep is attempted independently: an expiry comparison that
        // fails says nothing about whether a label's subject still exists.
        let expired_count = match delete_expired_labels(&db, backend).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("failed to clean up expired labels: {e}");
                0
            }
        };

        let orphaned_count = match delete_orphaned_labels(&db).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("failed to clean up orphaned labels: {e}");
                0
            }
        };

        let total = expired_count + orphaned_count;
        if total > 0 {
            tracing::info!(
                expired = expired_count,
                orphaned = orphaned_count,
                "cleaned up {total} labels"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, FixedOffset, Utc};
    use serial_test::serial;

    use super::*;

    const TEST_SRC: &str = "did:plc:labelgctest";

    /// One label per `exp` shape the column can hold, named for what the sweep
    /// must do with it.
    ///
    /// The expired normalised pair sits at the start of the current UTC day, so
    /// it shares the cutoff's date prefix and the character at the date/time
    /// boundary decides the comparison — the only arrangement in which a
    /// space-separated comparand gets the answer wrong. `live-negative-offset`
    /// is the converse: an instant four hours ahead written in a zone five
    /// hours behind UTC, so its wall clock reads an hour *before* the cutoff
    /// and a text comparison alone would delete a live label.
    ///
    /// The `Z` pair is an hour either side of the cutoff rather than at the
    /// start of the day, because a `Z` value inside the cutoff's own second is
    /// the one case that form orders imprecisely, and seeding it would make
    /// the run's outcome depend on the second it started in.
    async fn seed_labels(db: &sqlx::AnyPool, backend: DatabaseBackend) {
        let now = Utc::now();
        let start_of_day = now
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .expect("midnight is a valid time")
            .and_utc();
        let west = FixedOffset::west_opt(5 * 3600).expect("a five-hour western offset");
        let z = |dt: chrono::DateTime<Utc>| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string();

        let rows: [(&str, Option<String>); 10] = [
            ("expired-normalised", Some(start_of_day.to_rfc3339())),
            (
                "expired-normalised-fraction",
                Some("2020-01-01T00:00:00.123456+00:00".to_string()),
            ),
            ("expired-z", Some(z(now - Duration::hours(1)))),
            (
                "expired-z-fraction",
                Some("2020-01-01T00:00:00.123456Z".to_string()),
            ),
            (
                "live-normalised",
                Some((now + Duration::hours(1)).to_rfc3339()),
            ),
            ("live-z", Some(z(now + Duration::hours(1)))),
            (
                "live-negative-offset",
                Some((now + Duration::hours(4)).with_timezone(&west).to_rfc3339()),
            ),
            (
                "future-space-form",
                Some(
                    (now + Duration::hours(1))
                        .format("%Y-%m-%d %H:%M:%S+00")
                        .to_string(),
                ),
            ),
            ("unparseable", Some("whenever".to_string())),
            ("no-expiry", None),
        ];

        let sql = adapt_sql(
            "INSERT INTO happyview_labels (src, uri, val, cts, exp) VALUES (?, ?, ?, ?, ?)",
            backend,
        );
        for (val, exp) in rows {
            crate::db::query(&sql)
                .bind(TEST_SRC)
                .bind("at://did:plc:subject/app.test.post/1")
                .bind(val)
                .bind("2026-01-01T00:00:00+00:00")
                .bind(exp)
                .execute(db)
                .await
                .expect("seed a label");
        }
    }

    /// What [`seed_labels`] leaves behind: every live label, including the one
    /// a text comparison alone reads as past, the one with no expiry, and the
    /// two remaining shapes the cutoff cannot order, which are skipped rather
    /// than guessed at.
    fn expected_survivors() -> Vec<String> {
        [
            "future-space-form",
            "live-negative-offset",
            "live-normalised",
            "live-z",
            "no-expiry",
            "unparseable",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    async fn surviving_vals(db: &sqlx::AnyPool, backend: DatabaseBackend) -> Vec<String> {
        let sql = adapt_sql(
            "SELECT val FROM happyview_labels WHERE src = ? ORDER BY val",
            backend,
        );
        let rows: Vec<(String,)> = crate::db::query_as(&sql)
            .bind(TEST_SRC)
            .fetch_all(db)
            .await
            .expect("read back the seeded labels");
        rows.into_iter().map(|(val,)| val).collect()
    }

    async fn forget_seeded_labels(db: &sqlx::AnyPool, backend: DatabaseBackend) {
        let sql = adapt_sql("DELETE FROM happyview_labels WHERE src = ?", backend);
        crate::db::query(&sql)
            .bind(TEST_SRC)
            .execute(db)
            .await
            .expect("clear the seeded labels");
    }

    /// The sweep against SQLite, where a SQL-clock cutoff is well-typed and so
    /// runs: `datetime('now')` is space-separated, which sorts below the
    /// column's `T`, so an expiry on the cutoff's own UTC day reads as live
    /// whatever its hour. Only the orderable expiries go, and the live
    /// negative offset stays.
    #[tokio::test]
    async fn the_sweep_deletes_only_orderable_expiries_on_sqlite() {
        let db = crate::test_support::migrated_memory_pool().await;
        let backend = DatabaseBackend::Sqlite;

        seed_labels(&db, backend).await;

        let deleted = delete_expired_labels(&db, backend)
            .await
            .expect("the sweep should run");

        assert_eq!(
            deleted, 4,
            "every expiry in an orderable form is past the cutoff"
        );
        assert_eq!(
            surviving_vals(&db, backend).await,
            expected_survivors(),
            "a live label, and any shape the cutoff cannot order, stays"
        );
    }

    /// The same sweep against Postgres, which refuses to compare TEXT against
    /// `timestamptz` at all: a SQL-clock cutoff fails at parse time whatever
    /// the table holds, and `spawn_label_gc` reports that as a count of zero,
    /// so only executing the statement and inspecting its `Result`
    /// distinguishes a sweep that ran from one that could not.
    ///
    /// The skip below is invisible under libtest's captured output, so a
    /// `cargo test --lib` run with no `TEST_DATABASE_URL` — which is what
    /// `ci.yml`'s `unit-tests` job does — reports `ok` having proved nothing.
    /// The job that proves this one is `e2e-tests`, whose `cargo test --tests`
    /// includes the lib tests with a URL set.
    #[tokio::test]
    #[serial]
    async fn the_sweep_deletes_only_orderable_expiries_on_postgres() {
        if std::env::var("TEST_DATABASE_URL").is_err() {
            eprintln!("skipped (TEST_DATABASE_URL not set)");
            return;
        }
        let url = std::env::var("TEST_DATABASE_URL").expect("the caller checks it first");
        let backend = DatabaseBackend::from_url(&url);
        let db = crate::db::connect(&url, backend).await;

        forget_seeded_labels(&db, backend).await;
        // Drain anything already expired, so the count below is this test's
        // own. `rows_affected` is table-wide and this database is shared, so
        // the exact count holds only while nothing else writes an expiring
        // label to it: cargo runs one test binary at a time and no other test
        // in this one touches labels, but two suites pointed at one database
        // by hand would break it. The survivor assertion is scoped by `src`
        // and holds either way.
        delete_expired_labels(&db, backend)
            .await
            .expect("the sweep should run against an unseeded table");

        seed_labels(&db, backend).await;

        let deleted = delete_expired_labels(&db, backend)
            .await
            .expect("the sweep should run");

        assert_eq!(
            deleted, 4,
            "every expiry in an orderable form is past the cutoff"
        );
        assert_eq!(
            surviving_vals(&db, backend).await,
            expected_survivors(),
            "a live label, and any shape the cutoff cannot order, stays"
        );

        forget_seeded_labels(&db, backend).await;
    }

    /// One batch is a bound on the statement, not on the sweep: a backlog
    /// larger than `EXPIRY_BATCH_SIZE` has to be drained by the loop, and a
    /// sweep that stopped after its first statement would leave the remainder
    /// behind for an hour.
    #[tokio::test]
    async fn the_sweep_drains_a_backlog_larger_than_one_batch() {
        let db = crate::test_support::migrated_memory_pool().await;
        let backend = DatabaseBackend::Sqlite;
        let rows = EXPIRY_BATCH_SIZE + 1;

        crate::db::query(
            "INSERT INTO happyview_labels (src, uri, val, cts, exp)
             WITH RECURSIVE seq(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM seq WHERE i < ?)
             SELECT ?, 'at://did:plc:subject/app.test.post/1', 'bulk-' || i,
                    '2026-01-01T00:00:00+00:00', '2020-01-01T00:00:00+00:00'
             FROM seq",
        )
        .bind(rows)
        .bind(TEST_SRC)
        .execute(&db)
        .await
        .expect("seed a backlog");

        let deleted = delete_expired_labels(&db, backend)
            .await
            .expect("the sweep should run");

        assert_eq!(
            deleted, rows as u64,
            "every expired label goes in one sweep"
        );
        assert!(
            surviving_vals(&db, backend).await.is_empty(),
            "nothing is left for the next pass"
        );
    }

    /// An upstream labeler may send any offset the datetime format permits,
    /// and a reader comparing text against a `+00:00` cutoff would read a
    /// non-UTC one by wall clock.
    #[tokio::test]
    async fn a_labels_expiry_is_stored_as_a_utc_offset() {
        let db = crate::test_support::migrated_memory_pool().await;
        let state = crate::test_support::test_state_with_pool(db.clone());

        for (val, exp) in [
            ("z-suffixed", Some("2030-06-01T12:00:00Z".to_string())),
            ("far-offset", Some("2030-06-01T21:00:00+09:00".to_string())),
            ("unparseable", Some("whenever".to_string())),
        ] {
            apply_label(
                &state,
                &Label {
                    src: TEST_SRC.to_string(),
                    uri: "at://did:plc:subject/app.test.post/1".to_string(),
                    val: val.to_string(),
                    neg: false,
                    cts: "2026-01-01T00:00:00+00:00".to_string(),
                    exp,
                },
            )
            .await;
        }

        let rows: Vec<(String, Option<String>)> =
            crate::db::query_as("SELECT val, exp FROM happyview_labels ORDER BY val")
                .fetch_all(&db)
                .await
                .expect("read back the labels");

        let stored: std::collections::HashMap<String, Option<String>> = rows.into_iter().collect();
        assert_eq!(
            stored.len(),
            3,
            "every label persists, whatever its expiry looked like: {stored:?}"
        );
        assert_eq!(
            stored["far-offset"].as_deref(),
            Some("2030-06-01T12:00:00+00:00"),
            "an offset is converted to the same instant in UTC"
        );
        assert_eq!(
            stored["z-suffixed"].as_deref(),
            Some("2030-06-01T12:00:00+00:00"),
            "a Z suffix is rewritten to the offset form the column is compared in"
        );
        assert_eq!(
            stored["unparseable"], None,
            "an expiry no comparison can order is dropped for a deterministic NULL"
        );
    }
}

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
use crate::event_log::{EventLog, Severity, log_event};
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
            let active: Vec<(String,)> =
                sqlx::query_as("SELECT did FROM labeler_subscriptions WHERE status = 'active'")
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
        "SELECT cursor FROM labeler_subscriptions WHERE did = ?",
        state.db_backend,
    );
    let cursor: Option<(Option<i64>,)> = sqlx::query_as(&cursor_sql)
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
            "DELETE FROM labels WHERE src = ? AND uri = ? AND val = ?",
            backend,
        );
        if let Err(e) = sqlx::query(&delete_sql)
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
        // Normal label — upsert. Store timestamps as RFC3339 strings for portability.
        let insert_sql = adapt_sql(
            r#"
            INSERT INTO labels (src, uri, val, cts, exp)
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT (src, uri, val) DO UPDATE
                SET cts = EXCLUDED.cts,
                    exp = EXCLUDED.exp
            "#,
            backend,
        );

        if let Err(e) = sqlx::query(&insert_sql)
            .bind(&final_label.src)
            .bind(&final_label.uri)
            .bind(&final_label.val)
            .bind(&final_label.cts)
            .bind(&final_label.exp)
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
        "UPDATE labeler_subscriptions SET cursor = ?, updated_at = ? WHERE did = ?",
        backend,
    );
    if let Err(e) = sqlx::query(&update_sql)
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
    let subscriptions: Vec<(String,)> =
        sqlx::query_as("SELECT did FROM labeler_subscriptions WHERE status = 'active'")
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

/// Hourly task to clean up expired and orphaned labels.
pub async fn spawn_label_gc(db: sqlx::AnyPool, backend: DatabaseBackend) {
    tracing::info!("starting label garbage collection task");

    let interval = tokio::time::Duration::from_secs(3600); // 1 hour

    // Build database-specific cleanup query for expired labels
    let expired_sql = match backend {
        DatabaseBackend::Sqlite => {
            "DELETE FROM labels WHERE exp IS NOT NULL AND exp < datetime('now')".to_string()
        }
        DatabaseBackend::Postgres => {
            "DELETE FROM labels WHERE exp IS NOT NULL AND exp < NOW()".to_string()
        }
    };

    loop {
        tokio::time::sleep(interval).await;

        // Delete expired labels.
        let expired = sqlx::query(&expired_sql).execute(&db).await;

        let expired_count = match expired {
            Ok(r) => r.rows_affected(),
            Err(e) => {
                tracing::warn!("failed to clean up expired labels: {e}");
                0
            }
        };

        // Delete orphaned labels (no matching record).
        let orphaned = sqlx::query(
            "DELETE FROM labels WHERE NOT EXISTS (SELECT 1 FROM records WHERE records.uri = labels.uri)",
        )
        .execute(&db)
        .await;

        let orphaned_count = match orphaned {
            Ok(r) => r.rows_affected(),
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

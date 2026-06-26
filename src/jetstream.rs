use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use sqlx::AnyPool;
use tokio::sync::{Semaphore, watch};
use tokio_tungstenite::tungstenite::Message;

use crate::AppState;
use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::event_log::{EventLog, Severity, log_event};
use crate::record_handler::{self, LEXICON_SCHEMA_COLLECTION, RecordEvent};

// ---------------------------------------------------------------------------
// Jetstream event types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct JetstreamEvent {
    #[serde(rename = "kind")]
    event_type: String,
    did: String,
    time_us: i64,
    commit: Option<CommitEvent>,
    identity: Option<IdentityEvent>,
}

#[derive(Deserialize)]
struct CommitEvent {
    operation: String,
    collection: String,
    rkey: String,
    record: Option<Value>,
    cid: Option<String>,
}

#[derive(Deserialize)]
struct IdentityEvent {
    did: String,
    handle: Option<String>,
}

// ---------------------------------------------------------------------------
// Cursor persistence
// ---------------------------------------------------------------------------

async fn load_cursor(db: &AnyPool, backend: DatabaseBackend) -> Option<i64> {
    let sql = adapt_sql(
        "SELECT value FROM happyview_instance_settings WHERE key = ?",
        backend,
    );
    let row: Option<(String,)> = sqlx::query_as(&sql)
        .bind("jetstream_cursor")
        .fetch_optional(db)
        .await
        .ok()?;
    row.and_then(|(v,)| v.parse::<i64>().ok())
}

async fn save_cursor(db: &AnyPool, backend: DatabaseBackend, cursor: i64) {
    let now = now_rfc3339();
    let sql = adapt_sql(
        r#"
        INSERT INTO happyview_instance_settings (key, value, updated_at)
        VALUES (?, ?, ?)
        ON CONFLICT (key) DO UPDATE SET value = ?, updated_at = ?
        "#,
        backend,
    );
    if let Err(e) = sqlx::query(&sql)
        .bind("jetstream_cursor")
        .bind(cursor.to_string())
        .bind(&now)
        .bind(cursor.to_string())
        .bind(&now)
        .execute(db)
        .await
    {
        tracing::warn!("failed to save jetstream cursor: {e}");
    }
}

// ---------------------------------------------------------------------------
// URL builder
// ---------------------------------------------------------------------------

fn build_subscribe_url(base_url: &str, collections: &[String]) -> String {
    let base = base_url.trim_end_matches('/');
    let mut url = format!("{base}/subscribe?compress=false");

    let mut has_lexicon_schema = false;
    for col in collections {
        url.push_str(&format!("&wantedCollections={col}"));
        if col == LEXICON_SCHEMA_COLLECTION {
            has_lexicon_schema = true;
        }
    }

    if !has_lexicon_schema {
        url.push_str(&format!("&wantedCollections={LEXICON_SCHEMA_COLLECTION}"));
    }

    url
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Spawn a background task that connects to Jetstream's WebSocket stream and
/// processes record + identity events. Uses exponential backoff on disconnects.
pub fn spawn(state: AppState, mut collections_rx: watch::Receiver<Vec<String>>) {
    tokio::spawn(async move {
        let mut backoff = Duration::from_secs(2);
        let max_backoff = Duration::from_secs(60);

        loop {
            match run(&state, &mut collections_rx).await {
                Ok(()) => {
                    // Clean reconnect (collection change) — reset backoff.
                    backoff = Duration::from_secs(2);
                    tracing::info!("jetstream reconnecting due to collection change");
                }
                Err(e) => {
                    tracing::warn!("jetstream disconnected: {e}");
                    tracing::info!(
                        backoff_secs = backoff.as_secs(),
                        "reconnecting to jetstream after backoff"
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(max_backoff);
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Connection loop
// ---------------------------------------------------------------------------

async fn run(
    state: &AppState,
    collections_rx: &mut watch::Receiver<Vec<String>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let db = &state.db;
    let backend = state.db_backend;

    // Build subscribe URL with current collections.
    let collections = collections_rx.borrow().clone();
    let url = build_subscribe_url(&state.config.jetstream_url, &collections);

    // Load cursor from DB; skip if older than 72 hours.
    let cursor = load_cursor(db, backend).await;
    let subscribe_url = if let Some(c) = cursor {
        let now_us = chrono::Utc::now().timestamp_micros();
        let age_hours = (now_us - c) as f64 / 3_600_000_000.0;
        if age_hours > 72.0 {
            tracing::warn!(
                cursor = c,
                age_hours = age_hours,
                "jetstream cursor is older than 72 hours, skipping it"
            );
            url
        } else {
            format!("{url}&cursor={c}")
        }
    } else {
        url
    };

    tracing::info!(url = %subscribe_url, "connecting to jetstream");

    let (ws, _): (
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        _,
    ) = tokio::time::timeout(
        Duration::from_secs(15),
        tokio_tungstenite::connect_async(&subscribe_url),
    )
    .await
    .map_err(|_| "jetstream websocket connection timed out after 15s")??;

    log_event(
        db,
        EventLog {
            event_type: "jetstream.connected".to_string(),
            severity: Severity::Info,
            actor_did: None,
            subject: None,
            detail: serde_json::json!({ "url": subscribe_url }),
        },
        backend,
    )
    .await;

    let (_write, mut read) = ws.split();

    let semaphore = Arc::new(Semaphore::new(50));

    // Cursor batching state: save every 1000 events or 5 seconds.
    let mut latest_cursor: Option<i64> = cursor;
    let mut events_since_flush: u64 = 0;
    let mut last_flush = Instant::now();
    let flush_interval = Duration::from_secs(5);
    const FLUSH_EVENT_THRESHOLD: u64 = 1000;

    loop {
        tokio::select! {
            msg = read.next() => {
                let msg = match msg {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => {
                        tracing::warn!("jetstream websocket read error: {e}");
                        // Flush cursor before returning error.
                        if let Some(c) = latest_cursor {
                            save_cursor(db, backend, c).await;
                        }
                        log_event(
                            db,
                            EventLog {
                                event_type: "jetstream.disconnected".to_string(),
                                severity: Severity::Warn,
                                actor_did: None,
                                subject: None,
                                detail: serde_json::json!({ "reason": e.to_string() }),
                            },
                            backend,
                        )
                        .await;
                        return Err(e.into());
                    }
                    None => {
                        tracing::info!("jetstream websocket stream ended");
                        break;
                    }
                };

                let text = match msg {
                    Message::Text(t) => t,
                    Message::Close(_) => {
                        tracing::info!("jetstream websocket received close frame");
                        break;
                    }
                    other => {
                        tracing::debug!(msg_type = ?other, "ignoring non-text websocket message");
                        continue;
                    }
                };

                let event: JetstreamEvent = match serde_json::from_str(&text) {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::warn!("skipping unparseable jetstream event: {e}");
                        continue;
                    }
                };

                // Update cursor tracking.
                latest_cursor = Some(event.time_us);
                events_since_flush += 1;

                match event.event_type.as_str() {
                    "commit" => {
                        if let Some(commit) = event.commit {
                            tracing::debug!(
                                operation = %commit.operation,
                                collection = %commit.collection,
                                did = %event.did,
                                rkey = %commit.rkey,
                                "received commit event from jetstream"
                            );

                            let record_event = RecordEvent {
                                did: event.did.clone(),
                                collection: commit.collection,
                                rkey: commit.rkey,
                                action: commit.operation,
                                record: commit.record,
                                cid: commit.cid,
                            };

                            let sem = semaphore.clone();
                            let state = state.clone();
                            tokio::spawn(async move {
                                let _permit = sem.acquire().await.unwrap();
                                record_handler::handle_record_event(&state, &record_event).await;
                            });
                        }
                    }
                    "identity" => {
                        if let Some(identity) = event.identity {
                            tracing::debug!(
                                did = %identity.did,
                                handle = ?identity.handle,
                                "received identity event from jetstream"
                            );
                        }
                    }
                    "account" => {
                        tracing::debug!(
                            did = %event.did,
                            "received account event from jetstream (ignored)"
                        );
                    }
                    other => {
                        tracing::debug!(event_type = %other, "unknown jetstream event type");
                    }
                }

                // Flush cursor periodically.
                if (events_since_flush >= FLUSH_EVENT_THRESHOLD
                    || last_flush.elapsed() >= flush_interval)
                    && let Some(c) = latest_cursor
                {
                    save_cursor(db, backend, c).await;
                    events_since_flush = 0;
                    last_flush = Instant::now();
                }
            }
            _ = collections_rx.changed() => {
                let new_collections = collections_rx.borrow_and_update().clone();
                tracing::info!(?new_collections, "collection filter changed, reconnecting to jetstream");

                // Flush cursor before reconnecting.
                if let Some(c) = latest_cursor {
                    save_cursor(db, backend, c).await;
                }

                return Ok(());
            }
        }
    }

    // Stream ended cleanly — flush cursor and log disconnect.
    if let Some(c) = latest_cursor {
        save_cursor(db, backend, c).await;
    }

    log_event(
        db,
        EventLog {
            event_type: "jetstream.disconnected".to_string(),
            severity: Severity::Warn,
            actor_did: None,
            subject: None,
            detail: serde_json::json!({ "reason": "connection closed" }),
        },
        backend,
    )
    .await;

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_subscribe_url_basic() {
        let url = build_subscribe_url(
            "https://jetstream.example.com",
            &["app.bsky.feed.post".to_string()],
        );
        assert!(url.starts_with("https://jetstream.example.com/subscribe?compress=false"));
        assert!(url.contains("&wantedCollections=app.bsky.feed.post"));
        assert!(url.contains(&format!("&wantedCollections={LEXICON_SCHEMA_COLLECTION}")));
    }

    #[test]
    fn test_build_subscribe_url_strips_trailing_slash() {
        let url = build_subscribe_url(
            "https://jetstream.example.com/",
            &["app.bsky.feed.post".to_string()],
        );
        assert!(url.starts_with("https://jetstream.example.com/subscribe"));
        assert!(!url.contains("//subscribe"));
    }

    #[test]
    fn test_build_subscribe_url_does_not_duplicate_lexicon_schema() {
        let url = build_subscribe_url(
            "https://jetstream.example.com",
            &[
                "app.bsky.feed.post".to_string(),
                LEXICON_SCHEMA_COLLECTION.to_string(),
            ],
        );
        let count = url.matches(LEXICON_SCHEMA_COLLECTION).count();
        assert_eq!(
            count, 1,
            "lexicon schema collection should appear exactly once"
        );
    }

    #[test]
    fn test_build_subscribe_url_empty_collections() {
        let url = build_subscribe_url("https://jetstream.example.com", &[]);
        assert!(url.contains(&format!("&wantedCollections={LEXICON_SCHEMA_COLLECTION}")));
    }

    #[test]
    fn test_deserialize_commit_event() {
        let json = r#"{
            "kind": "commit",
            "did": "did:plc:abc123",
            "time_us": 1700000000000000,
            "commit": {
                "operation": "create",
                "collection": "app.bsky.feed.post",
                "rkey": "3k2y6e7wh4k2a",
                "record": {"text": "hello world", "$type": "app.bsky.feed.post"},
                "cid": "bafyreiabc123"
            }
        }"#;

        let event: JetstreamEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event_type, "commit");
        assert_eq!(event.did, "did:plc:abc123");
        assert_eq!(event.time_us, 1700000000000000);

        let commit = event.commit.unwrap();
        assert_eq!(commit.operation, "create");
        assert_eq!(commit.collection, "app.bsky.feed.post");
        assert_eq!(commit.rkey, "3k2y6e7wh4k2a");
        assert!(commit.record.is_some());
        assert_eq!(commit.cid.as_deref(), Some("bafyreiabc123"));
    }

    #[test]
    fn test_deserialize_identity_event() {
        let json = r#"{
            "kind": "identity",
            "did": "did:plc:abc123",
            "time_us": 1700000000000000,
            "identity": {
                "did": "did:plc:abc123",
                "handle": "alice.bsky.social"
            }
        }"#;

        let event: JetstreamEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event_type, "identity");

        let identity = event.identity.unwrap();
        assert_eq!(identity.did, "did:plc:abc123");
        assert_eq!(identity.handle.as_deref(), Some("alice.bsky.social"));
    }

    #[test]
    fn test_deserialize_delete_commit_no_record() {
        let json = r#"{
            "kind": "commit",
            "did": "did:plc:abc123",
            "time_us": 1700000000000000,
            "commit": {
                "operation": "delete",
                "collection": "app.bsky.feed.post",
                "rkey": "3k2y6e7wh4k2a"
            }
        }"#;

        let event: JetstreamEvent = serde_json::from_str(json).unwrap();
        let commit = event.commit.unwrap();
        assert_eq!(commit.operation, "delete");
        assert!(commit.record.is_none());
        assert!(commit.cid.is_none());
    }
}

//! The Jetstream consumer.
//!
//! One task reads the websocket and hands each event to the worker that owns
//! its DID (`workers`), which applies it (`apply`). The cursor saved is the
//! applied watermark (`watermark`, `cursor`), never the last frame read, so a
//! crash re-delivers events rather than skipping them. The v2
//! `subscribeEvents` stream is preferred, zstd-compressed with the server's
//! dictionary (`compression`).

pub mod apply;
pub mod compression;
pub mod config;
pub mod cursor;
pub mod event;
pub mod status;
pub mod watermark;
pub mod workers;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::{self, Message};

use crate::AppState;
use crate::event_log::{EventLog, Severity, log_event};
use config::{Protocol, ProtocolChoice, Settings};
use event::{CursorPoint, Frame};
use watermark::WatermarkHandle;
use workers::{Job, RetryPolicy, WorkerPool};

const MIN_BACKOFF: Duration = Duration::from_millis(250);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// The cursor is saved after this many events or `FLUSH_INTERVAL`, whichever
/// comes first, and when a session ends.
const FLUSH_EVERY_EVENTS: u64 = 1000;
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
/// Events each worker may have queued before reading the socket waits.
const WORKER_QUEUE_CAPACITY: usize = 512;

/// Start the consumer with settings from the environment.
pub fn spawn(state: AppState, collections_rx: watch::Receiver<Vec<String>>) -> JoinHandle<()> {
    let settings = Settings::from_env(&state.config.jetstream_url);
    spawn_with(state, collections_rx, settings, None)
}

/// Start the consumer with `settings`, retrying failed events by `retry`
/// (`RetryPolicy::default()` when `None`).
pub fn spawn_with(
    state: AppState,
    collections_rx: watch::Receiver<Vec<String>>,
    settings: Settings,
    retry: Option<RetryPolicy>,
) -> JoinHandle<()> {
    tokio::spawn(run(
        state,
        collections_rx,
        settings,
        retry.unwrap_or_default(),
    ))
}

/// Connection state carried across reconnects.
struct Session {
    protocol: Protocol,
    compression: bool,
    dictionary: Option<Arc<compression::Dictionary>>,
}

impl Session {
    fn new(settings: &Settings) -> Self {
        Self {
            protocol: match settings.protocol {
                ProtocolChoice::V1 => Protocol::V1,
                ProtocolChoice::Auto | ProtocolChoice::V2 => Protocol::V2,
            },
            compression: settings.compression,
            dictionary: None,
        }
    }
}

enum SessionEnd {
    CollectionsChanged,
    Closed { delivered: bool },
}

/// Why the read loop stopped.
enum Ended {
    CollectionsChanged,
    Closed(String),
}

async fn run(
    state: AppState,
    mut collections_rx: watch::Receiver<Vec<String>>,
    settings: Settings,
    retry: RetryPolicy,
) {
    // One pool and one watermark for the consumer's life, so a DID's events
    // stay in order across reconnects.
    let watermark = Arc::new(WatermarkHandle::new());
    let pool = WorkerPool::spawn(
        settings.workers,
        WORKER_QUEUE_CAPACITY,
        apply::apply_fn(state.clone(), collections_rx.clone()),
        Arc::clone(&watermark),
        retry,
        apply::give_up_fn(state.clone()),
    );
    status::set_worker_depths(pool.depth_counters());

    let mut session = Session::new(&settings);
    // The seq of the last v2 event handed to the pool. The pool outlives the
    // session, so a reconnect resumes past it rather than at the saved
    // cursor, which stops before any event still applying.
    let mut last_dispatched: Option<i64> = None;
    let mut backoff = MIN_BACKOFF;
    loop {
        let end = run_session(
            &state,
            &mut collections_rx,
            &settings,
            &mut session,
            &pool,
            &watermark,
            &mut last_dispatched,
        )
        .await;
        status::disconnected();
        match end {
            SessionEnd::CollectionsChanged => {
                // A newly tracked collection needs the events from the saved
                // cursor on, including those already dispatched.
                last_dispatched = None;
                backoff = MIN_BACKOFF;
            }
            SessionEnd::Closed { delivered } => {
                if delivered {
                    backoff = MIN_BACKOFF;
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

/// One connection: subscribe, read until it ends, drain the workers, save
/// the cursor.
async fn run_session(
    state: &AppState,
    collections_rx: &mut watch::Receiver<Vec<String>>,
    settings: &Settings,
    session: &mut Session,
    pool: &WorkerPool,
    watermark: &WatermarkHandle,
    last_dispatched: &mut Option<i64>,
) -> SessionEnd {
    let db = &state.db;
    let backend = state.db_backend;
    let host = settings.endpoint.host.as_str();
    let protocol = session.protocol;
    let collections = collections_rx.borrow_and_update().clone();

    let stored = match cursor::load(db, backend).await {
        Ok(stored) => stored,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load the jetstream cursor");
            return SessionEnd::Closed { delivered: false };
        }
    };
    let resume = match protocol {
        Protocol::V2 => v2_resume(
            cursor::resume_cursor(&stored, host, protocol),
            *last_dispatched,
        ),
        Protocol::V1 => cursor::resume_cursor(&stored, host, protocol),
    };

    if protocol == Protocol::V2 && session.compression && session.dictionary.is_none() {
        match compression::fetch_dictionary(
            &state.http,
            &config::dictionary_url(&settings.endpoint),
        )
        .await
        {
            Ok(dictionary) => session.dictionary = Some(Arc::new(dictionary)),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "could not fetch the jetstream zstd dictionary; subscribing uncompressed"
                );
                session.compression = false;
            }
        }
    }
    let dictionary = match protocol {
        Protocol::V2 if session.compression => session.dictionary.clone(),
        _ => None,
    };
    let dictionary_id = dictionary.as_ref().map(|d| d.id);
    let url = match protocol {
        Protocol::V2 => config::v2_url(&settings.endpoint, &collections, resume, dictionary_id),
        Protocol::V1 => config::v1_url(&settings.endpoint, &collections, resume),
    };

    tracing::info!(%url, protocol = protocol.as_str(), "connecting to jetstream");
    let ws = match tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async(url.as_str()),
    )
    .await
    {
        Ok(Ok((ws, _))) => ws,
        Ok(Err(tungstenite::Error::Http(response))) => {
            let status = response.status().as_u16();
            let body = String::from_utf8_lossy(response.body().as_deref().unwrap_or_default())
                .into_owned();
            tracing::warn!(status, %body, "jetstream refused the subscription");
            return SessionEnd::Closed { delivered: false };
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "jetstream connection failed");
            return SessionEnd::Closed { delivered: false };
        }
        Err(_) => {
            tracing::warn!(
                "jetstream connection timed out after {}s",
                CONNECT_TIMEOUT.as_secs()
            );
            return SessionEnd::Closed { delivered: false };
        }
    };

    status::connected(protocol, host, dictionary_id);
    log_event(
        db,
        EventLog {
            event_type: "jetstream.connected".to_string(),
            severity: Severity::Info,
            actor_did: None,
            subject: None,
            detail: serde_json::json!({
                "url": url,
                "protocol": protocol.as_str(),
                "compression": dictionary_id.is_some(),
                "dictionary_id": dictionary_id,
            }),
        },
        backend,
    )
    .await;

    let (_write, mut read) = ws.split();
    // A seq cursor is inclusive, so the event at it was applied already.
    let mut floor = match (protocol, resume) {
        (Protocol::V2, Some(c)) if c < cursor::TIMESTAMP_CURSOR_MIN => Some(c),
        _ => None,
    };
    let mut delivered = false;
    let mut since_flush: u64 = 0;
    let mut last_saved: Option<CursorPoint> = None;
    let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut collections_changed = std::pin::pin!(async {
        if collections_rx.changed().await.is_err() {
            // The sender is gone, so the filter can never change again.
            std::future::pending::<()>().await;
        }
    });

    let ended = loop {
        tokio::select! {
            message = read.next() => {
                let message = match message {
                    Some(Ok(message)) => message,
                    Some(Err(e)) => break Ended::Closed(e.to_string()),
                    None => break Ended::Closed("connection closed".to_string()),
                };
                let text = match message {
                    Message::Text(text) => text.as_str().to_owned(),
                    Message::Binary(frame) => match dictionary.as_deref() {
                        Some(dictionary) => match dictionary.decode(&frame) {
                            Ok(text) => text,
                            Err(e) => {
                                tracing::warn!(error = %e, "skipping an undecodable jetstream frame");
                                continue;
                            }
                        },
                        None => {
                            tracing::debug!("ignoring a binary frame on an uncompressed subscription");
                            continue;
                        }
                    },
                    Message::Close(_) => break Ended::Closed("close frame".to_string()),
                    _ => continue,
                };
                let frame = match protocol {
                    Protocol::V2 => event::parse_v2(&text),
                    Protocol::V1 => event::parse_v1(&text),
                };
                match frame {
                    Ok(Frame::Event(event)) => {
                        if let Some(seq) = event.cursor.seq() {
                            if floor.is_some_and(|floor| seq <= floor) {
                                continue;
                            }
                            floor = Some(seq);
                            *last_dispatched = Some(seq);
                        }
                        state
                            .telemetry_counters
                            .jetstream_received
                            .fetch_add(1, Ordering::Relaxed);
                        delivered = true;
                        let ticket = watermark.begin(event.cursor);
                        pool.dispatch(Job { ticket, event }).await;
                        since_flush += 1;
                        if since_flush >= FLUSH_EVERY_EVENTS {
                            flush(state, host, watermark, &mut last_saved).await;
                            since_flush = 0;
                        }
                    }
                    Ok(Frame::Info { name, message }) => {
                        tracing::info!(name, ?message, "jetstream notice");
                    }
                    Ok(Frame::Error { error, message }) => {
                        break Ended::Closed(format!("{error}: {}", message.unwrap_or_default()));
                    }
                    Ok(Frame::Skip) => {}
                    Err(e) => tracing::warn!(error = %e, "skipping an unparseable jetstream frame"),
                }
            }
            _ = ticker.tick() => {
                flush(state, host, watermark, &mut last_saved).await;
                since_flush = 0;
            }
            _ = &mut collections_changed => break Ended::CollectionsChanged,
        }
    };

    if tokio::time::timeout(settings.drain_timeout, watermark.wait_idle())
        .await
        .is_err()
    {
        tracing::warn!(
            in_flight = watermark.in_flight(),
            "jetstream events still applying; the saved cursor stops before them"
        );
    }
    flush(state, host, watermark, &mut last_saved).await;

    match ended {
        Ended::CollectionsChanged => {
            tracing::info!("collection filter changed, reconnecting to jetstream");
            SessionEnd::CollectionsChanged
        }
        Ended::Closed(reason) => {
            tracing::warn!(%reason, "jetstream disconnected");
            log_event(
                db,
                EventLog {
                    event_type: "jetstream.disconnected".to_string(),
                    severity: Severity::Warn,
                    actor_did: None,
                    subject: None,
                    detail: serde_json::json!({ "reason": reason }),
                },
                backend,
            )
            .await;
            SessionEnd::Closed { delivered }
        }
    }
}

/// Where a v2 session resumes: `saved` (from `cursor::resume_cursor`), or
/// past `last_dispatched` when that is further on. `last_dispatched` is a
/// seq on this host, so it wins over a witnessed-time cursor.
fn v2_resume(saved: Option<i64>, last_dispatched: Option<i64>) -> Option<i64> {
    match (saved, last_dispatched) {
        (Some(saved), Some(dispatched)) if saved < cursor::TIMESTAMP_CURSOR_MIN => {
            Some(saved.max(dispatched))
        }
        (_, Some(dispatched)) => Some(dispatched),
        (saved, None) => saved,
    }
}

/// Save the applied watermark when it moved.
async fn flush(
    state: &AppState,
    host: &str,
    watermark: &WatermarkHandle,
    last_saved: &mut Option<CursorPoint>,
) {
    let Some(point) = watermark.applied() else {
        return;
    };
    status::applied(&point);
    if *last_saved == Some(point) {
        return;
    }
    match cursor::save(&state.db, state.db_backend, host, &point).await {
        Ok(()) => *last_saved = Some(point),
        Err(e) => tracing::warn!(error = %e, "failed to save the jetstream cursor"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reconnect_resumes_past_what_was_dispatched() {
        const T: i64 = 1_779_719_000_000_000;
        assert_eq!(
            v2_resume(None, None),
            None,
            "a fresh instance starts at the tip"
        );
        assert_eq!(v2_resume(Some(10), None), Some(10));
        assert_eq!(v2_resume(Some(T), None), Some(T));
        assert_eq!(
            v2_resume(None, Some(7)),
            Some(7),
            "nothing saved yet, but events are queued"
        );
        assert_eq!(v2_resume(Some(10), Some(25)), Some(25));
        assert_eq!(v2_resume(Some(10), Some(4)), Some(10));
        assert_eq!(
            v2_resume(Some(T), Some(25)),
            Some(25),
            "a seq on this host beats a witnessed time"
        );
    }
}

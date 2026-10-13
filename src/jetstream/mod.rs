//! The Jetstream consumer.
//!
//! One task reads the websocket and hands each event to the worker that owns
//! its DID (`workers`), which applies it (`apply`). The cursor saved is the
//! applied watermark (`watermark`, `cursor`), never the last frame read, so a
//! crash re-delivers events rather than skipping them. The v2
//! `subscribeEvents` stream is preferred, zstd-compressed with the server's
//! dictionary (`compression`).

pub mod apply;
pub mod archive;
pub mod compression;
pub mod config;
pub mod cursor;
pub mod event;
pub mod status;
pub mod watermark;
pub mod workers;

use std::collections::HashSet;
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
    choice: ProtocolChoice,
    protocol: Protocol,
    compression: bool,
    dictionary: Option<Arc<compression::Dictionary>>,
    /// The cursor the last connection asked for.
    sent_cursor: Option<i64>,
    /// A saved seq the server refused (`CursorTooOld`) or that belongs to
    /// seq numbering it no longer has (`FutureCursor`). While it is still the
    /// saved one, resumes go by the saved witnessed time instead.
    stale_seq: Option<i64>,
    /// Connect without a cursor: the saved seq was refused as too old and no
    /// witnessed time will do. Cleared by the first event, and before a long
    /// `invalid_request` wait.
    from_tip: bool,
    /// A resume from the tip that lost events, reported once the first event
    /// shows where the stream picked up again.
    pending_gap: Option<Gap>,
    /// 400s in a row with no error name to go by (`handle_rejection`).
    unexplained_400s: u32,
    /// After `FutureCursor`, the first ticket of the new seq numbering:
    /// nothing is saved until it is applied, so an old-numbering seq still
    /// applying cannot become the saved cursor.
    save_barrier: Option<u64>,
    /// Gaps logged by this `run()`, by reason and the cursor sent, so a
    /// retry that hits the same one does not log it again.
    logged_gaps: HashSet<(&'static str, Option<i64>)>,
}

/// Limits for the Jetstream socket: no frame or message larger than the
/// biggest frame a v2 server sends decoded (the Go client's read limit),
/// in place of tungstenite's 16 MiB frames and 64 MiB messages.
fn socket_config() -> tungstenite::protocol::WebSocketConfig {
    tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(compression::MAX_DECODED_FRAME))
        .max_frame_size(Some(compression::MAX_DECODED_FRAME))
}

/// `reason` in a `jetstream.gap` event's detail.
const GAP_CURSOR_TOO_OLD: &str = "cursor_too_old";
const GAP_OUTDATED_CURSOR: &str = "outdated_cursor";
const GAP_FUTURE_CURSOR: &str = "future_cursor";

struct Gap {
    /// The cursor that was refused.
    sent: Option<i64>,
    /// The saved seq, the last one known applied.
    after_seq: Option<i64>,
    message: String,
}

impl Session {
    fn new(settings: &Settings) -> Self {
        Self {
            choice: settings.protocol,
            protocol: match settings.protocol {
                ProtocolChoice::V1 => Protocol::V1,
                ProtocolChoice::Auto | ProtocolChoice::V2 => Protocol::V2,
            },
            compression: settings.compression,
            dictionary: None,
            sent_cursor: None,
            stale_seq: None,
            from_tip: false,
            pending_gap: None,
            unexplained_400s: 0,
            save_barrier: None,
            logged_gaps: HashSet::new(),
        }
    }
}

enum SessionEnd {
    CollectionsChanged,
    Closed {
        delivered: bool,
    },
    Rejected(Rejection),
    /// `FutureCursor`, with a witnessed time to resume by instead.
    Renumbered,
}

/// A subscription the server refused before upgrading, by its XRPC error
/// name. v1 errors are plain text and land in `Other` (or `NotFound`).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Rejection {
    NotFound,
    CursorTooOld(String),
    UnknownDictionary(String),
    InvalidRequest(String),
    Unavailable,
    /// `message` is `HTTP <status>`, then the body if there was one.
    Other {
        status: u16,
        message: String,
    },
}

/// The body is whatever arrived with the headers, so it can be cut short or
/// missing; a known error name in the text still counts.
fn classify_rejection(status: u16, body: &[u8]) -> Rejection {
    #[derive(serde::Deserialize)]
    struct XrpcError {
        error: String,
        #[serde(default)]
        message: Option<String>,
    }
    let text = String::from_utf8_lossy(body);
    let parsed: Option<XrpcError> = serde_json::from_slice(body).ok();
    let name = parsed.as_ref().map(|e| e.error.clone()).or_else(|| {
        ["CursorTooOld", "UnknownZstdDictionary", "InvalidRequest"]
            .into_iter()
            .find(|name| text.contains(name))
            .map(str::to_string)
    });
    let message = parsed
        .and_then(|e| e.message)
        .unwrap_or_else(|| text.trim().to_string());
    match (status, name.as_deref()) {
        (404, _) => Rejection::NotFound,
        (503, _) => Rejection::Unavailable,
        (400, Some("CursorTooOld")) => Rejection::CursorTooOld(message),
        (400, Some("UnknownZstdDictionary")) => Rejection::UnknownDictionary(message),
        (400, Some("InvalidRequest")) => Rejection::InvalidRequest(message),
        _ if message.is_empty() => Rejection::Other {
            status,
            message: format!("HTTP {status}"),
        },
        _ => Rejection::Other {
            status,
            message: format!("HTTP {status}: {message}"),
        },
    }
}

/// What to do about a refusal. Returns how long to wait before the next
/// attempt, or `None` for the ordinary backoff.
async fn handle_rejection(
    state: &AppState,
    settings: &Settings,
    session: &mut Session,
    last_dispatched: &mut Option<i64>,
    rejection: Rejection,
) -> Option<Duration> {
    match rejection {
        Rejection::NotFound
            if session.choice == ProtocolChoice::Auto && session.protocol == Protocol::V2 =>
        {
            tracing::info!("jetstream has no v2 endpoint; subscribing to v1 /subscribe");
            session.protocol = Protocol::V1;
            // v1 resumes by witnessed time only.
            *last_dispatched = None;
            Some(Duration::ZERO)
        }
        Rejection::CursorTooOld(message) => {
            resume_without_seq(state, session, last_dispatched, message, true).await;
            Some(Duration::ZERO)
        }
        Rejection::UnknownDictionary(message) => {
            let refused = session.dictionary.as_ref().map(|d| d.id);
            match compression::fetch_dictionary(
                &state.http,
                &config::dictionary_url(&settings.endpoint),
            )
            .await
            {
                Ok(dictionary) if Some(dictionary.id) != refused => {
                    tracing::info!(
                        old = ?refused,
                        new = dictionary.id,
                        "jetstream rotated its zstd dictionary"
                    );
                    session.dictionary = Some(Arc::new(dictionary));
                }
                _ => {
                    tracing::warn!(
                        %message,
                        "jetstream refuses its own zstd dictionary; subscribing uncompressed"
                    );
                    session.compression = false;
                    session.dictionary = None;
                }
            }
            Some(Duration::ZERO)
        }
        Rejection::InvalidRequest(message) => {
            Some(invalid_request(state, settings, session, message).await)
        }
        // tungstenite hands over only the body bytes that came with the
        // headers, so a 400 may arrive with nothing to classify. Step down
        // through what could be wrong with the request, and only then wait.
        // The cursor is only dropped for a seq that a witnessed time can
        // replace: never for the tip, which would skip everything since.
        Rejection::Other {
            status: 400,
            message,
        } => {
            session.unexplained_400s += 1;
            let sent_seq = session
                .sent_cursor
                .is_some_and(|c| c < cursor::TIMESTAMP_CURSOR_MIN);
            let step = session.unexplained_400s;
            if step == 1 {
                tracing::warn!(%message, "jetstream refused the subscription; retrying uncompressed");
                session.compression = false;
                session.dictionary = None;
                return Some(Duration::ZERO);
            }
            if step == 2
                && sent_seq
                && resume_without_seq(state, session, last_dispatched, message.clone(), false).await
            {
                return Some(Duration::ZERO);
            }
            // Straight on from step 2 when the cursor stays.
            session.unexplained_400s = step.max(3);
            Some(invalid_request(state, settings, session, message).await)
        }
        Rejection::NotFound => {
            tracing::warn!("jetstream answered 404 for the subscription");
            None
        }
        Rejection::Unavailable => {
            tracing::warn!("jetstream is unavailable; retrying");
            None
        }
        Rejection::Other { message, .. } => {
            tracing::warn!(%message, "jetstream refused the subscription");
            None
        }
    }
}

/// Stop resuming by seq: the refused one, the saved one and the last one
/// dispatched all go, and the next connection resumes by the saved
/// witnessed time. When there is none, or it was the cursor refused, only
/// `to_tip` (a `CursorTooOld`) goes on to the live tip; otherwise nothing
/// changes. Returns whether the next connection resumes differently.
async fn resume_without_seq(
    state: &AppState,
    session: &mut Session,
    last_dispatched: &mut Option<i64>,
    message: String,
    to_tip: bool,
) -> bool {
    let stored = match cursor::load(&state.db, state.db_backend).await {
        Ok(stored) => stored,
        Err(e) => {
            // The next attempt tries again, and is likely refused again.
            tracing::warn!(error = %e, "failed to load the jetstream cursor");
            return false;
        }
    };
    let sent_time = session
        .sent_cursor
        .is_some_and(|c| c >= cursor::TIMESTAMP_CURSOR_MIN);
    let by_time = stored.time_us.is_some() && !sent_time;
    if !by_time && !to_tip {
        return false;
    }
    *last_dispatched = None;
    session.stale_seq = stored.seq;
    if by_time {
        tracing::warn!(
            %message,
            "jetstream refused the seq cursor; resuming by witnessed time"
        );
        return true;
    }
    tracing::warn!(
        %message,
        "jetstream refused the cursor and no witnessed time is left; resuming from the live tip"
    );
    session.from_tip = true;
    if session.sent_cursor.is_some() {
        session.pending_gap = Some(Gap {
            sent: session.sent_cursor,
            after_seq: stored.seq,
            message,
        });
    }
    true
}

/// Log a refusal that will not fix itself on a quick retry; returns how long
/// to wait before trying again. The retry keeps its cursor rather than
/// starting at the tip: a tip resume waiting on it would skip everything
/// from the saved cursor to whenever the refusals stop. A pending gap stays,
/// for when the retry has no cursor left either.
async fn invalid_request(
    state: &AppState,
    settings: &Settings,
    session: &mut Session,
    message: String,
) -> Duration {
    session.from_tip = false;
    let retry = settings.invalid_request_retry;
    tracing::error!(
        %message,
        retry_in_secs = retry.as_secs(),
        "jetstream rejected the subscription as invalid"
    );
    log_event(
        &state.db,
        EventLog {
            event_type: "jetstream.invalid_request".to_string(),
            severity: Severity::Error,
            actor_did: None,
            subject: None,
            detail: serde_json::json!({
                "message": message,
                "retry_in_secs": retry.as_secs(),
            }),
        },
        state.db_backend,
    )
    .await;
    retry
}

/// Log a `jetstream.gap` with `reason` and `detail`, once per `run()` for
/// each reason and cursor sent.
async fn log_gap(
    state: &AppState,
    session: &mut Session,
    reason: &'static str,
    sent: Option<i64>,
    mut detail: serde_json::Value,
) {
    if !session.logged_gaps.insert((reason, sent)) {
        return;
    }
    detail["reason"] = reason.into();
    log_event(
        &state.db,
        EventLog {
            event_type: "jetstream.gap".to_string(),
            severity: Severity::Warn,
            actor_did: None,
            subject: None,
            detail,
        },
        state.db_backend,
    )
    .await;
}

/// Why the read loop stopped.
enum Ended {
    CollectionsChanged,
    Closed(String),
    Renumbered,
}

/// Clears the status's worker counters when `run` exits by any path.
struct ClearWorkersOnDrop;

impl Drop for ClearWorkersOnDrop {
    fn drop(&mut self) {
        status::clear_workers();
    }
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
    status::set_workers(pool.depth_counters(), pool.busy_since_counters());
    // Declared after the pool, so it drops first only in name: the pool's own
    // drop resets its counters, and this stops the status holding them.
    let _clear_workers = ClearWorkersOnDrop;

    let mut session = Session::new(&settings);
    // The seq of the last v2 event handed to the pool. The pool outlives the
    // session, so a reconnect resumes past it rather than at the saved
    // cursor, which stops before any event still applying. Seqs are local to
    // one host; this needs no host of its own only because the endpoint is
    // fixed for the life of `run()`. Every path that gives up on a seq
    // (refused, renumbered, v1) clears it, or `v2_resume` would bring that
    // seq back.
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
            SessionEnd::Renumbered => {
                backoff = MIN_BACKOFF;
                tokio::time::sleep(backoff).await;
            }
            SessionEnd::Rejected(rejection) => {
                match handle_rejection(
                    &state,
                    &settings,
                    &mut session,
                    &mut last_dispatched,
                    rejection,
                )
                .await
                {
                    Some(wait) => tokio::time::sleep(wait).await,
                    None => {
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                    }
                }
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

    let mut stored = match cursor::load(db, backend).await {
        Ok(stored) => stored,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load the jetstream cursor");
            return SessionEnd::Closed { delivered: false };
        }
    };
    if stored.seq.is_some() && stored.seq == session.stale_seq {
        stored.seq = None;
    }
    let resume = match protocol {
        _ if session.from_tip => None,
        Protocol::V2 => v2_resume(
            cursor::resume_cursor(&stored, host, protocol),
            *last_dispatched,
        ),
        Protocol::V1 => cursor::resume_cursor(&stored, host, protocol),
    };
    session.sent_cursor = resume;
    if resume.is_some() {
        // Not a resume from the tip after all, so nothing to report.
        session.pending_gap = None;
    }

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
        tokio_tungstenite::connect_async_with_config(url.as_str(), Some(socket_config()), false),
    )
    .await
    {
        Ok(Ok((ws, _))) => ws,
        Ok(Err(tungstenite::Error::Http(response))) => {
            let status = response.status().as_u16();
            let body = response.body().as_deref().unwrap_or_default();
            return SessionEnd::Rejected(classify_rejection(status, body));
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
    session.unexplained_400s = 0;
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
                        session.from_tip = false;
                        if let Some(gap) = session.pending_gap.take() {
                            log_gap(state, session, GAP_CURSOR_TOO_OLD, gap.sent, serde_json::json!({
                                "message": gap.message,
                                "after_seq": gap.after_seq,
                                "resumed_at_seq": event.cursor.seq(),
                            }))
                            .await;
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
                            flush(state, host, watermark, &mut session.save_barrier, &mut last_saved).await;
                            since_flush = 0;
                        }
                    }
                    Ok(Frame::Info { name, message }) => {
                        tracing::info!(name, ?message, "jetstream notice");
                        match name.as_str() {
                            // A timestamp cursor older than the server keeps
                            // was moved up: the events in between are gone.
                            "OutdatedCursor" => {
                                log_gap(state, session, GAP_OUTDATED_CURSOR, resume, serde_json::json!({
                                    "message": message,
                                }))
                                .await;
                            }
                            // The seq resumed from is past the server's next
                            // one: its seqs started over or went back. The
                            // new, lower seqs must not be dropped as already
                            // seen, nor the old ones resumed from again. A
                            // witnessed time means the same on the new
                            // numbering, so with one saved the stream is
                            // picked up from it rather than from the tip.
                            "FutureCursor" => {
                                floor = None;
                                *last_dispatched = None;
                                if stored.seq.is_some() {
                                    session.stale_seq = stored.seq;
                                }
                                session.save_barrier = Some(watermark.next_ticket());
                                log_gap(state, session, GAP_FUTURE_CURSOR, resume, serde_json::json!({
                                    "from_seq": resume,
                                }))
                                .await;
                                // Only from a seq: a time refused this way
                                // would be refused again, so the tip it is.
                                let sent_seq = resume
                                    .is_some_and(|c| c < cursor::TIMESTAMP_CURSOR_MIN);
                                if stored.time_us.is_some() && sent_seq {
                                    break Ended::Renumbered;
                                }
                            }
                            _ => {}
                        }
                    }
                    Ok(Frame::Error { error, message }) => {
                        break Ended::Closed(format!("{error}: {}", message.unwrap_or_default()));
                    }
                    Ok(Frame::Skip) => {}
                    Err(e) => tracing::warn!(error = %e, "skipping an unparseable jetstream frame"),
                }
            }
            _ = ticker.tick() => {
                flush(state, host, watermark, &mut session.save_barrier, &mut last_saved).await;
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
    flush(
        state,
        host,
        watermark,
        &mut session.save_barrier,
        &mut last_saved,
    )
    .await;

    match ended {
        Ended::CollectionsChanged => {
            tracing::info!("collection filter changed, reconnecting to jetstream");
            SessionEnd::CollectionsChanged
        }
        Ended::Renumbered => {
            tracing::warn!("jetstream's seqs started over; reconnecting by witnessed time");
            SessionEnd::Renumbered
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

/// Save the applied watermark when it moved, unless it is still short of
/// `barrier` (`Session::save_barrier`), which is cleared once passed.
async fn flush(
    state: &AppState,
    host: &str,
    watermark: &WatermarkHandle,
    barrier: &mut Option<u64>,
    last_saved: &mut Option<CursorPoint>,
) {
    let point = match *barrier {
        Some(ticket) => {
            let Some(point) = watermark.applied_if_retired(ticket) else {
                return;
            };
            *barrier = None;
            point
        }
        None => match watermark.applied() {
            Some(point) => point,
            None => return,
        },
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

    /// Ruling FR6: a frame up to the decoded-frame cap is read (tungstenite's
    /// default frame limit is 16 MiB); a message over it is refused, however
    /// it is fragmented (the default message limit is 64 MiB).
    #[tokio::test]
    async fn the_socket_reads_frames_up_to_the_cap_and_no_larger_messages() {
        use futures_util::SinkExt;
        use tungstenite::protocol::WebSocketConfig;
        use tungstenite::protocol::frame::Frame;
        use tungstenite::protocol::frame::coding::{Data, OpCode};

        const MIB: usize = 1 << 20;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut ws = tokio_tungstenite::accept_async_with_config(
                stream,
                Some(WebSocketConfig::default()),
            )
            .await
            .expect("handshake");
            ws.send(Message::Binary(vec![b'a'; 20 * MIB].into()))
                .await
                .expect("send one large frame");
            let fragments = 33;
            for i in 0..fragments {
                let opcode = if i == 0 {
                    OpCode::Data(Data::Binary)
                } else {
                    OpCode::Data(Data::Continue)
                };
                let frame = Frame::message(vec![b'b'; MIB], opcode, i == fragments - 1);
                if ws.send(Message::Frame(frame)).await.is_err() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let (mut ws, _) = tokio_tungstenite::connect_async_with_config(
            format!("ws://{addr}"),
            Some(socket_config()),
            false,
        )
        .await
        .expect("connect");
        let first = ws
            .next()
            .await
            .expect("a message")
            .expect("a 20 MiB frame is under the cap");
        assert_eq!(first.len(), 20 * MIB);
        let second = ws.next().await.expect("an outcome");
        assert!(
            matches!(second, Err(tungstenite::Error::Capacity(_))),
            "a 33 MiB message must be refused: {second:?}"
        );
        server.abort();
    }

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

    #[test]
    fn refusals_are_classified_by_their_xrpc_error_name() {
        assert_eq!(
            classify_rejection(404, b"404 page not found"),
            Rejection::NotFound
        );
        assert_eq!(
            classify_rejection(503, br#"{"error":"ServiceUnavailable"}"#),
            Rejection::Unavailable
        );
        assert_eq!(
            classify_rejection(
                400,
                br#"{"error":"CursorTooOld","message":"cursor 5 below lookback floor 9"}"#
            ),
            Rejection::CursorTooOld("cursor 5 below lookback floor 9".into())
        );
        assert_eq!(
            classify_rejection(
                400,
                br#"{"error":"UnknownZstdDictionary","message":"current dictionary id is 7"}"#
            ),
            Rejection::UnknownDictionary("current dictionary id is 7".into())
        );
        assert_eq!(
            classify_rejection(400, br#"{"error":"InvalidRequest","message":"bad"}"#),
            Rejection::InvalidRequest("bad".into())
        );
        assert_eq!(
            classify_rejection(400, br#"{"error":"CursorTooOld","mess"#),
            Rejection::CursorTooOld(r#"{"error":"CursorTooOld","mess"#.into()),
            "a body cut short still names its error"
        );
        assert_eq!(
            classify_rejection(400, b"bad request"),
            Rejection::Other {
                status: 400,
                message: "HTTP 400: bad request".into()
            }
        );
        assert_eq!(
            classify_rejection(400, b""),
            Rejection::Other {
                status: 400,
                message: "HTTP 400".into()
            },
            "no body at all"
        );
        assert_eq!(
            classify_rejection(400, br#"{"error":"SomethingNew"}"#),
            Rejection::Other {
                status: 400,
                message: r#"HTTP 400: {"error":"SomethingNew"}"#.into()
            },
            "an error name this version does not know"
        );
    }
}

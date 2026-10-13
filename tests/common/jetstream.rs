//! A scripted Jetstream: the v2 `subscribeEvents` and v1 `/subscribe`
//! websockets and `getZstdDictionary`, enough to drive the consumer through
//! cursor resumes, compression, pre-upgrade errors and a retention floor.
//!
//! Events are scripted as v2 payloads with `push`. A connection gets every
//! scripted event at or after its cursor (all of them without one) that its
//! collection filter admits, then stays open until the client leaves or the
//! test calls `kick`. Events pushed later reach the next connection.
//!
//! The mock also serves a sealed archive (`planSnapshot`, `getSegment`,
//! `getBlock`), whose events are scripted with `seal`.
//!
//! The dictionaries in `tests/fixtures/jetstream/` are from
//! bluesky-social/jetstream v0.3.10 (`internal/subscribe/dictionaries/`),
//! dual-licensed MIT OR Apache-2.0, Copyright (c) 2022-2026 Bluesky Social
//! PBC and Contributors.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Map, Value, json};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::segment_writer::{self, ArchiveEvent};

/// Witnessed time of seq 0; seq `n` is witnessed `n` milliseconds later.
pub const BASE_US: i64 = 1_779_719_000_000_000;
pub const DICTIONARY_A: u32 = 20_260_811;
pub const DICTIONARY_B: u32 = 1_612_007_021;
const TIMESTAMP_CURSOR_MIN: i64 = 1_000_000_000_000_000;

pub fn witnessed_us(seq: i64) -> i64 {
    BASE_US + seq * 1_000
}

/// `event` witnessed `by_us` later than its seq says, as the events of a
/// server whose seqs started over are.
pub fn witnessed_later(mut event: Value, by_us: i64) -> Value {
    let time = wire_time(event_witnessed_us(&event) + by_us);
    event["time"] = json!(time);
    event["witnessedAt"] = json!(time);
    event
}

/// When a scripted event was witnessed.
fn event_witnessed_us(event: &Value) -> i64 {
    let time = event["witnessedAt"].as_str().expect("witnessedAt");
    chrono::DateTime::parse_from_rfc3339(time)
        .expect("witnessedAt is RFC 3339")
        .timestamp_micros()
}

fn wire_time(us: i64) -> String {
    chrono::DateTime::from_timestamp_micros(us)
        .expect("time in range")
        .format("%Y-%m-%dT%H:%M:%S%.6fZ")
        .to_string()
}

/// The rev of the commit at `seq`: TIDs from the witnessed time, so later
/// seqs have later revs.
pub fn rev_for(seq: i64) -> String {
    happyview::tid::tid_from_unix_microseconds(witnessed_us(seq))
}

pub fn fixture_dictionary(id: u32) -> Vec<u8> {
    let name = match id {
        DICTIONARY_A => "subscribe_events_20260811.zdict",
        DICTIONARY_B => "legacy_subscribe.zdict",
        _ => panic!("no fixture dictionary {id}"),
    };
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/jetstream")
            .join(name),
    )
    .expect("read dictionary fixture")
}

fn payload(kind: &str, seq: i64, did: &str) -> Map<String, Value> {
    let time = wire_time(witnessed_us(seq));
    let mut m = Map::new();
    m.insert(
        "$type".into(),
        json!(format!("network.bsky.jetstream.subscribeEvents#{kind}")),
    );
    m.insert("seq".into(), json!(seq));
    m.insert("did".into(), json!(did));
    m.insert("time".into(), json!(time));
    m.insert("witnessedAt".into(), json!(time));
    m
}

fn commit(
    seq: i64,
    did: &str,
    collection: &str,
    rkey: &str,
    operation: &str,
    record: Option<Value>,
) -> Value {
    let mut m = payload("commit", seq, did);
    m.insert("rev".into(), json!(rev_for(seq)));
    m.insert("operation".into(), json!(operation));
    m.insert("collection".into(), json!(collection));
    m.insert("rkey".into(), json!(rkey));
    if let Some(record) = record {
        let cid = happyview::cid_verify::compute_record_cid(&record)
            .expect("record encodes")
            .to_string();
        m.insert("cid".into(), json!(cid));
        m.insert("record".into(), record);
    }
    Value::Object(m)
}

pub fn create(seq: i64, did: &str, collection: &str, rkey: &str, record: Value) -> Value {
    commit(seq, did, collection, rkey, "create", Some(record))
}

pub fn delete(seq: i64, did: &str, collection: &str, rkey: &str) -> Value {
    commit(seq, did, collection, rkey, "delete", None)
}

pub fn account(seq: i64, did: &str, active: bool, status: Option<&str>) -> Value {
    let mut m = payload("account", seq, did);
    let mut inner = json!({
        "seq": seq,
        "did": did,
        "time": wire_time(witnessed_us(seq)),
        "active": active,
    });
    if let Some(status) = status {
        inner["status"] = json!(status);
    }
    m.insert("account".into(), inner);
    Value::Object(m)
}

pub fn identity(seq: i64, did: &str, handle: &str) -> Value {
    let mut m = payload("identity", seq, did);
    m.insert(
        "identity".into(),
        json!({"seq": seq, "did": did, "time": wire_time(witnessed_us(seq)), "handle": handle}),
    );
    Value::Object(m)
}

pub fn sync(seq: i64, did: &str) -> Value {
    let mut m = payload("sync", seq, did);
    m.insert(
        "sync".into(),
        json!({
            "seq": seq,
            "did": did,
            "blocks": {"$bytes": ""},
            "rev": rev_for(seq),
            "time": wire_time(witnessed_us(seq)),
        }),
    );
    Value::Object(m)
}

fn archive_event(seq: i64, kind: u8, did: &str, collection: &str, rkey: &str) -> ArchiveEvent {
    ArchiveEvent {
        seq: seq as u64,
        witnessed_at: witnessed_us(seq),
        kind,
        did: did.to_string(),
        collection: collection.to_string(),
        rkey: rkey.to_string(),
        rev: rev_for(seq),
        payload: Vec::new(),
    }
}

fn encode(value: &Value) -> Vec<u8> {
    happyview::cid_verify::record_to_dag_cbor(value).expect("encodes")
}

pub fn archive_create(
    seq: i64,
    did: &str,
    collection: &str,
    rkey: &str,
    record: Value,
) -> ArchiveEvent {
    ArchiveEvent {
        payload: encode(&record),
        ..archive_event(seq, 1, did, collection, rkey)
    }
}

pub fn archive_update(
    seq: i64,
    did: &str,
    collection: &str,
    rkey: &str,
    record: Value,
) -> ArchiveEvent {
    ArchiveEvent {
        payload: encode(&record),
        ..archive_event(seq, 2, did, collection, rkey)
    }
}

pub fn archive_delete(seq: i64, did: &str, collection: &str, rkey: &str) -> ArchiveEvent {
    archive_event(seq, 3, did, collection, rkey)
}

/// Markers carry no collection, rkey or rev.
fn archive_marker(seq: i64, kind: u8, did: &str, payload: &Value) -> ArchiveEvent {
    ArchiveEvent {
        rev: String::new(),
        payload: encode(payload),
        ..archive_event(seq, kind, did, "", "")
    }
}

pub fn archive_account(seq: i64, did: &str, active: bool, status: Option<&str>) -> ArchiveEvent {
    let mut inner = json!({
        "seq": seq,
        "did": did,
        "time": wire_time(witnessed_us(seq)),
        "active": active,
    });
    if let Some(status) = status {
        inner["status"] = json!(status);
    }
    archive_marker(seq, 5, did, &inner)
}

pub fn archive_sync(seq: i64, did: &str) -> ArchiveEvent {
    archive_marker(
        seq,
        6,
        did,
        &json!({
            "seq": seq,
            "did": did,
            "time": wire_time(witnessed_us(seq)),
            "rev": rev_for(seq),
            "blocks": {"$bytes": ""},
        }),
    )
}

pub fn archive_identity(seq: i64, did: &str) -> ArchiveEvent {
    archive_marker(
        seq,
        4,
        did,
        &json!({"seq": seq, "did": did, "time": wire_time(witnessed_us(seq))}),
    )
}

/// The v1 frame for a scripted v2 payload; v1 never carries `#sync`.
fn to_v1(payload: &Value) -> Option<Value> {
    let kind = payload["$type"].as_str()?.rsplit('#').next()?;
    let did = payload["did"].clone();
    let time_us = event_witnessed_us(payload);
    match kind {
        "commit" => {
            let mut commit = json!({
                "rev": payload["rev"],
                "operation": payload["operation"],
                "collection": payload["collection"],
                "rkey": payload["rkey"],
            });
            if let Some(record) = payload.get("record") {
                commit["record"] = record.clone();
                commit["cid"] = payload["cid"].clone();
            }
            Some(json!({"did": did, "time_us": time_us, "kind": "commit", "commit": commit}))
        }
        "account" | "identity" => {
            let mut frame = json!({"did": did, "time_us": time_us, "kind": kind});
            frame[kind] = payload[kind].clone();
            Some(frame)
        }
        _ => None,
    }
}

/// One connection attempt as the server saw it.
#[derive(Clone, Debug)]
pub struct Connection {
    /// `"v2"` or `"v1"`.
    pub path: &'static str,
    pub query: String,
}

fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (pair.to_string(), String::new()),
        })
        .collect()
}

impl Connection {
    pub fn params(&self, name: &str) -> Vec<String> {
        parse_query(&self.query)
            .into_iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v)
            .collect()
    }

    pub fn param(&self, name: &str) -> Option<String> {
        self.params(name).into_iter().next()
    }
}

#[derive(Default)]
struct Inner {
    events: Vec<Value>,
    v2_disabled: bool,
    rejections: VecDeque<(u16, Option<Value>)>,
    notices: VecDeque<(String, String)>,
    retention_floor: Option<i64>,
    tip: Option<i64>,
    served_dictionary: Option<u32>,
    accepted_dictionaries: HashSet<u32>,
    connections: Vec<Connection>,
    /// Sealed segments, oldest first: (name, written, events).
    segments: Vec<(
        String,
        segment_writer::WrittenSegment,
        Vec<segment_writer::ArchiveEvent>,
    )>,
    archive_disabled: Option<u16>,
    plan_page_entries: usize, // 0 means 1000
    stalls: usize,
    throttles: usize,
    block_failures: HashMap<(String, u32), usize>,
    corrupt_blocks: HashSet<(String, u32)>,
    /// Segments whose `getSegment` answers 500.
    failed_segments: HashSet<String>,
    plan_requests: Vec<Value>,
    segment_downloads: Vec<String>,
    block_fetches: Vec<(String, u32)>,
    authorizations: Vec<(String, Option<String>)>,
    /// Segments whose `getSegment` answer waits for `release_segment`.
    held_segments: HashMap<String, Arc<tokio::sync::Notify>>,
}

struct Shared {
    inner: Mutex<Inner>,
    kick: watch::Sender<u64>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct MockJetstream {
    addr: SocketAddr,
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

impl MockJetstream {
    /// A server that serves dictionary A and accepts it.
    pub async fn start() -> Self {
        let inner = Inner {
            served_dictionary: Some(DICTIONARY_A),
            accepted_dictionaries: HashSet::from([DICTIONARY_A]),
            ..Inner::default()
        };
        let shared = Arc::new(Shared {
            inner: Mutex::new(inner),
            kick: watch::channel(0).0,
        });
        let app = Router::new()
            .route(
                "/xrpc/network.bsky.jetstream.subscribeEvents",
                get(v2_subscribe),
            )
            .route("/subscribe", get(v1_subscribe))
            .route(
                "/xrpc/network.bsky.jetstream.getZstdDictionary",
                get(get_dictionary),
            )
            .route(
                "/xrpc/network.bsky.jetstream.planSnapshot",
                post(plan_snapshot),
            )
            .route("/xrpc/network.bsky.jetstream.getSegment", get(get_segment))
            .route("/xrpc/network.bsky.jetstream.getBlock", get(get_block))
            .with_state(Arc::clone(&shared));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock jetstream");
        let addr = listener.local_addr().expect("mock address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock jetstream");
        });
        Self { addr, shared, task }
    }

    pub fn url(&self) -> String {
        format!("ws://{}", self.addr)
    }

    pub fn http_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// What the consumer stores as `jetstream_host`.
    pub fn host(&self) -> String {
        self.addr.to_string()
    }

    pub fn push(&self, events: impl IntoIterator<Item = Value>) {
        self.shared.lock().events.extend(events);
    }

    /// Answer the v2 path with 404, as a v1-only Jetstream does.
    pub fn disable_v2(&self) {
        self.shared.lock().v2_disabled = true;
    }

    /// Refuse the next v2 connection before upgrading, with an XRPC error.
    pub fn reject_next(&self, status: u16, error: &str, message: &str) {
        self.shared
            .lock()
            .rejections
            .push_back((status, Some(json!({"error": error, "message": message}))));
    }

    /// Refuse the next v2 connection before upgrading, with no body at all.
    pub fn reject_next_bare(&self, status: u16) {
        self.shared.lock().rejections.push_back((status, None));
    }

    /// Keep only the events from `seq` on, as a server past its lookback
    /// does: a v2 seq cursor below it is refused with `CursorTooOld`, and a
    /// witnessed-time cursor below it is clamped with `#info OutdatedCursor`.
    pub fn set_retention_floor(&self, seq: i64) {
        self.shared.lock().retention_floor = Some(seq);
    }

    /// Where a `FutureCursor` connection starts: scripted events before
    /// `seq` count as already past. Without one it gets them all.
    pub fn set_tip(&self, seq: i64) {
        self.shared.lock().tip = Some(seq);
    }

    /// Forget every scripted event, as a server whose seqs started over.
    pub fn reset_events(&self) {
        self.shared.lock().events.clear();
    }

    /// Send an `#info` frame first on the next v2 connection. After a
    /// `FutureCursor` notice that connection ignores its cursor and starts
    /// at the tip (`set_tip`).
    pub fn notice_next(&self, name: &str, message: &str) {
        self.shared
            .lock()
            .notices
            .push_back((name.to_string(), message.to_string()));
    }

    pub fn serve_dictionary(&self, id: Option<u32>) {
        self.shared.lock().served_dictionary = id;
    }

    pub fn accept_dictionaries(&self, ids: &[u32]) {
        self.shared.lock().accepted_dictionaries = ids.iter().copied().collect();
    }

    pub fn connections(&self) -> Vec<Connection> {
        self.shared.lock().connections.clone()
    }

    /// Seal `events` (seq order) into the next archive segment and return its
    /// name.
    pub fn seal(&self, events: Vec<ArchiveEvent>, per_block: usize) -> String {
        let written = segment_writer::write_segment(&events, per_block);
        let mut inner = self.shared.lock();
        let name = format!("seg_{:010}.jss", inner.segments.len() + 1);
        inner.segments.push((name.clone(), written, events));
        name
    }

    /// Make `planSnapshot` answer `status`.
    pub fn disable_archive(&self, status: u16) {
        self.shared.lock().archive_disabled = Some(status);
    }

    pub fn set_plan_page_entries(&self, entries: usize) {
        self.shared.lock().plan_page_entries = entries;
    }

    /// The next `times` plan pages come back empty and not advanced.
    pub fn stall_plan(&self, times: usize) {
        self.shared.lock().stalls = times;
    }

    /// The next `times` archive requests are answered 503, `Retry-After: 0`.
    pub fn throttle_archive(&self, times: usize) {
        self.shared.lock().throttles = times;
    }

    /// `getBlock` answers 500 `times` times (`usize::MAX`: always).
    pub fn fail_block(&self, segment: &str, index: u32, times: usize) {
        self.shared
            .lock()
            .block_failures
            .insert((segment.to_string(), index), times);
    }

    /// `getBlock` serves bytes that are not a zstd frame.
    pub fn corrupt_block(&self, segment: &str, index: u32) {
        self.shared
            .lock()
            .corrupt_blocks
            .insert((segment.to_string(), index));
    }

    /// Every `getSegment` for `name` answers 500 until this is called again
    /// with the same name, which toggles it off.
    pub fn fail_segment(&self, name: &str) {
        let mut inner = self.shared.lock();
        if !inner.failed_segments.remove(name) {
            inner.failed_segments.insert(name.to_string());
        }
    }

    /// `getSegment` for `name` records the download, then waits for
    /// `release_segment` before answering.
    pub fn hold_segment(&self, name: &str) {
        self.shared
            .lock()
            .held_segments
            .insert(name.to_string(), Arc::new(tokio::sync::Notify::new()));
    }

    pub fn release_segment(&self, name: &str) {
        if let Some(notify) = self.shared.lock().held_segments.remove(name) {
            notify.notify_waiters();
            // A handler that recorded the download but had not started
            // waiting yet takes this permit.
            notify.notify_one();
        }
    }

    pub fn plan_requests(&self) -> Vec<Value> {
        self.shared.lock().plan_requests.clone()
    }

    /// Segment names, one per request starting at byte 0.
    pub fn segment_downloads(&self) -> Vec<String> {
        self.shared.lock().segment_downloads.clone()
    }

    pub fn block_fetches(&self) -> Vec<(String, u32)> {
        self.shared.lock().block_fetches.clone()
    }

    /// Every request's route and `Authorization` header.
    pub fn authorizations(&self) -> Vec<(String, Option<String>)> {
        self.shared.lock().authorizations.clone()
    }

    /// Close every open connection.
    pub fn kick(&self) {
        self.shared.kick.send_modify(|n| *n += 1);
    }
}

impl Drop for MockJetstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The scripted events a connection gets: at or after its cursor (a seq, or
/// a witnessed time from 1e15 up) and the retention floor, commits only in
/// its collections.
fn select(
    events: &[Value],
    params: &[(String, String)],
    collection_param: &str,
    floor: Option<i64>,
) -> Vec<Value> {
    let collections: Vec<&str> = params
        .iter()
        .filter(|(k, _)| k == collection_param)
        .map(|(_, v)| v.as_str())
        .collect();
    let cursor = params
        .iter()
        .find(|(k, _)| k == "cursor")
        .map(|(_, v)| v.parse::<i64>().expect("numeric cursor"));
    events
        .iter()
        .filter(|event| {
            let seq = event["seq"].as_i64().expect("seq");
            let after_cursor = match cursor {
                None => true,
                Some(c) if c < TIMESTAMP_CURSOR_MIN => seq >= c,
                Some(c) => event_witnessed_us(event) >= c,
            };
            let retained = floor.is_none_or(|floor| seq >= floor);
            let is_commit = event["$type"]
                .as_str()
                .is_some_and(|t| t.ends_with("#commit"));
            let wanted = !is_commit
                || collections.is_empty()
                || collections.contains(&event["collection"].as_str().unwrap_or_default());
            after_cursor && retained && wanted
        })
        .cloned()
        .collect()
}

fn authorization(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

async fn v2_subscribe(
    State(shared): State<Arc<Shared>>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    shared
        .lock()
        .authorizations
        .push(("v2".into(), authorization(&headers)));
    let query = query.unwrap_or_default();
    let params = parse_query(&query);
    let (frames, dictionary) = {
        let mut inner = shared.lock();
        inner.connections.push(Connection {
            path: "v2",
            query: query.clone(),
        });
        if inner.v2_disabled {
            return (StatusCode::NOT_FOUND, "404 page not found").into_response();
        }
        if let Some((status, body)) = inner.rejections.pop_front() {
            let status = StatusCode::from_u16(status).expect("status");
            return match body {
                Some(body) => (status, axum::Json(body)).into_response(),
                None => status.into_response(),
            };
        }
        let requested = params
            .iter()
            .find(|(k, _)| k == "zstdDictionary")
            .map(|(_, v)| v.parse::<u32>().expect("dictionary id"));
        let dictionary = match requested {
            Some(id) if !inner.accepted_dictionaries.contains(&id) => {
                let current = inner.served_dictionary.unwrap_or_default();
                return (
                    StatusCode::BAD_REQUEST,
                    axum::Json(json!({
                        "error": "UnknownZstdDictionary",
                        "message": format!("unknown zstd dictionary {id}; current dictionary id is {current}"),
                    })),
                )
                    .into_response();
            }
            Some(id) => Some(fixture_dictionary(id)),
            None => None,
        };
        let mut params = params;
        let mut notice = inner.notices.pop_front();
        if let Some(floor) = inner.retention_floor {
            let cursor = params
                .iter()
                .position(|(k, _)| k == "cursor")
                .map(|i| (i, params[i].1.parse::<i64>().expect("numeric cursor")));
            match cursor {
                Some((_, c)) if c < TIMESTAMP_CURSOR_MIN && c < floor => {
                    return (
                        StatusCode::BAD_REQUEST,
                        axum::Json(json!({
                            "error": "CursorTooOld",
                            "message": format!("cursor {c} below lookback floor {floor}"),
                        })),
                    )
                        .into_response();
                }
                Some((i, c)) if c >= TIMESTAMP_CURSOR_MIN && c < witnessed_us(floor) => {
                    params[i].1 = floor.to_string();
                    notice = Some((
                        "OutdatedCursor".to_string(),
                        format!("cursor clamped to seq {floor}"),
                    ));
                }
                _ => {}
            }
        }
        if notice
            .as_ref()
            .is_some_and(|(name, _)| name == "FutureCursor")
        {
            params.retain(|(k, _)| k != "cursor");
            if let Some(tip) = inner.tip {
                params.push(("cursor".to_string(), tip.to_string()));
            }
        }
        let mut frames: Vec<String> = Vec::new();
        if let Some((name, message)) = notice {
            frames.push(
                json!({"$type": "message", "payload": {
                    "$type": "network.bsky.jetstream.subscribeEvents#info",
                    "name": name,
                    "message": message,
                }})
                .to_string(),
            );
        }
        frames.extend(
            select(&inner.events, &params, "collections", inner.retention_floor)
                .into_iter()
                .map(|p| json!({"$type": "message", "payload": p}).to_string()),
        );
        (frames, dictionary)
    };
    let kick = shared.kick.subscribe();
    ws.on_upgrade(move |socket| serve(socket, frames, dictionary, kick))
}

async fn v1_subscribe(
    State(shared): State<Arc<Shared>>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    shared
        .lock()
        .authorizations
        .push(("v1".into(), authorization(&headers)));
    let query = query.unwrap_or_default();
    let params = parse_query(&query);
    let frames: Vec<String> = {
        let mut inner = shared.lock();
        inner.connections.push(Connection {
            path: "v1",
            query: query.clone(),
        });
        select(
            &inner.events,
            &params,
            "wantedCollections",
            inner.retention_floor,
        )
        .iter()
        .filter_map(to_v1)
        .map(|f| f.to_string())
        .collect()
    };
    let kick = shared.kick.subscribe();
    ws.on_upgrade(move |socket| serve(socket, frames, None, kick))
}

async fn get_dictionary(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    let served = {
        let mut inner = shared.lock();
        inner
            .authorizations
            .push(("dictionary".into(), authorization(&headers)));
        inner.served_dictionary
    };
    match served {
        Some(id) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/octet-stream")],
            fixture_dictionary(id),
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"error": "DictionaryNotFound"})),
        )
            .into_response(),
    }
}

fn throttled(inner: &mut Inner) -> Option<Response> {
    if inner.throttles == 0 {
        return None;
    }
    inner.throttles -= 1;
    Some(
        (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, "0")],
        )
            .into_response(),
    )
}

fn not_found(error: &str) -> Response {
    (StatusCode::NOT_FOUND, axum::Json(json!({ "error": error }))).into_response()
}

/// The kind names a row of `kind` answers to.
fn kind_name(kind: u8) -> &'static str {
    match kind {
        4 => "identity",
        5 => "account",
        6 => "sync",
        _ => "commit",
    }
}

fn strings(body: &Value, field: &str) -> Option<Vec<String>> {
    body[field].as_array().map(|items| {
        items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    })
}

async fn plan_snapshot(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    let mut inner = shared.lock();
    inner
        .authorizations
        .push(("plan".into(), authorization(&headers)));
    inner.plan_requests.push(body.clone());
    if let Some(response) = throttled(&mut inner) {
        return response;
    }
    if let Some(status) = inner.archive_disabled {
        return StatusCode::from_u16(status)
            .expect("status")
            .into_response();
    }
    let kinds = strings(&body, "kinds");
    let collections = strings(&body, "collections");
    let dids = strings(&body, "dids");
    if collections.is_some()
        && kinds
            .as_ref()
            .is_some_and(|k| !k.iter().any(|k| k == "commit"))
    {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({ "error": "InvalidRequest" })),
        )
            .into_response();
    }
    let after = body["afterSeq"].as_u64().unwrap_or(0);
    let highest = inner
        .segments
        .iter()
        .map(|(_, w, _)| w.blocks.last().map_or(0, |b| b.1))
        .max()
        .unwrap_or(0);
    let tip = body["beforeSeq"]
        .as_u64()
        .map_or(highest, |b| highest.min(b));
    let stats =
        json!({ "segmentsExamined": 0, "segmentsMatched": 0, "blocksMatched": 0, "entries": 0 });
    if inner.stalls > 0 {
        inner.stalls -= 1;
        return axum::Json(json!({
            "plannedThroughSeq": after,
            "sealedTipSeq": tip,
            "segments": [],
            "stats": stats,
        }))
        .into_response();
    }
    let matches = |e: &ArchiveEvent| {
        e.seq > after
            && e.seq <= tip
            && kinds
                .as_ref()
                .is_none_or(|k| k.iter().any(|k| k == kind_name(e.kind)))
            && (collections.is_none()
                || kind_name(e.kind) != "commit"
                || collections
                    .as_ref()
                    .is_some_and(|c| c.contains(&e.collection)))
            && dids.as_ref().is_none_or(|d| d.contains(&e.did))
    };
    let mut entries = Vec::new();
    for (position, (name, written, events)) in inner.segments.iter().enumerate() {
        let hits: Vec<u32> = written
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, (min, max, _, _))| {
                events
                    .iter()
                    .any(|e| e.seq >= *min && e.seq <= *max && matches(e))
            })
            .map(|(i, _)| i as u32)
            .collect();
        if hits.is_empty() {
            continue;
        }
        let mut entry = json!({
            "name": name,
            "index": position + 1,
            "checksum": written.checksum,
            "minSeq": written.blocks.first().map_or(0, |b| b.0),
            "maxSeq": written.blocks.last().map_or(0, |b| b.1),
        });
        if hits.len() * 4 >= written.blocks.len() * 3 {
            entry["mode"] = json!("segment");
        } else {
            let mut runs: Vec<(u32, u32)> = Vec::new();
            for index in hits {
                match runs.last_mut() {
                    Some(run) if run.1 + 1 == index => run.1 = index,
                    _ => runs.push((index, index)),
                }
            }
            entry["mode"] = json!("blocks");
            entry["blocks"] = runs
                .into_iter()
                .map(|(first, last)| json!({ "first": first, "last": last }))
                .collect();
        }
        entries.push(entry);
    }
    let limit = match inner.plan_page_entries {
        0 => 1000,
        n => n,
    };
    let planned_through = if entries.len() > limit {
        entries.truncate(limit);
        entries[limit - 1]["maxSeq"].as_u64().unwrap_or(tip)
    } else {
        tip
    };
    axum::Json(json!({
        "plannedThroughSeq": planned_through,
        "sealedTipSeq": tip,
        "segments": entries,
        "stats": stats,
    }))
    .into_response()
}

async fn get_segment(
    State(shared): State<Arc<Shared>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    // The answer is decided, and the download recorded, before a held
    // segment waits; the lock is never held across the wait.
    let (held, response) = segment_response(&shared, &query, &headers);
    if let Some(held) = held {
        held.notified().await;
    }
    response
}

fn segment_response(
    shared: &Shared,
    query: &HashMap<String, String>,
    headers: &HeaderMap,
) -> (Option<Arc<tokio::sync::Notify>>, Response) {
    let mut inner = shared.lock();
    inner
        .authorizations
        .push(("segment".into(), authorization(headers)));
    if let Some(response) = throttled(&mut inner) {
        return (None, response);
    }
    let name = query.get("name").cloned().unwrap_or_default();
    if inner.failed_segments.contains(&name) {
        return (None, StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    let Some((_, written, _)) = inner.segments.iter().find(|(n, _, _)| *n == name) else {
        return (None, not_found("SegmentNotFound"));
    };
    let (bytes, etag) = (written.bytes.clone(), format!("\"{}\"", written.checksum));
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.split_once('-'))
        .and_then(|(a, b)| {
            let start = a.parse::<usize>().ok()?;
            let end = b.parse::<usize>().ok();
            Some((start, end))
        });
    if range.is_none_or(|(start, _)| start == 0) {
        inner.segment_downloads.push(name.clone());
    }
    let held = inner.held_segments.get(&name).cloned();
    let if_range_differs = headers
        .get(header::IF_RANGE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|tag| tag != etag);
    let response = match range {
        Some((start, end)) if !if_range_differs && start < bytes.len() => {
            let end = end.unwrap_or(usize::MAX).min(bytes.len() - 1);
            (
                StatusCode::PARTIAL_CONTENT,
                [
                    (header::ETAG, etag),
                    (
                        header::CONTENT_RANGE,
                        format!("bytes {start}-{end}/{}", bytes.len()),
                    ),
                ],
                bytes[start..=end].to_vec(),
            )
                .into_response()
        }
        _ => (StatusCode::OK, [(header::ETAG, etag)], bytes).into_response(),
    };
    (held, response)
}

async fn get_block(
    State(shared): State<Arc<Shared>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let mut inner = shared.lock();
    inner
        .authorizations
        .push(("block".into(), authorization(&headers)));
    let segment = query.get("segment").cloned().unwrap_or_default();
    let index: u32 = query
        .get("blockIndex")
        .and_then(|v| v.parse().ok())
        .unwrap_or(u32::MAX);
    inner.block_fetches.push((segment.clone(), index));
    if let Some(response) = throttled(&mut inner) {
        return response;
    }
    let key = (segment.clone(), index);
    if let Some(left) = inner.block_failures.get_mut(&key).filter(|n| **n > 0) {
        if *left != usize::MAX {
            *left -= 1;
        }
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    if inner.corrupt_blocks.contains(&key) {
        return (StatusCode::OK, b"not a zstd frame".to_vec()).into_response();
    }
    let Some((_, written, _)) = inner.segments.iter().find(|(n, _, _)| *n == segment) else {
        return not_found("SegmentNotFound");
    };
    let Some(&(_, _, offset, size)) = written.blocks.get(index as usize) else {
        return not_found("BlockNotFound");
    };
    let start = offset as usize + 8;
    (
        StatusCode::OK,
        written.bytes[start..start + size as usize].to_vec(),
    )
        .into_response()
}

async fn serve(
    mut socket: WebSocket,
    frames: Vec<String>,
    dictionary: Option<Vec<u8>>,
    mut kick: watch::Receiver<u64>,
) {
    let mut compressor = dictionary
        .map(|d| zstd::bulk::Compressor::with_dictionary(3, &d).expect("zstd compressor"));
    for frame in frames {
        let message = match compressor.as_mut() {
            Some(c) => Message::Binary(c.compress(frame.as_bytes()).expect("compress").into()),
            None => Message::Text(frame.into()),
        };
        if socket.send(message).await.is_err() {
            return;
        }
    }
    loop {
        tokio::select! {
            _ = kick.changed() => {
                let _ = socket.send(Message::Close(None)).await;
                return;
            }
            message = socket.recv() => {
                if !matches!(message, Some(Ok(_))) {
                    return;
                }
            }
        }
    }
}

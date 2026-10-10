//! A scripted Jetstream: the v2 `subscribeEvents` and v1 `/subscribe`
//! websockets and `getZstdDictionary`, enough to drive the consumer through
//! cursor resumes, compression, pre-upgrade errors and a retention floor.
//!
//! Events are scripted as v2 payloads with `push`. A connection gets every
//! scripted event at or after its cursor (all of them without one) that its
//! collection filter admits, then stays open until the client leaves or the
//! test calls `kick`. Events pushed later reach the next connection.
//!
//! The dictionaries in `tests/fixtures/jetstream/` are from
//! bluesky-social/jetstream v0.3.10 (`internal/subscribe/dictionaries/`),
//! dual-licensed MIT OR Apache-2.0, Copyright (c) 2022-2026 Bluesky Social
//! PBC and Contributors.

use std::collections::{HashSet, VecDeque};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{RawQuery, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Map, Value, json};
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Witnessed time of seq 0; seq `n` is witnessed `n` milliseconds later.
pub const BASE_US: i64 = 1_779_719_000_000_000;
pub const DICTIONARY_A: u32 = 20_260_811;
pub const DICTIONARY_B: u32 = 1_612_007_021;
const TIMESTAMP_CURSOR_MIN: i64 = 1_000_000_000_000_000;

pub fn witnessed_us(seq: i64) -> i64 {
    BASE_US + seq * 1_000
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

/// The v1 frame for a scripted v2 payload; v1 never carries `#sync`.
fn to_v1(payload: &Value) -> Option<Value> {
    let kind = payload["$type"].as_str()?.rsplit('#').next()?;
    let did = payload["did"].clone();
    let time_us = witnessed_us(payload["seq"].as_i64()?);
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
    served_dictionary: Option<u32>,
    accepted_dictionaries: HashSet<u32>,
    connections: Vec<Connection>,
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

    /// Forget every scripted event, as a server whose seqs started over.
    pub fn reset_events(&self) {
        self.shared.lock().events.clear();
    }

    /// Send an `#info` frame first on the next v2 connection. After a
    /// `FutureCursor` notice that connection ignores its cursor and gets
    /// every scripted event, as from the live tip.
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
                Some(c) => witnessed_us(seq) >= c,
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

async fn v2_subscribe(
    State(shared): State<Arc<Shared>>,
    RawQuery(query): RawQuery,
    ws: WebSocketUpgrade,
) -> Response {
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
    ws: WebSocketUpgrade,
) -> Response {
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

async fn get_dictionary(State(shared): State<Arc<Shared>>) -> Response {
    let served = shared.lock().served_dictionary;
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

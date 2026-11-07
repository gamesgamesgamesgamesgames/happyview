//! The `/admin/backfill*` and `/admin/jetstream` responses, byte for byte.
//!
//! Every route is called against fixed jobs (a crawl, a replay, a gap replay,
//! a failed job with errors, a paused job and a failed account replay) by
//! callers with and without the permission it needs, and each response's
//! status and exact body is compared with `tests/fixtures/admin_backfill/`.
//! The fixtures were written from the code before the backfill job system
//! moved into the Jetstream source crate, so a move that changes a status, a
//! field, its order or an error message fails here.
//!
//! Ids and timestamps are seeded. Two fields cannot be, and are replaced
//! before comparing: `id` in the answer to a request that creates a job (a
//! random UUID), and the Jetstream status's `now_ms` (the clock).
//!
//! Runs on in-memory SQLite, and on Postgres too when `TEST_DATABASE_URL`
//! names one. `UPDATE_ADMIN_BACKFILL_FIXTURES=1` rewrites the fixtures.

mod common;

use axum::body::Body;
use axum::http::{Method, Request};
use happyview::AppState;
use happyview::db::{DatabaseBackend, adapt_sql};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

const ADMIN: (&str, &str) = ("user-admin", "did:plc:pinnedadmin");
const READER: (&str, &str) = ("user-reader", "did:plc:pinnedreader");
const NOBODY: (&str, &str) = ("user-nobody", "did:plc:pinnednobody");
/// The admin's key, limited to `backfill:read`.
const READ_KEY: &str = "hv_pinnedreadkey0123456789abcdef0123";
const PLATFORM_KEY: &str = "hv_pinnedplatformkey0123456789abcdef";

const CRAWL: &str = "job-crawl";
const REPLAY: &str = "job-replay";
const GAP: &str = "job-gap";
const FAILED: &str = "job-failed";
const PAUSED: &str = "job-paused";
const REPLAY_FAILED: &str = "job-replay-failed";

/// A collection with no lexicon, so a job started on it fails at once.
const MISSING: &str = "app.pinned.missing";

#[derive(Clone, Copy)]
enum Caller {
    Anonymous,
    Cookie(&'static str),
    Key(&'static str),
}

impl Caller {
    fn label(self) -> &'static str {
        match self {
            Caller::Anonymous => "anonymous",
            Caller::Cookie(did) if did == ADMIN.1 => "admin",
            Caller::Cookie(did) if did == READER.1 => "reader (backfill:read)",
            Caller::Cookie(_) => "nobody (no permissions)",
            Caller::Key(key) if key == READ_KEY => "api key (backfill:read)",
            Caller::Key(_) => "platform key",
        }
    }
}

struct Pinned {
    group: &'static str,
    entries: Vec<Value>,
}

impl Pinned {
    fn new(group: &'static str) -> Self {
        Self {
            group,
            entries: Vec::new(),
        }
    }

    fn push(&mut self, call: String, status: u16, body: String) {
        self.entries
            .push(json!({ "call": call, "status": status, "body": body }));
    }
}

struct App {
    state: AppState,
    router: axum::Router,
}

impl App {
    async fn send(
        &self,
        method: Method,
        uri: &str,
        caller: Caller,
        body: Option<Value>,
    ) -> (u16, String) {
        let response = self
            .router
            .clone()
            .oneshot(self.request(method, uri, caller, body))
            .await
            .expect("serve the request");
        let status = response.status().as_u16();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            String::from_utf8(bytes.to_vec()).expect("a utf-8 body"),
        )
    }

    /// The first event of an SSE response.
    async fn first_event(&self, uri: &str) -> (u16, String) {
        let response = self
            .router
            .clone()
            .oneshot(self.request(Method::GET, uri, Caller::Cookie(ADMIN.1), None))
            .await
            .expect("serve the request");
        let status = response.status().as_u16();
        let mut body = response.into_body();
        let mut out = Vec::new();
        // An SSE response stays open; any other ends after its body.
        while !out.ends_with(b"\n\n") {
            let Some(frame) =
                tokio::time::timeout(std::time::Duration::from_secs(10), body.frame())
                    .await
                    .expect("an event within ten seconds")
            else {
                break;
            };
            if let Ok(data) = frame.expect("a frame").into_data() {
                out.extend_from_slice(&data);
            }
        }
        (status, String::from_utf8(out).unwrap())
    }

    fn request(
        &self,
        method: Method,
        uri: &str,
        caller: Caller,
        body: Option<Value>,
    ) -> Request<Body> {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "127.0.0.1");
        match caller {
            Caller::Anonymous => {}
            Caller::Cookie(did) => {
                let (name, value) = common::auth::admin_cookie_header(did, &self.state.cookie_key);
                request = request.header(name, value);
            }
            Caller::Key(key) => {
                request = request.header("authorization", format!("Bearer {key}"));
            }
        }
        match body {
            Some(body) => request
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            None => request.body(Body::empty()).unwrap(),
        }
    }

    async fn exec(&self, sql: &str, binds: &[Bind<'_>]) {
        let sql = adapt_sql(sql, self.state.db_backend);
        let mut query = happyview::db::query(&sql);
        for bind in binds {
            query = match bind {
                Bind::Text(v) => query.bind(*v),
                Bind::Null => query.bind(None::<String>),
                Bind::NullInt => query.bind(None::<i32>),
                Bind::NullBig => query.bind(None::<i64>),
                Bind::Int(v) => query.bind(*v),
                Bind::Big(v) => query.bind(*v),
            };
        }
        query
            .execute(&self.state.db)
            .await
            .unwrap_or_else(|e| panic!("seed failed: {e}\n{sql}"));
    }
}

enum Bind<'a> {
    Text(&'a str),
    Null,
    NullInt,
    NullBig,
    Int(i32),
    Big(i64),
}

use Bind::{Big, Int, Null, NullBig, NullInt, Text};

fn sha256(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

async fn app(pool: sqlx::AnyPool, backend: DatabaseBackend) -> App {
    let mut config = happyview::test_support::test_config(backend);
    config.public_url = "http://127.0.0.1:0".into();
    // Unroutable: nothing these calls start may reach the network.
    config.plc_url = "http://127.0.0.1:1".into();
    config.relay_url = "http://127.0.0.1:1".into();
    config.platform_api_key_hash = Some(sha256(PLATFORM_KEY));
    config.session_secret = "test-session-secret-0123456789abcdef".into();
    let state = happyview::state::state_with_pool_on(config, pool, backend);
    let router = happyview::server::router(state.clone());
    App { state, router }
}

const BACKFILL_TABLES: [&str; 9] = [
    "happyview_backfill_replay_retries",
    "happyview_backfill_replay_sweeps",
    "happyview_backfill_errors",
    "happyview_backfill_queue",
    "happyview_backfill_cursors",
    "happyview_backfill_pds_stats",
    "happyview_backfill_completions",
    "happyview_backfill_repos",
    "happyview_backfill_jobs",
];

async fn seed(app: &App) {
    for table in BACKFILL_TABLES {
        app.exec(&format!("DELETE FROM {table}"), &[]).await;
    }
    for (id, _) in [ADMIN, READER, NOBODY] {
        app.exec(
            "DELETE FROM happyview_api_keys WHERE user_id = ?",
            &[Text(id)],
        )
        .await;
        app.exec(
            "DELETE FROM happyview_user_permissions WHERE user_id = ?",
            &[Text(id)],
        )
        .await;
        app.exec("DELETE FROM happyview_users WHERE id = ?", &[Text(id)])
            .await;
    }
    app.exec(
        "DELETE FROM happyview_event_logs WHERE event_type = 'auth.permission_denied'",
        &[],
    )
    .await;

    let at = "2026-01-01T00:00:00+00:00";
    for ((id, did), is_super) in [(ADMIN, 1), (READER, 0), (NOBODY, 0)] {
        app.exec(
            "INSERT INTO happyview_users (id, did, is_super, created_at) VALUES (?, ?, ?, ?)",
            &[Text(id), Text(did), Int(is_super), Text(at)],
        )
        .await;
    }
    app.exec(
        "INSERT INTO happyview_user_permissions (user_id, permission, granted_at) VALUES (?, ?, ?)",
        &[Text(READER.0), Text("backfill:read"), Text(at)],
    )
    .await;
    app.exec(
        "INSERT INTO happyview_api_keys (id, user_id, name, key_hash, key_prefix, permissions, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        &[
            Text("key-read"),
            Text(ADMIN.0),
            Text("read only"),
            Text(&sha256(READ_KEY)),
            Text("hv_pinne"),
            Text(r#"["backfill:read"]"#),
            Text(at),
        ],
    )
    .await;

    let job = "INSERT INTO happyview_backfill_jobs \
        (id, collection, did, scope, status, stage, total_repos, resolved_repos, processed_repos, total_records, error, \
         started_at, completed_at, created_at, queue_version, discovery_complete, error_counts, source, \
         replay_after_seq, replay_through_seq, replay_tip_seq, replay_bytes, replay_blocks_failed) \
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";

    // A finished crawl on the bounded queue.
    app.exec(
        job,
        &[
            Text(CRAWL),
            Text("app.pinned.post"),
            Null,
            Text("network"),
            Text("completed"),
            Text("completed"),
            Int(3),
            Int(3),
            Int(3),
            Int(12),
            Null,
            Text("2026-01-01T00:00:00+00:00"),
            Text("2026-01-01T01:00:00+00:00"),
            Text("2026-01-01T00:00:00+00:00"),
            Int(2),
            Int(1),
            Null,
            Text("crawl"),
            NullBig,
            NullBig,
            NullBig,
            Big(0),
            Int(0),
        ],
    )
    .await;
    // A finished replay.
    app.exec(
        job,
        &[
            Text(REPLAY),
            Text("app.pinned.post"),
            Null,
            Text("network"),
            Text("completed"),
            Text("completed"),
            Int(0),
            NullInt,
            Int(0),
            Int(40),
            Null,
            Text("2026-01-02T00:00:00+00:00"),
            Text("2026-01-02T02:00:00+00:00"),
            Text("2026-01-02T00:00:00+00:00"),
            Int(2),
            Int(1),
            Null,
            Text("replay"),
            Big(0),
            Big(5000),
            Big(5000),
            Big(123_456),
            Int(0),
        ],
    )
    .await;
    // A gap replay still running.
    app.exec(
        job,
        &[
            Text(GAP),
            Null,
            Null,
            Text("network"),
            Text("running"),
            Text("replaying"),
            Int(0),
            NullInt,
            Int(0),
            Int(7),
            Null,
            Text("2026-01-03T00:00:00+00:00"),
            Null,
            Text("2026-01-03T00:00:00+00:00"),
            Int(2),
            Int(0),
            Null,
            Text("replay"),
            Big(777),
            Big(800),
            Big(900),
            Big(2048),
            Int(1),
        ],
    )
    .await;
    // An account crawl that failed with errors, from before the bounded queue.
    app.exec(
        job,
        &[
            Text(FAILED),
            Text("app.pinned.post"),
            Null,
            Text("dids"),
            Text("failed"),
            Text("fetching_records"),
            Int(5),
            Int(4),
            Int(2),
            Int(9),
            Text("boom"),
            Text("2026-01-04T00:00:00+00:00"),
            Text("2026-01-04T00:30:00+00:00"),
            Text("2026-01-04T00:00:00+00:00"),
            Int(1),
            Int(1),
            Text(r#"{"timeout":2,"repo_not_found":1}"#),
            Text("crawl"),
            NullBig,
            NullBig,
            NullBig,
            Big(0),
            Int(0),
        ],
    )
    .await;
    // A paused crawl of a collection with no lexicon.
    app.exec(
        job,
        &[
            Text(PAUSED),
            Text(MISSING),
            Null,
            Text("network"),
            Text("paused"),
            Text("fetching_records"),
            Int(1),
            Int(0),
            Int(0),
            Int(0),
            Null,
            Text("2026-01-05T00:00:00+00:00"),
            Null,
            Text("2026-01-05T00:00:00+00:00"),
            Int(2),
            Int(1),
            Null,
            Text("crawl"),
            NullBig,
            NullBig,
            NullBig,
            Big(0),
            Int(0),
        ],
    )
    .await;
    // An account replay that failed after its account list was flushed.
    app.exec(
        job,
        &[
            Text(REPLAY_FAILED),
            Text("app.pinned.post"),
            Null,
            Text("dids"),
            Text("failed"),
            Text("replaying"),
            Int(2),
            NullInt,
            Int(0),
            Int(0),
            Text("archive unavailable"),
            Text("2026-01-06T00:00:00+00:00"),
            Text("2026-01-06T00:10:00+00:00"),
            Text("2026-01-06T00:00:00+00:00"),
            Int(2),
            Int(1),
            Null,
            Text("replay"),
            Big(0),
            Big(100),
            Big(200),
            Big(64),
            Int(1),
        ],
    )
    .await;

    // The crawl's recent completions, PDS counters, leftover units and cursor.
    for (id, did, records) in [
        (101, "did:plc:crawla", 4),
        (102, "did:plc:crawlb", 5),
        (103, "did:plc:crawlc", 3),
    ] {
        app.exec(
            "INSERT INTO happyview_backfill_completions (id, job_id, did, collection, pds_endpoint, records_fetched) VALUES (?, ?, ?, ?, ?, ?)",
            &[
                Big(id),
                Text(CRAWL),
                Text(did),
                Text("app.pinned.post"),
                Text("https://pds-one.example"),
                Int(records),
            ],
        )
        .await;
    }
    for (pds, repos, completed, records) in [
        ("https://pds-one.example", 2, 2, 9),
        ("https://pds-two.example", 1, 1, 3),
    ] {
        app.exec(
            "INSERT INTO happyview_backfill_pds_stats (job_id, pds_endpoint, repos, completed_repos, records, errors) VALUES (?, ?, ?, ?, ?, 0)",
            &[Text(CRAWL), Text(pds), Big(repos), Big(completed), Big(records)],
        )
        .await;
    }
    for (collection, did, pds) in [
        ("app.pinned.like", "did:plc:queuedb", None),
        (
            "app.pinned.post",
            "did:plc:queuedc",
            Some("https://pds-two.example"),
        ),
        ("app.pinned.post", "did:plc:queueda", None),
    ] {
        let pds = match pds {
            Some(pds) => Text(pds),
            None => Null,
        };
        app.exec(
            "INSERT INTO happyview_backfill_queue (job_id, collection, did, pds_endpoint) VALUES (?, ?, ?, ?)",
            &[Text(CRAWL), Text(collection), Text(did), pds],
        )
        .await;
    }
    app.exec(
        "INSERT INTO happyview_backfill_cursors (job_id, collection, relay_cursor, done) VALUES (?, ?, ?, 1)",
        &[Text(CRAWL), Text("app.pinned.post"), Text("cursor-1")],
    )
    .await;

    // The failed job's repo rows (legacy queue) and errors.
    for (did, pds, status, records) in [
        (
            "did:plc:failedb",
            Some("https://pds-one.example"),
            "completed",
            6,
        ),
        (
            "did:plc:faileda",
            Some("https://pds-one.example"),
            "completed",
            3,
        ),
        (
            "did:plc:failedc",
            Some("https://pds-two.example"),
            "failed",
            0,
        ),
        ("did:plc:failedd", None, "pending", 0),
    ] {
        let pds = match pds {
            Some(pds) => Text(pds),
            None => Null,
        };
        app.exec(
            "INSERT INTO happyview_backfill_repos (job_id, did, pds_endpoint, status, records_fetched) VALUES (?, ?, ?, ?, ?)",
            &[Text(FAILED), Text(did), pds, Text(status), Int(records)],
        )
        .await;
    }
    for (did, collection, phase, kind, message, attempts) in [
        (
            "did:plc:errorb",
            Some("app.pinned.post"),
            "fetch",
            "timeout",
            "operation timed out",
            3,
        ),
        (
            "did:plc:errora",
            None,
            "resolve",
            "timeout",
            "DID document request timed out",
            2,
        ),
        (
            "did:plc:errorc",
            Some("app.pinned.post"),
            "fetch",
            "repo_not_found",
            "PDS returned 400 Bad Request: Could not find repo",
            1,
        ),
    ] {
        let collection = match collection {
            Some(c) => Text(c),
            None => Null,
        };
        app.exec(
            "INSERT INTO happyview_backfill_errors (job_id, did, collection, phase, kind, message, attempts, last_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            &[
                Text(FAILED),
                Text(did),
                collection,
                Text(phase),
                Text(kind),
                Text(message),
                Int(attempts),
                Text("2026-01-04T00:20:00+00:00"),
            ],
        )
        .await;
    }
    // The failed replay's block failure: retryable, but its accounts are gone.
    app.exec(
        "INSERT INTO happyview_backfill_errors (job_id, did, collection, phase, kind, message, attempts, last_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            Text(REPLAY_FAILED),
            Text("segment-0001"),
            Null,
            Text("replay_block:3"),
            Text("replay_block"),
            Text("block 3 failed to decode"),
            Int(1),
            Text("2026-01-06T00:05:00+00:00"),
        ],
    )
    .await;
}

/// `body` with the value of every `"field":` replaced by `"<field>"`, for a
/// value that cannot be seeded. The rest of the body keeps its exact text.
/// The scrubbed values are numbers and UUIDs, which hold no `,` or `}`.
fn scrub(body: &str, field: &str) -> String {
    let key = format!("\"{field}\":");
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(at) = rest.find(&key) {
        let value_start = at + key.len();
        out.push_str(&rest[..value_start]);
        out.push_str(&format!("\"<{field}>\""));
        let tail = &rest[value_start..];
        let value_end = tail.find([',', '}']).unwrap_or(tail.len());
        rest = &tail[value_end..];
    }
    out.push_str(rest);
    out
}

const ROUTES: [(&str, &str, &str); 13] = [
    ("POST", "/admin/backfill", "backfill:create"),
    ("GET", "/admin/backfill/status", "backfill:read"),
    ("DELETE", "/admin/backfill/details", "backfill:create"),
    ("POST", "/admin/backfill/job-gap/cancel", "backfill:create"),
    ("POST", "/admin/backfill/job-gap/pause", "backfill:create"),
    (
        "POST",
        "/admin/backfill/job-paused/resume",
        "backfill:create",
    ),
    ("GET", "/admin/backfill/job-failed/events", "backfill:read"),
    ("GET", "/admin/backfill/job-crawl/repos", "backfill:read"),
    (
        "GET",
        "/admin/backfill/job-crawl/pds-summary",
        "backfill:read",
    ),
    ("GET", "/admin/backfill/job-failed/errors", "backfill:read"),
    (
        "POST",
        "/admin/backfill/job-failed/retry-failed",
        "backfill:create",
    ),
    (
        "DELETE",
        "/admin/backfill/job-crawl/details",
        "backfill:create",
    ),
    ("GET", "/admin/jetstream/status", "stats:read"),
];

fn method(name: &str) -> Method {
    Method::from_bytes(name.as_bytes()).unwrap()
}

/// A JSON body for the routes that read one, so a refusal is never a body
/// parse error.
fn route_body(verb: &str, uri: &str) -> Option<Value> {
    (verb == "POST" && (uri == "/admin/backfill" || uri.ends_with("/retry-failed")))
        .then(|| json!({}))
}

async fn auth_outcomes(app: &App) -> Pinned {
    let mut pinned = Pinned::new("auth");
    for (verb, uri, permission) in ROUTES {
        let callers: &[Caller] = match permission {
            "backfill:create" => &[
                Caller::Anonymous,
                Caller::Cookie(NOBODY.1),
                Caller::Cookie(READER.1),
                Caller::Key(READ_KEY),
                Caller::Key(PLATFORM_KEY),
            ],
            _ => &[
                Caller::Anonymous,
                Caller::Cookie(NOBODY.1),
                Caller::Key(PLATFORM_KEY),
            ],
        };
        for caller in callers {
            let (status, body) = app
                .send(method(verb), uri, *caller, route_body(verb, uri))
                .await;
            let body = if uri == "/admin/jetstream/status" && status == 200 {
                scrub(&body, "now_ms")
            } else {
                body
            };
            pinned.push(format!("{verb} {uri} as {}", caller.label()), status, body);
        }
    }
    pinned
}

/// The permission-denied rows the refusals above logged, without their
/// generated ids and times.
async fn denial_events(app: &App) -> Pinned {
    let sql = adapt_sql(
        "SELECT severity, actor_did, subject, detail FROM happyview_event_logs WHERE event_type = 'auth.permission_denied'",
        app.state.db_backend,
    );
    let mut rows: Vec<(String, Option<String>, Option<String>, String)> =
        happyview::db::query_as(&sql)
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    rows.sort();
    let mut pinned = Pinned::new("permission_denied_events");
    for (severity, actor, subject, detail) in rows {
        pinned.push(
            format!(
                "{} {} {}",
                severity,
                actor.unwrap_or_default(),
                subject.unwrap_or_default()
            ),
            0,
            detail,
        );
    }
    pinned
}

async fn reads(app: &App) -> Pinned {
    let mut pinned = Pinned::new("reads");
    let uris = [
        "/admin/backfill/status".to_string(),
        format!("/admin/backfill/{CRAWL}/repos"),
        format!("/admin/backfill/{CRAWL}/repos?phase=resolved"),
        format!("/admin/backfill/{CRAWL}/repos?limit=2"),
        format!("/admin/backfill/{CRAWL}/repos?limit=2&cursor=app.pinned.post%7Cdid:plc:queuedc"),
        format!("/admin/backfill/{CRAWL}/repos?cursor=nopipe"),
        format!("/admin/backfill/{CRAWL}/repos?phase=fetched"),
        format!("/admin/backfill/{CRAWL}/repos?phase=fetched&limit=2"),
        format!("/admin/backfill/{CRAWL}/repos?phase=fetched&limit=2&cursor=102"),
        format!("/admin/backfill/{CRAWL}/repos?phase=fetched&cursor=abc"),
        format!("/admin/backfill/{FAILED}/repos"),
        format!("/admin/backfill/{FAILED}/repos?phase=resolved"),
        format!("/admin/backfill/{FAILED}/repos?phase=fetched"),
        format!("/admin/backfill/{FAILED}/repos?limit=1"),
        format!("/admin/backfill/{FAILED}/repos?limit=1&cursor=did:plc:faileda"),
        "/admin/backfill/no-such-job/repos".to_string(),
        format!("/admin/backfill/{CRAWL}/pds-summary"),
        format!("/admin/backfill/{FAILED}/pds-summary"),
        "/admin/backfill/no-such-job/pds-summary".to_string(),
        format!("/admin/backfill/{FAILED}/errors"),
        format!("/admin/backfill/{FAILED}/errors?kind=timeout"),
        format!("/admin/backfill/{FAILED}/errors?limit=1"),
        format!("/admin/backfill/{FAILED}/errors?limit=1&cursor=did:plc:errora"),
        format!("/admin/backfill/{REPLAY_FAILED}/errors"),
        format!("/admin/backfill/{GAP}/errors"),
    ];
    for uri in uris {
        let (status, body) = app
            .send(Method::GET, &uri, Caller::Cookie(ADMIN.1), None)
            .await;
        pinned.push(format!("GET {uri}"), status, body);
    }
    for job in [CRAWL, REPLAY, GAP, FAILED] {
        let uri = format!("/admin/backfill/{job}/events");
        let (status, body) = app.first_event(&uri).await;
        pinned.push(format!("GET {uri} (first event)"), status, body);
    }
    let (status, body) = app
        .send(
            Method::GET,
            "/admin/jetstream/status",
            Caller::Cookie(ADMIN.1),
            None,
        )
        .await;
    pinned.push(
        "GET /admin/jetstream/status".into(),
        status,
        scrub(&body, "now_ms"),
    );
    pinned
}

/// Calls that change a job but start none.
async fn changes(app: &App) -> Pinned {
    let mut pinned = Pinned::new("changes");
    let calls: Vec<(&str, String, Option<Value>)> = vec![
        ("POST", "/admin/backfill/no-such-job/cancel".into(), None),
        ("POST", format!("/admin/backfill/{CRAWL}/cancel"), None),
        ("POST", "/admin/backfill/no-such-job/pause".into(), None),
        ("POST", format!("/admin/backfill/{CRAWL}/pause"), None),
        ("POST", format!("/admin/backfill/{GAP}/pause"), None),
        ("POST", format!("/admin/backfill/{GAP}/pause"), None),
        ("POST", format!("/admin/backfill/{GAP}/cancel"), None),
        ("POST", "/admin/backfill/no-such-job/resume".into(), None),
        ("POST", format!("/admin/backfill/{CRAWL}/resume"), None),
        (
            "POST",
            format!("/admin/backfill/{REPLAY_FAILED}/resume"),
            None,
        ),
        (
            "POST",
            "/admin/backfill/no-such-job/retry-failed".into(),
            Some(json!({})),
        ),
        (
            "POST",
            format!("/admin/backfill/{CRAWL}/retry-failed"),
            Some(json!({})),
        ),
        (
            "POST",
            format!("/admin/backfill/{FAILED}/retry-failed"),
            Some(json!({ "kinds": ["did_doc_invalid"] })),
        ),
        (
            "POST",
            format!("/admin/backfill/{REPLAY_FAILED}/retry-failed"),
            Some(json!({ "kinds": ["timeout"] })),
        ),
        (
            "POST",
            format!("/admin/backfill/{REPLAY_FAILED}/retry-failed"),
            Some(json!({})),
        ),
        (
            "POST",
            "/admin/backfill".into(),
            Some(json!({ "source": "sideways" })),
        ),
        (
            "POST",
            "/admin/backfill".into(),
            Some(json!({ "source": "replay", "dids": ["did:plc:someone"] })),
        ),
        (
            "POST",
            "/admin/backfill".into(),
            Some(json!({ "dids": ["  ", ""] })),
        ),
        (
            "POST",
            "/admin/backfill".into(),
            Some(json!({ "dids": ["did:bad", "did:plc:fine", "did:"] })),
        ),
        (
            "POST",
            "/admin/backfill".into(),
            Some(json!({
                "dids": (0..501).map(|i| format!("did:plc:many{i}")).collect::<Vec<_>>()
            })),
        ),
        ("DELETE", format!("/admin/backfill/{GAP}/details"), None),
        ("DELETE", format!("/admin/backfill/{FAILED}/details"), None),
        ("DELETE", format!("/admin/backfill/{CRAWL}/details"), None),
        ("DELETE", "/admin/backfill/details".into(), None),
        ("GET", "/admin/backfill/status".into(), None),
        ("GET", format!("/admin/backfill/{CRAWL}/repos"), None),
        (
            "GET",
            format!("/admin/backfill/{CRAWL}/repos?phase=fetched"),
            None,
        ),
        ("GET", format!("/admin/backfill/{CRAWL}/pds-summary"), None),
        ("GET", format!("/admin/backfill/{FAILED}/repos"), None),
    ];
    for (verb, uri, body) in calls {
        let (status, response) = app
            .send(method(verb), &uri, Caller::Cookie(ADMIN.1), body)
            .await;
        pinned.push(format!("{verb} {uri}"), status, response);
    }
    pinned
}

/// Calls that start a job, each new job's random `id` replaced.
/// Last, since the jobs they start run on in the background.
async fn starts(app: &App) -> Pinned {
    let mut pinned = Pinned::new("starts");
    let calls: Vec<(String, Value)> = vec![
        (
            format!("/admin/backfill/{FAILED}/retry-failed"),
            json!({ "kinds": ["repo_not_found"] }),
        ),
        (format!("/admin/backfill/{PAUSED}/resume"), json!({})),
        ("/admin/backfill".into(), json!({ "collection": MISSING })),
    ];
    for (uri, body) in calls {
        let body = (!uri.ends_with("/resume")).then_some(body);
        let (status, response) = app
            .send(Method::POST, &uri, Caller::Cookie(ADMIN.1), body)
            .await;
        // A resume answers with the job's own id; the others with a new one.
        let response = if uri.ends_with("/resume") {
            response
        } else {
            scrub(&response, "id")
        };
        pinned.push(format!("POST {uri}"), status, response);
    }
    pinned
}

fn fixture_path(group: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/admin_backfill")
        .join(format!("{group}.json"))
}

fn check(backend: DatabaseBackend, pinned: &Pinned, failures: &mut Vec<String>) {
    let path = fixture_path(pinned.group);
    let actual = Value::Array(pinned.entries.clone());
    if std::env::var("UPDATE_ADMIN_BACKFILL_FIXTURES").is_ok_and(|v| v == "1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string_pretty(&actual).unwrap()),
        )
        .unwrap();
        return;
    }
    let expected: Value = serde_json::from_str(
        &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display())),
    )
    .unwrap();
    let expected = expected.as_array().unwrap();
    if expected.len() != pinned.entries.len() {
        failures.push(format!(
            "{backend:?} {}: {} calls, fixture has {}",
            pinned.group,
            pinned.entries.len(),
            expected.len()
        ));
    }
    for (got, want) in pinned.entries.iter().zip(expected) {
        if got != want {
            failures.push(format!(
                "{backend:?} {}: {}\n  want {} {}\n  got  {} {}",
                pinned.group,
                want["call"],
                want["status"],
                want["body"],
                got["status"],
                got["body"]
            ));
        }
    }
}

async fn backends() -> Vec<(sqlx::AnyPool, DatabaseBackend)> {
    happyview::test_support::test_pools().await
}

#[tokio::test]
async fn admin_backfill_json_is_unchanged() {
    let _lock = common::db::acquire_test_lock().await;
    let mut failures = Vec::new();
    for (pool, backend) in backends().await {
        let app = app(pool, backend).await;
        seed(&app).await;
        let groups = [
            auth_outcomes(&app).await,
            denial_events(&app).await,
            reads(&app).await,
            changes(&app).await,
            starts(&app).await,
        ];
        for pinned in &groups {
            check(backend, pinned, &mut failures);
        }
    }
    assert!(
        failures.is_empty(),
        "admin backfill responses changed:\n{}",
        failures.join("\n")
    );
}

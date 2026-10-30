//! Admin `GET` routes read; they do not write.
//!
//! `csrf::guard` returns early for `GET`, `HEAD` and `OPTIONS`, which
//! `csrf::is_safe` names: a safe method is one with no side effect, so it has
//! nothing for a cross-site caller to abuse, and refusing cross-site
//! navigation would break every ordinary link into the dashboard. A `GET`
//! that *does* have a side effect is therefore reachable from any site that
//! can make the operator's browser follow a link, with the session cookie
//! attached, and the guard will not stop it — by design, on the premise this
//! file is here to hold up.
//! Nothing in the type system says a handler behind `get(...)` only reads,
//! and a name is no guard either: `list`, `status` and `preview` are all what
//! a mutating handler would be called if someone added one by mistake.
//!
//! So this calls every one of them, as a full admin, against a seeded
//! database, and checksums every table before and after each call. A route
//! that changes any table's row count or contents fails, and the failure
//! names the route and the table.
//!
//! **Why behavioural rather than a permission audit.** Reading each handler's
//! source for write statements, or for the permission it requires, measures
//! neither of the things that matter: a handler can write through a helper no
//! scan sees, and needing `settings:manage` to *read* is common enough here
//! that an allowlist of those reads runs to sixteen handlers. A checksum over
//! every table has no allowlist to grow.
//!
//! **What it does not cover: this watches the database and nothing else.** A
//! `GET` that mutates memory, writes a file, or makes an outbound request that
//! changes something elsewhere passes this sweep. The database is where the
//! damage would persist, which is why it is the thing checked, but a handler
//! that evicts a cache or posts to a PDS is outside what these tests can see.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

use common::app::TestApp;
use common::{echo_interpreter, fixtures};
use flate2::Compression;
use flate2::write::GzEncoder;
use happyview::db::adapt_sql;
use happyview::spaces::db as spaces_db;
use happyview::spaces::types::{AppAccess, Policy, Space, SpaceConfig};
use uuid::Uuid;

/// A table's state: how many rows, and a hash of their contents. Counting
/// alone would miss an `UPDATE`, which is the shape most of the plausible
/// accidents take — a `last_used_at`, a cached value written back on read.
type Fingerprint = BTreeMap<String, (i64, String)>;

const GAME: &str = "games.gamesgamesgamesgames.game";

/// Every router whose `GET` routes end up under `/admin`, as
/// `(file, the function that builds it)`.
///
/// The record source's routes are merged in by `admin_routes` from
/// `state.source.admin_routes()`, so they are admin `GET`s like any other and
/// the sweep has to drive them too.
const ROUTERS: &[(&str, &str)] = &[
    ("src/admin/mod.rs", "pub fn admin_routes"),
    (
        "crates/happyview-source-jetstream/src/admin.rs",
        "pub(crate) fn router()",
    ),
];

/// The admin router's `GET` routes, as `(path, handler)`.
///
/// Parsed from the routers' source because axum exposes no way to enumerate
/// what a `Router` holds. The parse only *finds* the routes — every assertion
/// below is made against a running instance — but a parse that quietly skipped
/// one would hand back a sweep that looks complete and is not, so anything it
/// cannot read fails rather than being passed over.
fn admin_get_routes() -> Vec<(String, String)> {
    let mut found = Vec::new();
    for (file, anchor) in ROUTERS {
        let source = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(file))
            .unwrap_or_else(|e| panic!("{file} should be readable: {e}"));
        let body = source
            .split_once(anchor)
            .unwrap_or_else(|| panic!("{anchor} should be in {file}"))
            .1;
        // To the end of that function, so a later router in the same file
        // cannot leak in.
        let body = body.split_once("\n}\n").map_or(body, |(before, _)| before);

        for chunk in body.split(".route(").skip(1) {
            let rest = chunk.trim_start();
            let path = rest.strip_prefix('"').and_then(|r| {
                let end = r.find('"')?;
                Some(r[..end].to_string())
            });
            // The chunk ends where this route's own method list does.
            let methods = rest.split("\n        .route(").next().unwrap_or(rest);
            let has_get = methods.contains("get(");
            let Some(path) = path else {
                assert!(
                    !has_get,
                    "{file}: a GET route's path is not a literal, so the sweep cannot call it. \
                     Teach this parse to read it:\n{}",
                    &methods[..methods.len().min(200)]
                );
                continue;
            };
            assert!(
                !methods.contains("get_service(") && !methods.contains("MethodFilter::GET"),
                "{file}: route '{path}' mounts a GET in a form this parse cannot read. Teach it, \
                 or this route escapes the sweep"
            );
            if !has_get {
                continue;
            }
            let after_path = &rest[path.len() + 2..];
            let at = after_path
                .find("get(")
                .expect("the chunk was found to contain get(");
            // A bare name as well as `module::name`: the record source's
            // router mounts its handlers unqualified.
            let handler: String = after_path[at + "get(".len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':')
                .collect();
            assert!(
                !handler.is_empty(),
                "{file}: route '{path}' mounts something other than a named handler behind get(), \
                 which this parse cannot follow"
            );
            found.push((path, handler));
        }
    }
    assert!(
        found.len() > 60,
        "the router parse found only {} GET routes, so it has stopped working",
        found.len()
    );
    found
}

/// Every table in the test database, bar sqlx's own migration ledger,
/// with the columns this compares.
async fn table_names(app: &TestApp, ignore: bool) -> BTreeMap<String, Vec<String>> {
    let sql = match app.state.db_backend {
        happyview::db::DatabaseBackend::Postgres => {
            // `::text` because information_schema columns are Postgres's `name`
            // type, which the `Any` driver cannot decode.
            "SELECT table_name::text FROM information_schema.tables \
             WHERE table_schema = 'public' AND table_type = 'BASE TABLE' \
             ORDER BY table_name"
        }
        happyview::db::DatabaseBackend::Sqlite => {
            "SELECT name FROM sqlite_master WHERE type = 'table' \
             AND name NOT LIKE 'sqlite_%' ORDER BY name"
        }
    };
    let rows = happyview::db::query_as::<(String,)>(sql)
        .fetch_all(&app.state.db)
        .await
        .expect("could not list the database's tables");
    let names: Vec<String> = rows
        .into_iter()
        .map(|(n,)| n)
        .filter(|n| n != "_sqlx_migrations")
        .collect();
    assert!(
        names.len() > 30,
        "only {} tables found, so the table listing has stopped working",
        names.len()
    );
    let mut out = BTreeMap::new();
    for name in names {
        let columns = columns_of(app, &name, ignore).await;
        out.insert(name, columns);
    }
    out
}

/// A [`TestApp`] with the plugin catalogue off: `GET /admin/plugins/official`
/// would otherwise reach the real registry, and these tests are about the
/// database, not the network.
async fn app_with_no_catalogue() -> TestApp {
    TestApp::new_with_registry_config(happyview::plugin::official_registry::RegistryConfig {
        registry: None,
        disabled: true,
    })
    .await
}

/// The columns a request writes by being *authenticated*, rather than by
/// anything its handler did, and which therefore say nothing about whether a
/// `GET` reads.
///
/// `UserAuth::authenticate` stamps `happyview_users.last_used_at` on every
/// cookie-authenticated admin request, from a detached `tokio::spawn` — so it
/// lands on an unpredictable side of any fingerprint taken around a request.
/// Excluded by column rather than by table, so every other column of
/// `happyview_users` (its permissions, `is_super`, the row count) stays under
/// test, and pinned by [`authentication_writes_only_the_last_used_stamp`] so
/// this exclusion cannot quietly widen.
const IGNORED_COLUMNS: &[(&str, &str)] = &[("happyview_users", "last_used_at")];

/// A table's columns, in a stable order, minus any in [`IGNORED_COLUMNS`].
async fn columns_of(app: &TestApp, table: &str, ignore: bool) -> Vec<String> {
    let columns: Vec<String> = match app.state.db_backend {
        happyview::db::DatabaseBackend::Postgres => happyview::db::query_as::<(String,)>(
            "SELECT column_name::text FROM information_schema.columns \
             WHERE table_schema = 'public' AND table_name = $1 ORDER BY ordinal_position",
        )
        .bind(table)
        .fetch_all(&app.state.db)
        .await
        .unwrap_or_else(|e| panic!("could not read {table}'s columns: {e}"))
        .into_iter()
        .map(|(c,)| c)
        .collect(),
        happyview::db::DatabaseBackend::Sqlite => happyview::db::query_as::<(String,)>(&format!(
            "SELECT name FROM pragma_table_info('{table}')"
        ))
        .fetch_all(&app.state.db)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|(c,)| c)
        .collect(),
    };
    columns
        .into_iter()
        .filter(|c| !ignore || !IGNORED_COLUMNS.contains(&(table, c.as_str())))
        .collect()
}

/// A row count and a content hash for every table.
///
/// The hash is order-independent (the rows are sorted by their own text), so
/// a different plan between two reads cannot look like a write. It is built
/// from an explicit column list rather than the whole row, which is what lets
/// [`IGNORED_COLUMNS`] be a column and not a table.
async fn fingerprint(app: &TestApp, tables: &BTreeMap<String, Vec<String>>) -> Fingerprint {
    let mut out = BTreeMap::new();
    for (table, columns) in tables {
        if columns.is_empty() {
            continue;
        }
        let sql = match app.state.db_backend {
            happyview::db::DatabaseBackend::Postgres => {
                let row = columns
                    .iter()
                    .map(|c| format!("\"{c}\""))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "SELECT count(*)::bigint, \
                     coalesce(md5(string_agg(row({row})::text, '' ORDER BY row({row})::text)), '') \
                     FROM \"{table}\""
                )
            }
            // SQLite has no row-to-text cast, so the row text is the columns
            // `quote()`d, which renders NULLs and blobs distinctly.
            happyview::db::DatabaseBackend::Sqlite => {
                let row_text = columns
                    .iter()
                    .map(|c| format!("quote(\"{c}\")"))
                    .collect::<Vec<_>>()
                    .join(" || '|' || ");
                format!(
                    "SELECT count(*), coalesce(group_concat(r, ''), '') FROM \
                     (SELECT {row_text} AS r FROM \"{table}\" ORDER BY r)"
                )
            }
        };
        let row = happyview::db::query_as::<(i64, String)>(&sql)
            .fetch_one(&app.state.db)
            .await
            .unwrap_or_else(|e| panic!("could not fingerprint {table}: {e}"));
        out.insert(table.clone(), row);
    }
    out
}

/// A fingerprint that has stopped moving: the same twice, 200 ms apart.
///
/// A handler can write from a detached `tokio::spawn`, which lands on its own
/// schedule. Reading once as soon as the response body is drained blames such
/// a write on whichever route happened to be next, or misses it entirely if it
/// was the last one — so each read waits for the database to settle.
async fn settled_fingerprint(app: &TestApp, tables: &BTreeMap<String, Vec<String>>) -> Fingerprint {
    let mut previous = fingerprint(app, tables).await;
    for _ in 0..25 {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let next = fingerprint(app, tables).await;
        if differences(&previous, &next).is_empty() {
            return next;
        }
        previous = next;
    }
    panic!("the database never stopped changing between two reads 200 ms apart");
}

/// The tables whose count or contents differ, as `(table, sentence)`.
fn differences(before: &Fingerprint, after: &Fingerprint) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (table, (count, hash)) in after {
        match before.get(table) {
            Some((was_count, was_hash)) => {
                if was_count != count {
                    out.push((
                        table.clone(),
                        format!("{table}: {was_count} rows became {count}"),
                    ));
                } else if was_hash != hash {
                    out.push((
                        table.clone(),
                        format!("{table}: {count} rows, but their contents changed"),
                    ));
                }
            }
            None => out.push((table.clone(), format!("{table}: the table itself appeared"))),
        }
    }
    for table in before.keys() {
        if !after.contains_key(table) {
            out.push((
                table.clone(),
                format!("{table}: the table itself went away"),
            ));
        }
    }
    out
}

/// Writes a `GET` makes on purpose: `(route, table, event_type, why)`.
///
/// Narrow by design. The route may move the table named here and nothing
/// else, and the change must be exactly one new row carrying `event_type` —
/// so a second write alongside the deliberate one, or a different kind of row
/// in the same table, still fails. A read that records having happened is a
/// real pattern and not an accident, but it is also still reachable cross-site
/// with the session cookie attached, so each entry is a decision someone made
/// rather than noise to be swept away.
const AUDITED_READS: &[(&str, &str, &str, &str)] = &[(
    "/spaces/{id}/records",
    "happyview_event_logs",
    "space.moderator_read",
    "log_content_read: an operator reading the contents of a permissioned \
     space is itself a privacy-relevant act, and spaces record it",
)];

/// How many rows of `event_type` the event log holds.
async fn audit_rows(app: &TestApp, event_type: &str) -> i64 {
    let sql = adapt_sql(
        "SELECT count(*) FROM happyview_event_logs WHERE event_type = ?",
        app.state.db_backend,
    );
    let (n,) = happyview::db::query_as::<(i64,)>(&sql)
        .bind(event_type)
        .fetch_one(&app.state.db)
        .await
        .expect("could not count audit rows");
    n
}

/// Query strings for routes that answer `400` without them, so the sweep
/// reaches what they actually do rather than their parameter parsing.
const QUERY: &[(&str, &str)] = &[
    ("/records", "collection=games.gamesgamesgamesgames.game"),
    ("/identity/resolve", "identifier=did:plc:testadmin"),
    ("/editor/declarations", "language=sweep"),
    // So it reaches the blob lookup rather than stopping at query extraction,
    // which is what its EXEMPT_FROM_SEEDING reason describes.
    (
        "/spaces/{id}/blob",
        "cid=bafyreibsrh5ijw2opbnpyopbnpyopbnpyopbnpyopbnpyopbnpyopbnpyop",
    ),
];

/// Routes the sweep cannot drive to a `2xx`, with the reason, and therefore
/// exercises only as far as the handler gets.
///
/// An entry here is a deliberate statement, not a shrug:
/// [`no_admin_get_route_writes`] fails both on an unlisted non-`2xx` and on a
/// listed route that now answers, so the list cannot rot in either direction.
const EXEMPT_FROM_SEEDING: &[(&str, &str)] = &[
    (
        "/identity/resolve",
        "it resolves an identifier against a PLC directory and verifies the handle \
         round-trips, which this harness does not stub, so it answers 400. Its whole \
         body is that one outbound call and the response built from it — there is no \
         later half for this sweep to miss",
    ),
    (
        "/spaces/{id}/blob",
        "it answers 404 until the space holds a record carrying a blob ref and the \
     blob itself is stored; what it does after that lookup is the same \
     log_content_read that /spaces/{id}/records is listed for in AUDITED_READS, \
     then an outbound fetch",
    ),
];

/// Routes whose response body is an open stream rather than a finite document,
/// and why each one's never ends.
///
/// The sweep drains every body, because a handler that streams could write
/// while the body is still being produced. A stream that stays open until the
/// client disconnects has no end to drain, so draining one does not answer the
/// question slowly, it never answers it: the first run of this sweep over the
/// record source's routes sat on `/backfill/{id}/events` for two and a half
/// hours. A listed route is still called and its status still recorded; only
/// the tail of its body goes unread, which costs this sweep whatever the
/// handler writes after its first flush.
const STREAMING_BODIES: &[(&str, &str)] = &[(
    "/backfill/{id}/events",
    "an SSE stream that sends one job snapshot and then forwards broadcast \
     events for as long as the client stays connected, so its body ends only \
     when the client leaves",
)];

/// A tar.gz project bundle, as `PUT /admin/project` takes.
fn bundle(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    for (path, contents) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, path, *contents).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

/// Seed an entity for every route that takes an `{id}` or an `{nsid}`, and
/// return the substitutions to make in the route paths.
///
/// A `404` means the handler stopped at its lookup, so whatever it does after
/// that is never reached and the sweep would be passing on routes it never
/// really ran. Seeded through the admin API wherever a route exists for it, so
/// what is seeded is what the API itself would have written; the rest go in as
/// SQL or through the same service function the handlers use, which `tests/`
/// may do.
async fn seed(app: &TestApp) -> BTreeMap<&'static str, String> {
    let mut ids = BTreeMap::new();

    let (status, body) = app
        .post_json_status(
            "/admin/lexicons",
            json!({ "lexicon_json": fixtures::game_record_lexicon(), "backfill": false }),
        )
        .await;
    assert!(status == 200 || status == 201, "seeding a lexicon: {body}");
    ids.insert("lexicon", GAME.to_string());

    let (status, body) = app
        .post_json_status(
            "/admin/api-clients",
            json!({
                "name": "read-only-sweep",
                "client_id_url": "https://sweep.example.com/oauth-client-metadata.json",
                "client_uri": "https://sweep.example.com",
                "redirect_uris": ["https://happyview.example.com/auth/callback"],
                "scopes": "atproto"
            }),
        )
        .await;
    assert!(
        status == 200 || status == 201,
        "seeding an api client: {body}"
    );
    let client_id = body["id"]
        .as_str()
        .expect("the created api client should carry an id")
        .to_string();
    // So `/api-clients/{id}/auth-key` has a key to answer with.
    let (status, body) = app
        .post_json_status(
            &format!("/admin/api-clients/{client_id}/auth-key/rotate"),
            json!({}),
        )
        .await;
    assert!(
        status == 200 || status == 201,
        "rotating an api client key: {body}"
    );
    ids.insert("api_client", client_id);

    // A bare array, not an object with a `users` key.
    let users = app.get_json("/admin/users").await;
    let user_id = users
        .as_array()
        .and_then(|u| u.first())
        .and_then(|u| u["id"].as_str())
        .expect("the admin user should be listed")
        .to_string();
    ids.insert("user", user_id);

    let (status, body) = app
        .post_json_status(
            "/admin/linked-repos",
            json!({
                "reason": "read-only sweep",
                "scopes": "atproto repo:com.example.note?action=create",
            }),
        )
        .await;
    assert!(
        status == 200 || status == 201,
        "seeding a linked repo: {body}"
    );
    ids.insert(
        "linked_repo",
        body["id"]
            .as_str()
            .expect("the created grant should carry an id")
            .to_string(),
    );

    // `plugins::get_secrets` and `get_allowed_hosts` look the plugin up in the
    // registry, not the database, so a row would not do. The capability is
    // added because `get_allowed_hosts` refuses a plugin without it, and a
    // refusal would leave the rest of that handler unswept.
    let mut plugin = echo_interpreter::plugin("sweep");
    if let Some(manifest) = plugin.manifest.as_mut() {
        manifest
            .capabilities
            .push(happyview::plugin::capabilities::PluginCapability::NetworkRequestDefined);
    }
    plugin.info = plugin
        .manifest
        .clone()
        .expect("the fixture has a manifest")
        .into();
    app.state.plugin_registry.register(plugin).await;
    ids.insert("plugin", "interpreter_echo".to_string());

    // `/service-entries/{id}/xrpcs` takes an i64, so a uuid would not even
    // parse.
    let (status, body) = app
        .post_json_status(
            "/admin/service-entries",
            json!({ "fragment_id": "#sweep", "service_type": "SweepAppView" }),
        )
        .await;
    assert!(
        status == 200 || status == 201,
        "seeding a service entry: {body}"
    );
    let entries = app.get_json("/admin/service-entries").await;
    let entry_id = entries
        .as_array()
        .and_then(|e| e.first())
        .and_then(|e| e["id"].as_i64())
        .expect("the created service entry should carry a numeric id");
    ids.insert("service_entry", entry_id.to_string());

    let space_id = Uuid::new_v4().to_string();
    let now = happyview::db::now_rfc3339();
    spaces_db::create_space(
        &app.state.db,
        app.state.db_backend,
        &Space {
            id: space_id.clone(),
            did: "did:plc:sweep-space".to_string(),
            authority_did: "did:plc:sweep-space".to_string(),
            creator_did: app.admin_did.clone(),
            type_nsid: "com.example.sweep".to_string(),
            skey: "main".to_string(),
            display_name: Some("Sweep".to_string()),
            description: None,
            read_policy: Policy::MemberList,
            write_policy: Policy::MemberList,
            app_access: AppAccess::Open,
            config: SpaceConfig::default(),
            revision: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        },
    )
    .await
    .expect("could not seed a space");
    ids.insert("space", space_id);

    let script_id = "sweep-script";
    let sql = adapt_sql(
        "INSERT INTO happyview_scripts (id, script_type, body, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(script_id)
        .bind("sweep")
        .bind("-- nothing runs this\n")
        .bind(&now)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .expect("could not seed a script");
    ids.insert("script", script_id.to_string());

    let sql = adapt_sql(
        "INSERT INTO happyview_dead_letter_scripts \
         (id, script_ref, host_kind, host_id, payload, error, attempts, created_at) \
         VALUES (1, ?, 'record', ?, '{}', 'seeded for the read-only sweep', 1, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(script_id)
        .bind(GAME)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .expect("could not seed a dead letter");
    ids.insert("dead_letter", "1".to_string());

    // The record source's `/backfill/{id}/*` reads need a job of their own;
    // `happyview_jobs` below is the script job queue, a different thing.
    let backfill_id = "33333333-3333-4333-8333-333333333333";
    let sql = adapt_sql(
        "INSERT INTO happyview_backfill_jobs (id, collection, status, stage, created_at) \
         VALUES (?, ?, 'completed', 'done', ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(backfill_id)
        .bind(GAME)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .expect("could not seed a backfill job");
    ids.insert("backfill", backfill_id.to_string());

    let job_id = "22222222-2222-4222-8222-222222222222";
    let sql = adapt_sql(
        "INSERT INTO happyview_jobs (id, job_type, status, input, progress, created_by, created_at) \
         VALUES (?, ?, 'completed', '{}', '{}', ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(job_id)
        .bind("test.read-only-sweep")
        .bind(&app.admin_did)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .expect("could not seed a job");
    ids.insert("job", job_id.to_string());

    // Deployed last: a project build installs a snapshot, and the routes above
    // are seeded against the instance as it was.
    let lexicon = json!({
        "lexicon": 1,
        "id": "com.example.sweep",
        "defs": {"main": {"type": "record", "key": "tid", "record": {"type": "object", "properties": {}}}}
    })
    .to_string();
    let cookie = app.admin_cookie();
    let deploy = Request::builder()
        .method("PUT")
        .uri("/admin/project")
        .header(cookie.0, cookie.1)
        .header("content-type", "application/gzip")
        .body(Body::from(bundle(&[(
            "src/lexicons/com/example/sweep.json",
            lexicon.as_bytes(),
        )])))
        .unwrap();
    let response = app.router.clone().oneshot(deploy).await.unwrap();
    let status = response.status().as_u16();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        status,
        200,
        "seeding a deployed project: {}",
        String::from_utf8_lossy(&body)
    );

    ids
}

/// The concrete path to call for a route, with `{id}`/`{nsid}` filled in from
/// what [`seed`] made.
fn concrete_path(path: &str, ids: &BTreeMap<&'static str, String>) -> String {
    // Longest prefix first, so `/api-clients/` does not shadow nothing and
    // `/lexicon-publishing/dns/` is not read as `/lexicons/`.
    const BY_PREFIX: &[(&str, &str)] = &[
        ("/lexicon-publishing/dns/", "lexicon"),
        ("/backfill/", "backfill"),
        ("/network-lexicons/resolve/", "lexicon"),
        ("/lexicons/", "lexicon"),
        ("/linked-repos/", "linked_repo"),
        ("/dead-letters/", "dead_letter"),
        ("/api-clients/", "api_client"),
        ("/service-entries/", "service_entry"),
        ("/plugins/", "plugin"),
        ("/scripts/", "script"),
        ("/spaces/", "space"),
        ("/users/", "user"),
        ("/jobs/", "job"),
    ];
    let fallback = "00000000-0000-4000-8000-000000000000".to_string();
    let value = BY_PREFIX
        .iter()
        .find(|(prefix, _)| path.starts_with(prefix))
        .and_then(|(_, key)| ids.get(*key))
        .unwrap_or(&fallback);
    path.replace("{id}", value).replace("{nsid}", value)
}

/// Every admin `GET` route, called as a full admin, changes no table.
///
/// The fingerprint is taken after every route rather than once at the end, so
/// a failure names the route that did it instead of the sweep as a whole.
#[tokio::test]
async fn no_admin_get_route_writes() {
    common::require_db!();

    let app = app_with_no_catalogue().await;

    let ids = seed(&app).await;
    let tables = table_names(&app, true).await;
    let routes = admin_get_routes();

    let mut previous = settled_fingerprint(&app, &tables).await;
    let mut offenders: Vec<String> = Vec::new();
    let mut not_reached: Vec<(&str, String)> = Vec::new();
    let mut streams: Vec<String> = Vec::new();
    let mut reached = 0usize;
    let mut audited_seen: Vec<&str> = Vec::new();

    // Twice: once on freshly seeded state and once after the whole pass, so a
    // write that only happens the second time round — a cache filled on first
    // read, a row written only when one already exists — is still caught, and
    // each route is called after every other route has run.
    for pass in 1..=2 {
        for (path, handler) in &routes {
            let mut uri = format!("/admin{}", concrete_path(path, &ids));
            if let Some((_, query)) = QUERY.iter().find(|(p, _)| p == path) {
                uri.push('?');
                uri.push_str(query);
            }
            let cookie = app.admin_cookie();
            let request = Request::builder()
                .uri(&uri)
                .header(cookie.0, cookie.1)
                .body(Body::empty())
                .unwrap();

            // Several of these resolve identities or reach a PDS. A slow one
            // must not hang the sweep, and a request that never answered is
            // not a request this sweep has checked.
            let audit_before = audit_rows(&app, "space.moderator_read").await;
            let call = app.router.clone().oneshot(request);
            let (status, body_ended) =
                match tokio::time::timeout(std::time::Duration::from_secs(20), call).await {
                    Ok(response) => {
                        let response = response.unwrap();
                        let status = response.status().as_u16();
                        // Drained, because a handler that streams could write
                        // while the body is still being produced; bounded,
                        // because a body that never ends would otherwise stop
                        // the sweep rather than fail it (see STREAMING_BODIES).
                        let drained = tokio::time::timeout(
                            std::time::Duration::from_secs(5),
                            response.into_body().collect(),
                        )
                        .await;
                        (Some(status), drained.is_ok())
                    }
                    Err(_) => (None, false),
                };

            if pass == 1 && status.is_some() {
                let listed = STREAMING_BODIES.iter().any(|(p, _)| p == path);
                if !body_ended && !listed {
                    streams.push(format!(
                        "{handler} ({uri}) did not finish its body within 5s; if it is a \
                         stream that stays open, list it in STREAMING_BODIES with a reason, \
                         and know that this sweep then sees only what it writes before its \
                         first flush"
                    ));
                } else if body_ended && listed {
                    streams.push(format!(
                        "STREAMING_BODIES lists {path}, but its body finished: drop the entry"
                    ));
                }
            }

            if pass == 1 {
                match status {
                    Some(code) if (200..300).contains(&code) => reached += 1,
                    Some(code) => {
                        not_reached.push((path.as_str(), format!("{handler} ({uri}) → {code}")))
                    }
                    // A timeout is not coverage: nothing was observed, so it
                    // has to fail unless the route is exempt.
                    None => not_reached
                        .push((path.as_str(), format!("{handler} ({uri}) → did not answer"))),
                }
            }

            let current = settled_fingerprint(&app, &tables).await;
            for (table, sentence) in differences(&previous, &current) {
                let audited = AUDITED_READS
                    .iter()
                    .find(|(route, audited_table, _, _)| route == path && *audited_table == table);
                match audited {
                    Some((_, _, event_type, _)) => {
                        let gained = audit_rows(&app, event_type).await - audit_before;
                        assert_eq!(
                            gained, 1,
                            "GET /admin{path} is allowed exactly one '{event_type}' row and \
                             added {gained}; {sentence}"
                        );
                        audited_seen.push(path.as_str());
                    }
                    None => offenders.push(format!(
                        "pass {pass}: GET /admin{path} ({handler}) — {sentence}"
                    )),
                }
            }
            previous = current;
        }
    }

    // Anything but a 2xx means the handler stopped before its own work — at
    // its lookup, or at a parameter it was not given — so the sweep never ran
    // the part of it that could write. Asserted rather than reported: a
    // printed list is a skip that reads like a pass, and a new GET route must
    // not be able to escape this sweep by having nothing seeded for it.
    let unexpected: Vec<&str> = not_reached
        .iter()
        .filter(|(path, _)| !EXEMPT_FROM_SEEDING.iter().any(|(listed, _)| listed == path))
        .map(|(_, what)| what.as_str())
        .collect();
    assert!(
        unexpected.is_empty(),
        "these routes did not answer 2xx, so this sweep never reached what they do. Seed what \
         they need, give them a query string in QUERY, or add the route to EXEMPT_FROM_SEEDING \
         with the reason it cannot be reached:\n  {}",
        unexpected.join("\n  ")
    );
    // And the exemptions do not rot: a listed route that now answers is one
    // the list should stop excusing.
    let stale: Vec<&str> = EXEMPT_FROM_SEEDING
        .iter()
        .filter(|(path, _)| {
            routes.iter().any(|(p, _)| p == path) && !not_reached.iter().any(|(got, _)| got == path)
        })
        .map(|(path, _)| *path)
        .collect();
    assert!(
        stale.is_empty(),
        "EXEMPT_FROM_SEEDING excuses routes that now answer 2xx: {stale:?}"
    );

    // Nor does AUDITED_READS rot: a route listed as writing on purpose that
    // no longer writes should stop being excused, or the next accidental
    // write to that table on that route would be permitted silently.
    let unused: Vec<&str> = AUDITED_READS
        .iter()
        .filter(|(route, _, _, _)| {
            routes.iter().any(|(p, _)| p == route) && !audited_seen.contains(route)
        })
        .map(|(route, _, _, _)| *route)
        .collect();
    assert!(
        unused.is_empty(),
        "AUDITED_READS lists routes that no longer write: {unused:?}"
    );

    // Nor does STREAMING_BODIES: a route is listed because its body has no
    // end, and the sweep reads less of a listed route than of any other, so
    // an entry that stops being true costs coverage silently.
    assert!(
        streams.is_empty(),
        "the sweep could not read these routes' bodies the way STREAMING_BODIES says it \
         should:\n  {}",
        streams.join("\n  ")
    );

    // One more settled read after the loop: the last route's detached write
    // has nothing following it to notice the change.
    let after_everything = settled_fingerprint(&app, &tables).await;
    for (_, sentence) in differences(&previous, &after_everything) {
        offenders.push(format!("after the whole sweep — {sentence}"));
    }

    assert!(
        offenders.is_empty(),
        "an admin GET route wrote to the database, and the CSRF guard exempts safe methods, \
         so it is reachable from any site that can make the operator's browser follow a link:\n  {}",
        offenders.join("\n  ")
    );

    // A sweep in which nothing reached a handler would pass for the wrong
    // reason: every route 404ing, or the admin cookie not authenticating.
    assert!(
        reached >= routes.len() / 2,
        "only {reached} of {} routes got past 404, so this sweep proves little",
        routes.len()
    );
}

/// Authentication's own write is exactly `happyview_users.last_used_at`.
///
/// This is what earns [`IGNORED_COLUMNS`] its one entry. If the auth layer
/// ever starts writing something else — a session row, an audit line — the
/// sweep above would quietly stop seeing it, so the exclusion is pinned here
/// rather than taken on trust.
#[tokio::test]
async fn authentication_writes_only_the_last_used_stamp() {
    common::require_db!();
    let app = app_with_no_catalogue().await;

    let every_column = table_names(&app, false).await;
    let swept = table_names(&app, true).await;
    let before_all = fingerprint(&app, &every_column).await;
    let before_swept = fingerprint(&app, &swept).await;

    // A route that plainly reads: the permission names are a constant.
    let _ = app.get_json("/admin/permissions").await;

    // The stamp is written from a detached task, so it lands on its own
    // schedule. Waiting for it is the point: this test is about what that
    // task does.
    let mut after_all = fingerprint(&app, &every_column).await;
    for _ in 0..50 {
        if !differences(&before_all, &after_all).is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        after_all = fingerprint(&app, &every_column).await;
    }

    let seen = differences(&before_all, &after_all);
    assert_eq!(
        seen.len(),
        1,
        "authenticating should move one thing and did not: {seen:?}"
    );
    assert!(
        seen[0].0 == "happyview_users" && seen[0].1.contains("contents changed"),
        "the row count should hold and only the contents move: {seen:?}"
    );

    // And with `last_used_at` excluded, as the sweep excludes it, that same
    // request moved nothing at all.
    let after_swept = fingerprint(&app, &swept).await;
    let residue = differences(&before_swept, &after_swept);
    assert!(
        residue.is_empty(),
        "excluding last_used_at should leave nothing: {residue:?}"
    );
}

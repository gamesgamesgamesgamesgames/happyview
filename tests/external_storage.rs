//! An external `RecordStorage` claiming collections: every record read and
//! write for a claimed collection goes to it, through `state.records`, and
//! nothing for it lands in the local tables. Runs on in-memory SQLite: the
//! instance and the fake storage each have their own database.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use happyview::AppState;
use happyview::db::DatabaseBackend;
use happyview::lexicon::ProcedureAction;
use happyview::plugin::library::LibraryCallContext;
use happyview::plugin::{LoadedPlugin, PluginInfo, PluginManifest, PluginSource};
use happyview::record_store::{PreparedRecord, RecordStore};
use happyview_provider::{
    Action, Delta, IngestOutcome, Origin, RecordEvent, RecordKey, SourceEvent,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use common::echo_interpreter;
use common::fake_storage::FakeStorage;

/// Claimed by the fake storage.
const CLAIMED: &str = "games.gamesgamesgamesgames.game";
const CLAIMED_QUERY: &str = "games.gamesgamesgamesgames.listGames";
/// Stored locally.
const LOCAL: &str = "app.local.note";
const LOCAL_QUERY: &str = "app.local.listNotes";
const DID: &str = "did:plc:extstore";
const ADMIN: &str = "did:plc:extadmin";

fn uri(collection: &str, rkey: &str) -> String {
    format!("at://{DID}/{collection}/{rkey}")
}

fn key(collection: &str, rkey: &str) -> RecordKey {
    RecordKey {
        did: DID.to_string(),
        collection: collection.to_string(),
        rkey: rkey.to_string(),
    }
}

fn prepared(collection: &str, rkey: &str, body: Value) -> PreparedRecord {
    PreparedRecord {
        did: DID.to_string(),
        collection: collection.to_string(),
        rkey: rkey.to_string(),
        record_json: body.to_string(),
        cid: format!("cid-{rkey}"),
        rev: None,
    }
}

fn cid_of(body: &Value) -> String {
    happyview::cid_verify::compute_record_cid(body)
        .expect("cid")
        .to_string()
}

fn tid(n: i64) -> String {
    happyview::tid::tid_from_unix_microseconds(1_700_000_000_000_000 + n)
}

fn record_event(
    collection: &str,
    rkey: &str,
    action: Action,
    body: Option<Value>,
    rev: Option<String>,
    stored: bool,
) -> SourceEvent {
    SourceEvent::Record(RecordEvent {
        key: key(collection, rkey),
        action,
        cid: body.as_ref().map(cid_of),
        rev,
        body,
        time_us: None,
        origin: Origin::Live,
        stored,
    })
}

fn record_lexicon(id: &str) -> Value {
    json!({
        "lexicon": 1,
        "id": id,
        "defs": {"main": {"type": "record", "key": "tid",
            "record": {"type": "object", "properties": {"text": {"type": "string"}}}}}
    })
}

fn query_lexicon(id: &str) -> Value {
    json!({
        "lexicon": 1,
        "id": id,
        "defs": {"main": {"type": "query",
            "parameters": {"type": "params", "properties": {"limit": {"type": "integer"}}},
            "output": {"encoding": "application/json"}}}
    })
}

async fn put_lexicon(state: &AppState, lexicon: Value, target: Option<&str>) {
    state
        .lexicons
        .put_manual(
            &state.db,
            state.db_backend,
            happyview::catalog::LexiconWrite {
                lexicon_json: lexicon,
                backfill: false,
                target_collection: target.map(str::to_string),
                action: ProcedureAction::Upsert,
                token_cost: None,
                authority_did: None,
            },
        )
        .await
        .expect("store a lexicon");
}

/// An instance on its own in-memory SQLite database with both collections'
/// lexicons and an admin, its record store routed to `storage` when given.
async fn instance(storage: Option<Arc<FakeStorage>>) -> AppState {
    let pool = happyview::test_support::migrated_memory_pool().await;
    let mut state = happyview::test_support::test_state_with_pool_on(pool, DatabaseBackend::Sqlite);
    if let Some(storage) = storage {
        state.records = state.records.clone().with_external(storage);
    }
    state.config.session_secret = "test-session-secret-0123456789abcdef".into();
    state
        .domain_cache
        .insert(happyview::domain::Domain {
            id: uuid::Uuid::new_v4().to_string(),
            url: "http://127.0.0.1:0".to_string(),
            is_primary: true,
            created_at: happyview::db::now_rfc3339(),
            updated_at: happyview::db::now_rfc3339(),
        })
        .await;
    for (record, query, collection) in [
        (CLAIMED, CLAIMED_QUERY, CLAIMED),
        (LOCAL, LOCAL_QUERY, LOCAL),
    ] {
        put_lexicon(&state, record_lexicon(record), Some(collection)).await;
        put_lexicon(&state, query_lexicon(query), Some(collection)).await;
    }
    happyview::db::query(
        "INSERT INTO happyview_users (id, did, is_super, created_at) VALUES (?, ?, 1, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(ADMIN)
    .bind(happyview::db::now_rfc3339())
    .execute(&state.db)
    .await
    .expect("seed the admin");
    state
}

async fn send(state: &AppState, request: Request<Body>) -> (StatusCode, Value) {
    let response = happyview::server::router(state.clone())
        .oneshot(request)
        .await
        .expect("request");
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn get(state: &AppState, path: &str) -> (StatusCode, Value) {
    send(
        state,
        Request::get(path)
            .header("host", "127.0.0.1")
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

async fn admin_delete(state: &AppState, uri: &str) -> (StatusCode, Value) {
    let (name, value) = common::auth::admin_cookie_header(ADMIN, &state.cookie_key);
    send(
        state,
        Request::delete(format!("/admin/records?uri={uri}"))
            .header("host", "127.0.0.1")
            .header(name, value)
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

/// Rows in the instance's own record table, whatever the routing.
async fn local_rows(state: &AppState) -> i64 {
    happyview::db::query_scalar::<i64>("SELECT COUNT(*) FROM happyview_records_v2")
        .fetch_one(&state.db)
        .await
        .expect("count local rows")
}

async fn install_script(state: &AppState, trigger: &str, body: &str) {
    state
        .scripts
        .put_manual(
            &state.db,
            state.db_backend,
            happyview::catalog::ScriptWrite {
                id: trigger.to_string(),
                script_type: "lua".to_string(),
                body: body.to_string(),
                description: None,
            },
        )
        .await
        .expect("install script");
}

#[tokio::test]
async fn a_claimed_collection_is_read_from_the_external_storage() {
    let storage = Arc::new(FakeStorage::claiming(&[CLAIMED]).await);
    storage
        .seed(&[prepared(CLAIMED, "g1", json!({"text": "elsewhere"}))])
        .await;
    let state = instance(Some(storage.clone())).await;

    let (status, body) = get(
        &state,
        &format!("/xrpc/{CLAIMED_QUERY}?uri={}", uri(CLAIMED, "g1")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["record"]["text"], "elsewhere");

    let (status, body) = get(&state, &format!("/xrpc/{CLAIMED_QUERY}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["records"].as_array().unwrap().len(), 1);
    assert_eq!(body["records"][0]["uri"], uri(CLAIMED, "g1"));

    assert_eq!(state.records.count_collection(CLAIMED).await.unwrap(), 1);
    assert_eq!(local_rows(&state).await, 0);
}

#[tokio::test]
async fn a_procedure_write_to_a_claimed_collection_is_a_pending_delta() {
    let storage = Arc::new(FakeStorage::claiming(&[CLAIMED]).await);
    let mut state = instance(Some(storage.clone())).await;
    put_lexicon(
        &state,
        common::fixtures::create_game_procedure_lexicon(),
        Some(CLAIMED),
    )
    .await;
    // The `happyview test` harness: the caller is forged and the write goes
    // to its in-process PDS, through the same procedure handler a session's
    // write takes.
    state.test = Some(Arc::new(happyview::testing::TestHarness::new("procedure")));
    let mut request = Request::post("/xrpc/games.gamesgamesgamesgames.createGame")
        .header("host", "127.0.0.1")
        .header("x-client-key", happyview::testing::TEST_CLIENT_KEY)
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"title": "Pending", "createdAt": "2026-01-02T03:04:05.000Z"}).to_string(),
        ))
        .unwrap();
    request
        .extensions_mut()
        .insert(happyview::testing::ForgedCaller::new(DID));
    let (status, body) = send(&state, request).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let deltas = storage.deltas();
    assert_eq!(deltas.len(), 1, "{deltas:?}");
    let (key, delta, for_rev) = &deltas[0];
    assert_eq!(key.uri(), body["uri"].as_str().unwrap());
    assert_eq!((key.did.as_str(), key.collection.as_str()), (DID, CLAIMED));
    assert_eq!(for_rev, &None);
    let Delta::Pending {
        cid,
        body: pending,
        created_at_us,
        ..
    } = delta
    else {
        panic!("not a pending delta: {delta:?}");
    };
    assert_eq!(cid, body["cid"].as_str().unwrap());
    let pending: Value = serde_json::from_str(pending).unwrap();
    assert_eq!(pending["title"], "Pending");
    assert_eq!(pending["$type"], CLAIMED);
    assert_eq!(*created_at_us, 1_767_323_045_000_000);
    assert_eq!(local_rows(&state).await, 0);
}

#[tokio::test]
async fn an_admin_delete_of_a_claimed_record_is_a_hide_delta() {
    let storage = Arc::new(FakeStorage::claiming(&[CLAIMED]).await);
    storage
        .seed(&[prepared(CLAIMED, "g1", json!({"text": "elsewhere"}))])
        .await;
    let state = instance(Some(storage.clone())).await;

    let (status, body) = admin_delete(&state, &uri(CLAIMED, "g1")).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(
        storage.deltas(),
        vec![(key(CLAIMED, "g1"), Delta::Hide, None)]
    );
}

#[tokio::test]
async fn a_stored_event_writes_only_the_hook_delta() {
    if !echo_interpreter::is_built() {
        eprintln!("skipping: {}", echo_interpreter::BUILD);
        return;
    }
    let storage = Arc::new(FakeStorage::claiming(&[CLAIMED]).await);
    let state = instance(Some(storage.clone())).await;
    state
        .plugin_registry
        .register(echo_interpreter::plugin("lua"))
        .await;
    let body = json!({"$type": CLAIMED, "text": "arrived"});
    let cid = cid_of(&body);

    // Skip: the record hook returns nil.
    install_script(&state, &format!("record.create:{CLAIMED}"), "value:none").await;
    let outcome = happyview::ingest::apply(
        &state,
        record_event(
            CLAIMED,
            "s1",
            Action::Create,
            Some(body.clone()),
            Some(tid(1)),
            true,
        ),
    )
    .await;
    assert_eq!(outcome, IngestOutcome::Skipped);

    // Replace: the record hook returns a body of its own.
    install_script(
        &state,
        &format!("record.create:{CLAIMED}"),
        r#"returns:{"text":"hooked"}"#,
    )
    .await;
    let outcome = happyview::ingest::apply(
        &state,
        record_event(
            CLAIMED,
            "s2",
            Action::Create,
            Some(body.clone()),
            Some(tid(2)),
            true,
        ),
    )
    .await;
    assert_eq!(outcome, IngestOutcome::Matched);

    // Proceed: no hook for updates.
    let outcome = happyview::ingest::apply(
        &state,
        record_event(
            CLAIMED,
            "s3",
            Action::Update,
            Some(body.clone()),
            Some(tid(3)),
            true,
        ),
    )
    .await;
    assert_eq!(outcome, IngestOutcome::Matched);

    // A batch takes the same path and writes nothing locally either.
    let batch = happyview::ingest::apply_batch(
        &state,
        vec![record_event(
            CLAIMED,
            "s4",
            Action::Update,
            Some(body.clone()),
            Some(tid(4)),
            true,
        )],
    )
    .await;
    assert_eq!(batch.outcomes, vec![IngestOutcome::Matched]);

    // A delete clears this instance's delta.
    let outcome = happyview::ingest::apply(
        &state,
        record_event(CLAIMED, "s1", Action::Delete, None, Some(tid(5)), true),
    )
    .await;
    assert_eq!(outcome, IngestOutcome::Matched);

    // Only the claiming store's own feed may deliver its records.
    let outcome = happyview::ingest::apply(
        &state,
        record_event(
            CLAIMED,
            "s6",
            Action::Create,
            Some(body.clone()),
            Some(tid(6)),
            false,
        ),
    )
    .await;
    assert_eq!(outcome, IngestOutcome::Invalid);

    assert_eq!(
        storage.deltas(),
        vec![
            (key(CLAIMED, "s1"), Delta::Hide, Some(tid(1))),
            (
                key(CLAIMED, "s2"),
                Delta::Replace {
                    cid: cid.clone(),
                    rev: Some(tid(2)),
                    body: r#"{"text":"hooked"}"#.to_string(),
                },
                Some(tid(2)),
            ),
            (key(CLAIMED, "s3"), Delta::Proceed, Some(tid(3))),
            (key(CLAIMED, "s4"), Delta::Proceed, Some(tid(4))),
            (key(CLAIMED, "s1"), Delta::Proceed, Some(tid(5))),
        ]
    );
    assert_eq!(local_rows(&state).await, 0);
}

#[tokio::test]
async fn plugin_queries_read_claimed_collections_from_the_route() {
    let storage = Arc::new(FakeStorage::claiming(&[CLAIMED]).await);
    storage
        .seed(&[
            prepared(CLAIMED, "g1", json!({"text": "one"})),
            prepared(CLAIMED, "g2", json!({"text": "two"})),
        ])
        .await;
    let state = instance(Some(storage.clone())).await;
    // A local record of the claimed collection, which no read may see.
    RecordStore::new(state.db.clone(), state.db_backend)
        .write_network(
            &state.db,
            &[prepared(CLAIMED, "stale", json!({"text": "local"}))],
        )
        .await
        .unwrap();
    let plugin = happyview::plugin::loader::load_from_file(std::path::Path::new(
        "tests/fixtures/sdk_objects",
    ))
    .await
    .expect("sdk_objects fixture not built");
    state.plugin_registry.install(plugin).await.unwrap();
    let call = |function: &'static str, args: Vec<Value>| {
        let state = state.clone();
        async move {
            state
                .plugin_executor()
                .call_library(
                    "sdk_objects",
                    function,
                    &args,
                    &LibraryCallContext::default(),
                    0,
                )
                .await
                .unwrap_or_else(|e| panic!("{function}: {e}"))
        }
    };

    let page = call("records_query", vec![json!({"collection": CLAIMED})]).await;
    let mut uris: Vec<String> = page["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["uri"].as_str().unwrap().to_string())
        .collect();
    uris.sort();
    assert_eq!(uris, vec![uri(CLAIMED, "g1"), uri(CLAIMED, "g2")]);
    assert_eq!(
        call("records_count", vec![json!({"collection": CLAIMED})]).await,
        json!(2)
    );
    let got = call("records_get", vec![json!(uri(CLAIMED, "g2"))]).await;
    assert_eq!(got["record"]["text"], "two");
    assert_eq!(
        call("records_get", vec![json!(uri(CLAIMED, "stale"))]).await,
        Value::Null
    );
    let found = call(
        "records_search",
        vec![json!({"collection": CLAIMED, "field": "text", "query": "on"})],
    )
    .await;
    assert_eq!(found.as_array().unwrap().len(), 1);
    assert_eq!(found[0]["uri"], uri(CLAIMED, "g1"));
}

#[tokio::test]
async fn external_storage_down_is_503_and_local_reads_work() {
    let storage = Arc::new(FakeStorage::claiming(&[CLAIMED]).await);
    storage
        .seed(&[prepared(CLAIMED, "g1", json!({"text": "elsewhere"}))])
        .await;
    let state = instance(Some(storage.clone())).await;
    state
        .records
        .write_network(&state.db, &[prepared(LOCAL, "n1", json!({"text": "here"}))])
        .await
        .unwrap();
    storage.down.store(true, Ordering::SeqCst);

    for path in [
        format!("/xrpc/{CLAIMED_QUERY}?uri={}", uri(CLAIMED, "g1")),
        format!("/xrpc/{CLAIMED_QUERY}"),
    ] {
        let (status, body) = get(&state, &path).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}: {body}");
        assert_eq!(
            body,
            json!({"error": "ServiceUnavailable", "message": "the record store is unavailable"})
        );
    }

    let (status, body) = get(
        &state,
        &format!("/xrpc/{LOCAL_QUERY}?uri={}", uri(LOCAL, "n1")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["record"]["text"], "here");
    let (status, body) = get(&state, &format!("/xrpc/{LOCAL_QUERY}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["records"].as_array().unwrap().len(), 1);
}

/// The `test_library` fixture, a library whose `sql_execute` runs raw SQL
/// through `host_db_execute`.
fn sql_library() -> LoadedPlugin {
    let manifest: PluginManifest = serde_json::from_value(json!({
        "id": "rawsql", "name": "rawsql", "version": "1.0.0", "api_version": "2",
        "plugin_type": "library", "namespace": "rawsql",
        "capabilities": ["library:call", "database:read", "database:write"],
    }))
    .unwrap();
    LoadedPlugin {
        info: PluginInfo {
            id: "rawsql".into(),
            name: "rawsql".into(),
            version: "1.0.0".into(),
            api_version: "2".into(),
            icon_url: None,
            required_secrets: vec![],
            auth_type: "oauth2".into(),
            config_schema: None,
        },
        source: PluginSource::File {
            path: "tests/fixtures/test_library".into(),
        },
        wasm_bytes: std::fs::read(
            "tests/fixtures/test_library/target/wasm32-unknown-unknown/release/test_library.wasm",
        )
        .expect("test_library fixture not built"),
        manifest: Some(manifest),
    }
}

#[tokio::test]
async fn raw_sql_writes_to_a_claimed_collection_are_refused() {
    let storage = Arc::new(FakeStorage::claiming(&[CLAIMED]).await);
    let state = instance(Some(storage.clone())).await;
    state.plugin_registry.register(sql_library()).await;
    let execute = |sql: String| {
        let state = state.clone();
        async move {
            state
                .plugin_executor()
                .call_library(
                    "rawsql",
                    "sql_execute",
                    &[json!(sql)],
                    &LibraryCallContext::default(),
                    0,
                )
                .await
        }
    };
    let insert = |collection: &str| {
        format!(
            "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at) \
             VALUES ('{}', '{DID}', '{collection}', 'r1', '{{}}', 'c', NULL, '2026-01-01T00:00:00+00:00')",
            uri(collection, "r1")
        )
    };

    let err = execute(insert(CLAIMED)).await.unwrap_err().to_string();
    assert!(err.contains(CLAIMED), "{err}");
    assert!(
        err.contains(
            "writes to records of a collection stored outside this instance must go through records.indexPut"
        ),
        "{err}"
    );
    let err = execute(format!(
        "DELETE FROM happyview_records WHERE uri = '{}'",
        uri(CLAIMED, "r1")
    ))
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains(CLAIMED), "{err}");
    // A write that names no collection cannot be checked, so it is refused.
    let err = execute("DELETE FROM happyview_records WHERE did = 'did:plc:x'".to_string())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("happyview_records"), "{err}");

    // A local collection is written as before.
    // Through the compat view's trigger, which SQLite does not count as an
    // affected row: the stored record is the evidence.
    execute(insert(LOCAL)).await.expect("a local write");
    assert!(
        state
            .records
            .get(&uri(LOCAL, "r1"))
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(local_rows(&state).await, 1);
}

#[tokio::test]
async fn label_gc_asks_the_external_storage_for_claimed_uris() {
    let storage = Arc::new(FakeStorage::claiming(&[CLAIMED]).await);
    storage
        .seed(&[prepared(CLAIMED, "live", json!({"text": "kept"}))])
        .await;
    let state = instance(Some(storage.clone())).await;
    state
        .records
        .write_network(
            &state.db,
            &[prepared(LOCAL, "held", json!({"text": "here"}))],
        )
        .await
        .unwrap();
    let subjects = [
        uri(CLAIMED, "live"),
        uri(CLAIMED, "gone"),
        uri(LOCAL, "held"),
        uri(LOCAL, "orphan"),
        DID.to_string(),
    ];
    for subject in &subjects {
        happyview::db::query(
            "INSERT INTO happyview_labels (src, uri, val, cts) VALUES ('did:plc:lab', ?, 'spam', ?)",
        )
        .bind(subject)
        .bind(happyview::db::now_rfc3339())
        .execute(&state.db)
        .await
        .unwrap();
    }

    let deleted = happyview::labeler::delete_orphaned_labels(&state.records)
        .await
        .expect("the sweep runs");
    assert_eq!(deleted, 2);
    let mut remaining: Vec<(String,)> =
        happyview::db::query_as("SELECT uri FROM happyview_labels ORDER BY uri")
            .fetch_all(&state.db)
            .await
            .unwrap();
    remaining.sort();
    let mut expected = vec![
        (DID.to_string(),),
        (uri(CLAIMED, "live"),),
        (uri(LOCAL, "held"),),
    ];
    expected.sort();
    assert_eq!(remaining, expected);
    let mut asked = storage.asked_live.lock().unwrap().clone();
    asked.sort();
    assert_eq!(asked, vec![uri(CLAIMED, "gone"), uri(CLAIMED, "live")]);
}

/// A read/write cycle on a local collection, as the outputs a client and an
/// operator see.
async fn local_cycle(state: &AppState) -> Vec<Value> {
    let mut out = Vec::new();
    let body = json!({"$type": LOCAL, "text": "hello"});
    let outcome = happyview::ingest::apply(
        state,
        record_event(LOCAL, "c1", Action::Create, Some(body), Some(tid(1)), false),
    )
    .await;
    out.push(json!(format!("{outcome:?}")));
    for path in [
        format!("/xrpc/{LOCAL_QUERY}?uri={}", uri(LOCAL, "c1")),
        format!("/xrpc/{LOCAL_QUERY}"),
    ] {
        let (status, body) = get(state, &path).await;
        out.push(json!([status.as_u16(), body]));
    }
    out.push(json!(state.records.count_collection(LOCAL).await.unwrap()));
    out.push(json!(state.records.count_all().await.unwrap()));
    out.push(json!(state.records.count_by_collection().await.unwrap()));
    let (status, body) = admin_delete(state, &uri(LOCAL, "c1")).await;
    out.push(json!([status.as_u16(), body]));
    let (status, body) = get(
        state,
        &format!("/xrpc/{LOCAL_QUERY}?uri={}", uri(LOCAL, "c1")),
    )
    .await;
    out.push(json!([status.as_u16(), body]));
    out
}

#[tokio::test]
async fn no_external_storage_means_no_behaviour_change() {
    // Recorded from this cycle before external storage existed.
    let expected = vec![
        json!("Matched"),
        json!([200, {"record": {"$type": LOCAL, "text": "hello", "uri": uri(LOCAL, "c1")}}]),
        json!([200, {"records": [{"$type": LOCAL, "text": "hello", "uri": uri(LOCAL, "c1")}]}]),
        json!(1),
        json!(1),
        json!([[LOCAL, 1]]),
        json!([204, null]),
        json!([404, {"error": "record not found"}]),
    ];
    let without = local_cycle(&instance(None).await).await;
    assert_eq!(without, expected);

    // An external storage claiming another collection changes nothing for
    // a local one.
    let storage = Arc::new(FakeStorage::claiming(&[CLAIMED]).await);
    let with = local_cycle(&instance(Some(storage.clone())).await).await;
    assert_eq!(with, expected);
    assert!(storage.deltas().is_empty());
}

/// A claimed record whose stored event's hook failed: the delivery failed
/// open (a Proceed delta) and left a dead letter. The claiming store holds
/// `stored` at rev `tid(10)`. Returns the dead letter's id.
async fn dead_lettered(state: &AppState, storage: &FakeStorage, stored: Value) -> String {
    state
        .plugin_registry
        .register(echo_interpreter::plugin("lua"))
        .await;
    let trigger = format!("record.create:{CLAIMED}");
    install_script(state, &trigger, "error:runtime").await;
    let body = json!({"$type": CLAIMED, "text": "arrived"});
    let outcome = happyview::ingest::apply(
        state,
        record_event(
            CLAIMED,
            "dl",
            Action::Create,
            Some(body),
            Some(tid(10)),
            true,
        ),
    )
    .await;
    assert_eq!(outcome, IngestOutcome::Matched, "fail-open");
    let mut record = prepared(CLAIMED, "dl", stored);
    record.rev = Some(tid(10));
    storage.seed(&[record]).await;

    let (name, value) = common::auth::admin_cookie_header(ADMIN, &state.cookie_key);
    let (status, list) = send(
        state,
        Request::get("/admin/dead-letters")
            .header("host", "127.0.0.1")
            .header(name, value)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(
        list["dead_letters"].as_array().map(Vec::len),
        Some(1),
        "{list}"
    );
    list["dead_letters"][0]["id"].as_str().unwrap().to_string()
}

/// Retry dead letter `id` through the admin route, with the hook now `hook`.
async fn retry(state: &AppState, id: &str, hook: &str) {
    install_script(state, &format!("record.create:{CLAIMED}"), hook).await;
    let (name, value) = common::auth::admin_cookie_header(ADMIN, &state.cookie_key);
    let (status, body) = send(
        state,
        Request::post(format!("/admin/dead-letters/{id}/retry"))
            .header("host", "127.0.0.1")
            .header("content-type", "application/json")
            .header(name, value)
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn open_dead_letters(state: &AppState) -> i64 {
    happyview::db::query_scalar::<i64>(
        "SELECT COUNT(*) FROM happyview_dead_letter_scripts WHERE resolved_at IS NULL",
    )
    .fetch_one(&state.db)
    .await
    .unwrap()
}

#[tokio::test]
async fn a_claimed_retry_whose_hook_now_skips_hides_the_version_it_was_made_from() {
    if !echo_interpreter::is_built() {
        eprintln!("skipping: {}", echo_interpreter::BUILD);
        return;
    }
    let storage = Arc::new(FakeStorage::claiming(&[CLAIMED]).await);
    let state = instance(Some(storage.clone())).await;
    let id = dead_lettered(
        &state,
        &storage,
        json!({"$type": CLAIMED, "text": "arrived"}),
    )
    .await;

    retry(&state, &id, "value:none").await;

    assert_eq!(
        storage.deltas().last(),
        Some(&(key(CLAIMED, "dl"), Delta::Hide, Some(tid(10))))
    );
    assert_eq!(open_dead_letters(&state).await, 0);
}

#[tokio::test]
async fn a_claimed_retry_leaves_a_record_changed_since_alone() {
    if !echo_interpreter::is_built() {
        eprintln!("skipping: {}", echo_interpreter::BUILD);
        return;
    }
    let storage = Arc::new(FakeStorage::claiming(&[CLAIMED]).await);
    let state = instance(Some(storage.clone())).await;
    let id = dead_lettered(
        &state,
        &storage,
        json!({"$type": CLAIMED, "text": "edited"}),
    )
    .await;
    let before = storage.deltas();

    retry(&state, &id, "value:none").await;

    assert_eq!(storage.deltas(), before, "no delta for a newer version");
    assert_eq!(open_dead_letters(&state).await, 0, "still resolved");
}

#[tokio::test]
async fn a_claimed_retry_whose_hook_now_replaces_names_the_replaced_body() {
    if !echo_interpreter::is_built() {
        eprintln!("skipping: {}", echo_interpreter::BUILD);
        return;
    }
    let storage = Arc::new(FakeStorage::claiming(&[CLAIMED]).await);
    let state = instance(Some(storage.clone())).await;
    let id = dead_lettered(
        &state,
        &storage,
        json!({"$type": CLAIMED, "text": "arrived"}),
    )
    .await;

    retry(&state, &id, r#"returns:{"text":"retried"}"#).await;

    let replaced = json!({"text": "retried"});
    assert_eq!(
        storage.deltas().last(),
        Some(&(
            key(CLAIMED, "dl"),
            Delta::Replace {
                cid: cid_of(&replaced),
                rev: None,
                body: replaced.to_string(),
            },
            Some(tid(10)),
        ))
    );
    assert_eq!(open_dead_letters(&state).await, 0);
}

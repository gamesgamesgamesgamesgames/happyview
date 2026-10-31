//! What script metrics cost a record, measured: backfill records through a
//! hook that waves them through, in batches, with `script_metrics_enabled`
//! off and on in alternating rounds so drift (a growing database, a warming
//! cache) falls on both sides alike.
//!
//! Ignored: it is a measurement, not a check, and only means anything in a
//! release build.
//!
//!   cargo build --manifest-path tests/fixtures/interpreter_echo/Cargo.toml \
//!     --target wasm32-unknown-unknown --release
//!   TEST_DATABASE_URL="sqlite://$(mktemp -d)/t.db?mode=rwc" \
//!     cargo test --release --test metrics_overhead -- --ignored --nocapture
//!
//! The echo interpreter's guest does next to nothing, so this is the trivial
//! hook's case: every microsecond metrics add is platform time a record pays.

mod common;

use std::time::{Duration, Instant};

use happyview::db::adapt_sql;
use happyview::host::AppHost;
use happyview::lexicon::ProcedureAction;
use happyview_provider::{Action, Host, Origin, RecordEvent, RecordKey, SourceEvent};
use serde_json::json;

use common::app::TestApp;
use common::echo_interpreter;

const COLLECTION: &str = "app.test.metrics.overhead";
const DID: &str = "did:plc:metricsoverhead";
const BATCH: usize = 500;
const BATCHES_PER_ROUND: usize = 5;
const ROUNDS: usize = 6;

fn event(rkey: &str) -> SourceEvent {
    let body = json!({"$type": COLLECTION, "text": rkey});
    SourceEvent::Record(RecordEvent {
        key: RecordKey {
            did: DID.to_string(),
            collection: COLLECTION.to_string(),
            rkey: rkey.to_string(),
        },
        action: Action::Create,
        cid: Some(
            happyview::cid_verify::compute_record_cid(&body)
                .unwrap()
                .to_string(),
        ),
        rev: None,
        body: Some(body),
        time_us: None,
        origin: Origin::Backfill {
            job_id: "metrics-overhead".to_string(),
        },
        stored: false,
    })
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement; run in release with --ignored --nocapture"]
async fn what_metrics_cost_a_trivially_hooked_record() {
    if std::env::var("TEST_DATABASE_URL").is_err() {
        happyview::test_support::unavailable(happyview::test_support::NO_DATABASE);
        return;
    }
    assert!(echo_interpreter::is_built(), "{}", echo_interpreter::BUILD);
    let mut app = TestApp::new().await;
    app.state
        .lexicons
        .put_manual(
            &app.state.db,
            app.state.db_backend,
            happyview::catalog::LexiconWrite {
                lexicon_json: json!({
                    "lexicon": 1,
                    "id": COLLECTION,
                    "defs": {"main": {"type": "record", "key": "tid",
                        "record": {"type": "object", "properties": {"text": {"type": "string"}}}}}
                }),
                backfill: false,
                target_collection: Some(COLLECTION.to_string()),
                action: ProcedureAction::Upsert,
                token_cost: None,
                authority_did: None,
                indexed: true,
            },
        )
        .await
        .expect("store the lexicon");
    let (tx, _) = tokio::sync::watch::channel(vec![COLLECTION.to_string()]);
    app.state.collections_tx = tx;
    app.state
        .plugin_registry
        .register(echo_interpreter::plugin("lua"))
        .await;
    let now = happyview::db::now_rfc3339();
    happyview::db::query(&adapt_sql(
        "INSERT INTO happyview_scripts (id, body, script_type, created_at, updated_at) \
         VALUES (?, 'value:other', 'lua', ?, ?)",
        app.state.db_backend,
    ))
    .bind(format!("record.index:{COLLECTION}"))
    .bind(&now)
    .bind(&now)
    .execute(&app.state.db)
    .await
    .expect("seed the hook");
    let host = AppHost::new(app.state.clone());

    let mut next = 0usize;
    let mut batch = || {
        let events = (0..BATCH)
            .map(|i| event(&format!("r{}", next + i)))
            .collect::<Vec<_>>();
        next += BATCH;
        events
    };
    // Warm the module cache, the prepared artifact and the database file.
    host.ingest_batch(batch()).await;

    let mut spent = [Duration::ZERO; 2];
    let mut records = [0usize; 2];
    for round in 0..ROUNDS * 2 {
        let on = round % 2 == 1;
        app.state.metrics.set_enabled(on);
        for _ in 0..BATCHES_PER_ROUND {
            let events = batch();
            let started = Instant::now();
            host.ingest_batch(events).await;
            spent[usize::from(on)] += started.elapsed();
            records[usize::from(on)] += BATCH;
        }
    }
    app.state.metrics.set_enabled(true);

    let per = |i: usize| spent[i].as_secs_f64() * 1000.0 / records[i] as f64;
    let (off, on) = (per(0), per(1));
    println!(
        "metrics off: {off:.4} ms/record over {} records\n\
         metrics on:  {on:.4} ms/record over {} records\n\
         overhead:    {:+.4} ms/record ({:+.2}%)",
        records[0],
        records[1],
        on - off,
        (on - off) / off * 100.0
    );
}

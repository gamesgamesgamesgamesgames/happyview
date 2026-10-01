//! The record-event and label runners, driven through their own entry points.
//!
//! The `interpreter_echo` fixture claims `lua`, which is what a script row's
//! `script_type` holds, so a run resolves a row and reaches the fixture exactly
//! as it would reach a deployed interpreter. The fixture interprets nothing —
//! its `source` is a directive — so what this pins is the runners' half: what
//! reaches an interpreter, how a returned value's kind becomes an outcome, and
//! the rows an operator reads after a failure.
//!
//! Language semantics belong to the interpreter and are pinned where it is
//! loaded, in `lua_interpreter_plugin.rs` and `lua_differential.rs`.

mod common;

use serde_json::{Value, json};

use common::app::TestApp;
use common::echo_interpreter;
use happyview::lua::{
    LabelAppliedEvent, LabelHookOutcome, RecordEventPayload, RecordHookOutcome,
    run_label_applied_script, run_record_event_script,
};

const NSID: &str = "com.example.thing";
const URI: &str = "at://did:plc:author/com.example.thing/3kabc1";
const AUTHOR: &str = "did:plc:author";
const LABEL_URI: &str = "at://did:plc:subject/app.bsky.feed.post/3kxyz1";
const LABELER: &str = "did:plc:labeler";

/// Skips the caller when the fixture's module is absent, naming the build that
/// produces it. Every fixture's `target/` is gitignored, so that is a step
/// nobody ran; panicking on it would report it as a failure of the runners.
macro_rules! require_fixture {
    () => {
        if !echo_interpreter::is_built() {
            eprintln!("skipping: {}", echo_interpreter::BUILD);
            return;
        }
    };
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// An app with the echo fixture installed as the interpreter for `lua`, the
/// language a seeded script row names.
async fn app() -> TestApp {
    let app = TestApp::new().await;
    app.state
        .plugin_registry
        .register(echo_interpreter::plugin("lua"))
        .await;
    app
}

async fn seed_script_as(app: &TestApp, trigger_id: &str, body: &str, script_type: &str) {
    let now = happyview::db::now_rfc3339();
    let sql = happyview::db::adapt_sql(
        "INSERT INTO happyview_scripts (id, body, script_type, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?)",
        app.state.db_backend,
    );
    happyview::db::query(&sql)
        .bind(trigger_id)
        .bind(body)
        .bind(script_type)
        .bind(&now)
        .bind(&now)
        .execute(&app.state.db)
        .await
        .expect("seed a script row");
}

async fn seed_script(app: &TestApp, trigger_id: &str, body: &str) {
    seed_script_as(app, trigger_id, body, "lua").await;
}

fn record_payload(action: &'static str) -> RecordEventPayload<'static> {
    RecordEventPayload {
        nsid: NSID,
        action,
        uri: URI,
        did: AUTHOR,
        rkey: "3kabc1",
        record: None,
    }
}

fn label_event() -> LabelAppliedEvent {
    LabelAppliedEvent {
        src: LABELER.into(),
        uri: LABEL_URI.into(),
        val: "spam".into(),
        neg: false,
        cts: "2026-01-01T00:00:00.000Z".into(),
        exp: None,
    }
}

fn continued(outcome: LabelHookOutcome) -> LabelAppliedEvent {
    match outcome {
        LabelHookOutcome::Continue(event) => event,
        LabelHookOutcome::Skip => panic!("the label should have been persisted"),
    }
}

fn replaced(outcome: RecordHookOutcome) -> Value {
    match outcome {
        RecordHookOutcome::Replace(value) => value,
        other => panic!("the body should have been replaced, got {other:?}"),
    }
}

/// One dead-letter row, column by column, so a test reads what an operator
/// reads rather than what the runner meant to write.
struct DeadLetter {
    script_ref: String,
    host_kind: String,
    host_id: String,
    payload: Value,
    error: String,
    attempts: i64,
    collection: Option<String>,
}

/// `(script_ref, host_kind, host_id, payload, error, attempts, collection)`.
type DeadLetterRow = (String, String, String, String, String, i64, Option<String>);

async fn dead_letters(app: &TestApp) -> Vec<DeadLetter> {
    let rows: Vec<DeadLetterRow> = happyview::db::query_as(&happyview::db::adapt_sql(
        "SELECT script_ref, host_kind, host_id, payload, error, attempts, collection \
         FROM happyview_dead_letter_scripts ORDER BY id",
        app.state.db_backend,
    ))
    .fetch_all(&app.state.db)
    .await
    .expect("read the dead letters");
    rows.into_iter()
        .map(|r| DeadLetter {
            script_ref: r.0,
            host_kind: r.1,
            host_id: r.2,
            payload: serde_json::from_str(&r.3).expect("the payload should be JSON"),
            error: r.4,
            attempts: r.5,
            collection: r.6,
        })
        .collect()
}

/// Every row of `event_type`, as `(actor_did, subject, detail)`.
async fn events(app: &TestApp, event_type: &str) -> Vec<(Option<String>, Option<String>, Value)> {
    let rows: Vec<(Option<String>, Option<String>, String)> =
        happyview::db::query_as(&happyview::db::adapt_sql(
            "SELECT actor_did, subject, detail FROM happyview_event_logs \
             WHERE event_type = ? ORDER BY created_at",
            app.state.db_backend,
        ))
        .bind(event_type)
        .fetch_all(&app.state.db)
        .await
        .expect("read the event log");
    rows.into_iter()
        .map(|r| {
            (
                r.0,
                r.1,
                serde_json::from_str(&r.2).expect("the detail should be JSON"),
            )
        })
        .collect()
}

fn detail_keys(detail: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = detail
        .as_object()
        .expect("a detail object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    keys
}

// ---------------------------------------------------------------------------
// What reaches the interpreter
// ---------------------------------------------------------------------------

/// The whole input, echoed back as the replacement body: the first argument of
/// `handle`, every context field a record event fills, and the budget the
/// instance's cached setting holds.
#[tokio::test]
async fn a_record_event_hands_the_interpreter_its_event_and_its_context() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    happyview::db::query(&happyview::db::adapt_sql(
        "INSERT INTO happyview_script_variables (key, value, created_at) VALUES (?, ?, ?)",
        app.state.db_backend,
    ))
    .bind("API_KEY")
    .bind("k")
    .bind(happyview::db::now_rfc3339())
    .execute(&app.state.db)
    .await
    .expect("seed a script variable");
    seed_script(&app, &format!("record.create:{NSID}"), "echo").await;

    let body = json!({ "title": "Hello" });
    let sent = replaced(
        run_record_event_script(
            &app.state,
            RecordEventPayload {
                record: Some(&body),
                ..record_payload("create")
            },
        )
        .await,
    );

    assert_eq!(sent["source"], "echo");
    assert_eq!(sent["kind"], "record_event");
    assert_eq!(
        sent["input"],
        json!({
            "action": "create",
            "uri": URI,
            "did": AUTHOR,
            "collection": NSID,
            "rkey": "3kabc1",
            "record": { "title": "Hello" },
        })
    );
    assert_eq!(sent["context"]["trigger"], format!("record.create:{NSID}"));
    assert_eq!(sent["context"]["caller_did"], AUTHOR);
    assert_eq!(sent["context"]["collection"], NSID);
    assert_eq!(sent["context"]["has_pds_auth"], false);
    assert_eq!(sent["context"]["env"], json!({ "API_KEY": "k" }));
    for absent in ["method", "params", "delegate_did", "space", "job"] {
        assert_eq!(sent["context"][absent], Value::Null, "{absent}");
    }
    assert_eq!(
        sent["limits"]["instructions"],
        app.state.script_limits.instruction_limit()
    );
}

/// The budget travels from the cache on every run, so a settings change
/// reaches the next event without a restart.
#[tokio::test]
async fn the_cached_instruction_budget_is_what_reaches_the_interpreter() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    seed_script(&app, &format!("record.create:{NSID}"), "echo").await;

    app.state.script_limits.set_instruction_limit(4_242);
    let sent = replaced(run_record_event_script(&app.state, record_payload("create")).await);
    assert_eq!(sent["limits"]["instructions"], 4_242);

    app.state.script_limits.set_instruction_limit(9_999);
    let sent = replaced(run_record_event_script(&app.state, record_payload("create")).await);
    assert_eq!(sent["limits"]["instructions"], 9_999);
}

/// A record-event script acts as the record's author, which is who a library
/// reads as the caller and who the run's log line is attributed to.
#[tokio::test]
async fn a_record_event_script_runs_as_the_records_author() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    seed_script(&app, &format!("record.create:{NSID}"), "host:script_log").await;

    run_record_event_script(&app.state, record_payload("create")).await;

    let logged = events(&app, "script.log").await;
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0].0.as_deref(), Some(AUTHOR));
    assert_eq!(logged[0].1, Some(format!("record.create:{NSID}")));
}

/// The whole input of a label run: the label as the first argument of
/// `handle`, the context fields a label fills and the ones it leaves empty,
/// and the budget the instance's cached setting holds.
///
/// A label run's outcome is the label it shaped rather than the value it
/// returned, so the input is read back off the run's own log line.
#[tokio::test]
async fn a_label_hands_the_interpreter_its_event_and_its_context() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    happyview::db::query(&happyview::db::adapt_sql(
        "INSERT INTO happyview_script_variables (key, value, created_at) VALUES (?, ?, ?)",
        app.state.db_backend,
    ))
    .bind("API_KEY")
    .bind("k")
    .bind(happyview::db::now_rfc3339())
    .execute(&app.state.db)
    .await
    .expect("seed a script variable");
    seed_script(&app, "labeler.apply:app.bsky.feed.post", "host:log_input").await;
    app.state.script_limits.set_instruction_limit(4_242);

    run_label_applied_script(&app.state, label_event()).await;

    let logged = events(&app, "script.log").await;
    assert_eq!(logged.len(), 1, "{logged:?}");
    let sent = &logged[0].2["fields"];

    assert_eq!(sent["source"], "host:log_input");
    assert_eq!(sent["kind"], "label");
    assert_eq!(
        sent["input"],
        json!({
            "src": LABELER,
            "uri": LABEL_URI,
            "val": "spam",
            "neg": false,
            "cts": "2026-01-01T00:00:00.000Z",
        })
    );
    assert_eq!(
        sent["context"]["trigger"],
        "labeler.apply:app.bsky.feed.post"
    );
    assert_eq!(sent["context"]["has_pds_auth"], false);
    assert_eq!(sent["context"]["env"], json!({ "API_KEY": "k" }));
    for absent in [
        "caller_did",
        "method",
        "collection",
        "params",
        "delegate_did",
        "space",
        "job",
    ] {
        assert_eq!(sent["context"][absent], Value::Null, "{absent}");
    }
    assert_eq!(sent["limits"]["instructions"], 4_242);
}

/// A label arrives from a subscription, so there is nobody for the run to act
/// as and no collection for it to read.
#[tokio::test]
async fn a_label_script_runs_as_nobody() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    seed_script(&app, "labeler.apply:app.bsky.feed.post", "host:script_log").await;

    run_label_applied_script(&app.state, label_event()).await;

    let logged = events(&app, "script.log").await;
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0].0, None);
    assert_eq!(
        logged[0].1.as_deref(),
        Some("labeler.apply:app.bsky.feed.post")
    );
}

// ---------------------------------------------------------------------------
// The three-way branch a record event reads
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_returned_table_replaces_the_body() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    seed_script(
        &app,
        &format!("record.create:{NSID}"),
        r#"returns:{"title":"rewritten"}"#,
    )
    .await;

    let outcome = run_record_event_script(&app.state, record_payload("create")).await;
    assert_eq!(
        outcome,
        RecordHookOutcome::Replace(json!({ "title": "rewritten" }))
    );
}

#[tokio::test]
async fn a_script_that_returned_nothing_skips_the_event() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    seed_script(&app, &format!("record.create:{NSID}"), "value:none").await;

    let outcome = run_record_event_script(&app.state, record_payload("create")).await;
    assert_eq!(outcome, RecordHookOutcome::Skip);
}

#[tokio::test]
async fn a_returned_value_that_is_neither_waves_the_event_through() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    seed_script(&app, &format!("record.create:{NSID}"), "value:other").await;

    let outcome = run_record_event_script(&app.state, record_payload("create")).await;
    assert_eq!(outcome, RecordHookOutcome::Proceed);
    // A failure proceeds too, so without this the assertion above would hold
    // for a script that never ran.
    assert!(dead_letters(&app).await.is_empty());
}

/// No row for the trigger is not the same answer as a script that returned
/// nothing: a delete reads `Skip` as "keep the record", and an instance with
/// no scripts at all must not spell that.
#[tokio::test]
async fn no_script_at_all_proceeds_without_a_row_or_an_event() {
    common::require_db!();
    require_fixture!();
    let app = app().await;

    for action in ["create", "delete"] {
        let outcome = run_record_event_script(&app.state, record_payload(action)).await;
        assert_eq!(outcome, RecordHookOutcome::Proceed, "{action}");
    }
    assert!(dead_letters(&app).await.is_empty());
    assert!(events(&app, "script.executed").await.is_empty());
    assert!(events(&app, "script.dead_lettered").await.is_empty());
}

/// Only a script that actually ran and returned nothing aborts a delete.
#[tokio::test]
async fn a_delete_is_aborted_only_by_a_script_that_returned_nothing() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    seed_script(&app, &format!("record.delete:{NSID}"), "value:none").await;

    let outcome = run_record_event_script(&app.state, record_payload("delete")).await;
    assert_eq!(outcome, RecordHookOutcome::Skip);
}

/// The cascade tries the action's own trigger before the wildcard, and the
/// trigger the interpreter is told about is the row that won.
#[tokio::test]
async fn the_cascade_prefers_the_action_trigger_over_the_index_one() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    seed_script(&app, &format!("record.create:{NSID}"), "value:object").await;
    seed_script(&app, &format!("record.index:{NSID}"), "value:object").await;

    let context = replaced(run_record_event_script(&app.state, record_payload("create")).await);
    assert_eq!(context["trigger"], format!("record.create:{NSID}"));

    let context = replaced(run_record_event_script(&app.state, record_payload("update")).await);
    assert_eq!(context["trigger"], format!("record.index:{NSID}"));
}

/// The cascade is about which trigger an event matches, not about which rows
/// happen to be runnable. An operator who bound this action to a script gets
/// that script's failure recorded, rather than the quietly different answer a
/// fall-through to the wildcard row would give.
#[tokio::test]
async fn the_action_trigger_still_wins_when_its_language_has_no_interpreter() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    seed_script_as(
        &app,
        &format!("record.create:{NSID}"),
        "value:object",
        "typescript",
    )
    .await;
    seed_script(&app, &format!("record.index:{NSID}"), "value:none").await;

    let outcome = run_record_event_script(&app.state, record_payload("create")).await;
    assert_eq!(
        outcome,
        RecordHookOutcome::Proceed,
        "the wildcard row's `value:none` would have skipped the event"
    );

    let rows = dead_letters(&app).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].script_ref, format!("record.create:{NSID}"));
    assert!(rows[0].error.contains("typescript"), "{}", rows[0].error);
}

// ---------------------------------------------------------------------------
// The branch a label reads
// ---------------------------------------------------------------------------

/// A field the script omitted falls back to the original, so a rewriting
/// script need only name what it changes.
#[tokio::test]
async fn a_returned_partial_table_merges_over_the_original_label() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    seed_script(
        &app,
        "labeler.apply:app.bsky.feed.post",
        r#"returns:{"val":"nsfw","neg":true}"#,
    )
    .await;

    let event = continued(run_label_applied_script(&app.state, label_event()).await);
    assert_eq!(event.val, "nsfw");
    assert!(event.neg);
    assert_eq!(event.src, LABELER);
    assert_eq!(event.uri, LABEL_URI);
    assert_eq!(event.cts, "2026-01-01T00:00:00.000Z");
    assert_eq!(event.exp, None);
}

#[tokio::test]
async fn a_label_script_that_returned_nothing_skips_persistence() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    seed_script(&app, "labeler.apply:app.bsky.feed.post", "value:none").await;

    assert!(matches!(
        run_label_applied_script(&app.state, label_event()).await,
        LabelHookOutcome::Skip
    ));
}

#[tokio::test]
async fn a_label_value_that_is_neither_persists_the_original() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    seed_script(&app, "labeler.apply:app.bsky.feed.post", "value:other").await;

    let event = continued(run_label_applied_script(&app.state, label_event()).await);
    assert_eq!(event.val, "spam");
    assert_eq!(event.src, LABELER);
    assert!(dead_letters(&app).await.is_empty());
}

/// Nothing claims the row's language, so the run produces no result at all.
/// The label is still persisted — a subscription must not stall on a plugin
/// an operator has not installed — and the dead letter is the trace that says
/// why the script did not shape it, recorded as the one attempt it was worth.
///
/// An interpreter is installed here, for a different language, so what the
/// runner refuses is this row rather than the instance.
#[tokio::test]
async fn a_label_row_whose_language_has_no_interpreter_dead_letters_and_persists() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    seed_script_as(
        &app,
        "labeler.apply:app.bsky.feed.post",
        "value:none",
        "typescript",
    )
    .await;

    let started = std::time::Instant::now();
    let event = continued(run_label_applied_script(&app.state, label_event()).await);
    let elapsed = started.elapsed();
    assert_eq!(event.val, "spam");
    assert!(events(&app, "script.executed").await.is_empty());

    let rows = dead_letters(&app).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].script_ref, "labeler.apply:app.bsky.feed.post");
    assert_eq!(rows[0].attempts, 1);
    assert!(rows[0].error.contains("typescript"), "{}", rows[0].error);
    assert_eq!(events(&app, "script.dead_lettered").await.len(), 1);
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "the run waited out a backoff: {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// What an operator reads
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_successful_record_run_is_logged_with_its_attempt_count() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    seed_script(&app, &format!("record.create:{NSID}"), "value:other").await;

    run_record_event_script(&app.state, record_payload("create")).await;

    let rows = events(&app, "script.executed").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (actor, subject, detail) = &rows[0];
    assert_eq!(*actor, None);
    assert_eq!(subject.as_deref(), Some(URI));
    assert_eq!(
        detail_keys(detail),
        ["attempts", "host_id", "host_kind", "trigger"]
    );
    assert_eq!(detail["host_kind"], "record");
    assert_eq!(detail["host_id"], format!("{NSID}:create"));
    assert_eq!(detail["trigger"], format!("record.create:{NSID}"));
    assert_eq!(detail["attempts"], 1);
}

#[tokio::test]
async fn a_successful_label_run_is_logged_with_its_attempt_count() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    seed_script(&app, "labeler.apply:app.bsky.feed.post", "value:other").await;

    run_label_applied_script(&app.state, label_event()).await;

    let rows = events(&app, "script.executed").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (actor, subject, detail) = &rows[0];
    assert_eq!(*actor, None);
    assert_eq!(subject.as_deref(), Some(LABEL_URI));
    assert_eq!(
        detail_keys(detail),
        ["attempts", "host_id", "host_kind", "trigger"]
    );
    assert_eq!(detail["host_kind"], "label");
    assert_eq!(detail["host_id"], LABELER);
    assert_eq!(detail["trigger"], "labeler.apply:app.bsky.feed.post");
    assert_eq!(detail["attempts"], 1);
}

/// Four attempts, one row, and the delete still happens: the record body a
/// dead-lettered script could not rewrite is not a reason to keep a record
/// its PDS no longer has.
#[tokio::test]
async fn four_failed_attempts_dead_letter_a_delete_and_let_it_through() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    seed_script(&app, &format!("record.delete:{NSID}"), "error:runtime").await;

    let outcome = run_record_event_script(&app.state, record_payload("delete")).await;
    assert_eq!(outcome, RecordHookOutcome::Proceed);

    let rows = dead_letters(&app).await;
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.script_ref, format!("record.delete:{NSID}"));
    assert_eq!(row.host_kind, "record");
    assert_eq!(row.host_id, format!("{NSID}:delete"));
    assert_eq!(row.attempts, 4);
    assert_eq!(row.collection.as_deref(), Some(NSID));
    assert_eq!(
        row.payload,
        json!({
            "trigger": format!("record.delete:{NSID}"),
            "action": "delete",
            "uri": URI,
            "did": AUTHOR,
            "collection": NSID,
            "rkey": "3kabc1",
            "record": Value::Null,
        })
    );
    // The category the failure arrived with and the interpreter's unparsed
    // text, line prefix included, since every reader of this column is an
    // operator debugging the script.
    assert!(row.error.starts_with("runtime: "), "{}", row.error);
    assert_eq!(happyview::error::parse_lua_line(&row.error).0, Some(7));

    let rows = events(&app, "script.dead_lettered").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (_, subject, detail) = &rows[0];
    assert_eq!(subject.as_deref(), Some(URI));
    assert_eq!(
        detail_keys(detail),
        ["error", "host_id", "host_kind", "trigger"]
    );
    assert_eq!(detail["host_kind"], "record");
    assert_eq!(detail["host_id"], format!("{NSID}:delete"));
    assert_eq!(
        happyview::error::parse_lua_line(detail["error"].as_str().expect("the error text")).0,
        Some(7)
    );
    assert!(events(&app, "script.executed").await.is_empty());
}

#[tokio::test]
async fn four_failed_attempts_dead_letter_a_label_and_persist_the_original() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    seed_script(&app, "labeler.apply:app.bsky.feed.post", "error:runtime").await;

    let event = continued(run_label_applied_script(&app.state, label_event()).await);
    assert_eq!(event.val, "spam");

    let rows = dead_letters(&app).await;
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.script_ref, "labeler.apply:app.bsky.feed.post");
    assert_eq!(row.host_kind, "label");
    assert_eq!(row.host_id, LABELER);
    assert_eq!(row.attempts, 4);
    assert_eq!(row.collection.as_deref(), Some("app.bsky.feed.post"));
    assert!(row.error.starts_with("runtime: "), "{}", row.error);
    assert_eq!(
        row.payload,
        json!({
            "src": LABELER,
            "uri": LABEL_URI,
            "val": "spam",
            "neg": false,
            "cts": "2026-01-01T00:00:00.000Z",
        })
    );
    assert_eq!(happyview::error::parse_lua_line(&row.error).0, Some(7));

    let rows = events(&app, "script.dead_lettered").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].1.as_deref(), Some(LABEL_URI));
    assert_eq!(rows[0].2["host_kind"], "label");
}

/// The dead-letter table has one error column and no category of its own, so
/// the category the failure arrived with has to be in that column's text. A
/// timeout and an exhausted heap are the two that describe themselves in no
/// prose, so they are the two that become unreadable without it.
#[tokio::test]
async fn a_dead_lettered_failure_keeps_its_category_in_the_error_column() {
    common::require_db!();
    require_fixture!();
    let app = app().await;
    seed_script(&app, &format!("record.create:{NSID}"), "error:timeout").await;
    seed_script(&app, "labeler.apply:app.bsky.feed.post", "error:memory").await;

    run_record_event_script(&app.state, record_payload("create")).await;
    run_label_applied_script(&app.state, label_event()).await;

    let rows = dead_letters(&app).await;
    assert_eq!(rows.len(), 2);
    let categories: Vec<&str> = rows
        .iter()
        .map(|row| {
            row.error
                .split_once(": ")
                .expect("the category leads the text")
                .0
        })
        .collect();
    assert_eq!(categories, ["timeout", "memory"]);
    // The line still points at the script, which is what reads that column
    // for the other end.
    for row in &rows {
        assert_eq!(happyview::error::parse_lua_line(&row.error).0, Some(7));
    }
}

/// Nothing is installed that claims the row's language, so the run produces no
/// result at all and the operator reads a dead letter naming the language.
///
/// It is recorded as **one** attempt, and the call does not wait out the
/// backoff three more would carry. No retry can install a plugin, and on a
/// busy collection a wait nothing can satisfy is ingest held up. The count is
/// what fails if the retry loop reclaims this case.
#[tokio::test]
async fn a_missing_interpreter_dead_letters_once_and_does_not_wait() {
    common::require_db!();
    let app = TestApp::new().await;
    seed_script(&app, &format!("record.create:{NSID}"), "value:other").await;

    let started = std::time::Instant::now();
    let outcome = run_record_event_script(&app.state, record_payload("create")).await;
    let elapsed = started.elapsed();
    assert_eq!(outcome, RecordHookOutcome::Proceed);

    let rows = dead_letters(&app).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].attempts, 1);
    assert!(rows[0].error.contains("lua"), "{}", rows[0].error);
    // Four attempts sleep 1 + 2 + 4 seconds between them.
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "the run waited out a backoff: {elapsed:?}"
    );
}

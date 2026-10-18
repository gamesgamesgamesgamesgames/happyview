//! The provider conformance cases.
//!
//! A record provider calls [`run`] with a function that builds its
//! [`Providers`] and a [`Feeder`] that puts records into its world. Every case
//! boots a fresh [`ProviderApp`] on the test database with them and checks
//! what HappyView then serves, so a provider passes by behaving as the
//! built-in one does, whatever it does inside.
//!
//! The cases use one collection, [`COLLECTION`]; a provider whose storage
//! claims collections claims that one. Hooks are Lua, so the run needs
//! `HAPPYVIEW_LUA_PLUGIN` as well as `TEST_DATABASE_URL`.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::time::Duration;

use futures_util::FutureExt;
use happyview_provider::{Action, Origin, RecordEvent, RecordKey, SourceEvent};
use serde_json::{Value, json};

use super::ProviderApp;
use super::app::LUA_PLUGIN_DIR;
use crate::Providers;

/// The collection every case writes.
pub const COLLECTION: &str = "com.example.conformance.note";

/// How long one case, boot included, may take before it fails.
const CASE_TIMEOUT: Duration = Duration::from_secs(60);

const ALICE: &str = "did:plc:conformancealice";
const BOB: &str = "did:plc:conformancebob";

/// Logs that it ran, then appends ` (hooked)` to the record's text.
const MARK: &str = r#"local log = require("internal.logging")

function handle(input)
  log.info("mark hook", { uri = input.uri, action = input.action })
  local record = input.record
  record.text = record.text .. " (hooked)"
  return record
end"#;

/// Logs that it ran, then skips the record.
const SKIP: &str = r#"local log = require("internal.logging")

function handle(input)
  log.info("skip hook", { uri = input.uri })
  return nil
end"#;

/// Logs the text of the body a delete carried, and lets the delete proceed.
const LOG_DELETE: &str = r#"local log = require("internal.logging")

function handle(input)
  local text = "<no body>"
  if input.record ~= nil then text = input.record.text end
  log.info("delete hook", { uri = input.uri, text = text })
  return true
end"#;

type Feed = Box<dyn Fn(RecordEvent) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// What [`Feeder::after_each`] returns: a check of the instance a case left.
pub type CheckFuture<'a> = Pin<Box<dyn Future<Output = ()> + 'a>>;

type Check = Box<dyn for<'a> Fn(&'static str, &'a ProviderApp) -> CheckFuture<'a> + Send + Sync>;

/// How a case puts records into the provider's world.
///
/// For the default providers, `put` and `delete` deliver the event the
/// provider's source would. For an external store, `put` writes the stored
/// row and then delivers the feed event with `stored: true`, and `delete`
/// removes the row and then delivers the delete, as the store's own feed
/// would. Either way a call returns once HappyView has taken the event in.
///
/// `put` is handed a create or an update with its body, CID and rev;
/// `delete` a delete with its rev and, as the last known body, the body the
/// record had. A feeder whose source does not carry a deleted record's body
/// drops it, and says so with [`Feeder::without_delete_bodies`].
pub struct Feeder {
    put: Feed,
    delete: Feed,
    delete_bodies: bool,
    check: Option<Check>,
}

impl Feeder {
    pub fn new<P, PF, D, DF>(put: P, delete: D) -> Self
    where
        P: Fn(RecordEvent) -> PF + Send + Sync + 'static,
        PF: Future<Output = ()> + Send + 'static,
        D: Fn(RecordEvent) -> DF + Send + Sync + 'static,
        DF: Future<Output = ()> + Send + 'static,
    {
        Self {
            put: Box::new(move |event| Box::pin(put(event))),
            delete: Box::new(move |event| Box::pin(delete(event))),
            delete_bodies: true,
            check: None,
        }
    }

    /// A provider's own check, run after each case passes, with the case's
    /// name and the instance it left: what the provider can see that the
    /// served view cannot, such as where the records were written. A
    /// failure fails the case.
    pub fn after_each<C>(mut self, check: C) -> Self
    where
        C: for<'a> Fn(&'static str, &'a ProviderApp) -> CheckFuture<'a> + Send + Sync + 'static,
    {
        self.check = Some(Box::new(check));
        self
    }

    /// The deletes this feeder delivers carry no body, so no case expects
    /// a delete hook to see one.
    pub fn without_delete_bodies(mut self) -> Self {
        self.delete_bodies = false;
        self
    }

    pub async fn put(&self, event: RecordEvent) {
        (self.put)(event).await
    }

    pub async fn delete(&self, event: RecordEvent) {
        (self.delete)(event).await
    }
}

/// Builds providers for one case; called once per case so cases do not
/// share state. Runs every case, then fails naming each one that failed;
/// each failure's message starts with its case's name.
pub async fn run<F, Fut>(make: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = (Providers, Feeder)>,
{
    assert!(
        std::env::var_os("TEST_DATABASE_URL").is_some(),
        "the conformance run needs TEST_DATABASE_URL"
    );
    assert!(
        std::env::var_os(LUA_PLUGIN_DIR).is_some(),
        "the conformance run needs the Lua interpreter plugin for its hooks: set \
         {LUA_PLUGIN_DIR} (scripts/fetch-lua-plugin.sh fetches one)"
    );

    let mut failures: Vec<String> = Vec::new();
    // Each case in a block of its own: its providers, feeder and source's
    // stream are dropped, and the test database's lock released, before the
    // next case boots.
    macro_rules! cases {
        ($($case:ident),* $(,)?) => {$({
            let name = stringify!($case);
            let case = AssertUnwindSafe(async {
                let (providers, feeder) = make().await;
                let app = ProviderApp::new(providers).await;
                $case(&app, &feeder).await;
                if let Some(check) = &feeder.check {
                    check(name, &app).await;
                }
            });
            let failure = match tokio::time::timeout(CASE_TIMEOUT, case.catch_unwind()).await {
                Ok(Ok(())) => None,
                Ok(Err(panic)) => Some(format!("{name}: {}", panic_message(&panic))),
                Err(_) => Some(format!("{name}: timed out after {CASE_TIMEOUT:?}")),
            };
            match failure {
                None => eprintln!("conformance: {name} ok"),
                Some(failure) => {
                    eprintln!("conformance: {failure}");
                    failures.push(failure);
                }
            }
        })*};
    }
    cases!(
        create_without_hook_is_served,
        replace_hook_changes_what_is_served,
        skip_hook_hides_the_record,
        update_reruns_the_hook,
        delete_removes_the_record,
        history_runs_hooks,
        account_deletion_removes_the_accounts_records,
        redelivery_is_idempotent,
    );
    if !failures.is_empty() {
        panic!("{}", failures.join("\n"));
    }
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        message.to_string()
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else {
        "the case panicked".to_string()
    }
}

/// How many times the hook that logs `message` ran for `uri`.
async fn hook_runs(app: &ProviderApp, message: &str, uri: &str) -> usize {
    app.script_logs()
        .await
        .iter()
        .filter(|(logged, fields)| logged == message && fields["uri"] == uri)
        .count()
}

fn key(did: &str, rkey: &str) -> RecordKey {
    RecordKey {
        did: did.to_string(),
        collection: COLLECTION.to_string(),
        rkey: rkey.to_string(),
    }
}

fn uri(did: &str, rkey: &str) -> String {
    key(did, rkey).uri()
}

fn body(text: &str) -> Value {
    json!({ "$type": COLLECTION, "text": text })
}

/// The `n`th rev, as a TID; a later `n` is a later commit.
fn rev(n: i64) -> String {
    crate::tid::tid_from_unix_microseconds(1_700_000_000_000_000 + n)
}

fn written(did: &str, rkey: &str, action: Action, text: &str, n: i64) -> RecordEvent {
    let body = body(text);
    RecordEvent {
        key: key(did, rkey),
        action,
        cid: Some(
            crate::cid_verify::compute_record_cid(&body)
                .expect("a record body has a CID")
                .to_string(),
        ),
        rev: Some(rev(n)),
        body: Some(body),
        time_us: Some(1_700_000_000_000_000 + n),
        origin: Origin::Live,
        stored: false,
    }
}

fn created(did: &str, rkey: &str, text: &str, n: i64) -> RecordEvent {
    written(did, rkey, Action::Create, text, n)
}

fn deleted(did: &str, rkey: &str, last_text: &str, n: i64) -> RecordEvent {
    RecordEvent {
        key: key(did, rkey),
        action: Action::Delete,
        cid: None,
        rev: Some(rev(n)),
        body: Some(body(last_text)),
        time_us: Some(1_700_000_000_000_000 + n),
        origin: Origin::Live,
        stored: false,
    }
}

async fn create_without_hook_is_served(app: &ProviderApp, feeder: &Feeder) {
    app.track(COLLECTION, false).await;
    feeder.put(created(ALICE, "a1", "plain", 1)).await;

    assert_eq!(
        app.get_record(&uri(ALICE, "a1")).await,
        Some(body("plain")),
        "the record is served as it arrived"
    );
    assert_eq!(app.list(COLLECTION).await, vec![uri(ALICE, "a1")]);
}

async fn replace_hook_changes_what_is_served(app: &ProviderApp, feeder: &Feeder) {
    app.track(COLLECTION, false).await;
    app.set_hook(COLLECTION, Action::Create, MARK).await;
    feeder.put(created(ALICE, "a1", "plain", 1)).await;

    assert_eq!(
        app.get_record(&uri(ALICE, "a1")).await,
        Some(body("plain (hooked)")),
        "the hook's body is served in place of the one that arrived"
    );
    assert_eq!(app.list(COLLECTION).await, vec![uri(ALICE, "a1")]);
}

async fn skip_hook_hides_the_record(app: &ProviderApp, feeder: &Feeder) {
    app.track(COLLECTION, false).await;
    app.set_hook(COLLECTION, Action::Create, SKIP).await;
    feeder.put(created(ALICE, "a1", "skipped", 1)).await;

    assert_eq!(
        app.get_record(&uri(ALICE, "a1")).await,
        None,
        "a record the hook skipped is not served"
    );
    assert_eq!(app.list(COLLECTION).await, Vec::<String>::new());
    assert_eq!(
        hook_runs(app, "skip hook", &uri(ALICE, "a1")).await,
        1,
        "the skip hook ran, once"
    );
}

async fn update_reruns_the_hook(app: &ProviderApp, feeder: &Feeder) {
    app.track(COLLECTION, false).await;
    app.set_hook(COLLECTION, Action::Create, MARK).await;
    app.set_hook(COLLECTION, Action::Update, MARK).await;
    feeder.put(created(ALICE, "a1", "first", 1)).await;
    assert_eq!(
        app.get_record(&uri(ALICE, "a1")).await,
        Some(body("first (hooked)")),
        "the create went through the hook"
    );

    feeder
        .put(written(ALICE, "a1", Action::Update, "second", 2))
        .await;
    assert_eq!(
        app.get_record(&uri(ALICE, "a1")).await,
        Some(body("second (hooked)")),
        "the update went through the hook, from the body it carried"
    );
    assert_eq!(app.list(COLLECTION).await, vec![uri(ALICE, "a1")]);
}

async fn delete_removes_the_record(app: &ProviderApp, feeder: &Feeder) {
    app.track(COLLECTION, false).await;
    app.set_hook(COLLECTION, Action::Delete, LOG_DELETE).await;
    feeder.put(created(ALICE, "a1", "doomed", 1)).await;
    feeder.put(created(ALICE, "a2", "kept", 2)).await;
    assert_eq!(
        app.list(COLLECTION).await,
        vec![uri(ALICE, "a1"), uri(ALICE, "a2")]
    );

    feeder.delete(deleted(ALICE, "a1", "doomed", 3)).await;
    assert_eq!(
        app.get_record(&uri(ALICE, "a1")).await,
        None,
        "a deleted record is not served"
    );
    assert_eq!(
        app.list(COLLECTION).await,
        vec![uri(ALICE, "a2")],
        "only the deleted record went"
    );

    let logs = app.script_logs().await;
    let seen: Vec<&Value> = logs
        .iter()
        .filter(|(message, fields)| message == "delete hook" && fields["uri"] == uri(ALICE, "a1"))
        .map(|(_, fields)| &fields["text"])
        .collect();
    assert_eq!(seen.len(), 1, "the delete hook ran once: {logs:?}");
    if feeder.delete_bodies {
        assert_eq!(
            seen[0], "doomed",
            "the delete hook was given the body the delete carried"
        );
    }
}

async fn history_runs_hooks(app: &ProviderApp, feeder: &Feeder) {
    // Present before anything tracks the collection.
    feeder.put(created(ALICE, "h1", "one", 1)).await;
    feeder.put(created(ALICE, "h2", "two", 2)).await;
    feeder.put(created(BOB, "h3", "three", 3)).await;
    feeder
        .put(written(ALICE, "h2", Action::Update, "two, edited", 4))
        .await;
    app.set_hook(COLLECTION, Action::Create, MARK).await;
    app.set_hook(COLLECTION, Action::Update, MARK).await;

    let job = app.track(COLLECTION, true).await;
    assert!(job.is_some(), "tracking the collection started a backfill");
    let source = app.state.source.clone();
    app.wait_until("the backfill to finish", || {
        let source = source.clone();
        async move { !source.backfill_running(COLLECTION).await }
    })
    .await;

    let expected = [
        (uri(ALICE, "h1"), "one (hooked)"),
        (uri(ALICE, "h2"), "two, edited (hooked)"),
        (uri(BOB, "h3"), "three (hooked)"),
    ];
    for (uri, text) in &expected {
        assert_eq!(
            app.get_record(uri).await,
            Some(body(text)),
            "{uri} came in through the backfill and its hook"
        );
    }
    let mut uris: Vec<String> = expected.iter().map(|(uri, _)| uri.clone()).collect();
    uris.sort();
    assert_eq!(app.list(COLLECTION).await, uris);
}

async fn account_deletion_removes_the_accounts_records(app: &ProviderApp, feeder: &Feeder) {
    app.track(COLLECTION, false).await;
    feeder.put(created(ALICE, "a1", "alice's", 1)).await;
    feeder.put(created(ALICE, "a2", "alice's too", 2)).await;
    feeder.put(created(BOB, "b1", "bob's", 3)).await;
    assert_eq!(app.list(COLLECTION).await.len(), 3);

    app.deliver(vec![SourceEvent::Account {
        did: ALICE.to_string(),
        active: false,
        status: Some("deleted".to_string()),
        as_of_us: None,
        seq: None,
        time_us: None,
        collections: None,
    }])
    .await;
    app.wait_until("the deleted account's records to go", move || async move {
        app.list(COLLECTION).await == vec![uri(BOB, "b1")]
    })
    .await;
    assert_eq!(app.get_record(&uri(ALICE, "a1")).await, None);
    assert_eq!(
        app.get_record(&uri(BOB, "b1")).await,
        Some(body("bob's")),
        "another account's records stay"
    );
}

/// Today the hook runs on every delivery, an identical redelivery and a
/// stale one included; what is idempotent is what gets served. Each
/// delivery is counted, so a hook run twice for one delivery fails here.
async fn redelivery_is_idempotent(app: &ProviderApp, feeder: &Feeder) {
    app.track(COLLECTION, false).await;
    app.set_hook(COLLECTION, Action::Create, MARK).await;
    app.set_hook(COLLECTION, Action::Update, MARK).await;
    let a1 = uri(ALICE, "a1");
    let mut deliveries = 0;
    let event = created(ALICE, "a1", "once", 1);
    for _ in 0..2 {
        feeder.put(event.clone()).await;
        deliveries += 1;
        assert_eq!(
            hook_runs(app, "mark hook", &a1).await,
            deliveries,
            "the hook ran once for each delivery of the create"
        );
    }
    assert_eq!(
        app.get_record(&a1).await,
        Some(body("once (hooked)")),
        "a redelivered create is served as the first delivery was"
    );

    let update = written(ALICE, "a1", Action::Update, "twice", 2);
    // An older commit arriving late, last, changes nothing either.
    for event in [update.clone(), update, event] {
        feeder.put(event).await;
        deliveries += 1;
        assert_eq!(
            hook_runs(app, "mark hook", &a1).await,
            deliveries,
            "the hook ran once for each delivery"
        );
    }
    assert_eq!(
        app.get_record(&a1).await,
        Some(body("twice (hooked)")),
        "a redelivered update is served as the first delivery was"
    );
    assert_eq!(app.list(COLLECTION).await, vec![a1]);
}

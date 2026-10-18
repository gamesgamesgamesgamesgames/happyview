//! The provider conformance cases, run against the pipeline every provider
//! feeds: once with records delivered by a source into the instance
//! database, and once with a claiming external store that holds the records
//! and delivers its feed with `stored: true`.
//!
//! ```sh
//! TEST_DATABASE_URL=... HAPPYVIEW_LUA_PLUGIN=... \
//!     cargo test --features testkit --test conformance
//! ```

mod common;

use std::sync::Arc;

use happyview::Providers;
use happyview::record_store::PreparedRecord;
use happyview::testkit::conformance::{self, COLLECTION, Feeder};
use happyview_provider::testkit::ScriptedSource;
use happyview_provider::{Delta, RecordEvent, SourceEvent};

use common::fake_storage::FakeStorage;

/// A feeder that sends each event down the scripted source's live stream
/// and waits for the host to take it in.
fn scripted_feeder(
    source: Arc<ScriptedSource>,
    events: tokio::sync::mpsc::UnboundedSender<SourceEvent>,
    prepare: impl Fn(RecordEvent) -> futures::future::BoxFuture<'static, RecordEvent>
    + Clone
    + Send
    + Sync
    + 'static,
) -> Feeder {
    let send = move |event: RecordEvent| {
        let source = source.clone();
        let events = events.clone();
        let prepare = prepare.clone();
        async move {
            let event = prepare(event).await;
            events
                .send(SourceEvent::Record(event))
                .expect("the scripted source is listening");
            source.flush().await;
        }
    };
    Feeder::new(send.clone(), send)
}

#[tokio::test]
async fn the_default_pipeline_conforms() {
    conformance::run(|| async {
        let (source, events) = ScriptedSource::new();
        let feeder = scripted_feeder(source.clone(), events, |event| {
            Box::pin(async move { event })
        });
        (
            Providers {
                source,
                storage: None,
            },
            feeder,
        )
    })
    .await;
}

#[tokio::test]
async fn a_claiming_external_store_conforms() {
    conformance::run(|| async {
        let (source, events) = ScriptedSource::new();
        let storage = Arc::new(FakeStorage::applying(&[COLLECTION]).await);
        let store = storage.clone();
        // The store writes its own row, then its feed delivers the event.
        let feeder = scripted_feeder(source.clone(), events, move |event: RecordEvent| {
            let store = store.clone();
            Box::pin(async move {
                match &event.body {
                    Some(body) if event.action != happyview_provider::Action::Delete => {
                        store
                            .store(PreparedRecord {
                                did: event.key.did.clone(),
                                collection: event.key.collection.clone(),
                                rkey: event.key.rkey.clone(),
                                record_json: body.to_string(),
                                cid: event.cid.clone().unwrap_or_default(),
                                rev: event.rev.clone(),
                            })
                            .await
                    }
                    _ => store.unstore(&event.key.uri()).await,
                }
                RecordEvent {
                    stored: true,
                    ..event
                }
            })
        });
        // What the served view cannot show: nothing of the claimed
        // collection was written locally, and core spoke to the store.
        let seen = storage.clone();
        let feeder = feeder.after_each(move |case, app| {
            let seen = seen.clone();
            Box::pin(async move {
                assert_eq!(
                    app.local_rows(COLLECTION).await,
                    0,
                    "a claimed collection has no local rows"
                );
                let deltas = seen.deltas();
                assert!(!deltas.is_empty(), "core wrote no delta to the store");
                if case == "delete_removes_the_record" {
                    // The store removed the record itself; what core owes it
                    // is clearing this instance's delta, at the delete's rev.
                    let deleted: Vec<_> = deltas
                        .iter()
                        .filter(|(key, _, _)| key.rkey == "a1")
                        .collect();
                    let (_, first, created_at) = deleted.first().expect("a1 had a delta");
                    let (_, last, deleted_at) = deleted.last().unwrap();
                    assert_eq!(*first, Delta::Proceed, "a1's create proceeded");
                    assert_eq!(*last, Delta::Proceed, "the delete cleared a1's delta");
                    assert!(
                        deleted_at.is_some() && deleted_at > created_at,
                        "the delete's delta is at the delete's rev: {deleted:?}"
                    );
                }
            })
        });
        (
            Providers {
                source,
                storage: Some(storage),
            },
            feeder,
        )
    })
    .await;
}

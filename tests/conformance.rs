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
use happyview_provider::{RecordEvent, SourceEvent};

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

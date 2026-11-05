//! A scripted [`RecordSource`] for provider tests (feature `testkit`).
//!
//! [`ScriptedSource`] stands in for a network: a test sends it
//! [`SourceEvent`]s on a channel, and it delivers them to the host in order
//! through [`Host::ingest_batch`], as a live source does. Every record event
//! it is sent is also kept as the scripted world's history, which its
//! backfill replays: a record sent for a collection nobody tracks yet is not
//! delivered live, as a filtering source would not deliver it, but a backfill
//! of that collection later brings it in.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use crate::host::Host;
use crate::source::RecordSource;
use crate::types::*;

/// A [`RecordSource`] whose live stream is fed by a channel.
///
/// Its backfill replays the record events it has been sent for the
/// requested collection (and DIDs, when the request names some), latest
/// version of each record first written, deleted records left out, with
/// [`Origin::Backfill`].
pub struct ScriptedSource {
    events: Mutex<Option<mpsc::UnboundedReceiver<SourceEvent>>>,
    flushes: mpsc::UnboundedSender<oneshot::Sender<()>>,
    flush_requests: Mutex<Option<mpsc::UnboundedReceiver<oneshot::Sender<()>>>>,
    host: OnceLock<Arc<dyn Host>>,
    /// Every record the stream has carried, by URI, as last sent.
    history: Mutex<BTreeMap<String, RecordEvent>>,
    /// Collections a backfill is replaying into, shared with its task.
    running: Arc<Mutex<HashSet<String>>>,
    backfilled: Mutex<HashSet<String>>,
    jobs: AtomicU64,
}

impl ScriptedSource {
    /// The source, and the sender that feeds its live stream. The stream
    /// ends once every sender is dropped.
    pub fn new() -> (Arc<Self>, mpsc::UnboundedSender<SourceEvent>) {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (flush_tx, flush_rx) = mpsc::unbounded_channel();
        let source = Arc::new(Self {
            events: Mutex::new(Some(events_rx)),
            flushes: flush_tx,
            flush_requests: Mutex::new(Some(flush_rx)),
            host: OnceLock::new(),
            history: Mutex::new(BTreeMap::new()),
            running: Arc::new(Mutex::new(HashSet::new())),
            backfilled: Mutex::new(HashSet::new()),
            jobs: AtomicU64::new(0),
        });
        (source, events_tx)
    }

    /// Returns once every event sent before the call has been handed to the
    /// host and the host has answered. Returns at once when the source has
    /// not started or its stream has ended.
    pub async fn flush(&self) {
        let (done_tx, done_rx) = oneshot::channel();
        if self.flushes.send(done_tx).is_ok() {
            let _ = done_rx.await;
        }
    }

    fn remember(&self, event: &SourceEvent) {
        let SourceEvent::Record(record) = event else {
            return;
        };
        let mut history = self.history.lock().unwrap();
        match record.action {
            Action::Create | Action::Update => {
                history.insert(record.key.uri(), record.clone());
            }
            Action::Delete => {
                history.remove(&record.key.uri());
            }
        }
    }

    /// Whether a live source filtering on the tracked list would deliver it.
    fn wanted(tracked: &[String], event: &SourceEvent) -> bool {
        match event {
            SourceEvent::Record(record) => {
                record.key.collection == LEXICON_SCHEMA_COLLECTION
                    || tracked.contains(&record.key.collection)
            }
            _ => true,
        }
    }

    async fn deliver(&self, host: &Arc<dyn Host>, batch: Vec<SourceEvent>) {
        for event in &batch {
            self.remember(event);
        }
        let tracked = host.tracked_collections().borrow().clone();
        let wanted: Vec<SourceEvent> = batch
            .into_iter()
            .filter(|event| Self::wanted(&tracked, event))
            .collect();
        if !wanted.is_empty() {
            let _ = host.ingest_batch(wanted).await;
        }
    }

    async fn run(
        self: Arc<Self>,
        host: Arc<dyn Host>,
        mut events: mpsc::UnboundedReceiver<SourceEvent>,
        mut flushes: mpsc::UnboundedReceiver<oneshot::Sender<()>>,
    ) {
        loop {
            // Events first: a flush answers only once nothing sent before
            // it is still queued.
            tokio::select! {
                biased;
                event = events.recv() => {
                    let Some(event) = event else { break };
                    let mut batch = vec![event];
                    while let Ok(event) = events.try_recv() {
                        batch.push(event);
                    }
                    self.deliver(&host, batch).await;
                }
                Some(done) = flushes.recv() => {
                    let _ = done.send(());
                }
            }
        }
    }

    fn replay(&self, collections: &[String], dids: &[String], job_id: &str) -> Vec<SourceEvent> {
        self.history
            .lock()
            .unwrap()
            .values()
            .filter(|record| collections.contains(&record.key.collection))
            .filter(|record| dids.is_empty() || dids.contains(&record.key.did))
            .map(|record| {
                SourceEvent::Record(RecordEvent {
                    origin: Origin::Backfill {
                        job_id: job_id.to_string(),
                    },
                    ..record.clone()
                })
            })
            .collect()
    }
}

#[async_trait]
impl RecordSource for ScriptedSource {
    fn id(&self) -> &'static str {
        "scripted"
    }

    fn migrations(
        &self,
        _backend: crate::db::DatabaseBackend,
    ) -> Option<&'static sqlx::migrate::Migrator> {
        None
    }

    async fn start(self: Arc<Self>, host: Arc<dyn Host>) -> Result<(), ProviderError> {
        if self.host.set(host.clone()).is_err() {
            return Err(ProviderError::Other(
                "the scripted source was started twice".into(),
            ));
        }
        let events = self.events.lock().unwrap().take();
        let flushes = self.flush_requests.lock().unwrap().take();
        if let (Some(events), Some(flushes)) = (events, flushes) {
            tokio::spawn(self.clone().run(host, events, flushes));
        }
        Ok(())
    }

    /// One route, `GET /admin/scripted`: the host layers its auth over a
    /// source's routes, which needs at least one.
    fn admin_routes(&self) -> axum::Router {
        axum::Router::new().route(
            "/scripted",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({ "source": "scripted" }))
            }),
        )
    }

    async fn start_backfill(&self, req: BackfillRequest) -> Result<Option<String>, BackfillError> {
        let Some(host) = self.host.get().cloned() else {
            return Err(BackfillError::Internal(
                "the scripted source has not started".into(),
            ));
        };
        let collections = match req.collection {
            Some(collection) => vec![collection],
            None => host.tracked_collections().borrow().clone(),
        };
        let job_id = format!("scripted-{}", self.jobs.fetch_add(1, Ordering::SeqCst) + 1);
        let events = self.replay(&collections, &req.dids, &job_id);
        {
            let mut running = self.running.lock().unwrap();
            let mut backfilled = self.backfilled.lock().unwrap();
            for collection in &collections {
                running.insert(collection.clone());
                backfilled.insert(collection.clone());
            }
        }
        let running = self.running.clone();
        tokio::spawn(async move {
            if !events.is_empty() {
                let _ = host.ingest_batch(events).await;
            }
            let mut running = running.lock().unwrap();
            for collection in &collections {
                running.remove(collection);
            }
        });
        Ok(Some(job_id))
    }

    async fn has_backfill(&self, collection: &str) -> bool {
        self.backfilled.lock().unwrap().contains(collection)
    }

    async fn backfill_running(&self, collection: &str) -> bool {
        self.running.lock().unwrap().contains(collection)
    }

    async fn status(&self) -> serde_json::Value {
        serde_json::json!({ "source": "scripted" })
    }

    async fn metrics(&self) -> Vec<(String, i64)> {
        Vec::new()
    }
}

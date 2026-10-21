---
title: "Record providers"
---

HappyView gets its records from a record source and, optionally, keeps some of them in an external record storage. Both are traits in an open crate, so you can build your own and run HappyView with them. The built-in setup is the Jetstream source with no external storage, which is what `happyview` runs unless you change it. Everything in [Backfill](backfill.md) and the Jetstream settings in [Configuration](../getting-started/configuration.md) (`JETSTREAM_URL` and the rest) describe that built-in source.

You only need this page if you are writing a provider. If you run HappyView as it ships, nothing here changes anything for you.

## What a provider is

A provider is one or both of:

- A **record source** (`RecordSource`) finds records on the network and hands them to HappyView. Jetstream is the one that ships. A source also owns its backfills and its admin routes.
- A **record storage** (`RecordStorage`) keeps the records of the collections it claims, outside the instance database. HappyView still runs record hooks, serves reads, and answers writes for those collections. It asks the storage where the bytes live.

HappyView gives a provider one thing, the `Host`. A provider never reaches into HappyView any other way, and anything it needs that `Host` lacks is a request to add it there.

The crates:

| Crate | Licence | What it is |
|---|---|---|
| `happyview-provider` | MIT OR Apache-2.0 | The contract: the three traits, the values that cross the boundary, and the database helpers a provider needs on both SQLite and Postgres. It has a `testkit` feature with a scripted source for tests. |
| `happyview-source-jetstream` | follows the repository | The Jetstream source, plus a builder to compose it into another provider. |
| `happyview` | follows the repository | The server. `happyview::run` takes your providers. |

Only what the crates re-export at their root is the stable API. Modules marked `#[doc(hidden)]` are public so HappyView's own tests can reach them. They are not part of the API and can change in any release.

## The `Host` trait

This is what HappyView gives a provider.

| Method | Meaning |
|---|---|
| `ingest(SourceEvent) -> IngestOutcome` | The whole ingest pipeline: tracked-collection check, CID check, record hook, storage write (or delta, for a `stored` event), label backfill, `com.atproto.lexicon.schema` handling, account purges and tombstones. Replaying an event is safe. |
| `ingest_batch(Vec<SourceEvent>) -> BatchOutcome` | The same, in order, in one write transaction where the storage allows. `BatchOutcome` holds one `IngestOutcome` per event and a `changed` flag for each (false for an identical repeat, a refused write, or anything not stored). |
| `ingest_with_error`, `ingest_batch_with_error` | The same two calls, plus the error text behind a failed event, for a source that reports or retries it. They default to calling the plain versions. |
| `tracked_collections() -> watch::Receiver<Vec<String>>` | The record collections this instance indexes. It changes as lexicons are added and removed. |
| `has_record_hook(collection, action) -> bool` | Whether a hook would run, so a source can skip work. |
| `held_dids_page(collections, after_did, limit)` | The DIDs that hold records in those collections, in order. |
| `holds_records(did, collections) -> bool` | Whether one DID holds records there. The default pages through `held_dids_page`; a host that can answer directly overrides it. |
| `database() -> ProviderDb` | The instance pool, a second pool for backfill work, and the backend, for the provider's own tables. |
| `setting(key)`, `put_setting(key, value)` | Instance settings. Keys are not namespaced. |
| `log_event(ProviderEvent)` | A row in the admin event log. |
| `http() -> reqwest::Client` | The shared HTTP client. |
| `instance() -> InstanceInfo` | The PLC, relay and public URLs, and the `User-Agent` HappyView sends. |
| `resolve_pds(did)`, `resolve_handle(handle)` | One attempt at a lookup, with no cache in front of it. `resolve_pds` fetches the DID document from the PLC directory (or the `did:web` host) and reads its PDS endpoint. `resolve_handle` resolves a handle to its DID, and returns a DID it is given after checking it. The `ResolveError` says whether a retry could help and after how long. |
| `counters() -> Arc<IngestCounters>` | The `received`, `matched` and `skipped` counters telemetry reports. A source increments them. `ingest` does not. |

## The `RecordSource` trait

| Method | Meaning |
|---|---|
| `id() -> &'static str` | A stable id. It names the source's migration table, `_happyview_source_<id>_migrations`. |
| `contract_version() -> u32` | Defaults to `CONTRACT_VERSION`. A source built for another version is refused at boot. |
| `migrations(backend) -> Option<&'static Migrator>` | The source's own schema for that backend. HappyView runs it after its own migrations. |
| `start(Arc<Self>, Arc<dyn Host>)` | Spawn live ingest and resume interrupted jobs, then return. |
| `admin_routes() -> axum::Router` | Mounted under `/admin`, behind HappyView's auth layer. The layer inserts an `AdminCaller` with the caller's DID and permission strings. A route takes `AdminCaller` or `AdminUser` (which also refuses the managed-hosting platform key) and calls `require("some:permission")`. A refusal is logged the way HappyView's own are. |
| `start_backfill(BackfillRequest) -> Result<Option<String>, BackfillError>` | For automatic and gap backfills. `None` means the source decided no job was needed. Otherwise the job id. |
| `has_backfill(collection)`, `backfill_running(collection)` | Whether a backfill exists or is running for a collection. |
| `status() -> serde_json::Value`, `metrics() -> Vec<(String, i64)>` | For the admin status page and telemetry. |

### What a source owes HappyView

- Deliver every commit for the tracked collections, and always for `com.atproto.lexicon.schema`, whatever the tracked list says. Also deliver account, identity and sync events.
- Send every record through `Host::ingest` or `ingest_batch`. A source never writes records itself.
- Deliver events for one record in order. Duplicates are fine.
- A backfill delivers a `Create` for every record it finds, so hooks run on history. It uses `Origin::Backfill { job_id }`.
- Count `received`, `matched` and `skipped` through `host.counters()`.

HappyView runs the record hook on every delivery, including an identical redelivery. Only what is served is idempotent, so a hook with side effects should expect to run twice.

## The `RecordStorage` trait

HappyView keeps its own record tables for every collection a storage does not claim. A storage claims collections and serves them.

| Method | Meaning |
|---|---|
| `claims(collection) -> bool` | Whether the storage keeps this collection. It must not change for the life of a request. |
| `claimed() -> Vec<String>` | The collections it claims right now, for stats and label cleanup. |
| `read_route(collection) -> ReadRoute` | A pool on which `happyview_records_v2`, `happyview_dids`, `happyview_collections` and `happyview_record_refs_v2` resolve to relations with the compact-record columns. HappyView runs its reads and the plugin query DSL against it. |
| `apply_delta(key, Delta, for_rev)` | Record the outcome of a hook, a local write, a delete or an admin delete. `Delta` is `Proceed` (clear any delta), `Replace`, `Hide` or `Pending` (a local write awaiting its echo). With `for_rev: Some(rev)` it applies only while the stored record is still at that rev, so a delta computed from an old version never lands on a newer one. `None` applies always. A record delete is a `Proceed` with `for_rev: None`. |
| `forget_account(did, collections)` | Account purges. It forgets this instance's deltas, not the records. |
| `delete_collection_batch(collection, limit)` | One batch of the delete-collection job. |
| `is_live(uri) -> bool` | Whether a record exists, for label cleanup. |
| `holds_account(did, collections) -> bool` | Whether the account holds records in these claimed collections (all of them when `None`). Before purging a deleted account it holds records for, HappyView asks the account's PDS whether it is still active; this lets an account held only by the storage get that check. An error counts as yes. The default, `false`, skips the check for such accounts. |
| `unavailable(&sqlx::Error) -> bool` | Whether the error means the storage is down. The default uses the same test as the instance database. |

## Claimed collections

For a collection a storage claims:

- **Reads** go to the storage's `read_route`. The list, get and query endpoints and the plugin query DSL behave as they do for any collection.
- **Writes** from HappyView go through the storage too. A local write is a `Pending` delta until the record comes back from the source.
- **Deletes** from an event clear the delta unconditionally: a `Proceed` with `for_rev: None`, so no `Replace` or `Hide` outlives the record and covers a later re-create of the same key. The storage owns removing the record itself. An admin delete does not clear anything. It hides the record with a `Hide` delta, also with `for_rev: None`.
- **Events** carry `stored: true` when the body is already in the storage. HappyView then runs the hook and writes only the hook's delta. HappyView refuses an event for a claimed collection that is not marked stored, with a `record.unstored_claimed` warning in the event log, so nothing is ever written to the local tables.
- **Raw SQL** reads and writes the local tables as before for unclaimed collections. A claimed collection's records are queryable in the storage's own database. A `db.raw` write that names one of its records is refused with an error that says to use `records.indexPut`, so a hook cannot create a local row that reads never see. While a storage is configured, a raw write to `happyview_records` that names no collection or record URI is refused too, because HappyView cannot tell whether it touches a claimed record.
- **Outages.** When the storage is down (`unavailable` says so), requests for a claimed collection answer 503 `ServiceUnavailable`, which a client can retry:
  - every XRPC read, whether the default get and list or a script's record queries;
  - a write that fails before anything else accepted it: a script's index write or index delete, and an admin delete.

  A write the caller's PDS has already accepted still answers 200. HappyView cannot show it until the storage is back, and the storage's own ingest brings the record in. An outage of the instance database stays a 500.

## Composing with Jetstream

A provider that wants Jetstream for most collections and its own path for the ones it claims builds the Jetstream source with two hooks:

```rust
let source = JetstreamSource::builder()
    .live_filter(move |collection| !storage.claims(collection))
    .executor_for(move |collection| executors.get(collection).cloned())
    .build();
```

- `live_filter` says which tracked collections to stream live. Leave claimed ones out. It is asked again only when the tracked list changes, so keep its answers stable.
- `executor_for` says who runs a backfill for a collection. A `BackfillExecutor` has a `name()` and an async `run(JobHandle, collection) -> Result<JobTotals, String>`.

An executor's job is an ordinary row in the job system. It is listed, paused, cancelled, resumed and swept like a crawl, and its `source` column holds the executor's `name()`. Through its `JobHandle` an executor reads the job's `id()` and the `host()`, reports `progress` and `stage`, calls `record_error`, polls `should_stop`, and keeps a resume point with `checkpoint`, which it reads back with `resume_from`. The checkpoint is stored per job and collection. `JobTotals.records` is added across resumes. An executor that panics fails its job. `record_error(None, ..)` is a single job-level slot; `Some(did)` keeps one row per DID.

How the pieces fit:

- A backfill of a collection that has an executor runs as that executor's job, whatever source was asked for.
- A backfill of every collection starts one executor job per executor collection and leaves them out of the main job. The response keeps the main job's id.
- Account backfills leave those collections out.
- A gap in the live stream is replayed for every collection the stream carries.
- If you forget `live_filter`, HappyView's safety net refuses the unstored events and warns. No data is lost, but you get a warning for every record.

## Running HappyView with your providers

`happyview::run(command, Providers)` is the one entry point. `Providers { source, storage }` takes the source and, optionally, a storage. `Providers::default()` is the built-in Jetstream source with no storage, which is what the `happyview` binary passes.

```rust
use std::process::ExitCode;

use happyview::{Providers, cli, run};
use happyview_source_jetstream::JetstreamSource;

fn main() -> ExitCode {
    let command = match cli::parse(std::env::args_os().skip(1)) {
        Ok(command) => command,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };
    let source = JetstreamSource::builder()
        .live_filter(|collection| !collection.starts_with("app.example.private."))
        .build();
    let providers = Providers { source, storage: None };
    let runtime = tokio::runtime::Runtime::new().expect("failed to start tokio");
    runtime.block_on(run(command, providers))
}
```

The binary accepts the same commands as `happyview`, and `run` prints the usage for `--help` itself. The `happyview` binary also loads a `.env` file before a plain serve; the example leaves that out, so add `dotenvy::dotenv().ok()` before `run` when the command is `Command::Serve` if you want it. Swap in your own `Arc<dyn RecordSource>` or set `storage` to `Some(Arc<dyn RecordStorage>)` the same way.

## Testing a provider

The conformance kit runs a fixed set of cases against your providers. Each case boots a fresh instance, feeds it records, and checks what HappyView then serves: hooks that replace, skip or leave a record on create, a skip on update, updates, deletes, history from a backfill, account removal, redelivery, and the 503 a claimed read answers while the storage is down. A provider passes by behaving as the built-in one does.

You supply two things: a function that builds your `Providers`, and a `Feeder` that puts records into your provider's world. The kit calls the function once per case so cases do not share state.

```rust
use happyview::testkit::conformance::{self, Feeder};

#[tokio::test]
async fn my_provider_conforms() {
    conformance::run(|| async {
        // `build_my_providers` is your own function: it makes fresh
        // providers and the feeder that puts records into their world.
        build_my_providers().await
    })
    .await;
}
```

`Feeder::new(put, delete)` takes two functions, each given a `RecordEvent`. For a source, they deliver the event the way your source would. For a claiming storage, `put` writes the stored row first and then delivers the event with `stored: true`, and `delete` removes the row and then delivers the delete. `Feeder::after_each` adds a check of your own after each case, and `without_delete_bodies` says your deletes carry no body. The cases use the collection `conformance::COLLECTION`. A storage claims that one, and must also give `Feeder::make_unavailable`, a function that takes the storage down so its reads fail as `unavailable` recognises. The 503 case fails, saying so, when a claiming storage leaves it out. For providers that claim nothing, that case checks only that the query serves.

`tests/conformance.rs` in the repository has two complete runs: the default providers with a scripted source, and an external store.

In your own crate, depend on HappyView with its `testkit` feature in `[dev-dependencies]`, and on the contract crate's `testkit` feature if you use `ScriptedSource`. The contract crate lives in the same repository as HappyView, so take both from the same git source:

```toml
[dev-dependencies]
happyview = { git = "https://github.com/gamesgamesgamesgamesgames/happyview", features = ["testkit"] }
happyview-provider = { git = "https://github.com/gamesgamesgamesgamesgames/happyview", features = ["testkit"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Put the test above in `tests/conformance.rs` and run it with the `testkit` feature:

```bash
TEST_DATABASE_URL=postgres://... HAPPYVIEW_LUA_PLUGIN=/path/to/lua-plugin \
  cargo test --features testkit --test conformance
```

- `HAPPYVIEW_LUA_PLUGIN` is required because the hooks are real Lua. Point it at the Lua interpreter plugin. CI fetches it with `scripts/fetch-lua-plugin.sh`.
- `TEST_DATABASE_URL` is a Postgres URL or a file SQLite URL such as `sqlite:///path/t.db?mode=rwc`. The conformance run panics if either variable is unset. It does not skip.

## Upgrading

Two telemetry log lines moved with the code into the contract crate. They now log under the target `happyview_provider::telemetry` (they were under the `happyview::telemetry::collect` modules). If you filter logs by target, for example with `RUST_LOG`, update the filter. Nothing else about log output, tables, endpoints, settings or JSON shapes changed.

The contract carries a version, `CONTRACT_VERSION` in `happyview-provider`. A change that breaks providers bumps it. At boot HappyView checks the record source's `contract_version()` and refuses a source built for another version. The check covers the record source only. `RecordStorage` carries no version, so a storage is not checked.

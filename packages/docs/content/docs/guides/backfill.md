---
title: "Backfill"
---

When you add a new record-type lexicon, HappyView starts indexing new records from that moment via [Jetstream](https://github.com/bluesky-social/jetstream). But what about records that already exist on the network? That's what backfill does: HappyView discovers repos via the relay and fetches records directly from each user's PDS.

## When backfill runs

- **Automatically** when a record-type lexicon is uploaded with `backfill: true` (the default). See [Lexicons - Backfill flag](lexicons.md#backfill-flag).
- **Manually** via `POST /admin/backfill` or the [dashboard](../getting-started/dashboard.md). You can scope a manual backfill to a specific collection, a specific DID, or both.

See the [admin API](../api-reference/admin/backfill.md) for endpoint details.

## How it works

A backfill job starts with a discovery phase and then pipelines resolution and fetching concurrently:

1. **Discovering repos** — HappyView calls the relay's `com.atproto.sync.listReposByCollection` to find repos that contain records for each target collection. Discovered DIDs are stored in a tracking table so progress can be resumed.
2. **Resolving PDS + Fetching records** (pipelined) — Resolution and fetching run concurrently. As each DID is resolved (via PLC directory or `did:web`), it's immediately handed off for record fetching — there's no need to wait for all DIDs to resolve before fetching begins. HappyView calls `com.atproto.repo.listRecords` on each PDS for the target collection(s), upserting each record into the local database. PDS endpoints are processed concurrently (up to 10 PDS hosts, 3 DIDs per host).

Progress counters (`total_repos`, `resolved_repos`, `processed_repos`, `total_records`) and the current `stage` are updated in real time. The dashboard's Backfill page shows live progress, and clicking a job opens a detail sheet with a stage-by-stage progress log.

### Rate limiting

All three phases handle HTTP 429 responses. HappyView reads the `RateLimit-Reset` header (a Unix timestamp, the atproto convention) to determine how long to wait, falling back to the `retry-after` header, then defaulting to 5 seconds.

## Job lifecycle

A backfill job has both a `status` (overall state) and a `stage` (current phase):

| Status       | Description                                          |
| ------------ | ---------------------------------------------------- |
| `running`    | Job is actively processing                           |
| `cancelling` | Cancel requested, waiting for the worker to stop     |
| `cancelled`  | Worker has stopped and cleaned up                    |
| `completed`  | Worker finished processing all resolvable repos      |
| `failed`     | An error occurred                                    |

The `stage` field tracks which phase the job is in: `pending`, `discovering_repos`, `resolving_and_fetching`, `completed`, `failed`, or `cancelled`.

## Cancelling a job

Running jobs can be cancelled via `POST /admin/backfill/{id}/cancel` or the Cancel button in the dashboard. Cancellation is two-phase:

1. The endpoint sets the job status to `cancelling`.
2. The worker checks for cancellation at natural checkpoints (between relay pages, every 100 DIDs during resolution, every 10 repos during fetching). When it detects the `cancelling` status, it stops work and sets the final status to `cancelled`.

This means there may be a short delay between clicking Cancel and the job fully stopping, depending on what the worker is doing at that moment.

## Resuming after restart

Backfill jobs survive server restarts. On startup, HappyView checks for jobs that were running when the server last stopped:

- **Running** jobs are re-spawned and resume from where they left off. Each phase is idempotent — discovery skips already-known DIDs, resolution skips already-resolved endpoints, and fetching skips already-completed repos.
- **Cancelling** jobs (where the cancel was requested but the worker hadn't stopped yet) are immediately finalised as `cancelled`.

Per-DID progress is tracked in the database, so a job that was halfway through fetching records will pick up from the next unprocessed repo, not start over.

## Re-running backfills

Re-running a backfill for a collection that's already been backfilled is safe. Each record is upserted by its AT URI, so existing records are refreshed in place and any new records discovered since the last run are added. This also picks up new repos that have joined the network since the previous backfill.

## Restoring deleted records

Deleting records from HappyView (via the dashboard or API) only removes them from the local database — the records still exist on the atproto network. To restore deleted records, create a backfill job for the affected collection. The backfill will re-discover the repos and re-fetch all records from each PDS, restoring any that were previously deleted.

## Diagnostics

The dashboard's backfill detail panel includes expandable sections for each processing phase. Clicking a phase row reveals per-repo and per-PDS detail data in real time.

### Per-repo tracking

Every DID discovered during a backfill job is tracked in the database with its PDS endpoint, processing status, and record count. This data powers three expandable sections:

- **Discovering repos** — lists all DIDs discovered for the job, with profile avatars and handles resolved from the Bluesky API.
- **Resolving PDS** — summarises PDS endpoints involved in the job, showing how many repos each PDS is responsible for and how many have been processed.
- **Fetching records** — lists completed repos with their record counts and PDS hostnames.

All three sections update in real time via SSE (Server-Sent Events) while the job is running.

### Data retention

Per-repo detail data is retained after job completion to support post-mortem analysis. A background task runs daily and deletes detail rows for jobs completed more than 28 days ago (configurable via the `backfill_retention_days` setting in **Settings > General**, or the `BACKFILL_RETENTION_DAYS` environment variable). Set to `0` to keep data indefinitely.

You can also manually clear detail data:

- **Per-job**: "Clear details" button in the job detail panel footer.
- **All completed jobs**: "Clear all details" button on the Backfill page header.

Both actions require the `backfill:create` permission.

## Next steps

- [Lexicons](lexicons.md#backfill-flag): Control whether lexicons trigger backfill on upload
- [Admin API — Backfill](../api-reference/admin/backfill.md): Full reference for backfill endpoints

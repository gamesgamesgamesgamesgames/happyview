---
title: "Backfill"
---

When you add a new record-type lexicon, HappyView starts indexing new records from that moment via [Jetstream](https://github.com/bluesky-social/jetstream). But what about records that already exist on the network? That's what backfill does: HappyView reads them from the Jetstream archive when it can, and otherwise discovers repos via the relay and fetches records directly from each user's PDS.

## When backfill runs

- **Automatically** when a record-type lexicon is uploaded with `backfill: true` (the default). See [Lexicons - Backfill flag](lexicons.md#backfill-flag).
- **Manually** via `POST /admin/backfill` or the [dashboard](../getting-started/dashboard.md). You can scope a manual backfill to a specific collection, a specific DID, or both.

See the [admin API](../api-reference/admin/backfill.md) for endpoint details.

## How it works

A backfill job starts with a discovery phase and then pipelines resolution and fetching concurrently:

1. **Discovering repos** — HappyView calls the relay's `com.atproto.sync.listReposByCollection` to find repos that contain records for each target collection. Each discovered repo becomes a work unit for the collection it was listed under, so it is fetched only for that collection. At most `BACKFILL_DISCOVERY_WINDOW` units (50,000 by default) are queued at once: discovery pauses while the queue is full and resumes as units finish, and finished units are deleted. The relay cursor for each collection is saved after every page, so a paused or restarted job carries on where it stopped.
2. **Resolving PDS + Fetching records** (pipelined, alongside discovery) — Resolution and fetching run concurrently. As each DID is resolved (via PLC directory or `did:web`), it's immediately handed off for record fetching — there's no need to wait for all DIDs to resolve before fetching begins. HappyView calls `com.atproto.repo.listRecords` on each PDS for the target collection(s), upserting each record into the local database. PDS endpoints are processed concurrently (up to 10 PDS hosts, 3 DIDs per host).

Progress counters (`total_repos`, `resolved_repos`, `processed_repos`, `total_records`) and the current `stage` are updated in real time. The repo counters count work units, so a repo discovered under two collections counts twice. The dashboard's Backfill page shows live progress, and clicking a job opens a detail sheet with a stage-by-stage progress log.

## Replay and crawl

A backfill job reads records from one of two sources:

- **Crawl** is the process described above: list repos from the relay, then fetch each one from its PDS. It is the default.
- **Replay** reads the Jetstream archive. It is an opt-in for collection backfills: one read of the archive replaces a crawl of every PDS.

Replay can only return what the archive holds. In Bluesky's archive, some repos' history starts later than their first record, so a replay can miss older records that a crawl finds. Use crawl when a backfill must be complete.

Account backfills (a job with `dids`) always crawl. Planning works in whole archive blocks, so replaying a few accounts downloads far more than crawling their PDSes. A job with accounts and `source: "auto"`, or with no `source` while the default is `replay`, runs as a crawl. One with `source: "replay"` is refused with `400`.

`BACKFILL_SOURCE` sets the default, and `POST /admin/backfill` takes a `source` that overrides it for one job. The values are `crawl` (the default), `auto` and `replay`. Unset or unrecognised, it means `crawl`. With `auto`, a collection backfill replays when `JETSTREAM_URL` points at a Jetstream with an archive, and crawls when the archive answers 404 or 501 or can't be reached. It also crawls when the archive asks for a key (401 or 403) and `JETSTREAM_API_KEY` isn't set, and logs once that a key would let it replay. When a key is set and the archive refuses it, the job fails instead. A job created as `replay` fails instead of falling back to a crawl.

A replay job downloads in two sweeps:

1. **Records.** The archive's commits for the job's collections, from the start of the archive to its newest sealed segment.
2. **Accounts.** The account and sync events for the accounts that hold records in those collections, from the last 12 hours of the archive.

Account takedowns, deletions and repo resyncs are applied as of when they happened. A takedown from three days ago removes only what the account had at that time, so records it wrote since stay.

The account sweep covers only the archive's newest hours because Jetstream compacts the rest. Compaction (every 4 hours by default) removes the creates and updates that a deleted account or a repo resync supersedes, so the records sweep already skips them. Sweeping account events over the whole archive would download many gigabytes for little gain. HappyView asks the archive for its segment list (`listSegments`), finds the first segment that ends within `BACKFILL_REPLAY_MARKER_TAIL_HOURS` (12 by default, three compaction intervals) of the newest one, and starts the account sweep just before it. The job logs where it starts. If the archive doesn't list its segments, or the request fails, the job sweeps the whole archive instead. A resumed job keeps the start it chose.

This has a known cost. Compaction keeps the records of taken-down and suspended accounts. If an account was taken down or suspended before those 12 hours, its records are replayed and stay indexed. A reactivation from before then doesn't lift a takedown HappyView recorded earlier. Records HappyView held before the job, from an earlier crawl or from before a gap in the live stream, aren't removed by an account deletion or repo resync from before then either. Set `BACKFILL_REPLAY_MARKER_TAIL_HOURS=0` to sweep account events over the whole archive. Do that for a self-hosted Jetstream with compaction turned off, where nothing is compacted away.

Each record goes through the same checks as a crawled one: the lexicon script runs, and the record's CID is checked against its body. A record that fails the check is skipped and logged.

The job page shows `seq {n} of {tip}` while it runs, along with the bytes downloaded and any failed blocks. A block that can't be downloaded or decoded after retries is recorded as a `replay_block` error. `POST /admin/backfill/{id}/retry-failed` starts a new job that fetches only those blocks, then applies the account events of every account those blocks wrote records for, so a later takedown still removes them. See [Retry failed](../api-reference/admin/backfill.md#retry-failed).

A replay job stops and fails rather than run on through an archive outage. Each request is retried first: a 502, 504 or other server error, or no answer at all, is retried 6 times over about 30 seconds, and a 503 is retried 5 times, waiting out its `Retry-After`. It stops after 16 blocks in a row that the archive didn't answer or answered with an HTTP error after those retries, with the error `the archive failed 16 blocks in a row; stopping (resume to continue)`. A block that arrives but doesn't decode, or is over a size cap, is recorded and skipped instead, and doesn't count towards the 16. A job also stops when a failed block can't be recorded, since a block that isn't recorded could never be retried. Either way it stops before it moves past the blocks in question.

A failed replay job can be resumed like a paused one: it carries on from where it stopped, without downloading again what it had finished. Two exceptions need a new backfill instead. A job that stopped because its error log is full (10,000 failures) stops again on every resume. An account replay job created before account backfills always crawled can't be resumed once its account list is gone, which happens when its details are cleared.

Replay needs a Jetstream that serves the v2 archive. Bluesky's hosted archive, which the default `JETSTREAM_URL` points at, requires a key: without `JETSTREAM_API_KEY`, `auto` jobs crawl and `replay` jobs fail. If `JETSTREAM_API_KEY` is set, it is sent to the archive and nowhere else. Replay jobs spool each segment to `BACKFILL_REPLAY_DIR` while they decode it, up to 1 GiB per segment.

Bluesky's hosted archive meters each key by the bytes it serves: about 1 GB at once, then about 2 MB per second. It advertises that quota on its responses, and HappyView paces every replay request to it, shared across every job using the key. Past the first gigabyte a replay downloads at about 2 MB/s however high `BACKFILL_REPLAY_CONCURRENCY` is set, so raising it doesn't speed replay up there. When the archive answers 429 anyway, every replay request waits out its `Retry-After` and tries again, for as long as it takes. Only a request that stays throttled for 15 minutes fails, and counts towards the 16 failed blocks above. A Jetstream that advertises no quota, such as a self-hosted one, isn't paced.

Crawl is what runs by default, for every account backfill, and for `auto` jobs when the Jetstream has no archive HappyView may use.

### Gaps in the live stream

When the live stream falls further behind than Jetstream keeps, Jetstream refuses the saved cursor (`CursorTooOld`). With `BACKFILL_SOURCE` set to `auto` or `replay`, HappyView replays the gap from the archive instead of skipping it. It resumes the live stream at the archive's newest sealed segment right away, and a replay job fills in the records between. The `jetstream.gap` event names that job in `replay_job_id`. If the archive can't be asked, or doesn't plan the gap within 30 seconds, HappyView resumes as before and a [backfill](#when-backfill-runs) is the way to recover what was missed. With `BACKFILL_SOURCE=crawl`, the default, gaps are never replayed.

### Rate limiting

All three crawl phases handle HTTP 429 responses. HappyView reads the `RateLimit-Reset` header (a Unix timestamp, the atproto convention) to determine how long to wait, falling back to the `retry-after` header, then defaulting to 5 seconds. Replay paces itself to the archive's quota instead; see [Replay and crawl](#replay-and-crawl).

## Job lifecycle

A backfill job has both a `status` (overall state) and a `stage` (current phase):

| Status       | Description                                          |
| ------------ | ---------------------------------------------------- |
| `running`    | Job is actively processing                           |
| `pausing`    | Pause requested, waiting for the worker to stop      |
| `paused`     | Worker has stopped; resume to carry on               |
| `cancelling` | Cancel requested, waiting for the worker to stop     |
| `cancelled`  | Worker has stopped and cleaned up                    |
| `completed`  | Worker finished processing all resolvable repos      |
| `failed`     | An error occurred                                    |

The `stage` field tracks which phase the job is in: `pending`, `discovering_repos`, `resolving_and_fetching`, `completed`, `failed`, or `cancelled`. A replay job moves through `replaying` (records), `replaying_accounts` (account events) and, in a retry job, `retrying_blocks`.

If a job runs out of work while repos it could not complete are still queued, HappyView pauses it rather than marking it completed, and records why in the job's `error` (for example "paused: 3 units could not be completed; resume to retry"). The dashboard shows such a job as **paused automatically** with that reason. Resuming it retries the queued repos.

## Cancelling a job

Running jobs can be cancelled via `POST /admin/backfill/{id}/cancel` or the Cancel button in the dashboard. Cancellation is two-phase:

1. The endpoint sets the job status to `cancelling`.
2. The worker checks for cancellation at natural checkpoints (between relay pages, every 100 DIDs during resolution, every 10 repos during fetching). When it detects the `cancelling` status, it stops work and sets the final status to `cancelled`.

This means there may be a short delay between clicking Cancel and the job fully stopping, depending on what the worker is doing at that moment.

## Resuming after restart

Backfill jobs survive server restarts. On startup, HappyView checks for jobs that were running when the server last stopped:

- **Running** jobs are re-spawned and resume from where they left off. Discovery continues from each collection's saved relay cursor, queued repos keep their resolved PDS, and finished repos have already left the queue. A repo cut short by the pause is fetched again; re-fetching an unchanged record does not rewrite it.
- **Cancelling** jobs (where the cancel was requested but the worker hadn't stopped yet) are immediately finalised as `cancelled`.

The queue is kept in the database, so a job that was halfway through fetching records will pick up from the next queued repo, not start over.

## Re-running backfills

Re-running a backfill for a collection that's already been backfilled is safe, whichever source it uses. Each record is upserted by its AT URI, so existing records are refreshed in place and any new records discovered since the last run are added. This also picks up new repos that have joined the network since the previous backfill.

## Restoring deleted records

Deleting records from HappyView (via the dashboard or API) only removes them from the local database — the records still exist on the atproto network. To restore deleted records, create a backfill job for the affected collection. The backfill will re-discover the repos and re-fetch all records from each PDS, restoring any that were previously deleted.

## Diagnostics

The dashboard's backfill detail panel includes expandable sections for each processing phase. Clicking a phase row reveals per-repo and per-PDS detail data in real time.

### Per-repo tracking

A backfill keeps a bounded queue of the repos it is working on, counters for its progress, a per-PDS summary and its 1,000 most recently fetched repos. These power three expandable sections:

A replay job has no repos to list, so it shows its seq progress instead of these sections.

- **Discovering repos** — the repos queued right now, with profile avatars and handles resolved from the Bluesky API. The count beside it is everything discovered so far, labelled "found so far" until discovery finishes.
- **Resolving PDS** — summarises PDS endpoints involved in the job, showing how many repos each PDS is responsible for and how many have been processed.
- **Fetching records** — the most recently fetched repos with their record counts.

Jobs created before the bounded queue keep their original row-per-repo detail until it is cleared.

All three sections update in real time via SSE (Server-Sent Events) while the job is running.

### Data retention

Per-repo detail data is retained after job completion to support post-mortem analysis. A background task runs daily and deletes the detail rows, and any repos left queued by a cancelled or failed job, of jobs that finished more than 28 days ago (configurable via the `backfill_retention_days` setting in **Settings > General**, or the `BACKFILL_RETENTION_DAYS` environment variable). Set to `0` to keep data indefinitely.

You can also manually clear detail data:

- **Per-job**: "Clear details" button in the job detail panel footer. A job created before the bounded queue can be cleared whenever it isn't running; any other job only once it is completed, cancelled or failed, since its queue is work it still has to do.
- **All finished jobs**: "Clear all details" button on the Backfill page header clears every completed, cancelled and failed job.

Both actions require the `backfill:create` permission.

## Next steps

- [Lexicons](lexicons.md#backfill-flag): Control whether lexicons trigger backfill on upload
- [Admin API — Backfill](../api-reference/admin/backfill.md): Full reference for backfill endpoints

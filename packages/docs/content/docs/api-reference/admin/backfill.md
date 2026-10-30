---
title: "Backfill"
---

Create and monitor historical backfill jobs. See the [Backfill guide](../../guides/backfill.md) for background.

```ts tab="TypeScript" tab-group="language"
const TOKEN = "hv_..."; // your API key
const headers = { Authorization: `Bearer ${TOKEN}` };
```
```js tab="JavaScript" tab-group="language"
const TOKEN = "hv_..."; // your API key
const headers = { Authorization: `Bearer ${TOKEN}` };
```
```rust tab="Rust" tab-group="language"
let token = "hv_..."; // your API key
```
```go tab="Go" tab-group="language"
token := "hv_..." // your API key
```
```sh tab="cURL" tab-group="language"
# All examples assume $TOKEN is an API key (hv_...)
AUTH="Authorization: Bearer $TOKEN"
```

## Create a backfill job

```
POST /admin/backfill
```

```ts tab="TypeScript" tab-group="language"
interface BackfillJob {
  id: string;
  status: string;
}

const response = await fetch("http://127.0.0.1:3000/admin/backfill", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    collection: "xyz.statusphere.status",
  }),
});
const data: BackfillJob = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/backfill", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    collection: "xyz.statusphere.status",
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let client = reqwest::Client::new();
let response = client
    .post("http://127.0.0.1:3000/admin/backfill")
    .bearer_auth(token)
    .json(&serde_json::json!({
        "collection": "xyz.statusphere.status"
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{"collection": "xyz.statusphere.status"}`)
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/admin/backfill", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/backfill \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{ "collection": "xyz.statusphere.status" }'
```

| Field        | Type     | Required | Description                                                                 |
| ------------ | -------- | -------- | ---------------------------------------------------------------------------- |
| `collection` | string   | no       | Limit to a single collection (backfills all if omitted)                     |
| `dids`       | string[] | no       | Accounts to backfill, as DIDs or handles (discovers all via relay if omitted) |
| `did`        | string   | no       | A single account; kept for compatibility and added to `dids`                |
| `source`     | string   | no       | `crawl`, `replay` or `auto`. Defaults to `BACKFILL_SOURCE` (`crawl` unless set). A job with `dids` always crawls, and `replay` with `dids` is refused. See [Replay and crawl](../../guides/backfill.md#replay-and-crawl) |

Every entry in `dids` must resolve, or the request fails with `400` listing each entry that did not and no job is created. Duplicates are ignored. A request can name at most 500 accounts. Any other `source` fails with `400` and the message `source must be one of auto, replay, crawl`. `source: "replay"` with `dids` fails with `400` and the message `replay is only available for collection backfills; account backfills always crawl`; account backfills crawl even when `BACKFILL_SOURCE` is `replay` or `auto`. A replay can only return what the archive holds: some repos' history in Bluesky's archive starts later than their first record. See [Replay and crawl](../../guides/backfill.md#replay-and-crawl).

**Response**: `201 Created`

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "running"
}
```

## Cancel a backfill job

```
POST /admin/backfill/{id}/cancel
```

Requests cancellation of a running backfill job. The job status transitions to `cancelling` immediately; the background worker will stop at its next checkpoint and set the final status to `cancelled`. See the [Backfill guide](../../guides/backfill.md#cancelling-a-job) for details on the two-phase process.

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  `http://127.0.0.1:3000/admin/backfill/${jobId}/cancel`,
  { method: "POST", headers },
);
const data = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  `http://127.0.0.1:3000/admin/backfill/${jobId}/cancel`,
  { method: "POST", headers },
);
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post(format!("http://127.0.0.1:3000/admin/backfill/{job_id}/cancel"))
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
url := fmt.Sprintf("http://127.0.0.1:3000/admin/backfill/%s/cancel", jobID)
req, _ := http.NewRequest("POST", url, nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST "http://127.0.0.1:3000/admin/backfill/$JOB_ID/cancel" -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "cancelling"
}
```

Returns `400` if the job is not currently running, or `404` if the job ID is not found.

## Pause a backfill job

```
POST /admin/backfill/{id}/pause
```

Requests a running backfill job to pause. The job status transitions to `pausing` immediately; the background worker will stop at its next checkpoint and set the status to `paused`. Paused jobs retain all progress and can be resumed later.

```sh tab="cURL" tab-group="language"
curl -X POST "http://127.0.0.1:3000/admin/backfill/$JOB_ID/pause" -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "pausing"
}
```

Returns `400` if the job is not currently running, or `404` if the job ID is not found.

## Resume a backfill job

```
POST /admin/backfill/{id}/resume
```

Resume a paused backfill job, or a failed replay job. The job status transitions back to `running`, its `error` is cleared, and processing continues from where it left off. A replay job carries on from the last plan entry it finished, so a job that failed on an archive outage, or after 16 failed blocks in a row, doesn't start over. A failed crawl job can't be resumed and returns `400`. So does a failed replay job of specific accounts (created before account backfills always crawled) whose account list is gone (it goes when the job's details are cleared), and a job that another request resumed first. A replay job that failed because its error log is full (10,000 failures) fails again when resumed; start a new backfill instead.

```sh tab="cURL" tab-group="language"
curl -X POST "http://127.0.0.1:3000/admin/backfill/$JOB_ID/resume" -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "running"
}
```

Returns `400` if the job is not currently paused, or `404` if the job ID is not found.

## List backfill jobs

```
GET /admin/backfill/status
```

```ts tab="TypeScript" tab-group="language"
interface BackfillJob {
  id: string;
  collection: string | null;
  did: string | null;
  scope: "network" | "dids";
  source: "auto" | "replay" | "crawl";
  replay: {
    after_seq: number;
    through_seq: number | null;
    tip_seq: number | null;
    bytes: number;
    blocks_failed: number;
  } | null;
  status: string;
  stage: string;
  total_repos: number | null;
  resolved_repos: number | null;
  processed_repos: number | null;
  total_records: number | null;
  error: string | null;
  started_at: string | null;
  completed_at: string | null;
  created_at: string;
  discovery_complete: boolean;
  recent_completions_limit: number | null;
}

const response = await fetch("http://127.0.0.1:3000/admin/backfill/status", {
  headers,
});
const data: BackfillJob[] = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/backfill/status", {
  headers,
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("http://127.0.0.1:3000/admin/backfill/status")
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/backfill/status", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/backfill/status -H "$AUTH"
```

**Response**: `200 OK`

```json
[
  {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "collection": "xyz.statusphere.status",
    "did": null,
    "scope": "network",
    "status": "completed",
    "stage": "completed",
    "total_repos": 42,
    "resolved_repos": 42,
    "processed_repos": 42,
    "total_records": 1000,
    "error": null,
    "started_at": "2025-01-01T00:01:00Z",
    "completed_at": "2025-01-01T00:05:00Z",
    "created_at": "2025-01-01T00:00:00Z",
    "discovery_complete": true,
    "recent_completions_limit": 1000
  }
]
```

A replay job looks like this:

```json
{
  "id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
  "collection": "xyz.statusphere.status",
  "did": null,
  "scope": "network",
  "source": "replay",
  "replay": {
    "after_seq": 0,
    "through_seq": 8412000,
    "tip_seq": 9120000,
    "bytes": 734003200,
    "blocks_failed": 0
  },
  "status": "running",
  "stage": "replaying",
  "total_repos": null,
  "resolved_repos": null,
  "processed_repos": null,
  "total_records": 18250,
  "error": null,
  "started_at": "2025-01-01T00:01:00Z",
  "completed_at": null,
  "created_at": "2025-01-01T00:00:00Z",
  "discovery_complete": true,
  "recent_completions_limit": null
}
```

`source` is the source the job was created with; a job with accounts is always `crawl`. `auto` stays until the job's first request to the archive, then becomes `replay` or, for a Jetstream with no archive, `crawl`. `replay` is `null` for a crawl job. For a replay job, `after_seq` is where it started (`0`, or the last applied seq of a gap replay), `through_seq` and `tip_seq` are the running sweep's position and the tip it is working towards, `bytes` is what it has downloaded from the archive, and `blocks_failed` counts blocks that failed after retries. `through_seq` and `tip_seq` are `null` until the first plan answers.

The `status` field tracks the overall job state (`running`, `pausing`, `paused`, `cancelling`, `cancelled`, `completed`, `failed`). The `stage` field tracks the current processing phase (`pending`, `discovering_repos`, `resolving_and_fetching`, `replaying`, `replaying_accounts`, `retrying_blocks`, `completed`, `failed`, `cancelled`). A replay job has no repo counters: `total_records` counts the records it has written. The `resolved_repos` counter tracks PDS resolution progress during the pipelined phase, while `processed_repos` tracks record fetching progress.

The three repo counters count work units. A unit is one repo under one collection: a network backfill discovers repos per collection, so a repo found under two collections is two units and counts twice. A unit for an account-targeted job covers every collection the job targets, so there one unit is one account. `total_repos` is the number discovered so far while a network backfill is still listing repos from the relay.

`scope` is `network` when repos are discovered through the relay and `dids` when the job targets specific accounts. `did` is set only when a job targets exactly one account.

`discovery_complete` is `false` while a network job is still listing repos from the relay. Discovery runs alongside resolving and fetching, and until it finishes `total_repos` counts the repos discovered so far. `recent_completions_limit` is set for jobs on the bounded queue (every job created after upgrading to the release that introduced it), which keep only that many completed repos for the `fetched` list. It is `null` for jobs created before that upgrade.

A job HappyView paused itself, because it ran out of work with repos it could not complete still queued, has `status` `paused` and the reason in `error` (for example `"paused: 3 units could not be completed; resume to retry"`). A job an operator paused has no `error`. Resuming clears it.

## List errors for a job

```
GET /admin/backfill/{id}/errors
```

Paginated failure detail for a backfill job, with exact totals for each kind. Requires `BackfillRead`.

| Param    | Type   | Required | Description                                          |
| -------- | ------ | -------- | ---------------------------------------------------- |
| `kind`   | string | no       | Only failures of this kind                           |
| `cursor` | string | no       | The `cursor` from the previous page                  |
| `limit`  | number | no       | Page size, 1 to 100 (default 50)                     |

```sh tab="cURL" tab-group="language"
curl "http://127.0.0.1:3000/admin/backfill/$JOB_ID/errors?kind=replay_block" -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "errors": [
    {
      "did": "seg_0000000001.jss",
      "collection": null,
      "phase": "replay_block:17",
      "kind": "replay_block",
      "message": "HTTP 502",
      "attempts": 1,
      "last_at": "2025-01-01T00:03:00Z"
    }
  ],
  "cursor": null,
  "counts": [
    { "kind": "replay_block", "count": 1, "retryable": true }
  ],
  "capped": false,
  "cap": 10000
}
```

For a crawl job, `did` is the account that failed and `phase` is the step (`resolve` or `fetch`). For a replay job, `did` is the archive segment's name, `collection` is `null`, and `phase` is `replay_block:<index>` for one block, or `replay_segment` for a whole segment that failed before its blocks could be listed.

`kind` is one of `dns_failure`, `connection_failed`, `timeout`, `pds_server_error`, `rate_limited`, `did_doc_not_found`, `did_doc_forbidden`, `did_doc_invalid`, `repo_not_found`, `repo_deactivated`, `repo_takendown`, `other` or `replay_block`. `counts` lists each kind that has failures, with its total and whether `retry-failed` retries it. The kinds `dns_failure`, `connection_failed`, `timeout`, `pds_server_error`, `rate_limited` and `replay_block` are retryable.

HappyView keeps detail for the first `cap` failures of a job. When `capped` is `true`, `counts` is still exact but `errors` holds only the first `cap`.

## Retry failed

```
POST /admin/backfill/{id}/retry-failed
```

Starts a new job for the failures of a finished job. Requires `BackfillCreate`.

```sh tab="cURL" tab-group="language"
curl -X POST "http://127.0.0.1:3000/admin/backfill/$JOB_ID/retry-failed" \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{ "kinds": ["replay_block"] }'
```

| Field   | Type     | Required | Description                                                                 |
| ------- | -------- | -------- | ---------------------------------------------------------------------------- |
| `kinds` | string[] | no       | Only retry failures of these kinds. Defaults to every retryable kind        |

What the new job does depends on the source of the job it retries:

- **Crawl:** a `dids` job for the accounts that failed, in the same collection. It starts at the resolve step, so the accounts' PDS lookups run again.
- **Replay:** a replay job that downloads exactly the failed blocks again. It runs in the `retrying_blocks` stage, then, in the `replaying_accounts` stage, applies the account and sync events of every account those blocks wrote records for. For a `dids` job it keeps the original accounts.

**Response**: `201 Created`

```json
{
  "id": "0b8f0f0e-3a3d-4f6b-9d56-6f6fd2d3c1a4"
}
```

Returns `400` when there is nothing to retry: no failures of a retryable kind, or `kinds` names none that the job has. A replay job of specific accounts also answers `400` when its account list is gone, which happens after its details are cleared. Start a new backfill instead. Returns `404` if the job ID is not found.

## List repos for a job

```
GET /admin/backfill/{id}/repos
```

Paginated per-repo detail for a backfill job. Requires `BackfillRead`.

| Param    | Type   | Required | Description                                                                                                       |
| -------- | ------ | -------- | ----------------------------------------------------------------------------------------------------------------- |
| `phase`  | string | no       | `fetched`: recently completed repos. `resolved`: queued repos whose PDS is known. Anything else: every queued repo |
| `cursor` | string | no       | Opaque cursor from a previous response with the same `phase`                                                      |
| `limit`  | number | no       | Max results per page (default 50, max 100)                                                                        |

A running backfill keeps at most `BACKFILL_DISCOVERY_WINDOW` repos queued and deletes each one when it finishes. `fetched` therefore lists the job's most recent completions, newest first, up to `recent_completions_limit` (1,000). The other phases list what is queued now, ordered by collection and then DID; queued repos have `status` `pending`. A repo queued under two collections appears once for each. `resolved` and the other queued phases share one cursor format. A `fetched` cursor sent to a queued phase, a queued-phase cursor sent to `fetched`, or a DID cursor kept from a job created before the bounded queue gets `400`.

Jobs created before the bounded queue (`recent_completions_limit` is `null`) keep a row for every repo and list them all in DID order as before: `fetched` lists completed repos, `resolved` repos whose PDS is known, and anything else every repo. Their cursor is the last DID.

```sh tab="cURL" tab-group="language"
curl "http://127.0.0.1:3000/admin/backfill/$JOB_ID/repos?phase=fetched&limit=10" -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "repos": [
    { "did": "did:plc:abc", "pds_endpoint": "https://pds.example.com", "status": "completed", "records_fetched": 42 }
  ],
  "cursor": "did:plc:def"
}
```

`cursor` is `null` when there are no more results.

## PDS summary for a job

```
GET /admin/backfill/{id}/pds-summary
```

Aggregated PDS breakdown for a backfill job. Requires `BackfillRead`. No pagination — returns all PDS endpoints in one response, sorted by repo count descending. For jobs on the bounded queue the counts are kept per PDS as the job runs, and `total_repos` counts work units resolved to that PDS, like the other counters: a repo queued under two collections counts twice.

```sh tab="cURL" tab-group="language"
curl "http://127.0.0.1:3000/admin/backfill/$JOB_ID/pds-summary" -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "pds_endpoints": [
    { "pds_endpoint": "https://morel.us-east.host.bsky.network", "total_repos": 1200, "completed_repos": 800, "total_records": 5000 }
  ]
}
```

## Stream backfill events (SSE)

```
GET /admin/backfill/{id}/events
```

Server-Sent Events stream of real-time backfill progress. Requires `BackfillRead`. The connection stays open until the job completes or the client disconnects. A keepalive comment is sent periodically to prevent timeouts.

Events are sent with `event: event` and a JSON `data` payload. Each event has a `type` field:

| Event type         | Description                                       |
| ------------------ | ------------------------------------------------- |
| `repo_discovered`  | A new DID was found during the discovery phase    |
| `repo_resolved`    | A DID's PDS endpoint was resolved                 |
| `repo_fetched`     | Record fetching completed for a DID               |
| `job_counters`     | Updated progress counters                         |
| `job_stage_changed`| The job moved to a new processing stage           |
| `job_completed`    | The job finished (completed, failed, or cancelled) |
| `job_snapshot`     | The job's current status, stage, counters, error counts and `discovery_complete`, sent on connect and after the stream falls behind |

## Flush job details

```
DELETE /admin/backfill/{id}/details
```

Delete the per-repo detail of one backfill job: its recent completions, its PDS summary, any repos left queued and, for jobs created before the bounded queue, its repo rows. Requires `BackfillCreate`.

A job on the bounded queue can be flushed only once it is `completed`, `cancelled` or `failed`, since its queue is the work it still has to do. Flushing one that is running, pausing, paused or cancelling returns `409`. A job created before the bounded queue can be flushed in any state, as before.

```sh tab="cURL" tab-group="language"
curl -X DELETE "http://127.0.0.1:3000/admin/backfill/$JOB_ID/details" -H "$AUTH"
```

**Response**: `204 No Content`

## Flush all job details

```
DELETE /admin/backfill/details
```

Delete the per-repo detail and leftover queue of every completed, cancelled and failed backfill job. Requires `BackfillCreate`.

```sh tab="cURL" tab-group="language"
curl -X DELETE "http://127.0.0.1:3000/admin/backfill/details" -H "$AUTH"
```

**Response**: `204 No Content`

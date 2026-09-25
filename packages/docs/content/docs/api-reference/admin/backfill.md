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

Every entry in `dids` must resolve, or the request fails with `400` listing each entry that did not and no job is created. Duplicates are ignored. A request can name at most 500 accounts.

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

Resume a paused backfill job. The job status transitions back to `running` and processing continues from where it left off.

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
    "created_at": "2025-01-01T00:00:00Z"
  }
]
```

The `status` field tracks the overall job state (`running`, `pausing`, `paused`, `cancelling`, `cancelled`, `completed`, `failed`). The `stage` field tracks the current processing phase (`pending`, `discovering_repos`, `resolving_and_fetching`, `completed`, `failed`, `cancelled`). The `resolved_repos` counter tracks PDS resolution progress during the pipelined phase, while `processed_repos` tracks record fetching progress.

`scope` is `network` when repos are discovered through the relay and `dids` when the job targets specific accounts. `did` is set only when a job targets exactly one account.

## List repos for a job

```
GET /admin/backfill/{id}/repos
```

Paginated list of per-DID tracking rows for a backfill job. Requires `BackfillRead`.

| Param    | Type   | Required | Description                                                                        |
| -------- | ------ | -------- | ---------------------------------------------------------------------------------- |
| `phase`  | string | no       | Filter: `discovered` (all), `resolved` (PDS known), `fetched` (completed)          |
| `cursor` | string | no       | Keyset cursor (DID) for pagination                                                 |
| `limit`  | number | no       | Max results per page (default 50, max 100)                                         |

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

Aggregated PDS breakdown for a backfill job. Requires `BackfillRead`. No pagination — returns all PDS endpoints in one response, sorted by repo count descending.

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

## Flush job details

```
DELETE /admin/backfill/{id}/details
```

Delete all per-repo tracking rows for a single backfill job. Requires `BackfillCreate`.

```sh tab="cURL" tab-group="language"
curl -X DELETE "http://127.0.0.1:3000/admin/backfill/$JOB_ID/details" -H "$AUTH"
```

**Response**: `204 No Content`

## Flush all job details

```
DELETE /admin/backfill/details
```

Delete per-repo tracking rows for all completed, cancelled, and failed backfill jobs. Requires `BackfillCreate`.

```sh tab="cURL" tab-group="language"
curl -X DELETE "http://127.0.0.1:3000/admin/backfill/details" -H "$AUTH"
```

**Response**: `204 No Content`

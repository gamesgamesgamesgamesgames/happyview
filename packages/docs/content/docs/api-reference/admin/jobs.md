---
title: "Jobs"
---

Admin API endpoints for managing background jobs. For a conceptual overview, see [Background Jobs](../../guides/background-jobs.md).

## List jobs

```http
GET /admin/jobs
```

Returns a paginated list of jobs, newest first.

**Query parameters:**

| Parameter | Type   | Description                                                                           |
| --------- | ------ | ------------------------------------------------------------------------------------- |
| `status`  | string | Filter by status (`pending`, `running`, `completed`, `failed`, `paused`, `cancelled`) |
| `limit`   | number | Maximum number of results (default: 50)                                               |
| `cursor`  | string | Pagination cursor from a previous response                                            |

**Permission:** `jobs:read`

**Response:**

```json
{
  "jobs": [
    {
      "id": "550e8400-e29b-41d4-a716-446655440000",
      "job_type": "export",
      "status": "completed",
      "input": { "collection": "xyz.statusphere.status" },
      "progress": { "processed": 1500 },
      "result": { "processed": 1500 },
      "error": null,
      "created_by": "did:plc:abc123",
      "inherit_auth": false,
      "started_at": "2026-07-01T12:00:05Z",
      "completed_at": "2026-07-01T12:02:30Z",
      "created_at": "2026-07-01T12:00:00Z"
    }
  ],
  "cursor": "next-page-cursor"
}
```

## Get job

```http
GET /admin/jobs/:id
```

Returns a single job by ID.

**Permission:** `jobs:read`

**Response:** Same shape as a single item in the list response.

## Cancel job

```http
POST /admin/jobs/:id/cancel
```

Request cancellation of a job. If the job is `pending` or `paused`, it's immediately set to `cancelled`. If the job is `running`, it's set to `cancelling` — the worker will stop the job when the script next calls `job.should_stop()`.

**Permission:** `jobs:manage`

**Response:**

```json
{
  "status": "cancelling"
}
```

**Errors:**

| Status | Condition                                      |
| ------ | ---------------------------------------------- |
| 404    | Job not found                                  |
| 409    | Job is already completed, failed, or cancelled |

## Pause job

```http
POST /admin/jobs/:id/pause
```

Request pause of a running job. Sets the status to `pausing` — the worker will pause the job when the script next calls `job.should_stop()`.

**Permission:** `jobs:manage`

**Response:**

```json
{
  "status": "pausing"
}
```

**Errors:**

| Status | Condition          |
| ------ | ------------------ |
| 404    | Job not found      |
| 409    | Job is not running |

## Resume job

```http
POST /admin/jobs/:id/resume
```

Resume a paused job. Sets the status back to `pending` so the worker picks it up again.

**Permission:** `jobs:manage`

**Response:**

```json
{
  "status": "pending"
}
```

**Errors:**

| Status | Condition         |
| ------ | ----------------- |
| 404    | Job not found     |
| 409    | Job is not paused |

## Job object

| Field          | Type         | Description                                                                     |
| -------------- | ------------ | ------------------------------------------------------------------------------- |
| `id`           | string       | UUID                                                                            |
| `job_type`     | string       | The type name passed to `jobs.create()`                                         |
| `status`       | string       | Current status (see [lifecycle](../../guides/background-jobs.md#job-lifecycle)) |
| `input`        | object       | Input data passed to `jobs.create()`                                            |
| `progress`     | object       | Last progress update from `job.progress()`                                      |
| `result`       | object\|null | Return value of the script on completion                                        |
| `error`        | string\|null | Error message on failure                                                        |
| `created_by`   | string       | DID of the user who enqueued the job                                            |
| `inherit_auth` | boolean      | Whether the job inherits the creator's PDS auth                                 |
| `started_at`   | string\|null | ISO 8601 timestamp when the worker started executing                            |
| `completed_at` | string\|null | ISO 8601 timestamp when the job finished                                        |
| `created_at`   | string       | ISO 8601 timestamp when the job was enqueued                                    |

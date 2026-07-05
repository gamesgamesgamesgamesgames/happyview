---
title: "Jobs API"
---

Lua API for creating and managing background jobs. For a conceptual overview, see [Background Jobs](../../guides/background-jobs.md).

## `jobs` table

The `jobs` table is available in **all** script contexts (procedures, queries, record scripts, label scripts, and job scripts). It provides functions for queuing new jobs.

### `jobs.create(job_type, input[, opts])`

Enqueue a new background job.

**Parameters:**

| Parameter  | Type   | Description                                                  |
| ---------- | ------ | ------------------------------------------------------------ |
| `job_type` | string | The job type name. Must match a `job.run:<type>` script trigger. |
| `input`    | table  | Input data passed to the job script via `job.input`.         |
| `opts`     | table? | Optional settings (see below).                               |

**Options:**

| Key    | Type    | Default | Description                                              |
| ------ | ------- | ------- | -------------------------------------------------------- |
| `auth` | boolean | `false` | Inherit the caller's PDS auth. When `true`, the job script can use `r:save()`, `r:delete()`, and blob uploads as the creating user. When `false`, only local operations (`r:save_local()`, `r:delete_local()`) are available. |

**Returns:** `string` — the new job's UUID.

**Requires:** An authenticated caller (`caller_did` must be set). Raises an error in unauthenticated contexts.

```lua
-- Enqueue a job without PDS auth (default)
function handle()
  local job_id = jobs.create("stats.rebuild", {
    collection = collection,
  })
  return { job_id = job_id }
end
```

```lua
-- Enqueue a job that needs to write records on behalf of the caller
function handle()
  local job_id = jobs.create("export", {
    collection = collection,
    format = input.format,
  }, { auth = true })
  return { job_id = job_id }
end
```

Jobs can enqueue other jobs — a job script can call `jobs.create()` to spawn follow-up work:

```lua
-- Inside a job script: fan out to per-collection jobs
function handle()
  local collections = job.input.collections
  local child_ids = {}
  for _, col in ipairs(collections) do
    table.insert(child_ids, jobs.create("export.collection", {
      collection = col,
      parent_job = job.id,
    }))
  end
  return { children = child_ids }
end
```

## `job` table

The `job` table is available **only inside job scripts** (trigger `job.run:<type>`). It is `nil` in all other script contexts.

### `job.id`

**Type:** `string`

The job's UUID.

### `job.input`

**Type:** `table`

The input table that was passed to `jobs.create()` when the job was queued.

### `job.progress(data)`

Persist a progress snapshot to the database.

**Parameters:**

| Parameter | Type  | Description                      |
| --------- | ----- | -------------------------------- |
| `data`    | table | Any Lua table — stored as JSONB. |

Each call overwrites the previous progress value. The snapshot is visible in the job detail panel in the dashboard and via `GET /admin/jobs/:id`.

```lua
job.progress({ phase = "fetching", fetched = 250, total = 1000 })
```

### `job.should_stop()`

Check whether the job has been paused or cancelled.

**Returns:** `boolean` — `true` if the operator requested a pause or cancel.

Pause and cancel are cooperative. The worker sets a flag when the operator requests it, but the script must call `job.should_stop()` and exit gracefully. If your script never checks, pause and cancel requests wait until the script finishes on its own.

```lua
for i, item in ipairs(items) do
  if job.should_stop() then
    return { partial = true, last = i }
  end
  process(item)
end
```

### `job.wait(seconds)`

Sleep for the specified duration.

**Parameters:**

| Parameter | Type   | Description                              |
| --------- | ------ | ---------------------------------------- |
| `seconds` | number | Duration in seconds (clamped to 0–3600). |

Values below 0 are clamped to 0. Values above 3600 are clamped to 3600.

```lua
-- Poll an external API with a delay between requests
for _, batch in ipairs(batches) do
  local resp = http.post("https://api.example.com/import", {
    body = json.encode(batch),
  })
  job.wait(1) -- rate limit
end
```

## Available APIs

Job scripts have access to all standard Lua APIs:

- [`db.*`](database-api.md) — database queries
- [`http.*`](http-api.md) — HTTP client
- [`xrpc.*`](xrpc-lua-api.md) — XRPC calls
- [`atproto.*`](atproto-api.md) — DID resolution, labels, signing
- [`Record.*`](record-api.md) — record operations
- [`json.*`](json-api.md) — JSON encode/decode
- [`jobs.*`](#jobs-table) — queue follow-up jobs
- [`log()`](utility-globals.md), [`now()`](utility-globals.md), [`TID()`](utility-globals.md#tid), [`toarray()`](utility-globals.md) — utility globals
- `env.<KEY>` — [script variables](../admin/script-variables.md)

Unlike XRPC and record scripts, job scripts have **no instruction count limit** — they can run arbitrarily long computations.

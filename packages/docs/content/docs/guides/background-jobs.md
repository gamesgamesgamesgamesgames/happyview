---
title: "Background Jobs"
---

Background jobs let you run long-running Lua scripts outside the request cycle. A script running in any context can queue a job with `jobs.create` from `happyview.jobs`, and a dedicated worker picks it up and executes the matching job script. Jobs are useful for data migrations, batch exports, external API syncs, or any work that's too slow for a synchronous request.

## How it works

1. **Queue** - any Lua script calls `jobs.create("my-type", { ... })`, passing a free-form type name and an input table. This inserts a row into the `happyview_jobs` table with status `pending` and returns the job's UUID.
2. **Match** - the worker resolves the job's type to a script by looking up the trigger `job.run:my-type`. If no script exists for that type, the job fails immediately.
3. **Execute** - the worker calls the script's `handle(input, ctx)` with the job's input as `input` and its controls on `ctx.job`. The script can report progress, check for cancellation, sleep, and return a result.

## Creating a job script

Job scripts are created from the [dashboard](../getting-started/dashboard.md) (Settings > Scripts > New) or via the [admin API](../api-reference/admin/scripts.md).

In the dashboard, select **Job** as the trigger source, then type a job type name. The type name is a free-form string that must match `/^[a-z0-9][a-z0-9._-]*$/` (max 128 characters). The resulting trigger id is `job.run:<type>`; for example, `job.run:export` or `job.run:data.migrate`.

### Trigger grammar

| Trigger          | Fires when                                              |
| ---------------- | ------------------------------------------------------- |
| `job.run:<type>` | A job with the matching type is picked up by the worker |

There is no cascade for job triggers; the type must match exactly.

### Script structure

Job scripts follow the same `handle(input, ctx)` contract as all other scripts. `input` is the table passed to `jobs.create`, and the return value becomes the job's `result` field.

```lua
function handle(input, ctx)
  for i, item in ipairs(input.items) do
    -- process each item
    ctx.job.progress({ processed = i, total = #input.items })

    if ctx.job.should_stop() then
      return { partial = true, processed = i }
    end
  end

  return { processed = #input.items }
end
```

## Controlling a running job

Inside a job script, `ctx.job` carries the job's id and its control functions. It is `nil` in every other script kind.

| Field / Function         | Type     | Description                                                 |
| ------------------------ | -------- | ----------------------------------------------------------- |
| `ctx.job.id`             | string   | The job's UUID                                              |
| `ctx.job.progress(data)` | function | Persist progress to the database (visible in the dashboard) |
| `ctx.job.should_stop()`  | function | Returns `true` if the job has been paused or cancelled      |
| `ctx.job.wait(seconds)`  | function | Sleep for 0–3600 seconds                                    |

### `ctx.job.progress(data)`

Call `ctx.job.progress()` to persist a progress snapshot. The `data` argument can be any Lua table: it's stored as JSONB and displayed in the job detail panel in the dashboard. Call it as often as you like; each call overwrites the previous progress value.

```lua
ctx.job.progress({ status = "indexing", page = 5, total_pages = 20 })
```

### `ctx.job.should_stop()`

Check `ctx.job.should_stop()` at natural checkpoints in your script. It returns `true` when an operator has paused or cancelled the job from the dashboard. Cancellation and pausing are **cooperative**: the worker sets a flag, but it's up to your script to check it and exit gracefully. If your script never checks, pause and cancel requests will wait until the script finishes on its own.

```lua
for i, repo in ipairs(repos) do
  if ctx.job.should_stop() then
    return { partial = true, last_processed = i }
  end
  process(repo)
end
```

### `ctx.job.wait(seconds)`

Pause execution for up to 3600 seconds (1 hour). Useful for rate-limited external API calls or scheduled delays. Values below 0 are clamped to 0; values above 3600 are clamped to 3600.

```lua
for _, batch in ipairs(batches) do
  push_to_api(batch)
  ctx.job.wait(2) -- respect rate limits
end
```

### Logging

`internal.logging`'s `info`, `warn` and `error` write to the job's own log as well as the event log, so the dashboard's job detail shows them alongside progress:

```lua
local log = require("internal.logging")

function handle(input, ctx)
  log.info("starting export", { collection = input.collection })
  -- ...
end
```

## Enqueuing jobs

Any Lua script can enqueue a job through `happyview.jobs`. This works in **all** script contexts: procedures, queries, record scripts, label scripts, and even other job scripts (a job can enqueue follow-up jobs).

```lua
local jobs = require("happyview.jobs")

-- In a procedure script
function handle(input, ctx)
  local job_id = jobs.create("export", {
    collection = ctx.collection,
    format = input.format,
  })
  return { job_id = job_id, status = "queued" }
end
```

`jobs.create(type, input[, opts])` returns the new job's UUID as a string. The `type` argument must match a `job.run:<type>` script trigger. If no matching script exists, the job will fail when the worker picks it up. `input` is any table; `opts.auth` is described below.

Enqueuing needs a caller: `jobs.create` raises `BAD_INPUT` when `ctx.caller_did` is `nil`, since the job runs as whoever enqueued it. Job types starting with `happyview.` are reserved for HappyView's own native jobs and are refused too.

## Authentication

By default, jobs run **without PDS auth**. This is intentional. Most jobs don't need to write records on behalf of a user, and granting auth by default would give long-running background scripts access to a user's PDS session unnecessarily.

### What's available without auth

Every job script, regardless of auth setting, has `ctx.caller_did` set to the DID of the user who enqueued it, `ctx.env` for script variables, and every [built-in module](../api-reference/lua/built-in-modules.md) and [library](../api-reference/lua/libraries.md). Without auth, `ctx.has_pds_auth` is `false` and everything that does not act on the creator's PDS works: reads through `happyview.db`, `happyview.sql` and `happyview.backlinks`; `happyview.http`; `happyview.xrpc`'s `query`; `happyview.atproto`; `happyview.linked_repos`; `happyview.jobs` for follow-up jobs; and `happyview.record`'s `load`, `save_local` and `delete_local`, which touch only HappyView's local index.

### What requires auth

Operations that act on the creator's PDS need their OAuth session. Without auth, these raise `NO_SESSION`:

- `create`, `put`, `delete` and `upload_blob` on `happyview.record`
- `procedure` on `happyview.xrpc`

### Opting into auth

To give a job access to the creator's PDS session, pass `{ auth = true }` as the third argument to `jobs.create`:

```lua
local jobs = require("happyview.jobs")

-- Without auth (default) - local-only operations
jobs.create("stats.rebuild", { collection = ctx.collection })

-- With auth - can write to the creator's PDS
jobs.create("sync-records", { collection = ctx.collection }, { auth = true })
```

`{ auth = true }` requires the enqueuing script to hold a DPoP session itself, so it works from a procedure called by an authenticated client and raises `NO_SESSION` from a runner that has none. When the job runs, the worker loads the creator's OAuth session. If the session is no longer valid (expired, revoked, or the user has no session), the job fails immediately with an error. The creating user must have a valid OAuth session when the job runs, not just when it was enqueued.

### When to use auth

Use `{ auth = true }` when the job needs to create, update, or delete records on the atproto network on behalf of the user. For example, batch record creation, cross-collection syncs, or migrations that write back to the user's PDS.

Leave auth off (the default) for jobs that only read data, compute aggregates, sync to external services, clean up local records, or perform any work that doesn't touch a user's PDS.

## Job lifecycle

Jobs move through these statuses:

| Status       | Description                                       |
| ------------ | ------------------------------------------------- |
| `pending`    | Queued, waiting for the worker to pick it up      |
| `running`    | Currently executing                               |
| `completed`  | Script returned successfully                      |
| `failed`     | Script raised an error                            |
| `pausing`    | Pause requested, waiting for the script to check  |
| `paused`     | Script exited after detecting the pause flag      |
| `cancelling` | Cancel requested, waiting for the script to check |
| `cancelled`  | Script exited after detecting the cancel flag     |

### Pausing and cancelling

Pause and cancel are requested via the dashboard or the [admin API](../api-reference/admin/jobs.md). Both are cooperative:

1. The endpoint sets the job's status to `pausing` or `cancelling`.
2. The worker continues running the script. At its next `ctx.job.should_stop()` check, it returns `true`.
3. The script should exit gracefully. Whatever it returns becomes the job's result.
4. The worker sets the final status to `paused` or `cancelled`.

If the script never calls `ctx.job.should_stop()`, the pause or cancel request waits until the script finishes naturally.

A paused job can be resumed via `POST /admin/jobs/:id/resume` or the Resume button in the dashboard. Resuming sets the status back to `pending`, and the worker picks it up again, but the script runs from the beginning. Use `input` or progress data to implement resumable logic.

### Recovery after restart

Jobs survive server restarts. On startup, the worker checks for orphaned jobs:

- **Running** jobs are reset to `pending` and re-queued.
- **Cancelling** jobs are finalised as `cancelled`.
- **Pausing** jobs are finalised as `paused`.

## Worker

The job worker runs as a background task inside the HappyView server process. It polls for pending jobs every 5 seconds and executes one job at a time. Job scripts have **no instruction count or wall-clock limit** (unlike XRPC, record and label scripts, which are capped by the instruction limit, and XRPC scripts by the wall clock, both set in Settings → General), so they can run arbitrarily long computations.

## Dashboard

The **Jobs** page in the dashboard (`/dashboard/jobs`) shows all background jobs in a filterable table. You can filter by status using the dropdown at the top.

Clicking a job row opens a detail sheet showing:

- Job ID, type, and status
- Input data (the table passed to `jobs.create`)
- Progress (the last value passed to `ctx.job.progress()`)
- Result or error
- Timestamps (created, started, completed)
- Action buttons: **Cancel**, **Pause**, or **Resume** depending on the current status

## Permissions

Job management requires specific permissions:

| Permission    | Grants                             |
| ------------- | ---------------------------------- |
| `jobs:read`   | View jobs in the dashboard and API |
| `jobs:manage` | Cancel, pause, and resume jobs     |

## Next steps

- [Admin API - Jobs](../api-reference/admin/jobs.md): Full reference for job endpoints
- [Script Contract](../api-reference/lua/script-contract.md): `input` and `ctx` for a job script
- [Lua Scripting](lua-scripting.md): General Lua scripting reference
- [Record & Label Scripts](record-scripts.md): Trigger grammar for all script types

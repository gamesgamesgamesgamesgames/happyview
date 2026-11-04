---
title: "Metrics"
---

Admin API endpoints for script execution metrics: how often each script runs, how long it takes, how it ends, and where its time goes.

Metrics are kept in memory by the process that runs the scripts, for the last hour by the minute and the last day by the half hour. Nothing is written to the database per run. Instead, once a minute, each minute that has closed is written to the `happyview_script_metrics` table, one row for each script, and for each collection and action, that saw runs in it. Idle scripts write nothing. Test files run by `happyview test` are not recorded.

The `1h` and `24h` windows are read from memory, and the `7d` and `30d` windows from the table. At startup the last day's rows are read back into memory, so a restart loses at most the minute that was open.

## Several instances on one database

Each row records the instance that wrote it. An instance names itself with `HAPPYVIEW_INSTANCE_ID`, or uses its hostname when that is unset, or a random id for that boot when it has no hostname either. Instances sharing a database each write their own rows, fold only their own minutes into hours, and at startup read only their own rows back.

That makes the two kinds of window answer different questions:

- `1h` and `24h` are the memory of the instance that answered the request, so they cover its runs only. Behind a load balancer, two requests can return different numbers.
- `7d` and `30d` add up every instance's rows, plus the minutes the answering instance hasn't written yet. Their responses carry `instances`, the number of instances whose runs the window counts.

Give each replica a stable, distinct `HAPPYVIEW_INSTANCE_ID`. A hostname that changes on every deploy, like a container's, or a random id, works for counting, but the replica can't read its own history back after a restart: its last hour and day start empty. Rows written under an id that never comes back are never folded into hours. Retention deletes them like any other row.

## Retention

Stored metrics follow the event log's retention by design: they are kept for `event_log_retention_days` (the `EVENT_LOG_RETENTION_DAYS` environment variable when no setting is stored, default `30`), and `0` keeps them indefinitely. Once an hour, rows older than that are deleted, and minute rows more than two days old are folded into hour rows. A folded hour answers the same counts and percentiles its minutes would have, so a stored window is read by the minute where it is newer than two days and by the hour before that.

A window longer than retention is answered with what is still stored. Its response carries `retention_days`, the retention in force, so a `30d` window on an instance that keeps 7 days reads as 7 days of data, not as a quiet month.

Recording is on by default. Turn it off with the `script_metrics_enabled` [instance setting](./settings.md) (`"false"`), or the `SCRIPT_METRICS_ENABLED` environment variable when no setting is stored. While it is off nothing is recorded or written to the table, what is already held stays readable until it leaves its window, and the active-runs gauge in [`/admin/metrics/live`](#live) keeps counting. Retention and folding still run over what is stored.

## What a run measures

| Field | Meaning |
| ----- | ------- |
| `wall_ms` | The whole run: building the script's input, instantiating the interpreter, running the script, and settling the library calls it left running |
| `guest_ms` | Time inside the script's own code: the call into the interpreter less the time spent in host imports |
| `prepare_ms_mean` | Building the script's input, including reading script variables, and attaching its prepared artifact |
| `instantiate_ms_mean` | Instantiating the interpreter or compiled module: `link_ms_mean` plus `allocate_ms_mean` |
| `link_ms_mean` | Building the linker the instance's imports are resolved through |
| `allocate_ms_mean` | The rest of instantiation: finding the module, building the store, the instance with its memory and stack, and running the module's initializer |
| `teardown_ms_mean` | Dropping the store and instance once the run is over |
| `host_ms_mean` | Time the script spent waiting on the host: database reads, library calls, HTTP requests |
| `settle_ms_mean` | Waiting for library calls the script started and never awaited |
| `host_imports` | The host imports the script called, each with its call count and mean time per call. A library call is counted once, under the import that made it |
| `memory_peak_bytes` | The interpreter's linear memory at the end of the run, which is its high-water mark |
| `instructions_mean` | Instructions per run, over the runs that reported a count. A compiled script module always does; an interpreted script does when its interpreter counts them (happyview-lua 1.0.0-alpha.8 and later). `null` when no run in the window reported one |
| `module_cache` | How often the interpreter's compiled module came from memory (`hits`) rather than disk or a compile (`misses`) |

Every mean is over the runs that reached that stage, so a run that found no interpreter does not pull the instantiation mean down. Durations are in milliseconds. Percentiles are accurate to about 1%; `max` is exact.

A run ends in one of these outcomes:

| Outcome | Meaning |
| ------- | ------- |
| `ok` | The script returned |
| `syntax`, `runtime`, `missing_handle` | The script failed, as its interpreter reported |
| `timeout` | The run exceeded its instruction limit or wall clock |
| `memory` | The run exceeded its memory limit |
| `trap` | The interpreter stopped without reporting why |
| `no_interpreter` | Nothing installed runs the script's language |
| `error` | The host could not run the script for another reason |

## List scripts

```http
GET /admin/metrics/scripts
```

One row per script that ran inside the window, busiest first. A script is keyed by its trigger, its kind and its layer (`project` for a project file, `dashboard` for a script saved in the dashboard), so one trigger served by both layers in the window has two rows.

**Query parameters:**

| Parameter | Type   | Description |
| --------- | ------ | ----------- |
| `window`  | string | `1h` (default), `24h`, `7d` or `30d`. Anything else is `400` |

**Permission:** `metrics:read`

**Response:**

```json
{
  "window": "1h",
  "enabled": true,
  "overflow": 0,
  "scripts": [
    {
      "trigger": "xrpc.query:games.gamesgamesgamesgames.listGames",
      "kind": "xrpc_query",
      "layer": "dashboard",
      "runs": 1,
      "outcomes": { "ok": 1 },
      "wall_ms": { "p50": 164.064, "p95": 164.064, "p99": 164.064, "max": 164.064, "mean": 164.064 },
      "guest_ms": { "p50": 0.332, "p95": 0.332, "p99": 0.332, "max": 0.332, "mean": 0.332 },
      "prepare_ms_mean": 162.625,
      "instantiate_ms_mean": 0.551,
      "link_ms_mean": 0.402,
      "allocate_ms_mean": 0.149,
      "teardown_ms_mean": 0.063,
      "host_ms_mean": 0.465,
      "settle_ms_mean": 0.002,
      "host_imports": [
        { "name": "host_script_log", "calls": 1, "ms_mean": 0.465 }
      ],
      "memory_peak_bytes": { "max": 1638400, "mean": 1638400.0 },
      "instructions_mean": null,
      "module_cache": { "hits": 1, "misses": 0 }
    }
  ]
}
```

`overflow` counts scripts that were not recorded because 1,024 were already held. Scripts with nothing left inside the last day make room for new ones.

A `7d` or `30d` response has the same shape plus `retention_days` ([Retention](#retention)) and `instances` ([Several instances](#several-instances-on-one-database)). Its rows add every instance's stored minutes and hours to the minutes this instance's memory has not yet written.

## Get one script

```http
GET /admin/metrics/scripts/:trigger
```

One script's totals for the window and a series of points across it, merged over the layers it ran from. The trigger contains `:`; encode it as `%3A` if your client does not send it as is.

**Query parameters:**

| Parameter | Type   | Description |
| --------- | ------ | ----------- |
| `window`  | string | `1h` (default), `24h`, `7d` or `30d` |
| `step`    | string | `1m` (default for `1h`) or `30m` (default for `24h`) with a window read from memory; `1h` (default for `7d`) or `1d` (default for `30d`) with a stored window. The last day is kept by the half hour, so `window=24h&step=1m` is `400`, and a step from the other pair is `400` too |

**Permission:** `metrics:read`

**Response:** the fields of a row in the list, plus `layers`, `window`, `step`, `enabled` and `points`, and `retention_days` and `instances` for a stored window. There is one point per step across the window, oldest first (60 for `1h` by the minute, 48 for `24h` by the half hour, 168 for `7d` by the hour, 30 for `30d` by the day, and so on); the newest is the step still open, and days are UTC days. A step with no runs has `runs: 0` and `null` latencies. The example omits the fields a list row also has, and shows the first and last of its 60 points.

```json
{
  "trigger": "xrpc.query:games.gamesgamesgamesgames.listGames",
  "kind": "xrpc_query",
  "layers": ["dashboard"],
  "window": "1h",
  "step": "1m",
  "enabled": true,
  "runs": 2,
  "outcomes": { "ok": 1, "runtime": 1 },
  "points": [
    {
      "start": "2026-10-30T20:30:00+00:00",
      "runs": 0,
      "outcomes": {},
      "wall_ms": { "p50": null, "p95": null, "max": null },
      "guest_ms": { "p50": null, "p95": null, "max": null },
      "host_ms_mean": null,
      "instantiate_ms_mean": null,
      "link_ms_mean": null,
      "allocate_ms_mean": null,
      "teardown_ms_mean": null
    },
    {
      "start": "2026-10-30T21:29:00+00:00",
      "runs": 2,
      "outcomes": { "ok": 1, "runtime": 1 },
      "wall_ms": { "p50": 1.555, "p95": 1.555, "max": 164.064 },
      "guest_ms": { "p50": 0.087, "p95": 0.087, "max": 0.332 },
      "host_ms_mean": 0.2325,
      "instantiate_ms_mean": 0.591,
      "link_ms_mean": 0.433,
      "allocate_ms_mean": 0.1575,
      "teardown_ms_mean": 0.0495
    }
  ]
}
```

**Errors:**

| Status | Condition |
| ------ | --------- |
| 400    | An unknown `window` or `step`, `step=1m` with `window=24h`, or a step that does not go with the window |
| 404    | The script has not run inside the window |

## Ingest

```http
GET /admin/metrics/ingest
```

The record pipeline: what each record of a collection costs on its way through ingest, around and inside its hook, so you can tell whether a slow backfill is the hook or the platform. One row per collection and action that saw records inside the window, busiest first. Every source is counted: the live stream, backfills, replays and dead-letter retries.

**Query parameters:**

| Parameter | Type   | Description |
| --------- | ------ | ----------- |
| `window`  | string | `1h` (default), `24h`, `7d` or `30d`. A stored window adds `retention_days` and `instances` |

**Permission:** `metrics:read`

A record's stages:

| Stage | Meaning |
| ----- | ------- |
| `resolve` | Looking up the record's hook. A batch looks each collection's hook up once, so this stays near zero |
| `cid_check` | Checking that the body hashes to its claimed CID |
| `hook` | Running the hook, its retries and their backoff included. Only for records a hook ran for |
| `hook.link`, `hook.allocate`, `hook.teardown` | Parts of `hook`, not more of it: building and dropping the hook runs' instances, as in [What a run measures](#what-a-run-measures) |
| `write` | Writing the record to the instance database. A batch writes in one transaction, shared evenly over its records |
| `delta` | For a collection an external storage holds, its `apply_delta` as ingest waited on it, no-ops included. A record source that can tell a skipped write from a real one reports its own as `write` and `write_skipped` on its jobs' [hook stats](./backfill.md#hook-stats) |

`record_ms` is a record's own time: its stages up to its decision plus its share of the write, not the time it waited for the rest of its batch. `script_ms_mean` is the hook's own time (its code and the host calls it made), over the records a hook ran for. `script_share` is script time over record time across every record, and `platform_share` is the rest: the lookup, the CID check, the write, and in the hook everything that is not the script's own code. `records_per_s` is over the part of the window that saw records. `outcomes` counts what was decided (`proceed`, `replace`, `skip`, `cid_mismatch`); `failed`, `timeouts` and `dead_lettered` count records whose hook failed at least once, timed out at least once, and was given up on.

`scripts_table_reads` counts, since the process started, how often a record hook's lookup read the scripts table rather than answering from the project or its batch's earlier lookup.

**Response:**

```json
{
  "window": "1h",
  "enabled": true,
  "overflow": 0,
  "scripts_table_reads": 12,
  "collections": [
    {
      "collection": "app.test.metrics.claimed",
      "action": "create",
      "records": 2,
      "records_per_s": 0.048,
      "record_ms": { "p50": 1.722, "p95": 1.722, "p99": 1.722, "max": 2.333, "mean": 2.0275 },
      "stages": {
        "resolve": { "n": 2, "ms_mean": 0.2155 },
        "cid_check": { "n": 2, "ms_mean": 0.0095 },
        "hook": { "n": 2, "ms_mean": 1.7955 },
        "hook.link": { "n": 2, "ms_mean": 0.393 },
        "hook.allocate": { "n": 2, "ms_mean": 0.1135 },
        "hook.teardown": { "n": 2, "ms_mean": 0.026 },
        "delta": { "n": 2, "ms_mean": 0.001 }
      },
      "outcomes": { "skip": 2 },
      "failed": 0,
      "timeouts": 0,
      "dead_lettered": 0,
      "script_ms_mean": 0.0715,
      "script_share": 0.035,
      "platform_share": 0.965
    }
  ]
}
```

## Live

```http
GET /admin/metrics/live
```

What is running now: script runs in progress by kind, background jobs waiting and running, and the plugin runtime's module cache since the process started. Cheap enough to poll every few seconds.

**Permission:** `metrics:read`

**Response:**

```json
{
  "enabled": true,
  "active_runs": {
    "xrpc_query": 0,
    "xrpc_procedure": 0,
    "record_event": 0,
    "label": 0,
    "job": 0
  },
  "jobs": { "pending": 0, "running": 0 },
  "backfill_jobs": [
    { "job_id": "883232fb-1817-47bf-ba4d-91dcd8b0657f", "idle_s": 0, "hooks": { "done": 1200000, "rate_per_s": 410.2 } }
  ],
  "module_cache": { "hits": 3, "misses": 1, "compiles": 1 }
}
```

`backfill_jobs` lists every backfill job this process holds [hook stats](./backfill.md#hook-stats) for that has not finished, most recently active first, with `idle_s` the seconds since its last record or report. A paused job stays listed until an hour after its last record. The example's `hooks` is cut short.

`module_cache` counts every module lookup the runtime made, libraries included. `compiles` is how many modules it has compiled; a miss that loaded a module from disk does not add to it.

## The table

`happyview_script_metrics` holds one row for each key and bucket:

| Column | Meaning |
| ------ | ------- |
| `kind` | `script` or `ingest` |
| `key` | A script's trigger, or a collection |
| `variant` | A script's kind and layer, as `record_event/dashboard`, or a collection's action |
| `resolution` | `minute`, or `hour` once folded |
| `bucket_start` | When the bucket starts, RFC 3339 in UTC |
| `counters` | The bucket's counts and sums, as JSON |
| `sketches` | The bucket's latency sketches, as JSON |

At worst, 50 scripts busy every minute write about 72,000 minute rows a day, kept for two days, and 1,200 hour rows a day after that. Most scripts are idle most minutes, so real use is far lower.


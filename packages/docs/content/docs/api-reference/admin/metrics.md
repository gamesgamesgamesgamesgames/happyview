---
title: "Metrics"
---

Admin API endpoints for script execution metrics: how often each script runs, how long it takes, how it ends, and where its time goes.

Metrics are kept in memory by the process that runs the scripts, for the last hour by the minute and the last day by the half hour. Nothing is written to the database per run, and a restart starts the windows over. Test files run by `happyview test` are not recorded.

Recording is on by default. Turn it off with the `script_metrics_enabled` [instance setting](./settings.md) (`"false"`), or the `SCRIPT_METRICS_ENABLED` environment variable when no setting is stored. While it is off nothing is recorded, what is already held stays readable until it leaves its window, and the active-runs gauge in [`/admin/metrics/live`](#live) keeps counting.

## What a run measures

| Field | Meaning |
| ----- | ------- |
| `wall_ms` | The whole run: building the script's input, instantiating the interpreter, running the script, and settling the library calls it left running |
| `guest_ms` | Time inside the script's own code: the call into the interpreter less the time spent in host imports |
| `prepare_ms_mean` | Building the script's input, including reading script variables, and attaching its prepared artifact |
| `instantiate_ms_mean` | Instantiating the interpreter or compiled module |
| `host_ms_mean` | Time the script spent waiting on the host: database reads, library calls, HTTP requests |
| `settle_ms_mean` | Waiting for library calls the script started and never awaited |
| `host_imports` | The host imports the script called, each with its call count and mean time per call. A library call is counted once, under the import that made it |
| `memory_peak_bytes` | The interpreter's linear memory at the end of the run, which is its high-water mark |
| `fuel_mean` | Fuel spent per run, for compiled script modules only. `null` for an interpreted script: interpreters run with unbounded fuel |
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
| `window`  | string | `1h` (default) or `24h`. Anything else is `400`: only these two are kept |

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
      "host_ms_mean": 0.465,
      "settle_ms_mean": 0.002,
      "host_imports": [
        { "name": "host_script_log", "calls": 1, "ms_mean": 0.465 }
      ],
      "memory_peak_bytes": { "max": 1638400, "mean": 1638400.0 },
      "fuel_mean": null,
      "module_cache": { "hits": 1, "misses": 0 }
    }
  ]
}
```

`overflow` counts scripts that were not recorded because 1,024 were already held. Scripts with nothing left inside the last day make room for new ones.

## Get one script

```http
GET /admin/metrics/scripts/:trigger
```

One script's totals for the window and a series of points across it, merged over the layers it ran from. The trigger contains `:`; encode it as `%3A` if your client does not send it as is.

**Query parameters:**

| Parameter | Type   | Description |
| --------- | ------ | ----------- |
| `window`  | string | `1h` (default) or `24h` |
| `step`    | string | `1m` (default for `1h`) or `30m` (default for `24h`). The last day is kept by the half hour, so `window=24h&step=1m` is `400` |

**Permission:** `metrics:read`

**Response:** the fields of a row in the list, plus `layers`, `window`, `step`, `enabled` and `points`. There is one point per step across the window, oldest first (60 for `1h` by the minute, 48 for `24h` by the half hour); a step with no runs has `runs: 0` and `null` latencies. The example omits the fields a list row also has, and shows the first and last of its 60 points.

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
      "host_ms_mean": null
    },
    {
      "start": "2026-10-30T21:29:00+00:00",
      "runs": 2,
      "outcomes": { "ok": 1, "runtime": 1 },
      "wall_ms": { "p50": 1.555, "p95": 1.555, "max": 164.064 },
      "guest_ms": { "p50": 0.087, "p95": 0.087, "max": 0.332 },
      "host_ms_mean": 0.2325
    }
  ]
}
```

**Errors:**

| Status | Condition |
| ------ | --------- |
| 400    | An unknown `window` or `step`, or `step=1m` with `window=24h` |
| 404    | The script has not run inside the window |

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
  "module_cache": { "hits": 3, "misses": 1, "compiles": 1 }
}
```

`module_cache` counts every module lookup the runtime made, libraries included. `compiles` is how many modules it has compiled; a miss that loaded a module from disk does not add to it.

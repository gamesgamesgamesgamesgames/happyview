---
title: "Lua Scripting"
---

Without Lua scripts, HappyView's query endpoints return raw records and procedure endpoints proxy simple creates and updates. To attach a script to an XRPC endpoint, create a script with trigger `xrpc.query:<nsid>` or `xrpc.procedure:<nsid>` — see [trigger grammar](record-scripts.md#trigger-grammar). Lua scripts let you go much further:

- Add filtering logic
- Transform responses
- Validate input
- Compose multi-record operations
- Build entirely custom behavior

Scripts run in a sandboxed Lua VM. They receive the request through [`handle(input, ctx)`](../api-reference/lua/script-contract.md) and reach everything else through `require`: the [built-in modules](../api-reference/lua/built-in-modules.md) for logging, time, TIDs and JSON, and the installed [libraries](../api-reference/lua/libraries.md) for records, the database, HTTP, XRPC and the rest.

For scripts that react to record changes or label events rather than XRPC requests, see [Record & Label Scripts](record-scripts.md).

## Script structure

Every script defines a `handle(input, ctx)` function. HappyView calls it when the trigger fires and, for XRPC endpoints, returns its result as JSON to the client.

```lua
local db = require("happyview.db")

function handle(input, ctx)
  return db.records(ctx.collection):limit(20):run()
end
```

Requires, helper functions and constants sit at file scope. They're evaluated once when the script loads; `handle` runs per request. The [Script Contract](../api-reference/lua/script-contract.md) lists what `input` and `ctx` carry for each script kind and the order requires go in.

## Sandbox

Scripts run in a restricted environment. The following standard Lua modules are **removed** and unavailable:

`io`, `debug`, `package`, `dofile`, `loadfile`, `load`, `collectgarbage`

Lua's own `require` is removed too, but HappyView installs its own in its place: it loads installed library plugins and the `internal.*` built-ins.

The `os` module is replaced with a safe subset exposing only `os.time`, `os.date`, `os.difftime`, and `os.clock`. Dangerous functions like `os.execute`, `os.remove`, `os.rename`, and `os.exit` are not available.

An instruction limit of 1,000,000 prevents infinite loops. Exceeding it terminates the script with an error.

See the [Standard Libraries](../api-reference/lua/standard-libraries.md) reference for the full list of available Lua modules and builtins.

## Reading records

`happyview.db` reads the index. A chain names the collection, narrows it, and `run()` answers a page of envelopes:

```lua
local db = require("happyview.db")

function handle(input, ctx)
  local page = db.records(ctx.collection)
    :where("status", "=", input.status)
    :did(input.did)
    :limit(tonumber(input.limit))
    :cursor(input.cursor)
    :run()

  for _, row in ipairs(page.records) do
    -- row.uri, row.did, row.cid; the body is row.record
  end

  return page
end
```

Each row is an envelope, `{ uri, did, collection, rkey, cid, indexed_at, record }`, and the page is `{ records, cursor }`. A step given `nil` is skipped, which is why the chain above can pass `input.did` and `input.cursor` straight through. `db.get(uri)` answers one envelope or `nil`.

## Writing records

`happyview.record` writes as the caller, to their PDS, and mirrors the write into the index:

```lua
local time = require("internal.time")
local record = require("happyview.record")

function handle(input, ctx)
  return record.create(ctx.collection, {
    text = input.text,
    createdAt = time.to_iso8601(time.now()),
  })
end
```

`create` and `put` answer `{ uri, cid }`. They need a caller with a PDS session, which a procedure has and a query does not; `ctx.has_pds_auth` says whether the current invocation does.

## Calling out

`happyview.http` makes outbound requests and `happyview.xrpc` calls XRPC methods, local or remote, as the caller:

```lua
local json = require("internal.json")
local http = require("happyview.http")
local xrpc = require("happyview.xrpc")

function handle(input, ctx)
  local resp = http.get("https://api.example.com/data")
  local data = json.decode(resp.body)

  local statuses = xrpc.query("xyz.statusphere.listStatuses", { limit = 5 })
  return { external = data, statuses = statuses }
end
```

An HTTP response is `{ status, body, headers }`. An XRPC call answers the decoded result directly and raises on failure.

## Background work

Anything too slow for a request goes to a job. `happyview.jobs` enqueues one from any script kind:

```lua
local jobs = require("happyview.jobs")

function handle(input, ctx)
  local job_id = jobs.create("export", { collection = ctx.collection })
  return { job_id = job_id }
end
```

See [Background Jobs](background-jobs.md) for the job script side.

## Debugging

### Logging

Use `internal.logging` to trace script execution. Every level writes to the server log at **debug** level with the field `lua_log`; `info`, `warn` and `error` are also recorded as `script.log` events in the [event logs](../api-reference/admin/events.md), readable with `GET /admin/events`:

```lua
local log = require("internal.logging")
local db = require("happyview.db")

function handle(input, ctx)
  log.debug("handle called", { limit = input.limit })
  local page = db.records(ctx.collection):limit(tonumber(input.limit)):run()
  log.info("query returned records", { count = #page.records })
  return page
end
```

To see debug output in stdout, make sure your `RUST_LOG` environment variable includes debug level for HappyView (the default `happyview=debug` works). See [Configuration](../getting-started/configuration.md).

### Error messages

When a script fails, the client receives the Lua error message, its type and the line it came from, as described under [Errors](../api-reference/lua/script-contract.md#errors) in the Script Contract. The same message is logged server-side at error level with its stack trace.

### Common mistakes

- **Missing `handle` function**: Every script must define `handle(input, ctx)` at file scope. If it's missing or misspelled, every call fails with `errorType: missing_handle`.
- **Reading a v2 global**: `db`, `params`, `caller_did`, `now()` and the other v2 globals raise an error naming the global. Read the value from `input` or `ctx`, or `require` the module — [Migrating Scripts](migrating-scripts.md) has the mapping and a codemod.
- **Calling `error()` for expected conditions**: Lua's `error()` triggers a 500 response carrying the message. For expected conditions like "record not found", return a structured error response instead: `return { error = "not found" }`.
- **Infinite loops**: The sandbox enforces a 1,000,000 instruction limit. If your script processes large data sets, paginate with `:limit()` and `:cursor()` instead of loading everything at once.

## Example scripts

See the example script references for complete, ready-to-use scripts:

**Queries:**

- [Get a record](../reference/script-examples/get-record.md) — fetch a single record by AT URI
- [Paginated list](../reference/script-examples/paginated-list.md) — list records with cursor-based pagination and DID filtering
- [List or fetch](../reference/script-examples/list-or-fetch.md) — combined single-record lookup and paginated listing
- [Expanded query](../reference/script-examples/expanded-query.md) — list statuses with user profiles in a single response
- [Verify signed record](../reference/script-examples/signed-record-verify.md) — fetch a record and verify its attestation signature

**Procedures:**

- [Create a record](../reference/script-examples/create-record.md) — simple write that saves input as a record
- [Upsert a record](../reference/script-examples/upsert-record.md) — create or update using a deterministic rkey
- [Update or delete](../reference/script-examples/update-or-delete.md) — single endpoint handling create, update, and delete
- [Batch save](../reference/script-examples/batch-save.md) — create several records from one request
- [Sidecar records](../reference/script-examples/sidecar-records.md) — create linked records across collections with a shared rkey
- [Cascading delete](../reference/script-examples/cascading-delete.md) — delete a record and all related records
- [Complex mutations](../reference/script-examples/complex-mutations.md) — load, transform, and save a record with multiple field changes
- [Signed record](../reference/script-examples/signed-record.md) — save a record with an attestation signature

**Record & Label Scripts:**

- [Algolia sync](../reference/script-examples/algolia-sync.md) — push records to an Algolia search index on create/update/delete

## Next steps

- [Script Contract](../api-reference/lua/script-contract.md): `input` and `ctx` for every script kind
- [Libraries](../api-reference/lua/libraries.md): every `happyview.*` module and where its surface is documented
- [Record & Label Scripts](record-scripts.md): React to record changes and label events in real time
- [Lexicons](lexicons.md): Understand how record, query, and procedure lexicons work together
- [Admin API — Scripts](../api-reference/admin/scripts.md): Manage scripts via the API
- [XRPC API](../api-reference/xrpc-api.md): See how endpoints behave with and without Lua scripts
- [Dashboard](../getting-started/dashboard.md#lua-editor): Use the web editor with context-aware completions

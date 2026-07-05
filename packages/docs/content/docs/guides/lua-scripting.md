---
title: "Lua Scripting"
---

Without Lua scripts, HappyView's query endpoints return raw records and procedure endpoints proxy simple creates and updates. To attach a script to an XRPC endpoint, create a script with trigger `xrpc.query:<nsid>` or `xrpc.procedure:<nsid>` — see [trigger grammar](label-scripts#trigger-grammar). Lua scripts let you go much further:

- Add filtering logic
- Transform responses
- Validate input
- Compose multi-record operations
- Build entirely custom behavior

Scripts run in a sandboxed Lua VM with access to the [Record API](#record-api), a [database API](#database-api), an [HTTP client API](#http-api), a [JSON API](#json-api), and a set of [context globals](#context-globals).

For scripts that react to record changes or label events (rather than XRPC requests), see [Record & Label Scripts](label-scripts).

## Script structure

Every script must define a `handle()` function. HappyView calls it when the XRPC endpoint is hit and returns its result as JSON to the client.

```lua
function handle()
  -- your logic here
  return { key = "value" }
end
```

You can define helper functions and variables outside `handle()`. They're evaluated once when the script loads, then `handle()` is called per request.

## Sandbox

Scripts run in a restricted environment. The following standard Lua modules are **removed** and unavailable:

`io`, `debug`, `package`, `require`, `dofile`, `loadfile`, `load`, `collectgarbage`

The `os` module is replaced with a safe subset exposing only `os.time`, `os.date`, `os.difftime`, and `os.clock`. Dangerous functions like `os.execute`, `os.remove`, `os.rename`, and `os.exit` are not available.

An instruction limit of 1,000,000 prevents infinite loops. Exceeding it terminates the script with an error.

See the [Standard Libraries](../api-reference/lua/standard-libraries.md) reference for the full list of available Lua modules and builtins.

## Context globals

These globals are set automatically before `handle()` is called.

### Procedure globals

| Global         | Type    | Description                                             |
| -------------- | ------- | ------------------------------------------------------- |
| `method`       | string  | The XRPC method name (e.g. `xyz.statusphere.setStatus`) |
| `input`        | table   | Parsed JSON request body                                |
| `params`       | table   | Query string parameters                                 |
| `caller_did`   | string  | DID of the authenticated user                           |
| `collection`   | string  | Target collection NSID                                  |
| `delegate_did` | string? | DID of the delegated account, if using write delegation |
| `env`          | table   | Script variables configured in the dashboard            |

### Query globals

| Global       | Type    | Description                                            |
| ------------ | ------- | ------------------------------------------------------ |
| `method`     | string  | The XRPC method name                                   |
| `params`     | table   | Query string parameters (all values are strings)       |
| `collection` | string  | Target collection NSID                                 |
| `caller_did` | string? | DID of the authenticated user (nil if unauthenticated) |
| `env`        | table   | Script variables configured in the dashboard           |

### Space globals

When a script handles a space-scoped request, the `space` global is set to a table with the space's metadata. For non-space requests, `space` is `nil`.

| Field           | Type   | Description                 |
| --------------- | ------ | --------------------------- |
| `space`         | string | The full `ats://` space URI |
| `space_id`      | string | Internal space identifier   |
| `did`           | string | The space's DID             |
| `authority_did` | string | The space authority's DID   |
| `type_nsid`     | string | Space type NSID             |
| `skey`          | string | Space key                   |

```lua
function handle()
  if space then
    log("handling request for space: " .. space.space)
    log("space type: " .. space.type_nsid)
  end
end
```

## Utility globals

Available in both queries and procedures:

| Function         | Returns | Description                                                                                                                                                                       |
| ---------------- | ------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `now()`          | string  | Current UTC timestamp in ISO 8601 format                                                                                                                                          |
| `log(message)`   | —       | Log a message (appears in server logs at debug level)                                                                                                                             |
| `TID()`          | string  | Generate a fresh atproto TID (13-character sortable identifier). Also provides conversion methods — see [Utility Globals reference](../api-reference/lua/utility-globals.md#tid). |
| `toarray(table)` | table   | Mark a table as a JSON array for serialization (see [below](#toarray))                                                                                                            |

### toarray

Lua tables don't distinguish between arrays and objects. When a table is serialized to JSON, an empty table `{}` becomes a JSON object `{}` instead of an array `[]`. The `toarray()` function marks a table so it always serializes as a JSON array — even when empty.

```lua
return { items = toarray(results) }
-- With results: [{"name": "a"}, {"name": "b"}]
-- Without results: {"items": []}   (not {"items": {}})
```

You don't need `toarray()` on results from `db.query`, `db.search`, `db.backlinks`, or `db.raw` — those already return properly marked arrays. Use it when you build a table yourself with `table.insert()`.

## Record API

The `Record` API is available in **procedure**, **query**, and **record/label** scripts. In procedure scripts the full API is available — writes are proxied to the caller's PDS and indexed locally. In query and record/label scripts it runs in **no-auth mode**: `Record.load`, `r:save_local()`, `r:delete_local()`, and `Record.delete_local()` work, but PDS-touching methods (`r:save()`, `r:delete()`) raise an error.

See the full [Record API reference](../api-reference/lua/record-api.md) for constructor, static methods, instance methods, fields, schema validation, and save behavior.

Quick example:

```lua
function handle()
  local r = Record(collection, input)
  r:save()
  return { uri = r._uri, cid = r._cid }
end
```

## Database API

The `db` table provides access to the database. Available in both queries and procedures.

See the full [Database API reference](../api-reference/lua/database-api.md) for `db.query`, `db.get`, `db.search`, `db.backlinks`, `db.count`, and `db.raw`.

Quick example:

```lua
function handle()
  local result = db.query({ collection = collection, limit = 20 })
  return { records = result.records, cursor = result.cursor }
end
```

## HTTP API

The `http` table provides async HTTP client functions. Available in both queries and procedures.

See the full [HTTP API reference](../api-reference/lua/http-api.md) for all methods, options, and response format.

Quick example:

```lua
local resp = http.get("https://api.example.com/data")
local data = json.decode(resp.body)
```

## XRPC Lua API

The `xrpc` table lets scripts call other XRPC endpoints — both local and proxied. Available in both queries and procedures.

See the full [XRPC Lua API reference](../api-reference/lua/xrpc-lua-api.md) for `xrpc.query` and `xrpc.procedure`.

Quick example:

```lua
local resp = xrpc.query("xyz.statusphere.listStatuses", { limit = 5 })
local data = json.decode(resp.body)
```

## atproto API

The `atproto` table provides atproto utility functions like DID resolution, label queries, and record signing.

See the full [atproto API reference](../api-reference/lua/atproto-api.md) for `atproto.resolve_service_endpoint`, `atproto.get_labels`, `atproto.get_labels_batch`, `atproto.sign`, and `atproto.verify_signature`.

## JSON API

The `json` global provides JSON serialization and deserialization.

See the full [JSON API reference](../api-reference/lua/json-api.md) for `json.encode` and `json.decode`.

## Jobs API

The `jobs` table lets any script queue background jobs for long-running work. Available in all script contexts.

See the full [Jobs API reference](../api-reference/lua/jobs-api.md) for `jobs.create()` and the `job` global available inside job scripts.

Quick example:

```lua
local job_id = jobs.create("export", { collection = collection })
return { job_id = job_id }
```

For the full guide on background jobs, see [Background Jobs](background-jobs.md).

## Debugging

### Logging

Use `log()` to trace script execution. Output appears in the server logs at **debug** level with the field `lua_log`, and is also recorded as a `script.log` event in the [event logs](../api-reference/admin/events.md) (accessible via `GET /admin/events`):

```lua
function handle()
  log("handle called with params: " .. tostring(params.limit))
  local result = db.query({ collection = collection, limit = params.limit })
  log("query returned " .. #result.records .. " records")
  return result
end
```

To see log output in stdout, make sure your `RUST_LOG` environment variable includes debug level for HappyView (the default `happyview=debug` works). See [Configuration](../getting-started/configuration.md).

### Error messages

When a script fails, the client receives a generic `500` response:

- `{"error": "script execution failed"}`: covers syntax errors, runtime errors, missing `handle()` function, and errors raised with `error()`
- `{"error": "script exceeded execution time limit"}`: the script hit the 1,000,000 instruction limit

The **full error message** is logged server-side at error level. Check the server logs to see the actual Lua error, including line numbers and stack traces.

### Common mistakes

- **Missing `handle()` function**: Every script must define a global `handle()` function. If it's missing or misspelled, the script fails silently with "script execution failed".
- **Calling `error()` for expected conditions**: Lua's `error()` triggers a 500 response. For expected conditions like "record not found", return a structured error response instead: `return { error = "not found" }`.
- **Infinite loops**: The sandbox enforces a 1,000,000 instruction limit. If your script processes large data sets, paginate with `db.query()` limits instead of loading everything at once.
- **Forgetting `params` values are strings**: All query string parameters arrive as strings. Use `tonumber(params.limit)` if you need a number.

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
- [Batch save](../reference/script-examples/batch-save.md) — create multiple records in parallel with `Record.save_all()`
- [Sidecar records](../reference/script-examples/sidecar-records.md) — create linked records across collections with a shared rkey
- [Cascading delete](../reference/script-examples/cascading-delete.md) — delete a record and all related records
- [Complex mutations](../reference/script-examples/complex-mutations.md) — load, transform, and save a record with multiple field changes
- [Signed record](../reference/script-examples/signed-record.md) — save a record with an attestation signature

**Record & Label Scripts:**

- [Algolia sync](../reference/script-examples/algolia-sync.md) — push records to an Algolia search index on create/update/delete

## Next steps

- [Record & Label Scripts](label-scripts): React to record changes and label events in real time
- [Lexicons](lexicons.md): Understand how record, query, and procedure lexicons work together
- [Admin API — Scripts](../api-reference/admin/scripts.md): Manage scripts via the API
- [XRPC API](../api-reference/xrpc-api.md): See how endpoints behave with and without Lua scripts
- [Dashboard](../getting-started/dashboard.md#lua-editor): Use the web editor with context-aware completions

---
title: "Script Contract"
---

A Lua script defines one function, `handle(input, ctx)`, and reaches HappyView only through that function's two arguments and `require`. The script body runs once when the script loads, so `require` calls and helpers sit at file scope; `handle` runs once per trigger firing.

```lua
local log = require("internal.logging")
local db = require("happyview.db")

function handle(input, ctx)
  log.info("listing", { trigger = ctx.trigger, limit = input.limit })
  return db.records(ctx.collection):limit(tonumber(input.limit)):run()
end
```

`input` is the trigger's payload and `ctx` is what the runtime knows about the invocation. Both are plain tables. Nothing else is injected: a script sees the [standard libraries](standard-libraries.md) the sandbox allows, `require`, and `handle`'s two arguments. Reading a v2 global name (`db`, `params`, `now`, …) raises an error that names it and points at [Migrating Scripts](../../guides/migrating-scripts.md).

## `require`

`require` loads two kinds of module: the [built-in modules](built-in-modules.md) under `internal.` and the installed [libraries](libraries.md) under `happyview.`. Each is loaded once per script run and returns a table. A name with no installed library plugin behind it fails at load with:

```
module '<name>' not found -- is the '<name>' library plugin installed?
```

Requires go at the top of the script, one `local` per module, in this order:

```lua
local log = require("internal.logging")
local time = require("internal.time")
local tids = require("internal.tids")
local json = require("internal.json")
local db = require("happyview.db")
local sql = require("happyview.sql")
local backlinks = require("happyview.backlinks")
local record = require("happyview.record")
local xrpc = require("happyview.xrpc")
local atproto = require("happyview.atproto")
local spaces = require("happyview.spaces")
local linked_repos = require("happyview.linked_repos")
local jobs = require("happyview.jobs")
local http = require("happyview.http")
```

A script requires only what it uses. The codemod writes its requires in this order and with these local names, so a migrated script and a hand-written one read alike.

## `input` by script kind

| Trigger | `input` |
| --- | --- |
| `xrpc.query:<nsid>` | The query-string parameters. A parameter the lexicon declares as `integer`, `number` or `boolean` arrives as that Lua type; any other value is a string, and a repeated key is an array of strings |
| `xrpc.procedure:<nsid>` | The request body |
| `record.index:<nsid>`, `record.create:<nsid>`, `record.update:<nsid>`, `record.delete:<nsid>` | `{ action, uri, did, collection, rkey, record }`. `action` is `"create"`, `"update"` or `"delete"`; `record` is `nil` on delete |
| `labeler.apply:<nsid>`, `labeler.apply:_actor` | `{ src, uri, val, neg, cts, exp }`. `exp` is absent when the label does not expire |
| `job.run:<type>` | The table passed to `jobs.create` |

## `ctx`

Every invocation gets the same keys. A key that does not apply to the trigger is `nil`.

| Field | Type | Value |
| --- | --- | --- |
| `trigger` | string | The script's trigger id, for logs and diagnostics |
| `caller_did` | string? | Procedure: the authenticated caller. Query: the caller, or `nil` when anonymous. Record event: the DID of the repo the event came from. Job: the DID that enqueued it. Label: `nil` |
| `has_pds_auth` | boolean | `true` when the script can act on a PDS as `caller_did`: a procedure whose caller holds a session, or a job created with `{ auth = true }`. `false` for queries, record events and labels |
| `env` | table | [Script variables](../admin/script-variables.md), keyed by name. Present for every kind |
| `method` | string? | Query and procedure: the XRPC method NSID |
| `collection` | string? | Query and procedure: the lexicon's `target_collection`. Record event: the record's collection |
| `params` | table? | Procedure: the query-string parameters. The body is `input` |
| `delegate_did` | string? | Procedure: the account the caller is delegated to write for |
| `space` | table? | Query and procedure on a space-scoped request: `{ uri, id, did, authority_did, spaceType, skey }` |
| `job` | table? | Job: `{ id, progress(data), should_stop(), wait(seconds) }`. See [Background Jobs](../../guides/background-jobs.md#controlling-a-running-job) |

`has_pds_auth` is what decides whether a library call that acts as the caller can succeed. `create`, `put`, `delete` and `upload_blob` on `happyview.record`, and `procedure` on `happyview.xrpc`, raise `NO_SESSION` when it is `false`.

## Return values

| Kind | `handle` returns |
| --- | --- |
| Query, procedure | The table serialised as the JSON response body |
| Record event | `nil` to skip indexing, a table to store in place of the record, `true` to store the record as-is. See [Record & Label Scripts](../../guides/record-scripts.md#record-script-return-values) |
| Label | `nil` to drop the label, a table to merge over it, `true` to keep it. See [Record & Label Scripts](../../guides/record-scripts.md#label-script-return-values) |
| Job | The value stored as the job's `result` |

## Errors

`error()` inside `handle` fails the invocation, and the message travels with it. For a query or procedure the client receives the error itself:

```json
{ "error": "script_error", "errorType": "runtime", "message": "<the Lua error>", "method": "xyz.statusphere.getStatus", "line": 12 }
```

`errorType` is `syntax`, `runtime`, `missing_handle` or `timeout`; the status is 500, except `timeout`, which is 408. `timeout` is the instruction limit or, for a query or procedure, the wall clock on `handle`, whichever comes first; both are set in Settings → General, or by `SCRIPT_INSTRUCTION_LIMIT` and `SCRIPT_WALL_CLOCK_SECONDS` in the [configuration](../../getting-started/configuration.md#environment-variables), which gives their defaults. `pcall` and `coroutine.resume` catch the error like any other but cannot save the run: once the budget is spent, every protected call re-raises it and a normal return still fails. `message` is the Lua error without its traceback and `line` is the script line that raised it, `null` when the error carries none. A message beginning `AUTH_ERROR:` is answered as a 401 with the rest of the message as `error`. Because `message` reaches the client, never interpolate a secret or a caller's data into `error()`; return a structured error table for an expected condition instead. The same error is also logged at error level and recorded in the [event log](../../guides/event-logs.md).

For a record or label event the script is retried and then dead-lettered; for a job the job's status becomes `failed` with the message as its `error`. Libraries raise Lua errors too, prefixed with the library and function that failed, so `pcall` is how a script handles an expected failure.

## Next steps

- [Built-in Modules](built-in-modules.md): `internal.logging`, `internal.time`, `internal.tids`, `internal.json`
- [Libraries](libraries.md): every `happyview.*` module, its capability and where its surface is documented
- [Lua Scripting](../../guides/lua-scripting.md): the sandbox, debugging and worked examples

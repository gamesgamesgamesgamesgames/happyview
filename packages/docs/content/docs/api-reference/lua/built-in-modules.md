---
title: "Built-in Modules"
---

`require` serves two kinds of module. Installed [libraries](libraries.md) are WASM plugins. Built-in modules are provided by the runtime directly, under the `internal.` prefix, because they need to be the host itself: a log line has to carry the trigger and script it came from, a clock has to be the host's clock. A plugin cannot claim a namespace starting with `internal.`.

## `internal.logging`

```lua
local log = require("internal.logging")
log.debug(message[, fields])
log.info(message[, fields])
log.warn(message[, fields])
log.error(message[, fields])
```

`fields` is a table stored as JSON on the event, so `log.error("save failed", { uri = uri, err = tostring(err) })` is queryable rather than a stringified blob. `info`, `warn` and `error` are recorded as `script.log` events in the [event log](../../guides/event-logs.md), carrying the trigger id and caller; `debug` writes only to the process log. In a job script, `info`/`warn`/`error` also write to that job's own log.

## `internal.time`

```lua
local time = require("internal.time")
time.now()                       -- unix milliseconds, integer
time.to_iso8601(ms)              -- "2026-09-13T15:04:05.000Z", raises on an out-of-range timestamp
time.from_iso8601(string)        -- unix milliseconds, or nil on an unparseable string
```

`time.to_iso8601(time.now())` is the current UTC timestamp for a `createdAt` field.

## `internal.tids`

```lua
local tids = require("internal.tids")
tids.create()            -- a fresh TID for now
tids.to_tid(ms)          -- TID for a unix-millisecond timestamp
tids.from_tid(tid)       -- unix milliseconds, raises on an invalid TID
```

A TID is atproto's 13-character sortable record key. `tids.create()` is the rkey to use when a script needs to name a record before writing it.

## `internal.json`

```lua
local json = require("internal.json")
json.encode(value)       -- Lua value -> JSON string
json.decode(string)      -- JSON string -> Lua value
json.to_array(table)     -- marks a table as a JSON array, so an empty one encodes as [] rather than {}
```

Encoding rules and examples: [JSON](json-api.md).

## Next steps

- [Script Contract](script-contract.md): `handle(input, ctx)` and where `ctx` comes from
- [Libraries](libraries.md): the `happyview.*` modules, reached the same way via `require`

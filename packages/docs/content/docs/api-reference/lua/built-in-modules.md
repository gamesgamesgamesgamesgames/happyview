---
title: "Built-in Modules"
---

`require` serves two kinds of module. Installed libraries are WASM plugins — see [Developing Plugins](../../guides/developing-plugins.md). Built-in modules are provided by the runtime directly, under the `internal.` prefix, because they need to be the host itself: a log line has to carry the trigger and script it came from, a clock has to be the host's clock. A plugin cannot claim a namespace starting with `internal.`.

## `internal.logging`

```lua
local log = require("internal.logging")
log.debug(message[, fields])
log.info(message[, fields])
log.warn(message[, fields])
log.error(message[, fields])
```

`fields` is a table stored as JSON on the event, so `log.error("save failed", { uri = uri, err = tostring(err) })` is queryable rather than a stringified blob. Events carry the trigger id and caller, same as today. `debug` writes only to the process log — it is not recorded as an event. In a job script, `info`/`warn`/`error` also write to that job's own log.

## `internal.time`

```lua
local time = require("internal.time")
time.now()                       -- unix milliseconds, integer
time.to_iso8601(ms)                -- "2026-09-13T15:04:05.000Z", raises on an out-of-range timestamp
time.from_iso8601(string)          -- unix milliseconds, or nil on an unparseable string
```

## `internal.tids`

```lua
local tids = require("internal.tids")
tids.create()                    -- a fresh TID for now
tids.to_tid(ms)                    -- TID for a unix-millisecond timestamp
tids.from_tid(tid)                 -- unix milliseconds, raises on an invalid TID
```

## Next steps

- [`handle(input, ctx)` contract](../../guides/lua-scripting.md#the-handleinput-ctx-contract): where `ctx` comes from
- [Developing Plugins](../../guides/developing-plugins.md): installed libraries, reached the same way via `require`
- [Utility Globals](utility-globals.md): the globals these built-ins replace

---
title: "Utility Globals"
---

Global functions available in all [Lua scripts](../../guides/lua-scripting.md) — queries, procedures, and [record/label scripts](../../guides/label-scripts). These don't belong to a specific API table — they're available at the top level of any script.

## now

```lua
local timestamp = now()
-- "2026-04-19T15:30:00Z"
```

Returns the current UTC time as an ISO 8601 string. Use this for `createdAt`, `updatedAt`, and similar timestamp fields.

## log

```lua
log("processing record: " .. uri)
log("count: " .. tostring(n))
```

Writes a message to the server logs at debug level and records a `script.log` event in the [event logs](../../api-reference/admin/events.md). Useful for debugging scripts during development. Log output appears in HappyView's stdout and is also accessible via `GET /admin/events`.

## TID

```lua
local id = TID()
-- "3abc123def456"
```

Generates a fresh atproto TID (Timestamp Identifier) — a 13-character, base32-sortable string derived from the current timestamp. TIDs are the standard record key format in atproto. Use this when creating records with a specific rkey:

```lua
local r = Record(collection, { text = "hello" })
r:set_rkey(TID())
r:save()
```

### TID.toISO8601

```lua
local iso = TID.toISO8601(tid)
-- "2026-04-19T15:30:00.123456Z"
```

Converts a TID to an ISO 8601 timestamp string with microsecond precision. This is lossy — the 10-bit clock ID embedded in the TID is discarded.

### TID.fromISO8601

```lua
local tid = TID.fromISO8601("2026-04-19T15:30:00Z")
```

Creates a TID from an ISO 8601 timestamp string. Accepts timezone offsets and fractional seconds. The resulting TID uses a zero clock ID, so it won't match any specific generated TID but will sort correctly relative to TIDs from the same moment.

### TID.toUnixMicroseconds

```lua
local us = TID.toUnixMicroseconds(tid)
-- 1745074200123456
```

Extracts the microsecond timestamp from a TID (microseconds since the Unix epoch). Lossy — drops the clock ID.

### TID.fromUnixMicroseconds

```lua
local tid = TID.fromUnixMicroseconds(1745074200123456)
```

Creates a TID from a microsecond timestamp. Uses a zero clock ID.

### TID.toNumber

```lua
local n = TID.toNumber(tid)
local same_tid = TID.fromNumber(n)
-- same_tid == tid
```

Decodes a TID to its full numeric representation (timestamp + clock ID). This is the only lossless conversion — `TID.fromNumber(TID.toNumber(tid))` always returns the original TID.

### TID.fromNumber

```lua
local tid = TID.fromNumber(n)
```

Encodes a number back into a TID. Inverse of `TID.toNumber`.

## toarray

```lua
return { items = toarray(results) }
```

Marks a Lua table so it serializes as a JSON array rather than a JSON object. This matters for empty tables — without `toarray`, an empty `{}` becomes a JSON object `{}` instead of an array `[]`.

```lua
-- Without toarray:
return { items = {} }
-- → {"items": {}}

-- With toarray:
return { items = toarray({}) }
-- → {"items": []}
```

You don't need `toarray()` on results from `db.query`, `db.search`, `db.backlinks`, or `db.raw` — those already return properly marked arrays. Use it when you build a table yourself with `table.insert()` or array-index assignment.

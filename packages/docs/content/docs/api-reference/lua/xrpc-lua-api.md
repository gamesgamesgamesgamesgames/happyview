---
title: "XRPC Lua API"
---

The `xrpc` table provides cross-endpoint XRPC calls. Available in all [Lua scripts](../../guides/lua-scripting.md) — queries, procedures, and [record/label scripts](../../guides/record-scripts).

## xrpc.query

```lua
local resp = xrpc.query("xyz.statusphere.listStatuses", {  -- required: XRPC method name
  limit = 10,                                               -- optional: query parameters
})
```

Calls an XRPC query. If the method matches a locally registered query lexicon, it runs locally. Otherwise, the request is proxied to the NSID's authority.

**Returns:** A table with:

| Field    | Type    | Description          |
| -------- | ------- | -------------------- |
| `status` | integer | HTTP status code     |
| `body`   | string  | Response body (JSON) |

The body is a raw JSON string — use `json.decode(resp.body)` to parse it.

### Examples

```lua
-- Call a local query endpoint
local resp = xrpc.query("xyz.statusphere.listStatuses", { limit = 5 })
local data = json.decode(resp.body)
for _, record in ipairs(data.records) do
  log(record.uri)
end

-- Call without parameters
local resp = xrpc.query("com.example.getConfig")

-- Proxy to a remote XRPC endpoint
local resp = xrpc.query("app.bsky.feed.getAuthorFeed", {
  actor = "did:plc:abc123",
  limit = 10,
})
```

## xrpc.procedure

```lua
local resp = xrpc.procedure(
  "xyz.statusphere.setStatus",    -- required: XRPC method name
  { status = "hello" },           -- required: request body
  { someParam = "value" }         -- optional: query parameters
)
```

Calls an XRPC procedure using the current request's `caller_did` for authentication. If the method matches a locally registered procedure lexicon, it runs locally. Otherwise, the request is proxied.

Requires a `caller_did` — raises an error without one.

**Returns:** A table with the same shape as `xrpc.query` responses (`status` and `body`).

### Examples

```lua
-- Call a local procedure
local resp = xrpc.procedure("xyz.statusphere.setStatus", {
  status = "hello",
  createdAt = now(),
})

if resp.status ~= 200 then
  return { error = "failed: " .. resp.body }
end

-- Parse the response
local result = json.decode(resp.body)
return { uri = result.uri }
```

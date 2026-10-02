---
title: "HTTP API"
---

The `http` table provides async HTTP client functions. Available in all [Lua scripts](../../guides/lua-scripting.md) — queries, procedures, and [record/label scripts](../../guides/record-scripts).

## Methods

All methods take a URL and an optional options table, and return a [response table](#response).

```lua
http.get(url, opts?)
http.post(url, opts?)
http.put(url, opts?)
http.patch(url, opts?)
http.delete(url, opts?)
http.head(url, opts?)
```

## Options

The optional second argument is a table with:

| Field     | Type   | Description                               |
| --------- | ------ | ----------------------------------------- |
| `headers` | table  | Request headers as key-value string pairs |
| `body`    | string | Request body (ignored for GET and HEAD)   |

## Response

Every method returns a table with:

| Field     | Type    | Description                                          |
| --------- | ------- | ---------------------------------------------------- |
| `status`  | integer | HTTP status code                                     |
| `body`    | string  | Response body text (empty string for HEAD)           |
| `headers` | table   | Response headers as key-value pairs (lowercase keys) |

## Examples

```lua
-- Simple GET
local resp = http.get("https://api.example.com/data")
-- resp.status = 200, resp.body = "...", resp.headers["content-type"] = "application/json"

-- GET with custom headers
local resp = http.get("https://api.example.com/data", {
  headers = { ["authorization"] = "Bearer token123" }
})

-- POST with JSON body
local resp = http.post("https://api.example.com/hook", {
  body = '{"key": "value"}',
  headers = { ["content-type"] = "application/json" }
})

-- PUT, PATCH, DELETE, HEAD follow the same pattern
local resp = http.put(url, { body = data, headers = { ... } })
local resp = http.patch(url, { body = data, headers = { ... } })
local resp = http.delete(url, { headers = { ... } })
local resp = http.head(url)
```

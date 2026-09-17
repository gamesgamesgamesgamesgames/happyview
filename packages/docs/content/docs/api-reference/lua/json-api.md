---
title: "JSON API"
---

The `json` global provides JSON serialization and deserialization. Available in all [Lua scripts](../../guides/lua-scripting.md) — queries, procedures, and [record/label scripts](../../guides/label-scripts).

<Callout type="info">
`require("internal.json")` provides the same `encode`/`decode`, plus `to_array` in place of the [`toarray`](utility-globals.md#toarray) global — see [Built-in Modules](built-in-modules.md#internaljson).
</Callout>

## json.encode

```lua
local str = json.encode({ key = "value", items = { 1, 2, 3 } })
-- '{"key":"value","items":[1,2,3]}'
```

Converts a Lua table to a JSON string.

## json.decode

```lua
local tbl = json.decode('{"key": "value"}')
-- tbl.key == "value"
```

Parses a JSON string into a Lua table. Returns an error if the input is not valid JSON.

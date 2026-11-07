---
title: "JSON"
---

`require("internal.json")` serialises Lua values to JSON and back. It is a [built-in module](built-in-modules.md#internaljson), available in every script kind.

```lua
local json = require("internal.json")
```

## `json.encode(value)`

```lua
local str = json.encode({ key = "value", items = { 1, 2, 3 } })
-- '{"key":"value","items":[1,2,3]}'
```

Converts a Lua value to a JSON string. A table with consecutive integer keys from 1 encodes as an array; any other table encodes as an object.

## `json.decode(string)`

```lua
local tbl = json.decode('{"key": "value"}')
-- tbl.key == "value"
```

Parses a JSON string into a Lua value. Raises when the input is not valid JSON.

## `json.to_array(table)`

```lua
return { items = json.to_array(results) }
-- With results:    {"items": [{"name": "a"}, {"name": "b"}]}
-- Without results: {"items": []}
```

Marks a table so it always encodes as a JSON array. Lua does not distinguish an empty array from an empty object, so an empty `{}` encodes as `{}` unless it is marked. Pages and record lists returned by a library are already marked; mark a table you build yourself with `table.insert` or index assignment, whether it is returned from `handle` or passed to `json.encode`.

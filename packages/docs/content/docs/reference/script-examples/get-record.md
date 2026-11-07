---
title: "Get a Record"
---

Fetch a single record by its AT URI.

**Lexicon type:** query

```lua
local db = require("happyview.db")

function handle(input, ctx)
  if not input.uri then
    return { error = "uri parameter is required" }
  end

  local row = db.get(input.uri)
  if not row then
    return { error = "not found" }
  end

  return { record = row }
end
```

## How it works

1. Check that the `uri` query parameter is present. Return a structured error if missing.
2. Look up the record with `db.get`, which returns an [envelope](../../api-reference/lua/libraries.md#conventions-every-library-follows) (`{ uri, did, collection, rkey, cid, indexed_at, record }`) or `nil`.
3. Return the envelope wrapped in an object. The record body is its `record` field.

## Usage

```
GET /xrpc/xyz.statusphere.getRecord?uri=at://did:plc:abc/xyz.statusphere.record/abc123
```

## Use case

A focused read endpoint for detail views or record verification. Returns structured error responses instead of calling `error()`, so the client gets a 200 with an error field it can handle gracefully rather than a 500.

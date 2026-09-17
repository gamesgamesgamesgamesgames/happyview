---
title: "List or Fetch"
---

This query handles both single-record lookups (when a `uri` param is provided) and paginated listing.

**Lexicon type:** query

```lua
local db = require("happyview.db")

function handle(input, ctx)
  if input.uri then
    local row = db.get(input.uri)
    if not row then
      return { error = "record not found" }
    end
    return { record = row }
  end

  return db.records(ctx.collection)
    :did(input.did)
    :limit(tonumber(input.limit) or 20)
    :cursor(input.cursor)
    :run()
end
```

## How it works

1. If a `uri` query parameter is provided, fetch that single record with `db.get` and return it. If it doesn't exist, return a structured error (using `error()` would trigger a 500 response).
2. Otherwise, list records from the target collection with a `db.records` chain, with optional filtering by `did` and cursor-based pagination. The `cursor` is an opaque string from a previous response — pass it through directly. A `limit` the lexicon declares as an `integer` arrives as a number already; `tonumber()` covers a lexicon that leaves it untyped, where it arrives as a string.

## Usage

```
GET /xrpc/xyz.statusphere.listRecords?limit=10
GET /xrpc/xyz.statusphere.listRecords?did=did:plc:abc
GET /xrpc/xyz.statusphere.listRecords?uri=at://did:plc:abc/xyz.statusphere.record/abc123
```

## Use case

Useful when one endpoint needs to handle both listing and single-record fetches.

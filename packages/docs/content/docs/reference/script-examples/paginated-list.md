---
title: "Paginated List"
---

List records from a collection with cursor-based pagination and an optional DID filter.

**Lexicon type:** query

```lua
local db = require("happyview.db")

function handle(input, ctx)
  local limit = tonumber(input.limit) or 20
  if limit > 100 then limit = 100 end

  return db.records(ctx.collection)
    :did(input.did)
    :limit(limit)
    :cursor(input.cursor)
    :run()
end
```

## How it works

1. Parse `limit` from the query string, defaulting to 20 and capping at 100.
2. Build a `db.records` chain on the target collection, narrowed by the optional DID filter and continued from the cursor. A step given `nil` is skipped, so a request without `did` or `cursor` needs no branching.
3. `run()` answers `{ records = [...], cursor = "..." }`, where each record is an envelope and `cursor` is an opaque string present when more records exist.

## Usage

```
GET /xrpc/xyz.statusphere.listStatuses
GET /xrpc/xyz.statusphere.listStatuses?limit=50
GET /xrpc/xyz.statusphere.listStatuses?did=did:plc:abc&limit=10
GET /xrpc/xyz.statusphere.listStatuses?cursor=<opaque>&limit=20
```

## Use case

A list endpoint for feeds, timelines, or browsing records by collection. The `cursor` value returned by `run()` is an opaque string. Clients pass it back as the `cursor` parameter to fetch the next page — don't parse or modify it.

---
title: "Expanded Query"
---

List statuses and include the profile of each user who created one.

**Lexicon type:** query

```lua
local json = require("internal.json")
local db = require("happyview.db")

function handle(input, ctx)
  local limit = tonumber(input.limit) or 20
  if limit > 100 then limit = 100 end

  local page = db.records("xyz.statusphere.status")
    :did(input.did)
    :limit(limit)
    :cursor(input.cursor)
    :run()

  -- Load each author's profile once
  local seen = {}
  local profiles = {}
  for _, status in ipairs(page.records) do
    if not seen[status.did] then
      seen[status.did] = true
      local profile = db.get("at://" .. status.did .. "/app.bsky.actor.profile/self")
      if profile then
        profiles[#profiles + 1] = profile
      end
    end
  end

  return {
    statuses = page.records,
    profiles = json.to_array(profiles),
    cursor = page.cursor,
  }
end
```

## How it works

1. Query statuses from the target collection with pagination, same as a normal list query.
2. Each envelope carries the author's `did`, so the unique DIDs fall out of the page without parsing URIs.
3. Look up each DID's `app.bsky.actor.profile/self` record (this is where Bluesky profiles live) with `db.get`. A profile that isn't indexed locally returns `nil` and is skipped.
4. Return statuses and profiles as separate keys, with the cursor from the status query. `json.to_array` keeps `profiles` a JSON array when it is empty.

## Usage

```
GET /xrpc/xyz.statusphere.listStatusesWithProfiles?limit=10
GET /xrpc/xyz.statusphere.listStatusesWithProfiles?did=did:plc:abc
GET /xrpc/xyz.statusphere.listStatusesWithProfiles?cursor=<opaque>&limit=20
```

```json
{
  "statuses": [
    { "uri": "at://did:plc:abc/xyz.statusphere.status/3abc123", "did": "did:plc:abc", "record": { "status": "😊", "createdAt": "..." } },
    { "uri": "at://did:plc:def/xyz.statusphere.status/3def456", "did": "did:plc:def", "record": { "status": "🌟", "createdAt": "..." } }
  ],
  "profiles": [
    { "uri": "at://did:plc:abc/app.bsky.actor.profile/self", "did": "did:plc:abc", "record": { "displayName": "Alice", "avatar": "..." } },
    { "uri": "at://did:plc:def/app.bsky.actor.profile/self", "did": "did:plc:def", "record": { "displayName": "Bob", "avatar": "..." } }
  ],
  "cursor": "MjAyNi0wMS0wMVQxMjowMDowMFp8YXQ6Ly9kaWQ6..."
}
```

## Use case

This avoids N+1 queries on the client side — the client gets statuses and profiles in one call. The deduplication step loads each profile only once even if multiple statuses share an author.

`db.get` reads from HappyView's local index. Profiles only appear if `app.bsky.actor.profile` is also indexed. Missing profiles are skipped. Each entry is an envelope, so a client reads `record.status` and `record.displayName`; the other envelope fields are elided above.

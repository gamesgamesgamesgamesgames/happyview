---
title: "Complex Mutations"
---

Load an existing record, apply multiple transformations, and save it back.

**Lexicon type:** procedure

```lua
local time = require("internal.time")
local db = require("happyview.db")
local record = require("happyview.record")

function handle(input, ctx)
  if not input.uri then
    return { error = "uri is required" }
  end

  local row = db.get(input.uri)
  if not row then
    return { error = "not found" }
  end
  local r = row.record

  -- Increment a counter
  r.likeCount = (r.likeCount or 0) + 1

  -- Merge tags, deduplicating and capping at 10
  r.tags = r.tags or {}
  if input.tags then
    for _, tag in ipairs(input.tags) do
      local found = false
      for _, t in ipairs(r.tags) do
        if t == tag then
          found = true
          break
        end
      end
      if not found then
        r.tags[#r.tags + 1] = tag
      end
    end
    -- Keep only the last 10
    while #r.tags > 10 do
      table.remove(r.tags, 1)
    end
  end

  -- Normalize a string field
  if input.title then
    r.title = string.gsub(input.title, "^%s+", "")
    r.title = string.gsub(r.title, "%s+$", "")
  end

  -- Set a computed field
  r.updatedAt = time.to_iso8601(time.now())

  return record.put(input.uri, r)
end
```

## How it works

1. Load the existing record with `db.get`. The envelope's `record` field is the stored body, a plain table you can change in place.
2. Apply transformations directly on the body's fields:
   - **Increment a counter**: use `or 0` to handle the field being `nil` on first access.
   - **Merge tags**: iterate over `input.tags`, skip duplicates already in `r.tags`, append new ones, then trim the list to 10.
   - **Normalize a string**: use `string.gsub` to trim whitespace.
   - **Set a timestamp**: [`time.to_iso8601(time.now())`](../../api-reference/lua/built-in-modules.md#internaltime) for UTC ISO 8601.
3. Write the body back with `record.put`, which calls `putRecord` to update the record on the user's PDS.

## Usage

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/xrpc/xyz.statusphere.updatePost", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    Authorization: `Bearer ${TOKEN}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    uri: "at://did:plc:abc/xyz.statusphere.post/abc123",
    tags: ["tutorial", "atproto"],
    title: "  My Post Title  ",
  }),
});
const data = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/xrpc/xyz.statusphere.updatePost", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    Authorization: `Bearer ${TOKEN}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    uri: "at://did:plc:abc/xyz.statusphere.post/abc123",
    tags: ["tutorial", "atproto"],
    title: "  My Post Title  ",
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("http://127.0.0.1:3000/xrpc/xyz.statusphere.updatePost")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("Bearer {}", token))
    .json(&serde_json::json!({
        "uri": "at://did:plc:abc/xyz.statusphere.post/abc123",
        "tags": ["tutorial", "atproto"],
        "title": "  My Post Title  "
    }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := `{
  "uri": "at://did:plc:abc/xyz.statusphere.post/abc123",
  "tags": ["tutorial", "atproto"],
  "title": "  My Post Title  "
}`
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/xrpc/xyz.statusphere.updatePost", bytes.NewBufferString(body))
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/xrpc/xyz.statusphere.updatePost \
  -H "X-Client-Key: $CLIENT_KEY" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "uri": "at://did:plc:abc/xyz.statusphere.post/abc123",
    "tags": ["tutorial", "atproto"],
    "title": "  My Post Title  "
  }'
```

## Use case

This pattern is useful when updates involve more than simple field replacement: counters, bounded lists, string normalization, or computed fields. All mutations happen in memory before the single `record.put` call, so there's no partial save: either all changes are written or none are.

The body is validated against the collection's lexicon before the write; pass `{ validate = false }` as a third argument to skip that check.

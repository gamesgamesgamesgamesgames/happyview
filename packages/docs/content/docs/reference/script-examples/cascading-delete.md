---
title: "Cascading Delete"
---

Delete a record and all related records across collections.

**Lexicon type:** procedure

```lua
local db = require("happyview.db")
local record = require("happyview.record")

function handle(input, ctx)
  if not input.uri then
    return { error = "uri is required" }
  end

  if not db.get(input.uri) then
    return { error = "not found" }
  end

  -- Find the caller's comments that reference this URI
  local comments = db.records("xyz.statusphere.comment")
    :where("postUri", "=", input.uri)
    :did(ctx.caller_did)
    :limit(100)
    :run()

  local to_delete = { input.uri }
  for _, comment in ipairs(comments.records) do
    to_delete[#to_delete + 1] = comment.uri
  end

  for _, uri in ipairs(to_delete) do
    record.delete(uri)
  end

  return { deleted = #to_delete }
end
```

## How it works

1. Check the primary record exists with `db.get`. Return early if it doesn't.
2. Query for related records: comments by the same user whose `postUri` field is the primary record's URI. `where` filters on a field of the stored body, so the match happens in the database rather than in Lua.
3. Delete everything with `record.delete`. Each call removes the record from the user's PDS and the local index.

## Usage

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/xrpc/xyz.statusphere.deletePost", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    Authorization: `Bearer ${TOKEN}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    uri: "at://did:plc:abc/xyz.statusphere.post/abc123",
  }),
});
const data = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/xrpc/xyz.statusphere.deletePost", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    Authorization: `Bearer ${TOKEN}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    uri: "at://did:plc:abc/xyz.statusphere.post/abc123",
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("http://127.0.0.1:3000/xrpc/xyz.statusphere.deletePost")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("Bearer {}", token))
    .json(&serde_json::json!({
        "uri": "at://did:plc:abc/xyz.statusphere.post/abc123"
    }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := `{ "uri": "at://did:plc:abc/xyz.statusphere.post/abc123" }`
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/xrpc/xyz.statusphere.deletePost", bytes.NewBufferString(body))
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/xrpc/xyz.statusphere.deletePost \
  -H "X-Client-Key: $CLIENT_KEY" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{ "uri": "at://did:plc:abc/xyz.statusphere.post/abc123" }'
```

```json
{
  "deleted": 4
}
```

## Use case

Cascading deletes are useful when your data model has parent-child relationships across collections. For example, deleting a post should also clean up its comments, reactions, or metadata records. This keeps the user's repo and the local index consistent.

Note that this only deletes records owned by `ctx.caller_did`. atproto records can only be deleted by their owner. If the related records could have more than 100 matches, paginate through all of them before deleting.

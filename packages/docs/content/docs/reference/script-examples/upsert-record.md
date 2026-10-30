---
title: "Upsert Record"
---

Create a new record, or update an existing one if the client provides its rkey.

**Lexicon type:** procedure

```lua
local time = require("internal.time")
local tids = require("internal.tids")
local db = require("happyview.db")
local record = require("happyview.record")

function handle(input, ctx)
  local rkey = input.rkey or tids.create()
  local uri = "at://" .. ctx.caller_did .. "/" .. ctx.collection .. "/" .. rkey
  local ts = time.to_iso8601(time.now())

  local existing = db.get(uri)
  if existing then
    -- Update existing record
    local body = existing.record
    body.status = input.status
    body.updatedAt = ts
    return record.put(uri, body)
  end

  -- Create new record
  return record.create(ctx.collection, {
    status = input.status,
    createdAt = ts,
    updatedAt = ts,
  }, { rkey = rkey })
end
```

## How it works

1. Use the client-provided `input.rkey` if present, otherwise mint a fresh TID with [`tids.create()`](../../api-reference/lua/built-in-modules.md#internaltids). This means omitting `rkey` always creates, while providing one enables updates.
2. Build the AT URI from the caller's DID, the target collection, and the rkey, then look it up with `db.get`.
3. If the record exists, change its fields and write it back with `record.put`, which calls `putRecord`.
4. If it doesn't exist, create it with `record.create`, passing the rkey in `opts` so `createRecord` uses that key. Both writes answer `{ uri, cid }`.

## Usage

**Create** (no rkey, so a new TID is generated):

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/xrpc/xyz.statusphere.setStatus", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    Authorization: `Bearer ${TOKEN}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({ status: "hello" }),
});
const data = await response.json();
// → { "uri": "at://did:plc:abc/xyz.statusphere.status/3abc123", "cid": "bafyrei..." }
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/xrpc/xyz.statusphere.setStatus", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    Authorization: `Bearer ${TOKEN}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({ status: "hello" }),
});
const data = await response.json();
// → { "uri": "at://did:plc:abc/xyz.statusphere.status/3abc123", "cid": "bafyrei..." }
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("http://127.0.0.1:3000/xrpc/xyz.statusphere.setStatus")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("Bearer {}", token))
    .json(&serde_json::json!({ "status": "hello" }))
    .send()
    .await?;
// → { "uri": "at://did:plc:abc/xyz.statusphere.status/3abc123", "cid": "bafyrei..." }
```
```go tab="Go" tab-group="language"
body := `{ "status": "hello" }`
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/xrpc/xyz.statusphere.setStatus", bytes.NewBufferString(body))
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
// → { "uri": "at://did:plc:abc/xyz.statusphere.status/3abc123", "cid": "bafyrei..." }
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/xrpc/xyz.statusphere.setStatus \
  -H "X-Client-Key: $CLIENT_KEY" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{ "status": "hello" }'
# → { "uri": "at://did:plc:abc/xyz.statusphere.status/3abc123", "cid": "bafyrei..." }
```

**Update** (pass the rkey back to update the same record):

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/xrpc/xyz.statusphere.setStatus", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    Authorization: `Bearer ${TOKEN}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({ rkey: "3abc123", status: "updated" }),
});
const data = await response.json();
// → { "uri": "at://did:plc:abc/xyz.statusphere.status/3abc123", "cid": "bafyrei..." }
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/xrpc/xyz.statusphere.setStatus", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    Authorization: `Bearer ${TOKEN}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({ rkey: "3abc123", status: "updated" }),
});
const data = await response.json();
// → { "uri": "at://did:plc:abc/xyz.statusphere.status/3abc123", "cid": "bafyrei..." }
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("http://127.0.0.1:3000/xrpc/xyz.statusphere.setStatus")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("Bearer {}", token))
    .json(&serde_json::json!({ "rkey": "3abc123", "status": "updated" }))
    .send()
    .await?;
// → { "uri": "at://did:plc:abc/xyz.statusphere.status/3abc123", "cid": "bafyrei..." }
```
```go tab="Go" tab-group="language"
body := `{ "rkey": "3abc123", "status": "updated" }`
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/xrpc/xyz.statusphere.setStatus", bytes.NewBufferString(body))
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
// → { "uri": "at://did:plc:abc/xyz.statusphere.status/3abc123", "cid": "bafyrei..." }
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/xrpc/xyz.statusphere.setStatus \
  -H "X-Client-Key: $CLIENT_KEY" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{ "rkey": "3abc123", "status": "updated" }'
# → { "uri": "at://did:plc:abc/xyz.statusphere.status/3abc123", "cid": "bafyrei..." }
```

## Use case

This is useful when the client knows whether it's creating or editing, but you want a single endpoint for both. The client omits `rkey` for new records and includes it when editing an existing one. The rkey from the initial create response acts as the record's stable identifier for future updates.

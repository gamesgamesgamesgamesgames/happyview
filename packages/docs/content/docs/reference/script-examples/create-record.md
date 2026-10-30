---
title: "Create Record"
---

The simplest write: take the request body, save it as a record, and return the URI.

**Lexicon type:** procedure

```lua
local record = require("happyview.record")

function handle(input, ctx)
  return record.create(ctx.collection, input)
end
```

## How it works

1. `record.create` on [`happyview.record`](../../api-reference/lua/libraries.md) takes the target collection and the fields from the request body, validates them against the collection's lexicon, and creates the record on the caller's PDS.
2. The write is mirrored into the local index at once.
3. `create` answers `{ uri, cid }`, which is returned as the response.

## Usage

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/xrpc/xyz.statusphere.createRecord", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    Authorization: `Bearer ${TOKEN}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    text: "Hello world",
    createdAt: "2025-01-01T00:00:00Z",
  }),
});
const data = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/xrpc/xyz.statusphere.createRecord", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    Authorization: `Bearer ${TOKEN}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    text: "Hello world",
    createdAt: "2025-01-01T00:00:00Z",
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("http://127.0.0.1:3000/xrpc/xyz.statusphere.createRecord")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("Bearer {}", token))
    .json(&serde_json::json!({
        "text": "Hello world",
        "createdAt": "2025-01-01T00:00:00Z"
    }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := `{ "text": "Hello world", "createdAt": "2025-01-01T00:00:00Z" }`
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/xrpc/xyz.statusphere.createRecord", bytes.NewBufferString(body))
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/xrpc/xyz.statusphere.createRecord \
  -H "X-Client-Key: $CLIENT_KEY" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{ "text": "Hello world", "createdAt": "2025-01-01T00:00:00Z" }'
```

## Use case

This is the simplest possible write procedure. It works well when the client is responsible for populating all record fields and no server-side validation or transformation is needed.

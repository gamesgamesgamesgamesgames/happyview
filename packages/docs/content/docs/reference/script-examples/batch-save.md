---
title: "Batch Save"
---

Create several records from one request.

**Lexicon type:** procedure

```lua
local json = require("internal.json")
local record = require("happyview.record")

function handle(input, ctx)
  local uris = {}
  for _, item in ipairs(input.items) do
    local ref = record.create(ctx.collection, item)
    uris[#uris + 1] = ref.uri
  end
  return { uris = json.to_array(uris) }
end
```

## How it works

1. Iterate over `input.items` and create each one with `record.create` on [`happyview.record`](../../api-reference/lua/libraries.md).
2. Collect the resulting AT URIs and return them. `json.to_array` keeps `uris` a JSON array when `items` is empty.

Writes happen one at a time, in order. If one fails, the records before it exist and the ones after it don't, and the error reaches the client as a 500; wrap `record.create` in `pcall` to report partial progress instead.

## Usage

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/xrpc/xyz.statusphere.batchCreate", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    Authorization: `Bearer ${TOKEN}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    items: [
      { text: "First", createdAt: "2025-01-01T00:00:00Z" },
      { text: "Second", createdAt: "2025-01-01T00:01:00Z" },
    ],
  }),
});
const data = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/xrpc/xyz.statusphere.batchCreate", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    Authorization: `Bearer ${TOKEN}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    items: [
      { text: "First", createdAt: "2025-01-01T00:00:00Z" },
      { text: "Second", createdAt: "2025-01-01T00:01:00Z" },
    ],
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("http://127.0.0.1:3000/xrpc/xyz.statusphere.batchCreate")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("Bearer {}", token))
    .json(&serde_json::json!({
        "items": [
            { "text": "First", "createdAt": "2025-01-01T00:00:00Z" },
            { "text": "Second", "createdAt": "2025-01-01T00:01:00Z" }
        ]
    }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := `{
  "items": [
    { "text": "First", "createdAt": "2025-01-01T00:00:00Z" },
    { "text": "Second", "createdAt": "2025-01-01T00:01:00Z" }
  ]
}`
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/xrpc/xyz.statusphere.batchCreate", bytes.NewBufferString(body))
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/xrpc/xyz.statusphere.batchCreate \
  -H "X-Client-Key: $CLIENT_KEY" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "items": [
      { "text": "First", "createdAt": "2025-01-01T00:00:00Z" },
      { "text": "Second", "createdAt": "2025-01-01T00:01:00Z" }
    ]
  }'
```

## Use case

Batch saving is useful when a single user action should create multiple records (e.g. importing data, multi-step forms).

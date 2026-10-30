---
title: "Signed Record"
---

Save a record with an attestation signature attached.

**Lexicon type:** procedure

```lua
local time = require("internal.time")
local record = require("happyview.record")
local atproto = require("happyview.atproto")

function handle(input, ctx)
  local body = { text = input.text, createdAt = time.to_iso8601(time.now()) }
  local ref = record.create(ctx.collection, body)

  local ok, sig = pcall(atproto.sign, body)
  if not ok then
    sig = nil
  end

  return { uri = ref.uri, cid = ref.cid, signature = sig }
end
```

## How it works

1. Create and save the record.
2. Sign the same fields with `sign` on [`happyview.atproto`](../../api-reference/lua/libraries.md). The `pcall` lets the script work without a signer configured, where `sign` raises `NO_SIGNER`.
3. Return the signature alongside the URI.

## Usage

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/xrpc/xyz.example.createPost", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    Authorization: `Bearer ${TOKEN}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({ text: "Hello world" }),
});
const data = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/xrpc/xyz.example.createPost", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    Authorization: `Bearer ${TOKEN}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({ text: "Hello world" }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("http://127.0.0.1:3000/xrpc/xyz.example.createPost")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("Bearer {}", token))
    .json(&serde_json::json!({ "text": "Hello world" }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := `{ "text": "Hello world" }`
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/xrpc/xyz.example.createPost", bytes.NewBufferString(body))
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/xrpc/xyz.example.createPost \
  -H "X-Client-Key: $CLIENT_KEY" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{ "text": "Hello world" }'
```

```json
{
  "uri": "at://did:plc:abc/xyz.example.post/3abc123",
  "cid": "bafyrei...",
  "signature": {
    "$type": "your.app.attestation",
    "key": "did:web:happyview.example.com#attestation",
    "signature": { "$bytes": "..." }
  }
}
```

## Use case

Attestation signatures let clients verify that a record was processed by your HappyView instance — useful for contributions, moderation decisions, or cross-instance data where provenance matters. The signature covers both the record content and the author's DID, so it can't be replayed across users or tampered with.

See [Attestation Signing](../../guides/attestation-signing.md) for setup and configuration, or [Verify Signed Record](signed-record-verify.md) for the read-side counterpart.

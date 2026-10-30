---
title: "Sidecar Records"
---

Create two records with different collection NSIDs but the same rkey, linking them together by key.

**Lexicon type:** procedure

```lua
local time = require("internal.time")
local tids = require("internal.tids")
local record = require("happyview.record")

function handle(input, ctx)
  local rkey = tids.create()
  local ts = time.to_iso8601(time.now())

  local post = record.create("xyz.statusphere.post", {
    text = input.text,
    createdAt = ts,
  }, { rkey = rkey })

  local metadata = record.create("xyz.statusphere.postMetadata", {
    lang = input.lang or "en",
    source = input.source or "web",
    createdAt = ts,
  }, { rkey = rkey })

  return { post = post, metadata = metadata }
end
```

## How it works

1. Mint a single TID with [`tids.create()`](../../api-reference/lua/built-in-modules.md#internaltids) to use as the rkey for both records.
2. Create a record in each collection with `record.create`, passing the shared rkey in `opts`.
3. Return both `{ uri, cid }` results so the client knows the identity of each record.

## Usage

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/xrpc/xyz.statusphere.createPost", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    Authorization: `Bearer ${TOKEN}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    text: "Hello world",
    lang: "en",
    source: "web",
  }),
});
const data = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/xrpc/xyz.statusphere.createPost", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    Authorization: `Bearer ${TOKEN}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    text: "Hello world",
    lang: "en",
    source: "web",
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("http://127.0.0.1:3000/xrpc/xyz.statusphere.createPost")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("Bearer {}", token))
    .json(&serde_json::json!({
        "text": "Hello world",
        "lang": "en",
        "source": "web"
    }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := `{ "text": "Hello world", "lang": "en", "source": "web" }`
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/xrpc/xyz.statusphere.createPost", bytes.NewBufferString(body))
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/xrpc/xyz.statusphere.createPost \
  -H "X-Client-Key: $CLIENT_KEY" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{ "text": "Hello world", "lang": "en", "source": "web" }'
```

The response includes URIs for both the post and its metadata:

```json
{
  "post": {
    "uri": "at://did:plc:abc/xyz.statusphere.post/3abc123",
    "cid": "bafyrei..."
  },
  "metadata": {
    "uri": "at://did:plc:abc/xyz.statusphere.postMetadata/3abc123",
    "cid": "bafyrei..."
  }
}
```

## Use case

Sidecar records are useful when you want to associate related data across collections without embedding everything in a single record. Because they share an rkey, you can derive one URI from the other:

```
at:// did:plc:abc /xyz.statusphere.post         /3abc123
at:// did:plc:abc /xyz.statusphere.postMetadata /3abc123
                                                 ^^^^^^^ same rkey
```

This is a common atproto pattern for keeping a primary record lean while storing auxiliary data (metadata, reactions, settings) in a companion collection.

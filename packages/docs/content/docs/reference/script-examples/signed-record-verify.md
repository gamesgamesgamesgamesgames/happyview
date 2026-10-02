---
title: "Verify Signed Record"
---

Fetch a record and verify its attestation signature.

**Lexicon type:** query

```lua
function handle()
  local record = db.get(params.uri)
  if not record then
    return { error = "not found" }
  end

  local verified = false
  if atproto.verify_signature and record.signature then
    local ok, result = pcall(
      atproto.verify_signature,
      { text = record.text, createdAt = record.createdAt },
      record.signature,
      params.did
    )
    if not ok then
      -- We could not check. Say so — don't report it as a bad signature.
      return { record = record, verified = nil, error = tostring(result) }
    end
    verified = result
  end

  return { record = record, verified = verified }
end
```

## How it works

1. Fetch the record by AT URI.
2. If a signature is present, rebuild the same field table that was signed and verify it with [`atproto.verify_signature()`](../../api-reference/lua/atproto-api.md#atprotoverify_signature).
3. Return `verified = true` if the signature is valid, `false` if it's missing, doesn't match, or the signer isn't configured.

The `pcall` matters. `verify_signature` returns `false` only when it checked and the signature didn't match; it *raises* when it couldn't check at all — unparseable signature bytes, a missing field. Assigning that to `verified` would report a decode bug as a forged record.

## Usage

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/xrpc/xyz.example.getPost?uri=at://did:plc:abc/xyz.example.post/3abc123&did=did:plc:abc",
);
const data = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/xrpc/xyz.example.getPost?uri=at://did:plc:abc/xyz.example.post/3abc123&did=did:plc:abc",
);
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("http://127.0.0.1:3000/xrpc/xyz.example.getPost")
    .query(&[
        ("uri", "at://did:plc:abc/xyz.example.post/3abc123"),
        ("did", "did:plc:abc"),
    ])
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/xrpc/xyz.example.getPost?uri=at://did:plc:abc/xyz.example.post/3abc123&did=did:plc:abc", nil)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl "http://127.0.0.1:3000/xrpc/xyz.example.getPost?uri=at://did:plc:abc/xyz.example.post/3abc123&did=did:plc:abc"
```

```json
{
  "record": {
    "uri": "at://did:plc:abc/xyz.example.post/3abc123",
    "text": "Hello world",
    "createdAt": "2026-04-30T12:00:00Z"
  },
  "verified": true
}
```

## Use case

Pair this with the [Signed Record](signed-record.md) procedure to create a write-then-verify flow. The query re-derives the CID from the same fields that were originally signed, so any tampering between write and read is caught.

See [Attestation Signing](../../guides/attestation-signing.md) for setup and configuration.

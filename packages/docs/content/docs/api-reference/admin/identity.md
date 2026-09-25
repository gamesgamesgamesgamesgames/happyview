---
title: "Identity"
---

Resolve accounts for display in admin tools.

```ts tab="TypeScript" tab-group="language"
const TOKEN = "hv_..."; // your API key
const headers = { Authorization: `Bearer ${TOKEN}` };
```
```js tab="JavaScript" tab-group="language"
const TOKEN = "hv_..."; // your API key
const headers = { Authorization: `Bearer ${TOKEN}` };
```
```rust tab="Rust" tab-group="language"
let token = "hv_..."; // your API key
```
```go tab="Go" tab-group="language"
token := "hv_..." // your API key
```
```sh tab="cURL" tab-group="language"
# All examples assume $TOKEN is an API key (hv_...)
AUTH="Authorization: Bearer $TOKEN"
```

## Resolve a handle or DID

```
GET /admin/identity/resolve?identifier=<handle-or-did>
```

Requires authentication. No specific permission is needed.

```ts tab="TypeScript" tab-group="language"
interface ResolvedIdentity {
  did: string;
  handle: string | null;
}

const params = new URLSearchParams({ identifier: "alice.bsky.social" });
const response = await fetch(
  `http://127.0.0.1:3000/admin/identity/resolve?${params}`,
  { headers },
);
const data: ResolvedIdentity = await response.json();
```
```js tab="JavaScript" tab-group="language"
const params = new URLSearchParams({ identifier: "alice.bsky.social" });
const response = await fetch(
  `http://127.0.0.1:3000/admin/identity/resolve?${params}`,
  { headers },
);
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let client = reqwest::Client::new();
let response = client
    .get("http://127.0.0.1:3000/admin/identity/resolve")
    .query(&[("identifier", "alice.bsky.social")])
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/identity/resolve?identifier=alice.bsky.social", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl "http://127.0.0.1:3000/admin/identity/resolve?identifier=alice.bsky.social" -H "$AUTH"
```

| Parameter    | Type   | Required | Description                                        |
| ------------ | ------ | -------- | -------------------------------------------------- |
| `identifier` | string | yes      | A handle (a leading `@` is allowed) or a DID       |

**Response**: `200 OK`

```json
{
  "did": "did:plc:abc123",
  "handle": "alice.bsky.social"
}
```

`handle` is `null` unless the DID document lists the handle in `alsoKnownAs` and the handle resolves to the same DID.

Returns `400` for malformed input, a handle that cannot be resolved, a DID whose document cannot be fetched, or a resolution that takes longer than 10 seconds. The error names the identifier and a general reason, never the underlying network error.

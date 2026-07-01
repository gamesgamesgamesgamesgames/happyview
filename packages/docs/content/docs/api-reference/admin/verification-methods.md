---
title: "Verification Methods"
---

Manage DID verification methods (P-256 keypairs) used for attestation signing and PLC operations.

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

## List verification methods

```
GET /admin/verification-methods
```

Requires `settings:manage` permission.

```ts tab="TypeScript" tab-group="language"
interface VerificationMethod {
  id: string;
  fragment_id: string;
  key_type: string;
  public_key_multibase: string;
  created_at: string;
}

const response = await fetch("http://127.0.0.1:3000/admin/verification-methods", {
  headers,
});
const data: VerificationMethod[] = await response.json();
```

```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/verification-methods", {
  headers,
});
const data = await response.json();
```

```rust tab="Rust" tab-group="language"
let response = client
    .get("http://127.0.0.1:3000/admin/verification-methods")
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```

```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/verification-methods", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```

```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/verification-methods -H "$AUTH"
```

**Response**: `200 OK`

```json
[
  {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "fragment_id": "#attestation",
    "key_type": "Multikey",
    "public_key_multibase": "zDnae...",
    "created_at": "2026-06-27T00:00:00Z"
  }
]
```

## Create a verification method

```
POST /admin/verification-methods
```

Requires `settings:manage` permission. Generates a new P-256 keypair. The private key is encrypted at rest with AES-256-GCM using `TOKEN_ENCRYPTION_KEY`.

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/verification-methods", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    fragment_id: "#attestation",
  }),
});
const data: VerificationMethod = await response.json();
```

```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/verification-methods", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    fragment_id: "#attestation",
  }),
});
const data = await response.json();
```

```rust tab="Rust" tab-group="language"
let response = client
    .post("http://127.0.0.1:3000/admin/verification-methods")
    .bearer_auth(token)
    .json(&serde_json::json!({
        "fragment_id": "#attestation"
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```

```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{"fragment_id": "#attestation"}`)
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/admin/verification-methods", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```

```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/verification-methods \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{"fragment_id": "#attestation"}'
```

| Field         | Type   | Required | Description                                                                       |
| ------------- | ------ | -------- | --------------------------------------------------------------------------------- |
| `fragment_id` | string | yes      | DID document fragment identifier, must start with `#` followed by alphanumerics or underscores |

**Response**: `201 Created`

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "fragment_id": "#attestation",
  "key_type": "Multikey",
  "public_key_multibase": "zDnae...",
  "created_at": "2026-06-27T00:00:00Z"
}
```

## Delete a verification method

```
DELETE /admin/verification-methods/{fragment_id}
```

Requires `settings:manage` permission.

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/verification-methods/attestation",
  { method: "DELETE", headers },
);
```

```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/verification-methods/attestation",
  { method: "DELETE", headers },
);
```

```rust tab="Rust" tab-group="language"
client
    .delete("http://127.0.0.1:3000/admin/verification-methods/attestation")
    .bearer_auth(token)
    .send()
    .await?;
```

```go tab="Go" tab-group="language"
req, _ := http.NewRequest("DELETE",
  "http://127.0.0.1:3000/admin/verification-methods/attestation", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```

```sh tab="cURL" tab-group="language"
curl -X DELETE http://127.0.0.1:3000/admin/verification-methods/attestation \
  -H "$AUTH"
```

The `fragment_id` path parameter is the identifier without the leading `#` (e.g. `attestation` for `#attestation`).

**Response**: `204 No Content`. Returns `404 Not Found` if no method with that fragment ID exists.

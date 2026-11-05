---
title: "Domains"
---

Manage the domains a HappyView instance serves. Each domain gets its own atproto OAuth client identity. The primary domain is set from `PUBLIC_URL` on first boot. All endpoints require the `settings:manage` permission.

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

## List domains

```
GET /admin/domains
```

```ts tab="TypeScript" tab-group="language"
interface Domain {
  id: string;
  url: string;
  is_primary: boolean;
  created_at: string;
  updated_at: string;
}

const response = await fetch("http://127.0.0.1:3000/admin/domains", {
  headers,
});
const data: Domain[] = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/domains", {
  headers,
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let client = reqwest::Client::new();
let response = client
    .get("http://127.0.0.1:3000/admin/domains")
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/domains", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/domains -H "$AUTH"
```

**Response**: `200 OK`

```json
[
  {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "url": "https://gamesgamesgamesgames.games",
    "is_primary": true,
    "created_at": "2026-04-16T00:00:00Z",
    "updated_at": "2026-04-16T00:00:00Z"
  }
]
```

## Add a domain

```
POST /admin/domains
```

```ts tab="TypeScript" tab-group="language"
interface Domain {
  id: string;
  url: string;
  is_primary: boolean;
  created_at: string;
  updated_at: string;
}

const response = await fetch("http://127.0.0.1:3000/admin/domains", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    url: "https://api.example.com",
  }),
});
const data: Domain = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/domains", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    url: "https://api.example.com",
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("http://127.0.0.1:3000/admin/domains")
    .bearer_auth(token)
    .json(&serde_json::json!({
        "url": "https://api.example.com"
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{"url": "https://api.example.com"}`)
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/admin/domains", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/domains \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{ "url": "https://api.example.com" }'
```

| Field | Type   | Required | Description                                                                                                         |
| ----- | ------ | -------- | ------------------------------------------------------------------------------------------------------------------- |
| `url` | string | yes      | Valid origin (scheme + host, no path or trailing slash). Must be `https` unless `PUBLIC_URL` is a loopback address. |

Returns `400 Bad Request` if the URL is invalid or already registered.

**Response**: `201 Created`

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440001",
  "url": "https://api.example.com",
  "is_primary": false,
  "created_at": "2026-04-16T00:00:00Z",
  "updated_at": "2026-04-16T00:00:00Z"
}
```

Also builds an OAuth client for the domain and updates the in-memory cache.

## Remove a domain

```
DELETE /admin/domains/{id}
```

```ts tab="TypeScript" tab-group="language"
await fetch("http://127.0.0.1:3000/admin/domains/550e8400-e29b-41d4-a716-446655440001", {
  method: "DELETE",
  headers,
});
```
```js tab="JavaScript" tab-group="language"
await fetch("http://127.0.0.1:3000/admin/domains/550e8400-e29b-41d4-a716-446655440001", {
  method: "DELETE",
  headers,
});
```
```rust tab="Rust" tab-group="language"
client
    .delete("http://127.0.0.1:3000/admin/domains/550e8400-e29b-41d4-a716-446655440001")
    .bearer_auth(token)
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("DELETE", "http://127.0.0.1:3000/admin/domains/550e8400-e29b-41d4-a716-446655440001", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X DELETE http://127.0.0.1:3000/admin/domains/550e8400-e29b-41d4-a716-446655440001 \
  -H "$AUTH"
```

Returns `400 Bad Request` if the domain is the primary domain (make another domain primary first), or if it's the instance's only domain. Returns `404 Not Found` if the domain doesn't exist.

**Response**: `204 No Content`

Also removes the domain's OAuth client and cache entry.

## Set primary domain

```
POST /admin/domains/{id}/primary
```

```ts tab="TypeScript" tab-group="language"
await fetch("http://127.0.0.1:3000/admin/domains/550e8400-e29b-41d4-a716-446655440001/primary", {
  method: "POST",
  headers,
});
```
```js tab="JavaScript" tab-group="language"
await fetch("http://127.0.0.1:3000/admin/domains/550e8400-e29b-41d4-a716-446655440001/primary", {
  method: "POST",
  headers,
});
```
```rust tab="Rust" tab-group="language"
client
    .post("http://127.0.0.1:3000/admin/domains/550e8400-e29b-41d4-a716-446655440001/primary")
    .bearer_auth(token)
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/admin/domains/550e8400-e29b-41d4-a716-446655440001/primary", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/domains/550e8400-e29b-41d4-a716-446655440001/primary \
  -H "$AUTH"
```

Sets the target domain as the primary. Unsets the current primary in a single operation. Returns `404 Not Found` if the domain doesn't exist.

**Response**: `204 No Content`

Also updates the in-memory cache and primary client reference.

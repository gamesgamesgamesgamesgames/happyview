---
title: "Labelers"
---

Manage external labeler subscriptions. See the [Labelers guide](../../guides/labelers.md) for background.

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

## Add a labeler

```
POST /admin/labelers
```

Requires `labelers:create` permission.

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/labelers", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({ did: "did:plc:ar7c4by46qjdydhdevvrndac" }),
});
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/labelers", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({ did: "did:plc:ar7c4by46qjdydhdevvrndac" }),
});
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("http://127.0.0.1:3000/admin/labelers")
    .bearer_auth(token)
    .json(&serde_json::json!({ "did": "did:plc:ar7c4by46qjdydhdevvrndac" }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{ "did": "did:plc:ar7c4by46qjdydhdevvrndac" }`)
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/admin/labelers", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/labelers \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{ "did": "did:plc:ar7c4by46qjdydhdevvrndac" }'
```

| Field | Type   | Required | Description                   |
| ----- | ------ | -------- | ----------------------------- |
| `did` | string | yes      | The labeler's atproto DID |

**Response**: `201 Created` (empty body)

## List labelers

```
GET /admin/labelers
```

Requires `labelers:read` permission.

```ts tab="TypeScript" tab-group="language"
interface Labeler {
  did: string;
  status: string;
  cursor: number | null;
  created_at: string;
  updated_at: string;
  declared: boolean;
}

const response = await fetch("http://127.0.0.1:3000/admin/labelers", {
  headers,
});
const data: Labeler[] = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/labelers", {
  headers,
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("http://127.0.0.1:3000/admin/labelers")
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/labelers", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/labelers -H "$AUTH"
```

**Response**: `200 OK`

```json
[
  {
    "did": "did:plc:ar7c4by46qjdydhdevvrndac",
    "status": "active",
    "cursor": 1234,
    "created_at": "2026-03-15T00:00:00Z",
    "updated_at": "2026-03-15T00:00:00Z"
  }
]
```

| Field        | Type         | Description                                        |
| ------------ | ------------ | -------------------------------------------------- |
| `did`        | string       | The labeler's DID                                  |
| `status`     | string       | `active` or `paused`                               |
| `cursor`     | number\|null | Last processed event cursor (null if never synced) |
| `created_at` | string       | ISO 8601 creation timestamp                        |
| `updated_at` | string       | ISO 8601 last-updated timestamp                    |

## Update a labeler

```
PATCH /admin/labelers/{did}
```

Requires `labelers:create` permission.

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/labelers/did:plc:ar7c4by46qjdydhdevvrndac",
  {
    method: "PATCH",
    headers: {
      ...headers,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ status: "paused" }),
  },
);
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/labelers/did:plc:ar7c4by46qjdydhdevvrndac",
  {
    method: "PATCH",
    headers: {
      ...headers,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ status: "paused" }),
  },
);
```
```rust tab="Rust" tab-group="language"
let response = client
    .patch("http://127.0.0.1:3000/admin/labelers/did:plc:ar7c4by46qjdydhdevvrndac")
    .bearer_auth(token)
    .json(&serde_json::json!({ "status": "paused" }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{ "status": "paused" }`)
req, _ := http.NewRequest("PATCH", "http://127.0.0.1:3000/admin/labelers/did:plc:ar7c4by46qjdydhdevvrndac", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X PATCH http://127.0.0.1:3000/admin/labelers/did:plc:ar7c4by46qjdydhdevvrndac \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{ "status": "paused" }'
```

| Field    | Type   | Required | Description                      |
| -------- | ------ | -------- | -------------------------------- |
| `status` | string | yes      | New status: `active` or `paused` |

**Response**: `200 OK`

## Delete a labeler

```
DELETE /admin/labelers/{did}
```

Requires `labelers:delete` permission. Removes the subscription and all labels emitted by this labeler.

`declared` is `true` for a labeler the project's `happyview.toml` subscribes to under `[labels] subscribe`. Deleting one returns `409 Conflict`; remove it from the file instead.

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/labelers/did:plc:ar7c4by46qjdydhdevvrndac",
  {
    method: "DELETE",
    headers,
  },
);
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/labelers/did:plc:ar7c4by46qjdydhdevvrndac",
  {
    method: "DELETE",
    headers,
  },
);
```
```rust tab="Rust" tab-group="language"
let response = client
    .delete("http://127.0.0.1:3000/admin/labelers/did:plc:ar7c4by46qjdydhdevvrndac")
    .bearer_auth(token)
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("DELETE", "http://127.0.0.1:3000/admin/labelers/did:plc:ar7c4by46qjdydhdevvrndac", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X DELETE http://127.0.0.1:3000/admin/labelers/did:plc:ar7c4by46qjdydhdevvrndac \
  -H "$AUTH"
```

**Response**: `204 No Content`

---
title: "Stats"
---

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

## Record counts

```
GET /admin/stats
```

```ts tab="TypeScript" tab-group="language"
interface CollectionCount {
  collection: string;
  count: number;
}

interface Stats {
  total_records: number;
  collections: CollectionCount[];
}

const response = await fetch("http://127.0.0.1:3000/admin/stats", {
  headers,
});
const data: Stats = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/stats", {
  headers,
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let client = reqwest::Client::new();
let response = client
    .get("http://127.0.0.1:3000/admin/stats")
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/stats", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/stats -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "total_records": 12345,
  "collections": [{ "collection": "xyz.statusphere.status", "count": 500 }]
}
```

## Admin and client counts

```
GET /admin/stats/counts
```

Returns the number of admin users (seats, including the super user) and registered API clients. It is cheap to call often, unlike the record counts above. Requires `stats:read`.

```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/stats/counts -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "users": 3,
  "api_clients": 2
}
```

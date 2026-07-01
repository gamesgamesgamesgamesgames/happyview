---
title: "Feature Flags"
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

## List feature flags

```
GET /admin/feature-flags
```

Returns all feature flags and their current status. Flags are backed by the `happyview_instance_settings` table — a flag is enabled when its key is set to `"true"`.

```ts tab="TypeScript" tab-group="language"
interface FeatureFlag {
  key: string;
  name: string;
  description: string;
  enabled: boolean;
}

const response = await fetch("http://127.0.0.1:3000/admin/feature-flags", {
  headers,
});
const data: FeatureFlag[] = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/feature-flags", {
  headers,
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let client = reqwest::Client::new();
let response = client
    .get("http://127.0.0.1:3000/admin/feature-flags")
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/feature-flags", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/feature-flags -H "$AUTH"
```

**Response**: `200 OK`

```json
[
  {
    "key": "feature.spaces_enabled",
    "name": "Permissioned Spaces",
    "description": "Collaborative data spaces with granular permissions, membership, and invites.",
    "enabled": false
  }
]
```

| Field         | Type    | Description                                  |
| ------------- | ------- | -------------------------------------------- |
| `key`         | string  | Settings key for the flag                    |
| `name`        | string  | Human-readable name                          |
| `description` | string  | What the flag controls                       |
| `enabled`     | boolean | Whether the flag is currently enabled        |

To toggle a flag, use `PUT /admin/settings/{key}` with the value `"true"` or `"false"`. See [Instance Settings](settings.md) for details.

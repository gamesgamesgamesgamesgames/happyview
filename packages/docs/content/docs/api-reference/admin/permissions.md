---
title: "Permissions"
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

## List permissions

```
GET /admin/permissions
```

Returns all available permission definitions and permission templates. When the Spaces feature flag is disabled, spaces-related permissions and template entries are excluded.

```ts tab="TypeScript" tab-group="language"
interface PermissionDef {
  key: string;
  name: string;
  description: string;
  category: string;
}

interface PermissionTemplate {
  key: string;
  label: string;
  permissions: string[];
}

interface PermissionsResponse {
  permissions: PermissionDef[];
  templates: PermissionTemplate[];
}

const response = await fetch("http://127.0.0.1:3000/admin/permissions", {
  headers,
});
const data: PermissionsResponse = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/permissions", {
  headers,
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let client = reqwest::Client::new();
let response = client
    .get("http://127.0.0.1:3000/admin/permissions")
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/permissions", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/permissions -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "permissions": [
    {
      "key": "lexicons:create",
      "name": "Create Lexicons",
      "description": "Upload lexicon schemas",
      "category": "Lexicons"
    }
  ],
  "templates": [
    {
      "key": "viewer",
      "label": "Viewer",
      "permissions": ["lexicons:read", "records:read", "stats:read", "events:read"]
    }
  ]
}
```

Templates are predefined permission bundles used when creating or updating users. See the [Permissions guide](../../guides/permissions.md) for the full list.

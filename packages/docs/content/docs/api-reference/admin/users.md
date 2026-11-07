---
title: "Users"
---

Manage admin users and their permissions. See the [Permissions guide](../../guides/permissions.md) for available permissions and templates.

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

## Create a user

```
POST /admin/users
```

Requires `users:create` permission. You cannot grant permissions you don't have yourself (escalation guard).

```ts tab="TypeScript" tab-group="language"
interface User {
  id: string;
  did: string;
  is_super: boolean;
  permissions: string[];
}

const response = await fetch("http://127.0.0.1:3000/admin/users", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    did: "did:plc:newuser",
    template: "maintainer",
  }),
});
const data: User = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/users", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    did: "did:plc:newuser",
    template: "maintainer",
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("http://127.0.0.1:3000/admin/users")
    .bearer_auth(token)
    .json(&serde_json::json!({
        "did": "did:plc:newuser",
        "template": "maintainer"
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "did": "did:plc:newuser",
  "template": "maintainer"
}`)
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/admin/users", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/users \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{
    "did": "did:plc:newuser",
    "template": "maintainer"
  }'
```

| Field         | Type     | Required | Description                                                                        |
| ------------- | -------- | -------- | ---------------------------------------------------------------------------------- |
| `did`         | string   | yes      | The atproto DID of the user to add                                             |
| `template`    | string   | no       | Permission template: `viewer`, `maintainer`, `manager`, or `full_access`. `operator`, the old name for `maintainer`, is still accepted |
| `permissions` | string[] | no       | Explicit list of permissions to grant (used instead of or in addition to `template`) |

If neither `template` nor `permissions` is provided, the user is created with no permissions.

**Response**: `201 Created`

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "did": "did:plc:newuser",
  "is_super": false,
  "permissions": ["lexicons:read", "records:read", "script-variables:read", "users:read", "api-keys:read", "api-keys:create", "api-keys:delete", "backfill:read", "backfill:create", "stats:read", "events:read"]
}
```

## List users

```
GET /admin/users
```

Requires `users:read` permission.

```ts tab="TypeScript" tab-group="language"
interface UserWithTimestamps {
  id: string;
  did: string;
  is_super: boolean;
  permissions: string[];
  created_at: string;
  last_used_at: string;
}

const response = await fetch("http://127.0.0.1:3000/admin/users", {
  headers,
});
const data: UserWithTimestamps[] = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/users", {
  headers,
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("http://127.0.0.1:3000/admin/users")
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/users", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/users -H "$AUTH"
```

**Response**: `200 OK`

```json
[
  {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "did": "did:plc:admin",
    "is_super": true,
    "permissions": ["lexicons:create", "lexicons:read", "lexicons:delete", "records:read", "records:delete", "records:delete-collection", "script-variables:create", "script-variables:read", "script-variables:delete", "users:create", "users:read", "users:update", "users:delete", "api-keys:create", "api-keys:read", "api-keys:delete", "backfill:create", "backfill:read", "stats:read", "events:read"],
    "created_at": "2025-01-01T00:00:00Z",
    "last_used_at": "2025-01-02T12:00:00Z"
  }
]
```

## Get a user

```
GET /admin/users/{id}
```

Requires `users:read` permission.

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/users/550e8400-e29b-41d4-a716-446655440000",
  { headers },
);
const data: UserWithTimestamps = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/users/550e8400-e29b-41d4-a716-446655440000",
  { headers },
);
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("http://127.0.0.1:3000/admin/users/550e8400-e29b-41d4-a716-446655440000")
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/users/550e8400-e29b-41d4-a716-446655440000", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/users/550e8400-e29b-41d4-a716-446655440000 -H "$AUTH"
```

**Response**: `200 OK` with the same shape as a single item from the list response.

## Update user permissions

```
PATCH /admin/users/{id}/permissions
```

Requires `users:update` permission. You cannot grant permissions you don't have yourself, and you cannot modify the super user's permissions.

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/users/550e8400-e29b-41d4-a716-446655440000/permissions",
  {
    method: "PATCH",
    headers: {
      ...headers,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      grant: ["lexicons:create", "lexicons:delete"],
      revoke: ["records:delete"],
    }),
  },
);
const data: User = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/users/550e8400-e29b-41d4-a716-446655440000/permissions",
  {
    method: "PATCH",
    headers: {
      ...headers,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      grant: ["lexicons:create", "lexicons:delete"],
      revoke: ["records:delete"],
    }),
  },
);
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .patch("http://127.0.0.1:3000/admin/users/550e8400-e29b-41d4-a716-446655440000/permissions")
    .bearer_auth(token)
    .json(&serde_json::json!({
        "grant": ["lexicons:create", "lexicons:delete"],
        "revoke": ["records:delete"]
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "grant": ["lexicons:create", "lexicons:delete"],
  "revoke": ["records:delete"]
}`)
req, _ := http.NewRequest("PATCH", "http://127.0.0.1:3000/admin/users/550e8400-e29b-41d4-a716-446655440000/permissions", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X PATCH http://127.0.0.1:3000/admin/users/550e8400-e29b-41d4-a716-446655440000/permissions \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{
    "grant": ["lexicons:create", "lexicons:delete"],
    "revoke": ["records:delete"]
  }'
```

| Field    | Type     | Required | Description           |
| -------- | -------- | -------- | --------------------- |
| `grant`  | string[] | no       | Permissions to add    |
| `revoke` | string[] | no       | Permissions to remove |

**Response**: `200 OK` with the updated user object.

## Transfer super user

```
POST /admin/users/transfer-super
```

Only the current super user can call this endpoint. Transfers super user status to another existing user.

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/users/transfer-super", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    target_user_id: "550e8400-e29b-41d4-a716-446655440000",
  }),
});
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/users/transfer-super", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    target_user_id: "550e8400-e29b-41d4-a716-446655440000",
  }),
});
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("http://127.0.0.1:3000/admin/users/transfer-super")
    .bearer_auth(token)
    .json(&serde_json::json!({
        "target_user_id": "550e8400-e29b-41d4-a716-446655440000"
    }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{ "target_user_id": "550e8400-e29b-41d4-a716-446655440000" }`)
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/admin/users/transfer-super", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/users/transfer-super \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{ "target_user_id": "550e8400-e29b-41d4-a716-446655440000" }'
```

| Field            | Type   | Required | Description                                |
| ---------------- | ------ | -------- | ------------------------------------------ |
| `target_user_id` | string | yes      | The ID of the user to receive super status |

**Response**: `200 OK`

## Delete a user

```
DELETE /admin/users/{id}
```

Requires `users:delete` permission. You cannot delete the super user or yourself.

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/users/550e8400-e29b-41d4-a716-446655440000",
  {
    method: "DELETE",
    headers,
  },
);
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/users/550e8400-e29b-41d4-a716-446655440000",
  {
    method: "DELETE",
    headers,
  },
);
```
```rust tab="Rust" tab-group="language"
let response = client
    .delete("http://127.0.0.1:3000/admin/users/550e8400-e29b-41d4-a716-446655440000")
    .bearer_auth(token)
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("DELETE", "http://127.0.0.1:3000/admin/users/550e8400-e29b-41d4-a716-446655440000", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X DELETE http://127.0.0.1:3000/admin/users/550e8400-e29b-41d4-a716-446655440000 \
  -H "$AUTH"
```

**Response**: `204 No Content`

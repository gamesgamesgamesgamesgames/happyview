---
title: "OAuth Keys"
---

Manage the ES256 key HappyView signs OAuth client assertions with. As a confidential OAuth client, HappyView authenticates to PDSes with `private_key_jwt` rather than as a public client — which is what earns its sessions the 2-year lifetime cap instead of 2 weeks. All endpoints require the `settings:manage` permission.

Every session records the `kid` of the key that established it and is refreshed with **that** key, never with whichever key happens to be current later. That is what makes rotation safe: an authorization server treats a mismatched `kid` as grounds to destroy the session, not merely to refuse the request.

Two operations, with very different consequences:

- **Rotate** mints a new key and marks the previous one `retiring`. A retiring key stays published in the JWKS and keeps signing every session established with it, so nothing is logged out. Rotation is safe and cheap.
- **Revoke** removes a key from the JWKS immediately and permanently breaks every session pinned to it. It is the response to a leaked or compromised key, not routine maintenance.

Because of that difference, **the current key cannot be revoked** — doing so would leave the instance unable to authenticate to any PDS at all. To contain a leaked key, rotate first (which demotes the leaked key to `retiring` and mints a fresh `current`), then revoke the retiring key.

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

## List keys

```
GET /admin/oauth/instance-key
```

Returns every key the instance holds, ordered `current`, then `retiring`, then `revoked`. Revoked keys are included deliberately, so an operator can confirm a revoke took effect.

`session_count` is the number of live sessions pinned to that key, counted across the dashboard, linked-repo, and DPoP session tables. For a **revoked** key the number is retained rather than zeroed: revoking does not delete session rows, so the count tells you how many users that revoke forced to re-authenticate.

```ts tab="TypeScript" tab-group="language"
interface InstanceKey {
  kid: string;
  status: "current" | "retiring" | "revoked";
  created_at: string;
  session_count: number;
}

const response = await fetch(
  "http://127.0.0.1:3000/admin/oauth/instance-key",
  { headers },
);
const data: { keys: InstanceKey[] } = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/oauth/instance-key",
  { headers },
);
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let client = reqwest::Client::new();
let response = client
    .get("http://127.0.0.1:3000/admin/oauth/instance-key")
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/oauth/instance-key", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -H "$AUTH" http://127.0.0.1:3000/admin/oauth/instance-key
```

```json
{
  "keys": [
    {
      "kid": "f0c3a1e2-...",
      "status": "current",
      "created_at": "2026-08-29T10:14:02Z",
      "session_count": 128
    },
    {
      "kid": "8b41d0aa-...",
      "status": "retiring",
      "created_at": "2026-02-11T08:02:55Z",
      "session_count": 37
    }
  ]
}
```

## Rotate the key

```
POST /admin/oauth/instance-key/rotate
```

Mints a new `current` key and demotes the previous one to `retiring`. Nothing is logged out: the retiring key stays in the published JWKS and keeps signing refreshes for every session established with it. New sessions use the new key.

`orphaned_sessions` counts sessions that predate key pinning and therefore carry no recorded `kid`. These cannot be protected by this or any future rotation.

```ts tab="TypeScript" tab-group="language"
interface RotationResult {
  kid: string;
  orphaned_sessions: number;
}

const response = await fetch(
  "http://127.0.0.1:3000/admin/oauth/instance-key/rotate",
  { method: "POST", headers },
);
const data: RotationResult = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/oauth/instance-key/rotate",
  { method: "POST", headers },
);
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let client = reqwest::Client::new();
let response = client
    .post("http://127.0.0.1:3000/admin/oauth/instance-key/rotate")
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/admin/oauth/instance-key/rotate", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST -H "$AUTH" http://127.0.0.1:3000/admin/oauth/instance-key/rotate
```

```json
{
  "kid": "f0c3a1e2-...",
  "orphaned_sessions": 0
}
```

## Revoke a key

```
DELETE /admin/oauth/instance-key/{kid}
```

Removes the key from the published JWKS immediately and destroys every session pinned to it — those users must log in again. Use this when a key has leaked, not to tidy up old keys; a retiring key with no remaining sessions is swept automatically.

`sessions_destroyed` reports how many sessions the revoke invalidated.

Errors:

| Status | Meaning |
| ------ | ------- |
| `400`  | The target is the `current` key. Rotate first, then revoke the retiring key. |
| `404`  | No key with that `kid` belongs to this instance. |

```ts tab="TypeScript" tab-group="language"
interface RevokeResult {
  kid: string;
  sessions_destroyed: number;
}

const response = await fetch(
  `http://127.0.0.1:3000/admin/oauth/instance-key/${kid}`,
  { method: "DELETE", headers },
);
const data: RevokeResult = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  `http://127.0.0.1:3000/admin/oauth/instance-key/${kid}`,
  { method: "DELETE", headers },
);
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let client = reqwest::Client::new();
let response = client
    .delete(format!(
        "http://127.0.0.1:3000/admin/oauth/instance-key/{kid}"
    ))
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
url := "http://127.0.0.1:3000/admin/oauth/instance-key/" + kid
req, _ := http.NewRequest("DELETE", url, nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X DELETE -H "$AUTH" \
  "http://127.0.0.1:3000/admin/oauth/instance-key/$KID"
```

```json
{
  "kid": "8b41d0aa-...",
  "sessions_destroyed": 37
}
```

## Responding to a leaked key

1. `POST /admin/oauth/instance-key/rotate` — the leaked key becomes `retiring`, a fresh key becomes `current`, and the instance keeps working throughout.
2. `GET /admin/oauth/instance-key` — confirm the leaked key is now `retiring` and note its `session_count`, which is how many users step 3 will sign out.
3. `DELETE /admin/oauth/instance-key/{kid}` on the leaked key.

Doing this in the other order is refused: the current key cannot be revoked, because that would leave the instance with no key to authenticate with.

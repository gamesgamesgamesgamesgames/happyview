---
title: "API Clients"
---

API clients identify third-party applications that call HappyView's XRPC endpoints. Every request — authenticated or not — needs an `X-Client-Key` header (or `client_key` query param). Requests without one get `401 Unauthorized`. The client key is HappyView's rate-limit bucket.

A single API client represents your application, not individual users. Create one client for your app and use the same client key across all instances. Users authenticate separately via OAuth — the client key identifies _your app_, not _who is using it_.

Each client has an `hvc_`-prefixed client key and an `hvs_`-prefixed client secret. The secret is only returned at creation and is sha256-hashed in the database. Server-to-server callers pass the secret as `X-Client-Secret`. Browser callers use the `Origin` header, which is matched against the client's `client_uri`. Mismatches currently log warnings rather than rejecting the request, but rate limiting applies either way. See [Authentication — XRPC](../../getting-started/authentication.md#xrpc-api-client-identification) for the client-side view, and the [API Keys guide](../../guides/api-keys.md) for how admin API keys differ from API clients.

<Callout type="idea" title="Third-Party API Clients">
Third-party apps can also create, list, and delete their own API clients programmatically via the [XRPC API](../oauth/api-clients.md), without needing admin access.
</Callout>

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

## List API clients

```
GET /admin/api-clients
```

Requires `api-clients:view`. Returns clients ordered by `created_at` descending. Secrets are never returned.

```ts tab="TypeScript" tab-group="language"
interface ApiClient {
  id: string;
  client_key: string;
  name: string;
  client_id_url: string;
  client_uri: string;
  redirect_uris: string[];
  scopes: string;
  rate_limit_capacity: number;
  rate_limit_refill_rate: number;
  is_active: boolean;
  created_by: string;
  created_at: string;
  updated_at: string;
}

const response = await fetch("http://127.0.0.1:3000/admin/api-clients", {
  headers,
});
const data: ApiClient[] = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/api-clients", {
  headers,
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let client = reqwest::Client::new();
let response = client
    .get("http://127.0.0.1:3000/admin/api-clients")
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/api-clients", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/api-clients -H "$AUTH"
```

**Response**: `200 OK`

```json
[
  {
    "id": "01J9...",
    "client_key": "hvc_a1b2c3...",
    "name": "My Game Client",
    "client_id_url": "https://example.com/client-metadata.json",
    "client_uri": "https://example.com",
    "redirect_uris": ["https://example.com/callback"],
    "scopes": "atproto",
    "rate_limit_capacity": 200,
    "rate_limit_refill_rate": 5.0,
    "is_active": true,
    "created_by": "did:plc:...",
    "created_at": "2026-04-13T12:00:00Z",
    "updated_at": "2026-04-13T12:00:00Z",
    "parent_client_id": null,
    "owner_did": null
  }
]
```

## Create an API client

```
POST /admin/api-clients
```

Requires `api-clients:create`. Generates a `client_key` and `client_secret`. Store the secret — it won't be shown again.

```ts tab="TypeScript" tab-group="language"
interface ApiClient {
  id: string;
  client_key: string;
  client_secret: string;
  name: string;
  client_id_url: string;
}

const response = await fetch("http://127.0.0.1:3000/admin/api-clients", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    name: "My Game Client",
    client_id_url: "https://example.com/client-metadata.json",
    client_uri: "https://example.com",
    redirect_uris: ["https://example.com/callback"],
    scopes: "atproto",
    rate_limit_capacity: 200,
    rate_limit_refill_rate: 5.0,
  }),
});
const data: ApiClient = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/api-clients", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    name: "My Game Client",
    client_id_url: "https://example.com/client-metadata.json",
    client_uri: "https://example.com",
    redirect_uris: ["https://example.com/callback"],
    scopes: "atproto",
    rate_limit_capacity: 200,
    rate_limit_refill_rate: 5.0,
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("http://127.0.0.1:3000/admin/api-clients")
    .bearer_auth(token)
    .json(&serde_json::json!({
        "name": "My Game Client",
        "client_id_url": "https://example.com/client-metadata.json",
        "client_uri": "https://example.com",
        "redirect_uris": ["https://example.com/callback"],
        "scopes": "atproto",
        "rate_limit_capacity": 200,
        "rate_limit_refill_rate": 5.0
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "name": "My Game Client",
  "client_id_url": "https://example.com/client-metadata.json",
  "client_uri": "https://example.com",
  "redirect_uris": ["https://example.com/callback"],
  "scopes": "atproto",
  "rate_limit_capacity": 200,
  "rate_limit_refill_rate": 5.0
}`)
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/admin/api-clients", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/api-clients \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "My Game Client",
    "client_id_url": "https://example.com/client-metadata.json",
    "client_uri": "https://example.com",
    "redirect_uris": ["https://example.com/callback"],
    "scopes": "atproto",
    "rate_limit_capacity": 200,
    "rate_limit_refill_rate": 5.0
  }'
```

| Field                    | Type     | Required | Description                                                                            |
| ------------------------ | -------- | -------- | -------------------------------------------------------------------------------------- |
| `name`                   | string   | yes      | Human-readable display name                                                            |
| `client_id_url`          | string   | yes      | URL to the client's published OAuth client metadata document                           |
| `client_uri`             | string   | yes      | The client's home/landing URL                                                          |
| `redirect_uris`          | string[] | yes      | Allowed OAuth redirect URIs                                                            |
| `scopes`                 | string   | no       | Space-separated OAuth scopes (default `"atproto"`)                                     |
| `rate_limit_capacity`    | integer  | no       | Per-client token bucket capacity. Falls back to `DEFAULT_RATE_LIMIT_CAPACITY` if unset |
| `rate_limit_refill_rate` | number   | no       | Tokens added per second. Falls back to `DEFAULT_RATE_LIMIT_REFILL_RATE` if unset       |

**Response**: `201 Created`

```json
{
  "id": "01J9...",
  "client_key": "hvc_a1b2c3...",
  "client_secret": "hvs_d4e5f6...",
  "name": "My Game Client",
  "client_id_url": "https://example.com/client-metadata.json"
}
```

The new client is immediately registered with the OAuth registry and rate limiter, so it can authenticate without restarting HappyView.

## Get an API client

```
GET /admin/api-clients/{id}
```

Requires `api-clients:view`. Returns the same shape as the list endpoint, or `404 Not Found`.

## Update an API client

```
PUT /admin/api-clients/{id}
```

Requires `api-clients:edit`. All fields are optional — only provided fields are changed. Updating either rate-limit field re-registers the client with the rate limiter using the new values.

```ts tab="TypeScript" tab-group="language"
await fetch("http://127.0.0.1:3000/admin/api-clients/01J9...", {
  method: "PUT",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    name: "Renamed Client",
    rate_limit_capacity: 500,
  }),
});
```
```js tab="JavaScript" tab-group="language"
await fetch("http://127.0.0.1:3000/admin/api-clients/01J9...", {
  method: "PUT",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    name: "Renamed Client",
    rate_limit_capacity: 500,
  }),
});
```
```rust tab="Rust" tab-group="language"
client
    .put("http://127.0.0.1:3000/admin/api-clients/01J9...")
    .bearer_auth(token)
    .json(&serde_json::json!({
        "name": "Renamed Client",
        "rate_limit_capacity": 500
    }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "name": "Renamed Client",
  "rate_limit_capacity": 500
}`)
req, _ := http.NewRequest("PUT", "http://127.0.0.1:3000/admin/api-clients/01J9...", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X PUT http://127.0.0.1:3000/admin/api-clients/01J9... \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "Renamed Client",
    "rate_limit_capacity": 500
  }'
```

| Field                    | Type     | Description                                                            |
| ------------------------ | -------- | ---------------------------------------------------------------------- |
| `name`                   | string   | New display name                                                       |
| `client_uri`             | string   | New home URL                                                           |
| `redirect_uris`          | string[] | Replace the allowed redirect URIs                                      |
| `scopes`                 | string   | Replace the OAuth scopes                                               |
| `rate_limit_capacity`    | integer  | New bucket capacity. Pass `null` to clear the override                 |
| `rate_limit_refill_rate` | number   | New refill rate. Pass `null` to clear the override                     |
| `is_active`              | boolean  | Disable (`false`) or re-enable (`true`) the client without deleting it |

**Response**: `204 No Content`

The OAuth registry is updated in place. The `client_id_url` is immutable — to change it, delete and recreate the client.

## Delete an API client

```
DELETE /admin/api-clients/{id}
```

Requires `api-clients:delete`. Removes the client from the OAuth registry, the rate limiter, and the client identity store.

```ts tab="TypeScript" tab-group="language"
await fetch("http://127.0.0.1:3000/admin/api-clients/01J9...", {
  method: "DELETE",
  headers,
});
```
```js tab="JavaScript" tab-group="language"
await fetch("http://127.0.0.1:3000/admin/api-clients/01J9...", {
  method: "DELETE",
  headers,
});
```
```rust tab="Rust" tab-group="language"
client
    .delete("http://127.0.0.1:3000/admin/api-clients/01J9...")
    .bearer_auth(token)
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("DELETE", "http://127.0.0.1:3000/admin/api-clients/01J9...", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X DELETE http://127.0.0.1:3000/admin/api-clients/01J9... -H "$AUTH"
```

**Response**: `204 No Content`

## AT Protocol client authentication keys

A client can delegate its AT Protocol OAuth signing key to HappyView, so HappyView authenticates to users' PDSes as a **confidential** client on its behalf. Confidential clients get far longer session lifetimes than public ones (the authorization server's policy — 2 years rather than 2 weeks on a conforming PDS).

Delegating takes two steps: provision a key here, then publish the fields it returns in the client metadata document served at the client's Client ID URL. Until both are done the client stays public, which is the default and requires no action.

Whether the client is *confidential* is not a flag HappyView sets — it is a conclusion the authorization server reaches by fetching the app's published document. These endpoints only provision the key and report what that document currently says.

### Provision the key

```
POST /admin/api-clients/{id}/auth-key
```

Requires `api-clients:edit`. Generates an ES256 keypair for this client. Idempotent — returns the existing key if one is already provisioned.

```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/api-clients/01J9.../auth-key -H "$AUTH"
```

**Response**: `200 OK` with the key's `kid` and the `jwks_uri` to publish.

### List every key

```
GET /admin/api-clients/{id}/auth-keys
```

Requires `api-clients:view`. Every key the client holds, ordered `current`, then `retiring`, then `revoked`, each with a live `session_count`.

Distinct from `GET .../auth-key`, which reports only the current key. An operator revoking a leaked key needs to see the **retiring** one and how many sessions revoking it will destroy. Revoked keys are listed so a revoke is visibly done.

```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/api-clients/01J9.../auth-keys -H "$AUTH"
```

```json
{
  "keys": [
    { "kid": "f0c3...", "status": "current",  "created_at": "...", "session_count": 12 },
    { "kid": "8b41...", "status": "retiring", "created_at": "...", "session_count": 3 }
  ],
  "jwks_uri": "https://happyview.example.com/oauth/clients/01J9.../jwks.json"
}
```

### Re-check the published document

```
POST /admin/api-clients/{id}/auth-key/recheck
```

Requires `api-clients:edit`. Re-fetches the client's published metadata and reports whether HappyView can act as a confidential client for it, with the reason if not.

### Rotate the key

```
POST /admin/api-clients/{id}/auth-key/rotate
```

Requires `api-clients:edit`. Mints a new `current` key and demotes the previous one to `retiring`. **Nothing is logged out**: a retiring key stays in the published JWKS and keeps signing refreshes for the sessions established with it.

### Revoke a key

```
DELETE /admin/api-clients/{id}/auth-key/{kid}
```

Requires `api-clients:edit`. Removes the key from the published JWKS immediately and strands every session pinned to it — those users must re-authenticate.

This is the response to a **leaked or compromised key**, not routine cleanup. Rotation alone is not containment: the rotated-out key remains published and valid, which is precisely what keeps existing sessions working. A retiring key with no remaining sessions is swept automatically.

| Status | Meaning |
| ------ | ------- |
| `400`  | The target is the `current` key. Rotate first, then revoke the retiring key. |
| `404`  | No key with that `kid` belongs to this client. |

```sh tab="cURL" tab-group="language"
curl -X DELETE http://127.0.0.1:3000/admin/api-clients/01J9.../auth-key/$KID -H "$AUTH"
```

```json
{ "kid": "8b41...", "sessions_destroyed": 3 }
```

### Remove the key entirely

```
DELETE /admin/api-clients/{id}/auth-keys
```

Requires `api-clients:edit`. Revokes **every** key this client holds and leaves its JWKS empty, un-delegating it completely. Unlike the per-key revoke above, this one does take the `current` key — removing the keyset is the point.

<Callout type="warn" title="Remove the published fields too, or the app breaks">
This does not downgrade the app to a public client. If its published document still advertises `token_endpoint_auth_method: "private_key_jwt"` and this `jwks_uri`, an authorization server will fetch an empty key set and reject the client outright, and `POST /oauth/client-assertion` will return `400`. **The app is broken, not downgraded, until those three fields are removed from its published metadata too.** Coordinate both halves.
</Callout>

Returns `400` if the client already holds no key.

```sh tab="cURL" tab-group="language"
curl -X DELETE http://127.0.0.1:3000/admin/api-clients/01J9.../auth-keys -H "$AUTH"
```

```json
{ "revoked": 2, "sessions_destroyed": 15 }
```

### Responding to a leaked client key

1. `POST .../auth-key/rotate` — the leaked key becomes `retiring`, a fresh key takes over, and the client keeps working.
2. `GET .../auth-keys` — confirm it is now `retiring` and note its `session_count`, which is how many users step 3 signs out.
3. `DELETE .../auth-key/{kid}` on the leaked key.

The reverse order is refused: the current key cannot be revoked, because that would leave the client unable to authenticate at all.

If instead you want to stop delegating altogether — not replace the key, but have HappyView hold none — use `DELETE .../auth-keys` above, and remove the three published fields from the app's metadata document at the same time.

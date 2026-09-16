---
title: "Third-Party API Clients"
---

Third-party applications can manage their own API clients via the `dev.happyview.*` XRPC endpoints. A third-party client is always tied to exactly one parent — the admin-created top-level API client whose DPoP session made the request. Only one level of nesting is allowed; third-party clients cannot create further children. Each third-party client gets its own rate limit bucket with instance default settings.

All endpoints use [DPoP authentication](../../getting-started/authentication.md#authenticating-users-for-procedures). See the [admin API client docs](../admin/api-clients.md) for managing clients through the admin API, and the [API Clients guide](../../guides/api-clients.md) for how API clients work.

<Callout type="info">
Only top-level API clients can call these endpoints. Third-party (child) clients receive `401 Unauthorized` or `403 Forbidden`.
</Callout>

## Authentication

All requests require three headers:

| Header          | Value                                                        |
| --------------- | ------------------------------------------------------------ |
| `Authorization` | `DPoP <access_token>`                                        |
| `DPoP`          | A DPoP proof JWT (method matches the HTTP method, `htu` is scheme + host + path, no query string) |
| `X-Client-Key`  | The parent client's `client_key`                             |

The access token must belong to a valid DPoP session for the parent client.

## List clients

```
GET /xrpc/dev.happyview.listApiClients
```

Returns all API clients owned by the authenticated user.

**Response**: `200 OK`

```json
{
  "clients": [
    {
      "id": "550e8400-e29b-41d4-a716-446655440000",
      "clientKey": "hvc_a1b2c3d4e5f6...",
      "name": "My App",
      "clientIdUrl": "https://myapp.example.com/client-metadata.json",
      "clientUri": "https://myapp.example.com",
      "redirectUris": ["https://myapp.example.com/callback"],
      "clientType": "confidential",
      "scopes": "atproto",
      "allowedOrigins": [],
      "isActive": true,
      "createdAt": "2026-04-28T12:00:00Z"
    }
  ]
}
```

## Get a client

```
GET /xrpc/dev.happyview.getApiClient?id=<client_id>
```

| Parameter | Type   | Required | Description       |
| --------- | ------ | -------- | ----------------- |
| `id`      | string | yes      | The client's UUID |

**Response**: `200 OK`

```json
{
  "client": {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "clientKey": "hvc_a1b2c3d4e5f6...",
    "name": "My App",
    "clientIdUrl": "https://myapp.example.com/client-metadata.json",
    "clientUri": "https://myapp.example.com",
    "redirectUris": ["https://myapp.example.com/callback"],
    "clientType": "confidential",
    "scopes": "atproto",
    "allowedOrigins": [],
    "isActive": true,
    "createdAt": "2026-04-28T12:00:00Z"
  }
}
```

Returns `404` if the client doesn't exist or isn't owned by the authenticated user.

## Create a client

```
POST /xrpc/dev.happyview.createApiClient
```

```ts tab="TypeScript" tab-group="language"
const CLIENT_KEY = "hvc_parent_key"; // parent API client key
const ACCESS_TOKEN = "eyJhbG..."; // DPoP access token
const DPOP_PROOF = "eyJhbG..."; // DPoP proof JWT

interface CreateClientResponse {
  client: {
    id: string;
    clientKey: string;
    name: string;
    clientIdUrl: string;
    clientUri: string;
    redirectUris: string[];
    clientType: string;
    scopes: string;
    allowedOrigins: string[];
    isActive: boolean;
    createdAt: string;
  };
  clientSecret?: string;
}

const response = await fetch(
  "https://happyview.example.com/xrpc/dev.happyview.createApiClient",
  {
    method: "POST",
    headers: {
      "X-Client-Key": CLIENT_KEY,
      Authorization: `DPoP ${ACCESS_TOKEN}`,
      DPoP: DPOP_PROOF,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      name: "My Third-Party App",
      clientIdUrl: "https://myapp.example.com/client-metadata.json",
      clientUri: "https://myapp.example.com",
      redirectUris: ["https://myapp.example.com/callback"],
      clientType: "confidential",
    }),
  },
);

const data: CreateClientResponse = await response.json();
```
```js tab="JavaScript" tab-group="language"
const CLIENT_KEY = "hvc_parent_key"; // parent API client key
const ACCESS_TOKEN = "eyJhbG..."; // DPoP access token
const DPOP_PROOF = "eyJhbG..."; // DPoP proof JWT

const response = await fetch(
  "https://happyview.example.com/xrpc/dev.happyview.createApiClient",
  {
    method: "POST",
    headers: {
      "X-Client-Key": CLIENT_KEY,
      Authorization: `DPoP ${ACCESS_TOKEN}`,
      DPoP: DPOP_PROOF,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      name: "My Third-Party App",
      clientIdUrl: "https://myapp.example.com/client-metadata.json",
      clientUri: "https://myapp.example.com",
      redirectUris: ["https://myapp.example.com/callback"],
      clientType: "confidential",
    }),
  },
);

const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let client = reqwest::Client::new();
let client_key = "hvc_parent_key"; // parent API client key
let access_token = "eyJhbG..."; // DPoP access token
let dpop_proof = "eyJhbG..."; // DPoP proof JWT

let response = client
    .post("https://happyview.example.com/xrpc/dev.happyview.createApiClient")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", dpop_proof)
    .json(&serde_json::json!({
        "name": "My Third-Party App",
        "clientIdUrl": "https://myapp.example.com/client-metadata.json",
        "clientUri": "https://myapp.example.com",
        "redirectUris": ["https://myapp.example.com/callback"],
        "clientType": "confidential"
    }))
    .send()
    .await?;

let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
clientKey := "hvc_parent_key" // parent API client key
accessToken := "eyJhbG..."    // DPoP access token
dpopProof := "eyJhbG..."      // DPoP proof JWT

body := bytes.NewBufferString(`{
  "name": "My Third-Party App",
  "clientIdUrl": "https://myapp.example.com/client-metadata.json",
  "clientUri": "https://myapp.example.com",
  "redirectUris": ["https://myapp.example.com/callback"],
  "clientType": "confidential"
}`)

req, _ := http.NewRequest("POST",
  "https://happyview.example.com/xrpc/dev.happyview.createApiClient", body)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
req.Header.Set("Content-Type", "application/json")

resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST https://happyview.example.com/xrpc/dev.happyview.createApiClient \
  -H "X-Client-Key: hvc_parent_key" \
  -H "Authorization: DPoP eyJhbG..." \
  -H "DPoP: eyJhbG..." \
  -H "Content-Type: application/json" \
  -d '{
    "name": "My Third-Party App",
    "clientIdUrl": "https://myapp.example.com/client-metadata.json",
    "clientUri": "https://myapp.example.com",
    "redirectUris": ["https://myapp.example.com/callback"],
    "clientType": "confidential"
  }'
```

| Field             | Type     | Required | Description                                                    |
| ----------------- | -------- | -------- | -------------------------------------------------------------- |
| `name`            | string   | yes      | Display name for the client                                    |
| `clientIdUrl`     | string   | yes      | Unique OAuth client ID URL                                     |
| `clientUri`       | string   | yes      | The client's homepage URL                                      |
| `redirectUris`    | string[] | yes      | OAuth redirect URIs                                            |
| `scopes`          | string   | no       | Space-separated OAuth scopes (default `"atproto"`)             |
| `clientType`      | string   | no       | `"confidential"` or `"public"` (default `"confidential"`)     |
| `allowedOrigins`  | string[] | no       | CORS allowed origins (relevant for public clients)             |

**Response**: `201 Created`

```json
{
  "client": {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "clientKey": "hvc_a1b2c3d4e5f6...",
    "name": "My Third-Party App",
    "clientIdUrl": "https://myapp.example.com/client-metadata.json",
    "clientUri": "https://myapp.example.com",
    "redirectUris": ["https://myapp.example.com/callback"],
    "clientType": "confidential",
    "scopes": "atproto",
    "allowedOrigins": [],
    "isActive": true,
    "createdAt": "2026-04-28T12:00:00Z"
  },
  "clientSecret": "hvs_f6e5d4c3b2a1..."
}
```

The `clientSecret` is only present for confidential clients and is only returned in this response. It is stored as a SHA-256 hash and cannot be retrieved again.

## Delete a client

```
POST /xrpc/dev.happyview.deleteApiClient
```

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/dev.happyview.deleteApiClient",
  {
    method: "POST",
    headers: {
      "X-Client-Key": CLIENT_KEY,
      Authorization: `DPoP ${ACCESS_TOKEN}`,
      DPoP: DPOP_PROOF,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      id: "550e8400-e29b-41d4-a716-446655440000",
    }),
  },
);
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/dev.happyview.deleteApiClient",
  {
    method: "POST",
    headers: {
      "X-Client-Key": CLIENT_KEY,
      Authorization: `DPoP ${ACCESS_TOKEN}`,
      DPoP: DPOP_PROOF,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      id: "550e8400-e29b-41d4-a716-446655440000",
    }),
  },
);
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("https://happyview.example.com/xrpc/dev.happyview.deleteApiClient")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", dpop_proof)
    .json(&serde_json::json!({
        "id": "550e8400-e29b-41d4-a716-446655440000"
    }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{"id": "550e8400-e29b-41d4-a716-446655440000"}`)

req, _ := http.NewRequest("POST",
  "https://happyview.example.com/xrpc/dev.happyview.deleteApiClient", body)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
req.Header.Set("Content-Type", "application/json")

resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST https://happyview.example.com/xrpc/dev.happyview.deleteApiClient \
  -H "X-Client-Key: hvc_parent_key" \
  -H "Authorization: DPoP eyJhbG..." \
  -H "DPoP: eyJhbG..." \
  -H "Content-Type: application/json" \
  -d '{ "id": "550e8400-e29b-41d4-a716-446655440000" }'
```

| Field | Type   | Required | Description       |
| ----- | ------ | -------- | ----------------- |
| `id`  | string | yes      | The client's UUID |

**Response**: `200 OK` with `{}`

Returns `404` if the client doesn't exist or isn't owned by the authenticated user. Deleting a client cascades to all its children.

## Confidential client authentication

These two endpoints are unrelated to the third-party (XRPC) CRUD flow above — they belong to any top-level API client that wants to run its own atproto OAuth flow against a user's PDS as a **confidential** client (`private_key_jwt`) instead of a public one. See the [API Clients guide](../../guides/api-clients.md#confidential-clients-attaching-the-client-assertion) for the full flow, or generate and inspect the key from the dashboard's Settings > API Clients > "AT Protocol Client Auth" card.

### Get the client's JWKS

```
GET /oauth/clients/{id}/jwks.json
```

Public and unauthenticated — this is the URL your app publishes as `jwks_uri` in its `client_id_url` document, so the user's PDS (a stranger to HappyView) can fetch it during authorization. An id with no provisioned key returns an empty key set rather than a 404, so this endpoint can't be used to enumerate which client ids exist.

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/oauth/clients/550e8400-e29b-41d4-a716-446655440000/jwks.json",
);

const jwks = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/oauth/clients/550e8400-e29b-41d4-a716-446655440000/jwks.json",
);

const jwks = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("https://happyview.example.com/oauth/clients/550e8400-e29b-41d4-a716-446655440000/jwks.json")
    .send()
    .await?;

let jwks: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
resp, err := http.Get(
  "https://happyview.example.com/oauth/clients/550e8400-e29b-41d4-a716-446655440000/jwks.json",
)
```
```sh tab="cURL" tab-group="language"
curl https://happyview.example.com/oauth/clients/550e8400-e29b-41d4-a716-446655440000/jwks.json
```

**Response**: `200 OK`

```json
{
  "keys": [
    {
      "kty": "EC",
      "crv": "P-256",
      "x": "...",
      "y": "...",
      "kid": "b3a1...",
      "alg": "ES256",
      "use": "sig"
    }
  ]
}
```

`keys` is empty until an authentication key has been provisioned for this client (`POST /admin/api-clients/{id}/auth-key`).

### Sign a client assertion

```
POST /oauth/client-assertion
```

Mints a `private_key_jwt` assertion for the calling client, for use in an atproto OAuth flow the client is running itself against a user's PDS. Needed **twice per flow** — once for the pushed authorization request (PAR), once for the token exchange — since each assertion expires after 60 seconds and only covers one request.

Authenticate as a confidential client with `X-Client-Key` + `X-Client-Secret`, or as a public client with `X-Client-Key` + an active DPoP-bound session (`Authorization: DPoP <token>` + a `DPoP` proof covering this request).

```ts tab="TypeScript" tab-group="language"
const CLIENT_KEY = "hvc_...";
const CLIENT_SECRET = "hvs_...";

interface ClientAssertionResponse {
  clientAssertion: string;
  clientAssertionType: string;
  expiresIn: number;
}

const response = await fetch(
  "https://happyview.example.com/oauth/client-assertion",
  {
    method: "POST",
    headers: {
      "X-Client-Key": CLIENT_KEY,
      "X-Client-Secret": CLIENT_SECRET,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ issuer: "https://bsky.social" }),
  },
);

const data: ClientAssertionResponse = await response.json();
```
```js tab="JavaScript" tab-group="language"
const CLIENT_KEY = "hvc_...";
const CLIENT_SECRET = "hvs_...";

const response = await fetch(
  "https://happyview.example.com/oauth/client-assertion",
  {
    method: "POST",
    headers: {
      "X-Client-Key": CLIENT_KEY,
      "X-Client-Secret": CLIENT_SECRET,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ issuer: "https://bsky.social" }),
  },
);

const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let client_key = "hvc_...";
let client_secret = "hvs_...";

let response = client
    .post("https://happyview.example.com/oauth/client-assertion")
    .header("X-Client-Key", client_key)
    .header("X-Client-Secret", client_secret)
    .json(&serde_json::json!({ "issuer": "https://bsky.social" }))
    .send()
    .await?;

let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
clientKey := "hvc_..."
clientSecret := "hvs_..."

body := bytes.NewBufferString(`{"issuer": "https://bsky.social"}`)

req, _ := http.NewRequest("POST",
  "https://happyview.example.com/oauth/client-assertion", body)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("X-Client-Secret", clientSecret)
req.Header.Set("Content-Type", "application/json")

resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST https://happyview.example.com/oauth/client-assertion \
  -H "X-Client-Key: hvc_..." \
  -H "X-Client-Secret: hvs_..." \
  -H "Content-Type: application/json" \
  -d '{"issuer": "https://bsky.social"}'
```

| Field    | Type   | Required | Description                                                          |
| -------- | ------ | -------- | ---------------------------------------------------------------------|
| `issuer` | string | yes      | The `issuer` URL from the target PDS authorization server's metadata |
| `kid`    | string | no       | Sign with this specific key instead of whichever is current. Required when refreshing — see below |

**Response**: `200 OK`

```json
{
  "client_assertion": "eyJhbG...",
  "client_assertion_type": "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
  "expires_in": 60,
  "kid": "8b41d0aa-..."
}
```

Attach `client_assertion` and `client_assertion_type` as form parameters on the PAR request and, separately, on the token exchange request — each needs its own call to this endpoint.

#### Refreshing a token: store the `kid`

`kid` names the key that signed the assertion. **Store it alongside the session you establish**, and pass it back as the `kid` field whenever you request an assertion to *refresh* that session.

This matters after a key rotation. An authorization server binds a session to the key that established it. If you refresh with an assertion signed by a newer key, the server sees a key it never bound the session to — and a conforming server responds by **destroying the session**, not by refusing the one request. The user is silently signed out, typically hours later, with nothing to connect it to the rotation.

Omit `kid` for an initial authorization, where any currently valid key is correct.

Returns `400` (`this client has no authentication key; provision one first`) if no key has been provisioned yet via `POST /admin/api-clients/{id}/auth-key`.

Returns `400` (`no usable authentication key '<kid>' ...`) if the requested `kid` is unknown or has been revoked. A revoked key means any session established under it is already gone, so run a fresh authorization rather than a refresh.

## Errors

| Status | Error                                     | Cause                                                            |
| ------ | ----------------------------------------- | ---------------------------------------------------------------- |
| 400    | `Invalid client_type`                     | `client_type` is not `"confidential"` or `"public"`              |
| 400    | `invalid request body`                    | Missing required fields or malformed JSON                        |
| 401    | `requires DPoP authentication`            | `Authorization` header is missing or doesn't use the DPoP scheme |
| 401    | `requires an API client key`              | `X-Client-Key` header is absent                                  |
| 401    | `token_expired`                           | The access token has expired                                     |
| 401    | `Invalid client`                          | `X-Client-Key` doesn't match a known client                     |
| 401    | `child clients cannot manage API clients` | The calling client is itself a third-party (child) client        |
| 403    | `Child clients cannot create API clients` | The calling client is itself a third-party (child) client        |
| 404    | `API client not found`                    | No client with that ID owned by the authenticated user           |
| 409    | `client_id_url already registered`        | Another client already uses that `clientIdUrl`                   |

## Operational notes

Each third-party client gets its own rate limit bucket using the instance's default capacity and refill rate (`DEFAULT_RATE_LIMIT_CAPACITY` / `DEFAULT_RATE_LIMIT_REFILL_RATE`). Deactivating or deleting a parent via the [admin API](../admin/api-clients.md) cascades to all its children.

The admin API clients list (`GET /admin/api-clients`) returns `parent_client_id` and `owner_did` fields for each client and supports `?parent_id=` filtering. The dashboard's API Clients table shows these as "Parent Client" and "Owner" columns.

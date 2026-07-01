---
title: "Managing Spaces"
---

<Callout type="error" title="Experimental">
This API is experimental and will change. See the [Permissioned Spaces overview](../spaces.md) for context.
</Callout>

## Creating a space

```ts tab="TypeScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.simplespace.createSpace", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    type: "com.example.forum",
    skey: "main",
    displayName: "My Forum",
    description: "A place for discussion",
    mintPolicy: "member-list",
  }),
});
interface CreateSpaceResponse {
  uri: string;
}
const data: CreateSpaceResponse = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.simplespace.createSpace", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    type: "com.example.forum",
    skey: "main",
    displayName: "My Forum",
    description: "A place for discussion",
    mintPolicy: "member-list",
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("https://happyview.example.com/xrpc/com.atproto.simplespace.createSpace")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .json(&serde_json::json!({
        "type": "com.example.forum",
        "skey": "main",
        "displayName": "My Forum",
        "description": "A place for discussion",
        "mintPolicy": "member-list"
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "type": "com.example.forum",
  "skey": "main",
  "displayName": "My Forum",
  "description": "A place for discussion",
  "mintPolicy": "member-list"
}`)
req, _ := http.NewRequest("POST",
  "https://happyview.example.com/xrpc/com.atproto.simplespace.createSpace", body)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST 'https://happyview.example.com/xrpc/com.atproto.simplespace.createSpace' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>' \
  -H 'Content-Type: application/json' \
  -d '{
    "type": "com.example.forum",
    "skey": "main",
    "displayName": "My Forum",
    "description": "A place for discussion",
    "mintPolicy": "member-list"
  }'
```

**Input:**

| Field            | Type          | Required | Description                                       |
| ---------------- | ------------- | -------- | ------------------------------------------------- |
| `type`           | string (NSID) | Yes      | The space type; describes what this space is for  |
| `skey`           | string        | Yes      | Space key; differentiates spaces of the same type |
| `displayName`    | string        | No       | Human-readable name                               |
| `description`    | string        | No       | Description of the space                          |
| `mintPolicy`     | string        | No       | `member-list` (default), `public`, or `managing-app` |
| `appAccess`      | object        | No       | `{"type": "open"}` (default) or `{"type": "allowList", "allowed": [...]}` |
| `managingAppDid` | string        | No       | DID of the application that manages this space    |
| `config`         | object        | No       | Space configuration (see below)                   |

**Response (201):**

```json
{
  "uri": "ats://did:plc:abc123/com.example.forum/main"
}
```

The creator is automatically added as a write member. Use [`com.atproto.space.getSpace`](#getting-a-space) to retrieve the full space object.

### Space configuration

The `config` object supports:

| Field              | Type    | Default | Description                                               |
| ------------------ | ------- | ------- | --------------------------------------------------------- |
| `membershipPublic` | boolean | `false` | Whether the member list is visible without authentication |
| `recordsPublic`    | boolean | `false` | Whether records are readable without membership           |

Additional fields are preserved as-is.

## Getting a space

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/com.atproto.space.getSpace?space=ats://did:plc:abc123/com.example.forum/main",
  {
    headers: {
      "X-Client-Key": CLIENT_KEY,
      "Authorization": `DPoP ${ACCESS_TOKEN}`,
      "DPoP": DPOP_PROOF,
    },
  },
);
interface GetSpaceResponse {
  uri: string;
  space: Space;
  config: SpaceConfig;
}
const data: GetSpaceResponse = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/com.atproto.space.getSpace?space=ats://did:plc:abc123/com.example.forum/main",
  {
    headers: {
      "X-Client-Key": CLIENT_KEY,
      "Authorization": `DPoP ${ACCESS_TOKEN}`,
      "DPoP": DPOP_PROOF,
    },
  },
);
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("https://happyview.example.com/xrpc/com.atproto.space.getSpace")
    .query(&[("space", "ats://did:plc:abc123/com.example.forum/main")])
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET",
  "https://happyview.example.com/xrpc/com.atproto.space.getSpace?space=ats://did:plc:abc123/com.example.forum/main",
  nil)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl 'https://happyview.example.com/xrpc/com.atproto.space.getSpace?space=ats://did:plc:abc123/com.example.forum/main' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>'
```

If `membershipPublic` is `false`, the caller must be authenticated and be a member (or the authority) to see the space. Non-members receive a `404 Not Found`.

## Listing spaces

Returns spaces where the authenticated user is a member.

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/com.atproto.space.listSpaces?limit=20",
  {
    headers: {
      "X-Client-Key": CLIENT_KEY,
      "Authorization": `DPoP ${ACCESS_TOKEN}`,
      "DPoP": DPOP_PROOF,
    },
  },
);
interface Space {
  uri: string;
  isOwner: boolean;
}
interface ListSpacesResponse {
  spaces: Space[];
  cursor?: string;
}
const data: ListSpacesResponse = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/com.atproto.space.listSpaces?limit=20",
  {
    headers: {
      "X-Client-Key": CLIENT_KEY,
      "Authorization": `DPoP ${ACCESS_TOKEN}`,
      "DPoP": DPOP_PROOF,
    },
  },
);
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("https://happyview.example.com/xrpc/com.atproto.space.listSpaces")
    .query(&[("limit", "20")])
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET",
  "https://happyview.example.com/xrpc/com.atproto.space.listSpaces?limit=20",
  nil)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl 'https://happyview.example.com/xrpc/com.atproto.space.listSpaces?limit=20' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>'
```

**Parameters:**

| Field    | Type    | Required | Default        | Description                  |
| -------- | ------- | -------- | -------------- | ---------------------------- |
| `did`    | string  | No       | authenticated user | Filter by DID              |
| `limit`  | integer | No       | 50             | Max spaces to return (1-100) |
| `cursor` | string  | No       |                | Pagination cursor            |

**Response:**

```json
{
  "spaces": [
    {
      "uri": "ats://did:plc:abc123/com.example.forum/main",
      "isOwner": true
    }
  ],
  "cursor": "MjAyNi0wNS0wOVQxMjowMDowMFp8YXRzOi8vZGlkOnBsYzphYmMxMjMvY29tLmV4YW1wbGUuZm9ydW0vbWFpbg"
}
```

## Updating a space

Only the space authority or a HappyView super admin can update a space.

```ts tab="TypeScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.simplespace.updateSpace", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "ats://did:plc:abc123/com.example.forum/main",
    displayName: "Updated Forum Name",
    mintPolicy: "public",
  }),
});
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.simplespace.updateSpace", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "ats://did:plc:abc123/com.example.forum/main",
    displayName: "Updated Forum Name",
    mintPolicy: "public",
  }),
});
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("https://happyview.example.com/xrpc/com.atproto.simplespace.updateSpace")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .json(&serde_json::json!({
        "space": "ats://did:plc:abc123/com.example.forum/main",
        "displayName": "Updated Forum Name",
        "mintPolicy": "public"
    }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "space": "ats://did:plc:abc123/com.example.forum/main",
  "displayName": "Updated Forum Name",
  "mintPolicy": "public"
}`)
req, _ := http.NewRequest("POST",
  "https://happyview.example.com/xrpc/com.atproto.simplespace.updateSpace", body)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST 'https://happyview.example.com/xrpc/com.atproto.simplespace.updateSpace' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>' \
  -H 'Content-Type: application/json' \
  -d '{
    "space": "ats://did:plc:abc123/com.example.forum/main",
    "displayName": "Updated Forum Name",
    "mintPolicy": "public"
  }'
```

All fields except `space` are optional. Only provided fields are updated. To clear an optional field, pass `null`.

## Deleting a space

Only the space authority or a HappyView super admin can delete a space.

```ts tab="TypeScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.simplespace.deleteSpace", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "ats://did:plc:abc123/com.example.forum/main",
  }),
});
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.simplespace.deleteSpace", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "ats://did:plc:abc123/com.example.forum/main",
  }),
});
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("https://happyview.example.com/xrpc/com.atproto.simplespace.deleteSpace")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .json(&serde_json::json!({
        "space": "ats://did:plc:abc123/com.example.forum/main"
    }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{"space": "ats://did:plc:abc123/com.example.forum/main"}`)
req, _ := http.NewRequest("POST",
  "https://happyview.example.com/xrpc/com.atproto.simplespace.deleteSpace", body)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST 'https://happyview.example.com/xrpc/com.atproto.simplespace.deleteSpace' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>' \
  -H 'Content-Type: application/json' \
  -d '{"space": "ats://did:plc:abc123/com.example.forum/main"}'
```

<Callout type="warn">
Deleting a space cascades to all associated records, members, repo state, oplog entries, notification registrations, and credentials.
</Callout>

## Getting configuration

Returns the simplespace configuration for a space. Requires admin access (space authority or super admin).

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/com.atproto.simplespace.getConfig?space=ats://did:plc:abc123/com.example.forum/main",
  {
    headers: {
      "X-Client-Key": CLIENT_KEY,
      "Authorization": `DPoP ${ACCESS_TOKEN}`,
      "DPoP": DPOP_PROOF,
    },
  },
);
interface SpaceConfig {
  $type: "com.atproto.simplespace.defs#spaceConfig";
  mintPolicy: string;
  appAccess: object;
  managingApp: string | null;
}
const data: SpaceConfig = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/com.atproto.simplespace.getConfig?space=ats://did:plc:abc123/com.example.forum/main",
  {
    headers: {
      "X-Client-Key": CLIENT_KEY,
      "Authorization": `DPoP ${ACCESS_TOKEN}`,
      "DPoP": DPOP_PROOF,
    },
  },
);
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("https://happyview.example.com/xrpc/com.atproto.simplespace.getConfig")
    .query(&[("space", "ats://did:plc:abc123/com.example.forum/main")])
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET",
  "https://happyview.example.com/xrpc/com.atproto.simplespace.getConfig?space=ats://did:plc:abc123/com.example.forum/main",
  nil)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl 'https://happyview.example.com/xrpc/com.atproto.simplespace.getConfig?space=ats://did:plc:abc123/com.example.forum/main' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>'
```

**Response:**

```json
{
  "$type": "com.atproto.simplespace.defs#spaceConfig",
  "mintPolicy": "member-list",
  "appAccess": { "type": "open" },
  "managingApp": null
}
```

| Field         | Type   | Description                                                              |
| ------------- | ------ | ------------------------------------------------------------------------ |
| `mintPolicy`  | string | `member-list`, `public`, or `managing-app`                               |
| `appAccess`   | object | `{"type": "open"}` or `{"type": "allowList", "allowed": ["did:...", ...]}` |
| `managingApp` | string \| null | DID of the application that manages this space                  |

## Updating configuration

Updates the simplespace configuration for a space. Requires admin access (space authority or super admin).

```ts tab="TypeScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.simplespace.updateConfig", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "ats://did:plc:abc123/com.example.forum/main",
    mintPolicy: "public",
    appAccess: { type: "allowList", allowed: ["did:web:myapp.example.com"] },
  }),
});
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.simplespace.updateConfig", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "ats://did:plc:abc123/com.example.forum/main",
    mintPolicy: "public",
    appAccess: { type: "allowList", allowed: ["did:web:myapp.example.com"] },
  }),
});
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("https://happyview.example.com/xrpc/com.atproto.simplespace.updateConfig")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .json(&serde_json::json!({
        "space": "ats://did:plc:abc123/com.example.forum/main",
        "mintPolicy": "public",
        "appAccess": { "type": "allowList", "allowed": ["did:web:myapp.example.com"] }
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "space": "ats://did:plc:abc123/com.example.forum/main",
  "mintPolicy": "public",
  "appAccess": {"type": "allowList", "allowed": ["did:web:myapp.example.com"]}
}`)
req, _ := http.NewRequest("POST",
  "https://happyview.example.com/xrpc/com.atproto.simplespace.updateConfig", body)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST 'https://happyview.example.com/xrpc/com.atproto.simplespace.updateConfig' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>' \
  -H 'Content-Type: application/json' \
  -d '{
    "space": "ats://did:plc:abc123/com.example.forum/main",
    "mintPolicy": "public",
    "appAccess": {"type": "allowList", "allowed": ["did:web:myapp.example.com"]}
  }'
```

**Input:**

| Field          | Type           | Required | Description                                                              |
| -------------- | -------------- | -------- | ------------------------------------------------------------------------ |
| `space`        | string         | Yes      | Space URI                                                                |
| `mintPolicy`   | string         | No       | `member-list`, `public`, or `managing-app`                               |
| `appAccess`    | object         | No       | `{"type": "open"}` or `{"type": "allowList", "allowed": ["did:...", ...]}` |
| `managingApp`  | string \| null | No       | DID of the managing app, or `null` to clear                              |

All fields except `space` are optional. Only provided fields are updated. The response returns the updated configuration in the same format as `getConfig`.

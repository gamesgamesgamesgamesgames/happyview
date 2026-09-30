---
title: "Managing Spaces"
---

<Callout type="error" title="Experimental">
This API is experimental and will change. See the [Permissioned Spaces overview](../spaces.md) for context.
</Callout>

## Space authority

Every space has an **authority**: the DID that other services resolve to find the space's host and the key that signs its [credentials](./credentials.md). Spaces created through HappyView use HappyView's [service identity](../../getting-started/service-identity.md) DID, `did:web` or `did:plc`, as both the authority and the DID in the space URI:

```
at://did:web:happyview.example.com/space/com.example.forum/main
```

The account that creates the space is its **creator**. The creator administers the space: only the creator, or a HappyView super admin, can update or delete it, change its member list, and manage its invites.

Because every space on an instance shares the instance's DID, a space key is unique per space type across the whole instance. A second account creating a space with the same type and `skey` gets `409 Conflict`.

If the instance has no published service identity, HappyView anchors new spaces on the creator's DID instead. The creator's DID is then both the authority and the DID in the URI, and only HappyView can verify the space's credentials.

### Publishing the space host

When a space is created, HappyView adds two entries to its DID document if they are missing:

- `#atproto_space_host`, a service entry of type `AtprotoSpaceHost` pointing at the instance
- `#atproto_space`, the verification method that signs space credentials

With a `did:web` identity, HappyView serves both immediately. With a `did:plc` identity, the operator must [sync service entries to the PLC directory](../../api-reference/admin/service-entries.md#sync-to-plc-directory) before other services can resolve them.

### OAuth scopes

Apps reach spaces through `space:` OAuth scopes, which name the space's authority. The default, `authority=self`, covers only spaces whose authority is the signed-in account. It does not cover spaces whose authority is HappyView. Apps must request `authority=<HappyView's DID>`, or `authority=*` for any authority:

```
atproto space:com.example.forum?authority=did:web:happyview.example.com
```

A session without a covering grant receives `403 Forbidden`. Scopes are fixed when a session is created, so fixing a missing grant requires a new authorization.

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
    readPolicy: { $type: "com.atproto.simplespace.defs#memberListPolicy" },
    writePolicy: { $type: "com.atproto.simplespace.defs#memberListPolicy" },
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
    readPolicy: { $type: "com.atproto.simplespace.defs#memberListPolicy" },
    writePolicy: { $type: "com.atproto.simplespace.defs#memberListPolicy" },
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
        "readPolicy": { "$type": "com.atproto.simplespace.defs#memberListPolicy" },
        "writePolicy": { "$type": "com.atproto.simplespace.defs#memberListPolicy" }
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
  "readPolicy": {"$type": "com.atproto.simplespace.defs#memberListPolicy"},
  "writePolicy": {"$type": "com.atproto.simplespace.defs#memberListPolicy"}
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
    "readPolicy": {"$type": "com.atproto.simplespace.defs#memberListPolicy"},
    "writePolicy": {"$type": "com.atproto.simplespace.defs#memberListPolicy"}
  }'
```

**Input:**

| Field         | Type          | Required | Description                                       |
| ------------- | ------------- | -------- | ------------------------------------------------- |
| `type`        | string (NSID) | Yes      | The space type; describes what this space is for  |
| `skey`        | string        | Yes      | Space key; differentiates spaces of the same type |
| `displayName` | string        | No       | Human-readable name                               |
| `description` | string        | No       | Description of the space                          |
| `readPolicy`  | object        | No       | Who can obtain credentials to read the space. See [Policies](#policies). Defaults to the member-list policy. |
| `writePolicy` | object        | No       | Whose writes the space accepts. See [Policies](#policies). Defaults to the member-list policy. |
| `appAccess`   | object        | No       | Which apps can obtain credentials. See [App access](#app-access). Defaults to open. |
| `config`      | object        | No       | Space configuration (see below)                   |

**Response (201):**

```json
{
  "uri": "at://did:web:happyview.example.com/space/com.example.forum/main"
}
```

The creator is automatically added as a member with `read` and `write`. Use [`com.atproto.simplespace.getSpace`](#getting-a-space) to retrieve the full space object.

### Policies

A space has two independent policies. The **read policy** decides who can obtain a [space credential](./credentials.md). The **write policy** decides whose writes the space tracks: which repos appear in [`listRepos`](./records.md#listing-repos) and whose [write notifications](./notifications.md) HappyView accepts and forwards.

Each policy is one of:

| `$type` | Grants access to |
|---|---|
| `com.atproto.simplespace.defs#memberListPolicy` | Members with the matching flag: `read` for the read policy, `write` for the write policy. The default. |
| `com.atproto.simplespace.defs#publicPolicy` | Everyone |
| `com.atproto.simplespace.defs#managingAppPolicy` | Whoever the managing app approves |

A managing-app policy names the app's service identifier in `managingApp`:

```json
{
  "$type": "com.atproto.simplespace.defs#managingAppPolicy",
  "managingApp": "did:web:forum.example.com#forum"
}
```

HappyView resolves the identifier's fragment to a service entry in the app's DID document, and a bare DID to its `#atproto_pds` entry. For each decision it calls `com.atproto.simplespace.checkUserAccess` at that endpoint with service auth from HappyView, passing `space`, `user`, and `access` (`read` or `write`). The managing-app policy requires the space's authority to be HappyView's instance DID, because the app expects the authority to sign the call.

A policy with an unknown `$type` fails with `400 UnsupportedPolicy`.

### App access

`appAccess` controls which apps can obtain credentials for the space:

| Value | Meaning |
|---|---|
| `{"$type": "com.atproto.simplespace.defs#open"}` | Any app. The default. |
| `{"$type": "com.atproto.simplespace.defs#allowList", "allowed": ["https://app.example.com/client-metadata.json"]}` | Only apps whose attested OAuth `client_id` is in `allowed` |

An unknown variant fails with `400 UnsupportedAppAccess`. See [App access control](./credentials.md#app-access-control) for how the check runs.

### Space configuration

The `config` object supports:

| Field               | Type    | Default | Description |
| ------------------- | ------- | ------- | ----------- |
| `membership_public` | boolean | `false` | Whether the space, its member list, and its repo list are visible without authentication |
| `records_public`    | boolean | `false` | Stored with the space. HappyView does not enforce it. |

The field names are snake_case, unlike the rest of the request.

Additional fields are preserved as-is. One recognized additional field is `allowedCollections` (a JSON array of collection NSID strings), auto-populated at creation time from the space type lexicon's `defs.main.collections`. When present and non-empty, `createRecord`/`putRecord`/`applyWrites` reject writes (`400`) to any collection not on the list; deletes are never restricted. A space without this field, or with an empty list, allows writes to any collection.

### Legacy policy fields

The following forms are deprecated and kept until v3:

- **`policy` or `mintPolicy`**: a single policy name, `"public"`, `"member-list"`, or `"managing-app"`, applied to both reads and writes. For `"managing-app"`, the app goes in a sibling `managingApp` or `managingAppDid` field. `readPolicy` and `writePolicy` take precedence when present.
- **`appAccess` with `type`**: `{"type": "open"}` and `{"type": "allowList", "allowed": [...]}` are read as the matching `$type` variants.

## Getting a space

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/com.atproto.simplespace.getSpace?space=at://did:web:happyview.example.com/space/com.example.forum/main",
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
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/com.atproto.simplespace.getSpace?space=at://did:web:happyview.example.com/space/com.example.forum/main",
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
    .get("https://happyview.example.com/xrpc/com.atproto.simplespace.getSpace")
    .query(&[("space", "at://did:web:happyview.example.com/space/com.example.forum/main")])
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET",
  "https://happyview.example.com/xrpc/com.atproto.simplespace.getSpace?space=at://did:web:happyview.example.com/space/com.example.forum/main",
  nil)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl 'https://happyview.example.com/xrpc/com.atproto.simplespace.getSpace?space=at://did:web:happyview.example.com/space/com.example.forum/main' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>'
```

**Response:**

```json
{
  "uri": "at://did:web:happyview.example.com/space/com.example.forum/main",
  "space": {
    "id": "5d1c8e0a-7b2f-4c39-8e61-3f4a9b2d7e10",
    "did": "did:web:happyview.example.com",
    "authority_did": "did:web:happyview.example.com",
    "creator_did": "did:plc:creator123",
    "type": "com.example.forum",
    "skey": "main",
    "display_name": "My Forum",
    "description": "A place for discussion",
    "read_policy": { "$type": "com.atproto.simplespace.defs#memberListPolicy" },
    "write_policy": { "$type": "com.atproto.simplespace.defs#memberListPolicy" },
    "app_access": { "$type": "com.atproto.simplespace.defs#open" },
    "config": {
      "membership_public": false,
      "records_public": false,
      "allowedCollections": ["com.example.forum.post"]
    },
    "revision": "3l2tkbx7225co",
    "created_at": "2026-09-30T12:00:00Z",
    "updated_at": "2026-09-30T12:00:00Z"
  },
  "config": {
    "$type": "com.atproto.simplespace.defs#spaceConfig",
    "readPolicy": { "$type": "com.atproto.simplespace.defs#memberListPolicy" },
    "writePolicy": { "$type": "com.atproto.simplespace.defs#memberListPolicy" },
    "appAccess": { "$type": "com.atproto.simplespace.defs#open" }
  }
}
```

If `membership_public` is `false`, the caller must be authenticated and be the creator or a member. Everyone else receives `404 Not Found`.

`dev.happyview.space.getSpace` is a deprecated alias, kept until v3.

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
interface SpaceView {
  uri: string;
  isOwner: boolean;
}
interface ListSpacesResponse {
  spaces: SpaceView[];
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
      "uri": "at://did:web:happyview.example.com/space/com.example.forum/main",
      "isOwner": true
    }
  ],
  "cursor": "MjAyNi0wOS0zMFQxMjowMDowMFp8YXQ6Ly9kaWQ6d2ViOmhhcHB5dmlldy5leGFtcGxlLmNvbS9zcGFjZS9jb20uZXhhbXBsZS5mb3J1bS9tYWlu"
}
```

`isOwner` is `true` when the user is the space's creator.

## Updating a space

Only the space's creator or a HappyView super admin can update a space.

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
    space: "at://did:web:happyview.example.com/space/com.example.forum/main",
    displayName: "Updated Forum Name",
    readPolicy: { $type: "com.atproto.simplespace.defs#publicPolicy" },
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
    space: "at://did:web:happyview.example.com/space/com.example.forum/main",
    displayName: "Updated Forum Name",
    readPolicy: { $type: "com.atproto.simplespace.defs#publicPolicy" },
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
        "space": "at://did:web:happyview.example.com/space/com.example.forum/main",
        "displayName": "Updated Forum Name",
        "readPolicy": { "$type": "com.atproto.simplespace.defs#publicPolicy" }
    }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "space": "at://did:web:happyview.example.com/space/com.example.forum/main",
  "displayName": "Updated Forum Name",
  "readPolicy": {"$type": "com.atproto.simplespace.defs#publicPolicy"}
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
    "space": "at://did:web:happyview.example.com/space/com.example.forum/main",
    "displayName": "Updated Forum Name",
    "readPolicy": {"$type": "com.atproto.simplespace.defs#publicPolicy"}
  }'
```

All fields except `space` are optional, and take the same values as in `createSpace`, including the [legacy policy fields](#legacy-policy-fields). Only provided fields are updated. A supplied policy, `appAccess`, or `config` replaces the current value whole. To clear `displayName` or `description`, pass `null`.

The response contains the space URI and the updated space object, in the same shape as `getSpace`'s `uri` and `space` fields.

## Deleting a space

Only the space's creator or a HappyView super admin can delete a space.

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
    space: "at://did:web:happyview.example.com/space/com.example.forum/main",
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
    space: "at://did:web:happyview.example.com/space/com.example.forum/main",
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
        "space": "at://did:web:happyview.example.com/space/com.example.forum/main"
    }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{"space": "at://did:web:happyview.example.com/space/com.example.forum/main"}`)
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
  -d '{"space": "at://did:web:happyview.example.com/space/com.example.forum/main"}'
```

**Response (200):**

```json
{
  "success": true
}
```

<Callout type="warn">
Deleting a space cascades to all associated records, members, repo state, oplog entries, notification registrations, and credentials.
</Callout>

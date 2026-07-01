---
title: "Invites"
---

<Callout type="error" title="Experimental">
This API is experimental and will change. See the [Permissioned Spaces overview](../spaces.md) for context.
</Callout>

Invites let space authorities distribute membership tokens without knowing recipients' DIDs in advance.

<Callout type="info" title="HappyView Extension">
Invites are a HappyView-specific feature, not part of the AT Protocol spaces spec. They may be replaced by a different mechanism in the future.
</Callout>

## Creating an invite

Only the space authority or a super admin can create invites.

```ts tab="TypeScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/dev.happyview.space.createInvite", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "ats://did:plc:abc123/com.example.forum/main",
    access: "write",
    maxUses: 10,
    expiresAt: "2026-06-01T00:00:00Z",
  }),
});
interface CreateInviteResponse {
  inviteId: string;
  token: string;
  access: string;
  maxUses: number;
  expiresAt: string;
}
const data: CreateInviteResponse = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/dev.happyview.space.createInvite", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "ats://did:plc:abc123/com.example.forum/main",
    access: "write",
    maxUses: 10,
    expiresAt: "2026-06-01T00:00:00Z",
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("https://happyview.example.com/xrpc/dev.happyview.space.createInvite")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .json(&serde_json::json!({
        "space": "ats://did:plc:abc123/com.example.forum/main",
        "access": "write",
        "maxUses": 10,
        "expiresAt": "2026-06-01T00:00:00Z"
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "space": "ats://did:plc:abc123/com.example.forum/main",
  "access": "write",
  "maxUses": 10,
  "expiresAt": "2026-06-01T00:00:00Z"
}`)
req, _ := http.NewRequest("POST",
  "https://happyview.example.com/xrpc/dev.happyview.space.createInvite", body)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST 'https://happyview.example.com/xrpc/dev.happyview.space.createInvite' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>' \
  -H 'Content-Type: application/json' \
  -d '{
    "space": "ats://did:plc:abc123/com.example.forum/main",
    "access": "write",
    "maxUses": 10,
    "expiresAt": "2026-06-01T00:00:00Z"
  }'
```

**Input:**

| Field | Type | Required | Default | Description |
|---|---|---|---|---|
| `space` | string | Yes | | The space this invite is for |
| `access` | string | No | `read` | Access level granted on acceptance (`read`, `read_self`, or `write`) |
| `maxUses` | integer | No | unlimited | Maximum number of times the invite can be redeemed |
| `expiresAt` | string (datetime) | No | never | When the invite expires |

**Response (201):**

```json
{
  "inviteId": "uuid",
  "token": "a1b2c3d4e5f6...",
  "access": "write",
  "maxUses": 10,
  "expiresAt": "2026-06-01T00:00:00Z"
}
```

<Callout type="warn">
The `token` is only returned once. It is stored as a SHA-256 hash — HappyView cannot recover the plaintext.
</Callout>

## Accepting an invite

Any authenticated user can accept an invite token to join the space.

```ts tab="TypeScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/dev.happyview.space.acceptInvite", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    token: "a1b2c3d4e5f6...",
  }),
});
interface AcceptInviteResponse {
  uri: string;
  access: string;
}
const data: AcceptInviteResponse = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/dev.happyview.space.acceptInvite", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    token: "a1b2c3d4e5f6...",
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("https://happyview.example.com/xrpc/dev.happyview.space.acceptInvite")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .json(&serde_json::json!({
        "token": "a1b2c3d4e5f6..."
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{"token": "a1b2c3d4e5f6..."}`)
req, _ := http.NewRequest("POST",
  "https://happyview.example.com/xrpc/dev.happyview.space.acceptInvite", body)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST 'https://happyview.example.com/xrpc/dev.happyview.space.acceptInvite' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>' \
  -H 'Content-Type: application/json' \
  -d '{
    "token": "a1b2c3d4e5f6..."
  }'
```

**Response (201):**

```json
{
  "uri": "ats://did:plc:abc123/com.example.forum/main",
  "access": "write"
}
```

Acceptance fails if:

- The token is invalid (no matching hash found)
- The invite has been revoked
- The invite has reached its `maxUses`
- The invite has expired
- The user is already a member of the space

## Revoking an invite

```ts tab="TypeScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/dev.happyview.space.revokeInvite", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "ats://did:plc:abc123/com.example.forum/main",
    inviteId: "uuid",
  }),
});
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/dev.happyview.space.revokeInvite", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "ats://did:plc:abc123/com.example.forum/main",
    inviteId: "uuid",
  }),
});
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("https://happyview.example.com/xrpc/dev.happyview.space.revokeInvite")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .json(&serde_json::json!({
        "space": "ats://did:plc:abc123/com.example.forum/main",
        "inviteId": "uuid"
    }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "space": "ats://did:plc:abc123/com.example.forum/main",
  "inviteId": "uuid"
}`)
req, _ := http.NewRequest("POST",
  "https://happyview.example.com/xrpc/dev.happyview.space.revokeInvite", body)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST 'https://happyview.example.com/xrpc/dev.happyview.space.revokeInvite' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>' \
  -H 'Content-Type: application/json' \
  -d '{
    "space": "ats://did:plc:abc123/com.example.forum/main",
    "inviteId": "uuid"
  }'
```

Revoking an invite prevents future redemptions but does not remove members who already redeemed it.

## Listing invites

Only the space authority or a super admin can list invites.

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/dev.happyview.space.listInvites?space=ats://did:plc:abc123/com.example.forum/main",
  {
    headers: {
      "X-Client-Key": CLIENT_KEY,
      "Authorization": `DPoP ${ACCESS_TOKEN}`,
      "DPoP": DPOP_PROOF,
    },
  },
);
interface Invite {
  id: string;
  access: string;
  maxUses: number;
  uses: number;
  expiresAt: string;
  revoked: boolean;
  createdBy: string;
  createdAt: string;
}
const data: { invites: Invite[] } = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/dev.happyview.space.listInvites?space=ats://did:plc:abc123/com.example.forum/main",
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
    .get("https://happyview.example.com/xrpc/dev.happyview.space.listInvites")
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
  "https://happyview.example.com/xrpc/dev.happyview.space.listInvites?space=ats://did:plc:abc123/com.example.forum/main",
  nil)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl 'https://happyview.example.com/xrpc/dev.happyview.space.listInvites?space=ats://did:plc:abc123/com.example.forum/main' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>'
```

**Parameters:**

| Field   | Type   | Required | Description                       |
| ------- | ------ | -------- | --------------------------------- |
| `space` | string | Yes      | The space to list invites for     |

**Response:**

```json
{
  "invites": [
    {
      "id": "uuid",
      "access": "write",
      "maxUses": 10,
      "uses": 3,
      "expiresAt": "2026-06-01T00:00:00Z",
      "revoked": false,
      "createdBy": "did:plc:abc123",
      "createdAt": "2026-05-09T12:00:00Z"
    }
  ]
}
```

The token itself is never returned in list responses — only the invite metadata.

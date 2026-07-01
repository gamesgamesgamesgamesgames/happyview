---
title: "Write Notifications"
---

<Callout type="error" title="Experimental">
This API is experimental and will change. See the [Permissioned Spaces overview](../spaces.md) for context.
</Callout>

Write notifications let external services receive webhooks when records change in a space. A service registers an endpoint, and HappyView pushes notifications to it when records are created, updated, or deleted — or when the space itself is deleted.

Registrations expire after 24 hours and must be renewed.

## Registering for notifications

Requires DPoP auth or a space credential. The caller provides the DID of the service that will receive notifications and the HTTPS endpoint to deliver them to.

```ts tab="TypeScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.space.registerNotify", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "ats://did:plc:abc123/com.example.forum/main",
    serviceDid: "did:web:feed.example.com",
    endpoint: "https://feed.example.com/webhooks/space-writes",
  }),
});
interface RegisterNotifyResponse {
  id: string;
}
const data: RegisterNotifyResponse = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.space.registerNotify", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "ats://did:plc:abc123/com.example.forum/main",
    serviceDid: "did:web:feed.example.com",
    endpoint: "https://feed.example.com/webhooks/space-writes",
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("https://happyview.example.com/xrpc/com.atproto.space.registerNotify")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .json(&serde_json::json!({
        "space": "ats://did:plc:abc123/com.example.forum/main",
        "serviceDid": "did:web:feed.example.com",
        "endpoint": "https://feed.example.com/webhooks/space-writes"
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "space": "ats://did:plc:abc123/com.example.forum/main",
  "serviceDid": "did:web:feed.example.com",
  "endpoint": "https://feed.example.com/webhooks/space-writes"
}`)
req, _ := http.NewRequest("POST",
  "https://happyview.example.com/xrpc/com.atproto.space.registerNotify", body)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST 'https://happyview.example.com/xrpc/com.atproto.space.registerNotify' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>' \
  -H 'Content-Type: application/json' \
  -d '{
    "space": "ats://did:plc:abc123/com.example.forum/main",
    "serviceDid": "did:web:feed.example.com",
    "endpoint": "https://feed.example.com/webhooks/space-writes"
  }'
```

**Input:**

| Field        | Type   | Required | Description                                      |
| ------------ | ------ | -------- | ------------------------------------------------ |
| `space`      | string | Yes      | Space URI (`ats://...`)                          |
| `serviceDid` | string | Yes      | DID of the service receiving notifications       |
| `endpoint`   | string | Yes      | HTTPS endpoint to deliver notifications to       |

**Response (200):**

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000"
}
```

## Write notification payload

When a record is created, updated, or deleted in a space, HappyView POSTs a JSON payload to each registered endpoint:

```json
{
  "space": "space-id",
  "did": "did:plc:author",
  "collection": "com.example.forum.post",
  "rkey": "3jwq5dya2gy2z",
  "cid": "bafyreie5cvv4h45feadgeuwhbcutmh6t7ceseocckahdoe6uat64zmz454"
}
```

| Field        | Type          | Description                                      |
| ------------ | ------------- | ------------------------------------------------ |
| `space`      | string        | Internal space ID                                |
| `did`        | string        | DID of the author who made the change            |
| `collection` | string (NSID) | Collection the record belongs to                 |
| `rkey`       | string        | Record key                                       |
| `cid`        | string?       | CID of the new record value (null for deletes)   |

Notifications are delivered to both per-author registrations (matching `serviceDid`) and space-wide registrations (no author filter). Delivery is best-effort — if the endpoint is unreachable, the notification is dropped.

## Pushing a write notification

Server-to-server endpoint. Triggers write notifications to all registered endpoints for a space. This is used internally by HappyView when records change, but can also be called externally.

```ts tab="TypeScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.space.notifyWrite", {
  method: "POST",
  headers: {
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "ats://did:plc:abc123/com.example.forum/main",
    did: "did:plc:author456",
    collection: "com.example.forum.post",
    rkey: "3jwq5dya2gy2z",
    cid: "bafyreie5cvv4h45feadgeuwhbcutmh6t7ceseocckahdoe6uat64zmz454",
  }),
});
const data = await response.json();
// { "success": true }
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.space.notifyWrite", {
  method: "POST",
  headers: {
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "ats://did:plc:abc123/com.example.forum/main",
    did: "did:plc:author456",
    collection: "com.example.forum.post",
    rkey: "3jwq5dya2gy2z",
    cid: "bafyreie5cvv4h45feadgeuwhbcutmh6t7ceseocckahdoe6uat64zmz454",
  }),
});
const data = await response.json();
// { "success": true }
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("https://happyview.example.com/xrpc/com.atproto.space.notifyWrite")
    .json(&serde_json::json!({
        "space": "ats://did:plc:abc123/com.example.forum/main",
        "did": "did:plc:author456",
        "collection": "com.example.forum.post",
        "rkey": "3jwq5dya2gy2z",
        "cid": "bafyreie5cvv4h45feadgeuwhbcutmh6t7ceseocckahdoe6uat64zmz454"
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "space": "ats://did:plc:abc123/com.example.forum/main",
  "did": "did:plc:author456",
  "collection": "com.example.forum.post",
  "rkey": "3jwq5dya2gy2z",
  "cid": "bafyreie5cvv4h45feadgeuwhbcutmh6t7ceseocckahdoe6uat64zmz454"
}`)
req, _ := http.NewRequest("POST",
  "https://happyview.example.com/xrpc/com.atproto.space.notifyWrite", body)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST 'https://happyview.example.com/xrpc/com.atproto.space.notifyWrite' \
  -H 'Content-Type: application/json' \
  -d '{
    "space": "ats://did:plc:abc123/com.example.forum/main",
    "did": "did:plc:author456",
    "collection": "com.example.forum.post",
    "rkey": "3jwq5dya2gy2z",
    "cid": "bafyreie5cvv4h45feadgeuwhbcutmh6t7ceseocckahdoe6uat64zmz454"
  }'
```

**Input:**

| Field        | Type          | Required | Description                                      |
| ------------ | ------------- | -------- | ------------------------------------------------ |
| `space`      | string        | Yes      | Space URI (`ats://...`)                          |
| `did`        | string        | Yes      | DID of the author who made the change            |
| `collection` | string (NSID) | Yes      | Collection the record belongs to                 |
| `rkey`       | string        | Yes      | Record key                                       |
| `cid`        | string        | No       | CID of the record (omit for deletes)             |

**Response (200):**

```json
{
  "success": true
}
```

## Notifying space deletion

Server-to-server endpoint. Notifies all registered endpoints that a space has been deleted. Registered endpoints receive `{ "space": "<space-id>" }`.

```ts tab="TypeScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.space.notifySpaceDeleted", {
  method: "POST",
  headers: {
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "ats://did:plc:abc123/com.example.forum/main",
  }),
});
const data = await response.json();
// { "success": true }
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.space.notifySpaceDeleted", {
  method: "POST",
  headers: {
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "ats://did:plc:abc123/com.example.forum/main",
  }),
});
const data = await response.json();
// { "success": true }
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("https://happyview.example.com/xrpc/com.atproto.space.notifySpaceDeleted")
    .json(&serde_json::json!({
        "space": "ats://did:plc:abc123/com.example.forum/main"
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "space": "ats://did:plc:abc123/com.example.forum/main"
}`)
req, _ := http.NewRequest("POST",
  "https://happyview.example.com/xrpc/com.atproto.space.notifySpaceDeleted", body)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST 'https://happyview.example.com/xrpc/com.atproto.space.notifySpaceDeleted' \
  -H 'Content-Type: application/json' \
  -d '{
    "space": "ats://did:plc:abc123/com.example.forum/main"
  }'
```

**Input:**

| Field   | Type   | Required | Description             |
| ------- | ------ | -------- | ----------------------- |
| `space` | string | Yes      | Space URI (`ats://...`) |

**Response (200):**

```json
{
  "success": true
}
```

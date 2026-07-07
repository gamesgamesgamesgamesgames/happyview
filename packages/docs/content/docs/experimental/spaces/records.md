---
title: "Records"
---

<Callout type="error" title="Experimental">
This API is experimental and will change. See the [Permissioned Spaces overview](../spaces.md) for context.
</Callout>

Space records are stored separately from public AT Protocol records. They use the `at://` scheme with a `space` path segment to distinguish them from public records:

```
at:// did:plc:abcdefghijklmnop1234567890 / space / com.example.forum / main        / did:plc:author / com.example.forum.post / abcdefghijklmnop1234567890
      └── space DID ───────────────────┘           └── space type ─┘   └── skey ─┘   └── author ──┘   └── collection ──────┘   └── rkey ────────────────┘
```

## Creating a record

Requires `write` membership in the space. The rkey is auto-generated using a TID.

```ts tab="TypeScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.space.createRecord", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "at://did:plc:abc123/space/com.example.forum/main",
    collection: "com.example.forum.post",
    record: {
      $type: "com.example.forum.post",
      text: "Hello from the forum!",
      createdAt: "2026-05-09T12:00:00Z",
    },
  }),
});
interface CreateRecordResponse {
  uri: string;
  cid: string;
}
const data: CreateRecordResponse = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.space.createRecord", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "at://did:plc:abc123/space/com.example.forum/main",
    collection: "com.example.forum.post",
    record: {
      $type: "com.example.forum.post",
      text: "Hello from the forum!",
      createdAt: "2026-05-09T12:00:00Z",
    },
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("https://happyview.example.com/xrpc/com.atproto.space.createRecord")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .json(&serde_json::json!({
        "space": "at://did:plc:abc123/space/com.example.forum/main",
        "collection": "com.example.forum.post",
        "record": {
            "$type": "com.example.forum.post",
            "text": "Hello from the forum!",
            "createdAt": "2026-05-09T12:00:00Z"
        }
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "space": "at://did:plc:abc123/space/com.example.forum/main",
  "collection": "com.example.forum.post",
  "record": {
    "$type": "com.example.forum.post",
    "text": "Hello from the forum!",
    "createdAt": "2026-05-09T12:00:00Z"
  }
}`)
req, _ := http.NewRequest("POST",
  "https://happyview.example.com/xrpc/com.atproto.space.createRecord", body)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST 'https://happyview.example.com/xrpc/com.atproto.space.createRecord' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>' \
  -H 'Content-Type: application/json' \
  -d '{
    "space": "at://did:plc:abc123/space/com.example.forum/main",
    "collection": "com.example.forum.post",
    "record": {
      "$type": "com.example.forum.post",
      "text": "Hello from the forum!",
      "createdAt": "2026-05-09T12:00:00Z"
    }
  }'
```

**Input:**

| Field        | Type          | Required | Description             |
| ------------ | ------------- | -------- | ----------------------- |
| `space`      | string        | Yes      | The space to write into |
| `collection` | string (NSID) | Yes      | The record collection   |
| `record`     | object        | Yes      | The record data         |

**Response (201):**

```json
{
  "uri": "at://did:plc:abc123/space/com.example.forum/main/did:plc:author/com.example.forum.post/3l2tkbx7225co",
  "cid": "bafyrei..."
}
```

`createRecord` always inserts a new record. If a record with the generated URI already exists, it returns `409 Conflict`.

## Updating a record

Requires `write` membership in the space.

```ts tab="TypeScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.space.putRecord", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "at://did:plc:abc123/space/com.example.forum/main",
    collection: "com.example.forum.post",
    rkey: "3k2abc",
    record: {
      $type: "com.example.forum.post",
      text: "Hello from the forum!",
      createdAt: "2026-05-09T12:00:00Z",
    },
  }),
});
interface PutRecordResponse {
  uri: string;
  cid: string;
}
const data: PutRecordResponse = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.space.putRecord", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "at://did:plc:abc123/space/com.example.forum/main",
    collection: "com.example.forum.post",
    rkey: "3k2abc",
    record: {
      $type: "com.example.forum.post",
      text: "Hello from the forum!",
      createdAt: "2026-05-09T12:00:00Z",
    },
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("https://happyview.example.com/xrpc/com.atproto.space.putRecord")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .json(&serde_json::json!({
        "space": "at://did:plc:abc123/space/com.example.forum/main",
        "collection": "com.example.forum.post",
        "rkey": "3k2abc",
        "record": {
            "$type": "com.example.forum.post",
            "text": "Hello from the forum!",
            "createdAt": "2026-05-09T12:00:00Z"
        }
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "space": "at://did:plc:abc123/space/com.example.forum/main",
  "collection": "com.example.forum.post",
  "rkey": "3k2abc",
  "record": {
    "$type": "com.example.forum.post",
    "text": "Hello from the forum!",
    "createdAt": "2026-05-09T12:00:00Z"
  }
}`)
req, _ := http.NewRequest("POST",
  "https://happyview.example.com/xrpc/com.atproto.space.putRecord", body)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST 'https://happyview.example.com/xrpc/com.atproto.space.putRecord' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>' \
  -H 'Content-Type: application/json' \
  -d '{
    "space": "at://did:plc:abc123/space/com.example.forum/main",
    "collection": "com.example.forum.post",
    "rkey": "3k2abc",
    "record": {
      "$type": "com.example.forum.post",
      "text": "Hello from the forum!",
      "createdAt": "2026-05-09T12:00:00Z"
    }
  }'
```

**Input:**

| Field        | Type          | Required | Description                                                      |
| ------------ | ------------- | -------- | ---------------------------------------------------------------- |
| `space`      | string        | Yes      | The space to write into                                          |
| `collection` | string (NSID) | Yes      | The record collection                                            |
| `rkey`       | string        | Yes      | The record key                                                   |
| `record`     | object        | Yes      | The record data                                                  |
| `swapRecord` | string        | No       | Expected CID of the existing record (for optimistic concurrency) |

**Response (201):**

```json
{
  "uri": "at://did:plc:abc123/space/com.example.forum/main/did:plc:author/com.example.forum.post/3k2abc",
  "cid": "bafyrei..."
}
```

The author DID is taken from the authenticated user. You can only write records as yourself, so the URI's author component will always be your DID.

`putRecord` performs an upsert: if a record with the same collection + rkey already exists for this author in this space, it's overwritten. Use `swapRecord` to prevent unintended overwrites (see [Optimistic concurrency](#optimistic-concurrency) below).

## Getting a record

Requires `read` membership (or a valid [space credential](credentials.md)).

Members with `read_self` access can only retrieve their own records. Attempting to read another user's record returns `403 Forbidden`.

```ts tab="TypeScript" tab-group="language"
const params = new URLSearchParams({
  space: "at://did:plc:abc123/space/com.example.forum/main",
  collection: "com.example.forum.post",
  rkey: "3k2abc",
});
const response = await fetch(
  `https://happyview.example.com/xrpc/com.atproto.space.getRecord?${params}`,
  {
    headers: {
      "X-Client-Key": CLIENT_KEY,
      "Authorization": `DPoP ${ACCESS_TOKEN}`,
      "DPoP": DPOP_PROOF,
    },
  },
);
interface GetRecordResponse {
  uri: string;
  cid: string;
  value: Record<string, unknown>;
}
const data: GetRecordResponse = await response.json();
```
```js tab="JavaScript" tab-group="language"
const params = new URLSearchParams({
  space: "at://did:plc:abc123/space/com.example.forum/main",
  collection: "com.example.forum.post",
  rkey: "3k2abc",
});
const response = await fetch(
  `https://happyview.example.com/xrpc/com.atproto.space.getRecord?${params}`,
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
    .get("https://happyview.example.com/xrpc/com.atproto.space.getRecord")
    .query(&[
        ("space", "at://did:plc:abc123/space/com.example.forum/main"),
        ("collection", "com.example.forum.post"),
        ("rkey", "3k2abc"),
    ])
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET",
  "https://happyview.example.com/xrpc/com.atproto.space.getRecord?space=at://did:plc:abc123/space/com.example.forum/main&collection=com.example.forum.post&rkey=3k2abc",
  nil)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl 'https://happyview.example.com/xrpc/com.atproto.space.getRecord?space=at://did:plc:abc123/space/com.example.forum/main&collection=com.example.forum.post&rkey=3k2abc' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>'
```

**Parameters:**

| Field        | Type          | Required | Description                     |
| ------------ | ------------- | -------- | ------------------------------- |
| `space`      | string        | Yes      | The space containing the record |
| `collection` | string (NSID) | Yes      | The record collection           |
| `rkey`       | string        | Yes      | The record key                  |

**Response:**

```json
{
  "uri": "at://did:plc:abc123/space/com.example.forum/main/did:plc:author/com.example.forum.post/3k2abc",
  "cid": "bafyrei...",
  "value": {
    "$type": "com.example.forum.post",
    "text": "Hello from the forum!",
    "createdAt": "2026-05-09T12:00:00Z"
  }
}
```

## Listing records

```ts tab="TypeScript" tab-group="language"
const params = new URLSearchParams({
  space: "at://did:plc:abc123/space/com.example.forum/main",
  collection: "com.example.forum.post",
  limit: "20",
});
const response = await fetch(
  `https://happyview.example.com/xrpc/com.atproto.space.listRecords?${params}`,
  {
    headers: {
      "X-Client-Key": CLIENT_KEY,
      "Authorization": `DPoP ${ACCESS_TOKEN}`,
      "DPoP": DPOP_PROOF,
    },
  },
);
interface RecordEntry {
  collection: string;
  rkey: string;
  cid: string;
}
interface ListRecordsResponse {
  records: RecordEntry[];
  cursor?: string;
}
const data: ListRecordsResponse = await response.json();
```
```js tab="JavaScript" tab-group="language"
const params = new URLSearchParams({
  space: "at://did:plc:abc123/space/com.example.forum/main",
  collection: "com.example.forum.post",
  limit: "20",
});
const response = await fetch(
  `https://happyview.example.com/xrpc/com.atproto.space.listRecords?${params}`,
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
    .get("https://happyview.example.com/xrpc/com.atproto.space.listRecords")
    .query(&[
        ("space", "at://did:plc:abc123/space/com.example.forum/main"),
        ("collection", "com.example.forum.post"),
        ("limit", "20"),
    ])
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET",
  "https://happyview.example.com/xrpc/com.atproto.space.listRecords?space=at://did:plc:abc123/space/com.example.forum/main&collection=com.example.forum.post&limit=20",
  nil)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl 'https://happyview.example.com/xrpc/com.atproto.space.listRecords?space=at://did:plc:abc123/space/com.example.forum/main&collection=com.example.forum.post&limit=20' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>'
```

**Parameters:**

| Field        | Type    | Required | Default | Description                       |
| ------------ | ------- | -------- | ------- | --------------------------------- |
| `space`      | string  | Yes      |         | The space to list from            |
| `repo`       | string  | No       |         | Filter by author DID              |
| `collection` | string  | No       |         | Filter by collection NSID         |
| `limit`      | integer | No       | 50      | Max records to return (1-100)     |
| `cursor`     | string  | No       |         | Pagination cursor                 |
| `reverse`    | boolean | No       | `false` | Reverse sort order (oldest first) |

**Response:**

```json
{
  "records": [
    {
      "collection": "com.example.forum.post",
      "rkey": "3k2abc",
      "cid": "bafyrei..."
    }
  ],
  "cursor": "MjAyNi0wNS0wOVQxMjowMDowMFp8YXRzOi8vZGlkOnBsYzphYmMxMjMvY29tLmV4YW1wbGUuZm9ydW0vbWFpbg"
}
```

## Deleting a record

You can only delete your own records. Requires `write` membership.

```ts tab="TypeScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.space.deleteRecord", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "at://did:plc:abc123/space/com.example.forum/main",
    collection: "com.example.forum.post",
    rkey: "3k2abc",
  }),
});
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.space.deleteRecord", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "at://did:plc:abc123/space/com.example.forum/main",
    collection: "com.example.forum.post",
    rkey: "3k2abc",
  }),
});
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("https://happyview.example.com/xrpc/com.atproto.space.deleteRecord")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .json(&serde_json::json!({
        "space": "at://did:plc:abc123/space/com.example.forum/main",
        "collection": "com.example.forum.post",
        "rkey": "3k2abc"
    }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "space": "at://did:plc:abc123/space/com.example.forum/main",
  "collection": "com.example.forum.post",
  "rkey": "3k2abc"
}`)
req, _ := http.NewRequest("POST",
  "https://happyview.example.com/xrpc/com.atproto.space.deleteRecord", body)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST 'https://happyview.example.com/xrpc/com.atproto.space.deleteRecord' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>' \
  -H 'Content-Type: application/json' \
  -d '{
    "space": "at://did:plc:abc123/space/com.example.forum/main",
    "collection": "com.example.forum.post",
    "rkey": "3k2abc"
  }'
```

**Input:**

| Field        | Type          | Required | Description                                                      |
| ------------ | ------------- | -------- | ---------------------------------------------------------------- |
| `space`      | string        | Yes      | The space containing the record                                  |
| `collection` | string (NSID) | Yes      | The record collection                                            |
| `rkey`       | string        | Yes      | The record key                                                   |
| `swapRecord` | string        | No       | Expected CID of the existing record (for optimistic concurrency) |

Attempting to delete another user's record returns `403 Forbidden`.

## Batch writes (applyWrites)

`applyWrites` performs multiple create, update, and delete operations in a single request. Requires `write` membership.

```ts tab="TypeScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.space.applyWrites", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "at://did:plc:abc123/space/com.example.forum/main",
    writes: [
      {
        action: "create",
        collection: "com.example.forum.post",
        value: { $type: "com.example.forum.post", text: "First post" },
      },
      {
        action: "update",
        collection: "com.example.forum.post",
        rkey: "3k2abc",
        value: { $type: "com.example.forum.post", text: "Edited post" },
        swapRecord: "bafyrei...",
      },
      {
        action: "delete",
        collection: "com.example.forum.post",
        rkey: "old-post",
      },
    ],
  }),
});
interface ApplyWritesResult {
  uri?: string;
  cid?: string;
}
const data: { results: ApplyWritesResult[] } = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.space.applyWrites", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "at://did:plc:abc123/space/com.example.forum/main",
    writes: [
      {
        action: "create",
        collection: "com.example.forum.post",
        value: { $type: "com.example.forum.post", text: "First post" },
      },
      {
        action: "update",
        collection: "com.example.forum.post",
        rkey: "3k2abc",
        value: { $type: "com.example.forum.post", text: "Edited post" },
        swapRecord: "bafyrei...",
      },
      {
        action: "delete",
        collection: "com.example.forum.post",
        rkey: "old-post",
      },
    ],
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("https://happyview.example.com/xrpc/com.atproto.space.applyWrites")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .json(&serde_json::json!({
        "space": "at://did:plc:abc123/space/com.example.forum/main",
        "writes": [
            {
                "action": "create",
                "collection": "com.example.forum.post",
                "value": { "$type": "com.example.forum.post", "text": "First post" }
            },
            {
                "action": "update",
                "collection": "com.example.forum.post",
                "rkey": "3k2abc",
                "value": { "$type": "com.example.forum.post", "text": "Edited post" },
                "swapRecord": "bafyrei..."
            },
            {
                "action": "delete",
                "collection": "com.example.forum.post",
                "rkey": "old-post"
            }
        ]
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "space": "at://did:plc:abc123/space/com.example.forum/main",
  "writes": [
    {
      "action": "create",
      "collection": "com.example.forum.post",
      "value": { "$type": "com.example.forum.post", "text": "First post" }
    },
    {
      "action": "update",
      "collection": "com.example.forum.post",
      "rkey": "3k2abc",
      "value": { "$type": "com.example.forum.post", "text": "Edited post" },
      "swapRecord": "bafyrei..."
    },
    {
      "action": "delete",
      "collection": "com.example.forum.post",
      "rkey": "old-post"
    }
  ]
}`)
req, _ := http.NewRequest("POST",
  "https://happyview.example.com/xrpc/com.atproto.space.applyWrites", body)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST 'https://happyview.example.com/xrpc/com.atproto.space.applyWrites' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>' \
  -H 'Content-Type: application/json' \
  -d '{
    "space": "at://did:plc:abc123/space/com.example.forum/main",
    "writes": [
      {
        "action": "create",
        "collection": "com.example.forum.post",
        "value": { "$type": "com.example.forum.post", "text": "First post" }
      },
      {
        "action": "update",
        "collection": "com.example.forum.post",
        "rkey": "3k2abc",
        "value": { "$type": "com.example.forum.post", "text": "Edited post" },
        "swapRecord": "bafyrei..."
      },
      {
        "action": "delete",
        "collection": "com.example.forum.post",
        "rkey": "old-post"
      }
    ]
  }'
```

**Input:**

| Field        | Type   | Required | Description                                          |
| ------------ | ------ | -------- | ---------------------------------------------------- |
| `space`      | string | Yes      | The space to write into                              |
| `swapCommit` | string | No       | Expected space revision (for optimistic concurrency) |
| `writes`     | array  | Yes      | List of write operations                             |

Each write operation has an `action` field:

| Action   | Fields                                       | Description                                          |
| -------- | -------------------------------------------- | ---------------------------------------------------- |
| `create` | `collection`, `value`, `rkey?`               | Insert a new record. Auto-generates rkey if omitted. |
| `update` | `collection`, `rkey`, `value`, `swapRecord?` | Upsert a record.                                     |
| `delete` | `collection`, `rkey`, `swapRecord?`          | Delete a record.                                     |

**Response:**

```json
{
  "results": [
    { "uri": "at://...", "cid": "bafyrei..." },
    { "uri": "at://...", "cid": "bafyrei..." },
    {}
  ]
}
```

Each entry in `results` corresponds to the write at the same index. Create and update operations return `uri` and `cid`; delete operations return an empty object.

## Optimistic concurrency

`swapRecord` and `swapCommit` provide optimistic concurrency control to prevent lost updates when multiple clients write to the same space.

### swapRecord

Pass the `swapRecord` field on `putRecord`, `deleteRecord`, or individual operations within `applyWrites`. The value is the CID of the record you expect to be replacing. If the record's current CID doesn't match, the operation fails with `409 Conflict`.

```json
{
  "space": "at://did:plc:abc123/space/com.example.forum/main",
  "collection": "com.example.forum.post",
  "rkey": "3k2abc",
  "record": { "text": "updated safely" },
  "swapRecord": "bafyrei_old_cid"
}
```

### swapCommit

Pass the `swapCommit` field on `applyWrites` to assert the space's current revision. If another client has written to the space since you last read its state, the operation fails with `409 Conflict` before any writes are applied.

The space's current revision is available as `revision` in the space object returned by `com.atproto.space.getSpace`.

```json
{
  "space": "at://did:plc:abc123/space/com.example.forum/main",
  "swapCommit": "3l2tkbx7225co",
  "writes": [...]
}
```

## Latest commit

Returns the per-user signed commit for a space, including the current revision and deniable commit data.

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/com.atproto.space.getLatestCommit?space=at://did:plc:abc123/space/com.example.forum/main&did=did:plc:author",
  {
    headers: {
      "X-Client-Key": CLIENT_KEY,
      "Authorization": `DPoP ${ACCESS_TOKEN}`,
      "DPoP": DPOP_PROOF,
    },
  },
);
interface LatestCommitResponse {
  rev: string | null;
  commit: {
    ver: number;
    hash: string;
    ikm: string;
    sig: string;
    mac: string;
    rev: string;
  } | null;
}
const data: LatestCommitResponse = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/com.atproto.space.getLatestCommit?space=at://did:plc:abc123/space/com.example.forum/main&did=did:plc:author",
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
    .get("https://happyview.example.com/xrpc/com.atproto.space.getLatestCommit")
    .query(&[
        ("space", "at://did:plc:abc123/space/com.example.forum/main"),
        ("did", "did:plc:author"),
    ])
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET",
  "https://happyview.example.com/xrpc/com.atproto.space.getLatestCommit?space=at://did:plc:abc123/space/com.example.forum/main&did=did:plc:author",
  nil)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl 'https://happyview.example.com/xrpc/com.atproto.space.getLatestCommit?space=at://did:plc:abc123/space/com.example.forum/main&did=did:plc:author' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>'
```

**Parameters:**

| Field   | Type   | Required | Description                          |
| ------- | ------ | -------- | ------------------------------------ |
| `space` | string | Yes      | The space URI                        |
| `did`   | string | Yes      | The DID of the user to get state for |

**Response:**

| Field    | Type         | Description                                                    |
| -------- | ------------ | -------------------------------------------------------------- |
| `rev`    | string/null  | Current revision for this user's repo in the space             |
| `commit` | object/null  | Deniable commit data (`ver`, base64url-encoded `hash`, `ikm`, `sig`, `mac`, and `rev`) |

## Record operation log

Returns the operation log for a user in a space. Each write (create, update, delete) is recorded as an oplog entry.

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/com.atproto.space.listRepoOps?space=at://did:plc:abc123/space/com.example.forum/main&did=did:plc:author",
  {
    headers: {
      "X-Client-Key": CLIENT_KEY,
      "Authorization": `DPoP ${ACCESS_TOKEN}`,
      "DPoP": DPOP_PROOF,
    },
  },
);
const data = await response.json();
// data.ops — array of oplog entries
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/com.atproto.space.listRepoOps?space=at://did:plc:abc123/space/com.example.forum/main&did=did:plc:author",
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
    .get("https://happyview.example.com/xrpc/com.atproto.space.listRepoOps")
    .query(&[
        ("space", "at://did:plc:abc123/space/com.example.forum/main"),
        ("did", "did:plc:author"),
    ])
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET",
  "https://happyview.example.com/xrpc/com.atproto.space.listRepoOps?space=at://did:plc:abc123/space/com.example.forum/main&did=did:plc:author",
  nil)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl 'https://happyview.example.com/xrpc/com.atproto.space.listRepoOps?space=at://did:plc:abc123/space/com.example.forum/main&did=did:plc:author' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>'
```

**Parameters:**

| Field    | Type    | Required | Description                                    |
| -------- | ------- | -------- | ---------------------------------------------- |
| `space`  | string  | Yes      | The space URI                                  |
| `did`    | string  | Yes      | The DID of the user whose ops to list          |
| `limit`          | integer | No       | Max number of entries to return (default 100, max 1000) |
| `cursor`         | string  | No       | Revision to start after (for pagination)       |
| `excludeValues`  | boolean | No       | If `true`, omit record values from response (default `false`) |

**Response:**

```json
{
  "ops": [
    {
      "id": "...",
      "rev": "3l2tkbx7225co",
      "idx": 0,
      "action": "create",
      "collection": "com.example.forum.post",
      "rkey": "3k2abc",
      "cid": "bafyrei...",
      "prev": null,
      "value": { "text": "hello world" },
      "createdAt": "2026-05-09T12:00:00Z"
    }
  ]
}
```

Each entry records a single write operation. The `action` is one of `create`, `update`, or `delete`. The `prev` field contains the CID of the record before the operation (for updates and deletes). The `value` field contains the record's current value (omitted for deletes or when `excludeValues=true`).

## Repo export

Exports a user's full repo within a space as a CAR v1 file. The file contains two roots: the signed commit and a DRISL index (flat DAG-CBOR map of `collection/rkey` to CID). Record blocks follow in lexicographic order.

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/com.atproto.space.getRepo?space=at://did:plc:abc123/space/com.example.forum/main&did=did:plc:author",
  {
    headers: {
      "X-Client-Key": CLIENT_KEY,
      "Authorization": `DPoP ${ACCESS_TOKEN}`,
      "DPoP": DPOP_PROOF,
    },
  },
);
const car = await response.arrayBuffer();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/com.atproto.space.getRepo?space=at://did:plc:abc123/space/com.example.forum/main&did=did:plc:author",
  {
    headers: {
      "X-Client-Key": CLIENT_KEY,
      "Authorization": `DPoP ${ACCESS_TOKEN}`,
      "DPoP": DPOP_PROOF,
    },
  },
);
const car = await response.arrayBuffer();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("https://happyview.example.com/xrpc/com.atproto.space.getRepo")
    .query(&[
        ("space", "at://did:plc:abc123/space/com.example.forum/main"),
        ("did", "did:plc:author"),
    ])
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .send()
    .await?;
let bytes = response.bytes().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET",
  "https://happyview.example.com/xrpc/com.atproto.space.getRepo?space=at://did:plc:abc123/space/com.example.forum/main&did=did:plc:author",
  nil)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl 'https://happyview.example.com/xrpc/com.atproto.space.getRepo?space=at://did:plc:abc123/space/com.example.forum/main&did=did:plc:author' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>' \
  --output repo.car
```

**Parameters:**

| Field   | Type   | Required | Description                          |
| ------- | ------ | -------- | ------------------------------------ |
| `space` | string | Yes      | The space URI                        |
| `did`   | string | Yes      | The DID of the user whose repo to export |

The response body is a CAR v1 file with content type `application/vnd.ipld.car`.

## Listing repos

Returns the list of users who have records in a space, along with their current revision.

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/com.atproto.space.listRepos?space=at://did:plc:abc123/space/com.example.forum/main",
  {
    headers: {
      "X-Client-Key": CLIENT_KEY,
      "Authorization": `DPoP ${ACCESS_TOKEN}`,
      "DPoP": DPOP_PROOF,
    },
  },
);
interface Repo {
  did: string;
  rev: string | null;
}
const data: { repos: Repo[] } = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/com.atproto.space.listRepos?space=at://did:plc:abc123/space/com.example.forum/main",
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
    .get("https://happyview.example.com/xrpc/com.atproto.space.listRepos")
    .query(&[("space", "at://did:plc:abc123/space/com.example.forum/main")])
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET",
  "https://happyview.example.com/xrpc/com.atproto.space.listRepos?space=at://did:plc:abc123/space/com.example.forum/main",
  nil)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl 'https://happyview.example.com/xrpc/com.atproto.space.listRepos?space=at://did:plc:abc123/space/com.example.forum/main' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>'
```

**Parameters:**

| Field   | Type   | Required | Description   |
| ------- | ------ | -------- | ------------- |
| `space` | string | Yes      | The space URI |

**Response:**

```json
{
  "repos": [
    { "did": "did:plc:author1", "rev": "3l2tkbx7225co" },
    { "did": "did:plc:author2", "rev": null }
  ]
}
```

## Getting a blob

Retrieves a blob from a space. The blob is fetched from the author's PDS and proxied through HappyView with access control.

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/com.atproto.space.getBlob?space=at://did:plc:abc123/space/com.example.forum/main&cid=bafyrei...",
  {
    headers: {
      "X-Client-Key": CLIENT_KEY,
      "Authorization": `DPoP ${ACCESS_TOKEN}`,
      "DPoP": DPOP_PROOF,
    },
  },
);
const blob = await response.blob();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/com.atproto.space.getBlob?space=at://did:plc:abc123/space/com.example.forum/main&cid=bafyrei...",
  {
    headers: {
      "X-Client-Key": CLIENT_KEY,
      "Authorization": `DPoP ${ACCESS_TOKEN}`,
      "DPoP": DPOP_PROOF,
    },
  },
);
const blob = await response.blob();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("https://happyview.example.com/xrpc/com.atproto.space.getBlob")
    .query(&[
        ("space", "at://did:plc:abc123/space/com.example.forum/main"),
        ("cid", "bafyrei..."),
    ])
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .send()
    .await?;
let bytes = response.bytes().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET",
  "https://happyview.example.com/xrpc/com.atproto.space.getBlob?space=at://did:plc:abc123/space/com.example.forum/main&cid=bafyrei...",
  nil)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl 'https://happyview.example.com/xrpc/com.atproto.space.getBlob?space=at://did:plc:abc123/space/com.example.forum/main&cid=bafyrei...' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>' \
  --output image.jpg
```

**Parameters:**

| Field   | Type   | Required | Description              |
| ------- | ------ | -------- | ------------------------ |
| `space` | string | Yes      | The space URI            |
| `cid`   | string | Yes      | The CID of the blob      |

The response body is the raw blob data with the original `Content-Type` header preserved.

## Cross-service access

Records can also be read using a [space credential](credentials.md) instead of direct membership. Pass the credential as a Bearer token:

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/com.atproto.space.getRecord?space=...&collection=...&rkey=...",
  {
    headers: {
      "Authorization": `Bearer ${SPACE_CREDENTIAL}`,
    },
  },
);
const data = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/com.atproto.space.getRecord?space=...&collection=...&rkey=...",
  {
    headers: {
      "Authorization": `Bearer ${SPACE_CREDENTIAL}`,
    },
  },
);
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("https://happyview.example.com/xrpc/com.atproto.space.getRecord")
    .query(&[("space", "..."), ("collection", "..."), ("rkey", "...")])
    .header("Authorization", format!("Bearer {}", space_credential))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET",
  "https://happyview.example.com/xrpc/com.atproto.space.getRecord?space=...&collection=...&rkey=...",
  nil)
req.Header.Set("Authorization", "Bearer "+spaceCredential)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl 'https://happyview.example.com/xrpc/com.atproto.space.getRecord?...' \
  -H 'Authorization: Bearer eyJhbGciOiJFUzI1NiIsInR5cCI6InNwYWNlX2NyZWRlbnRpYWwifQ...'
```

A feed generator or other service that isn't a direct member can use a credential issued by the space authority to read data without joining the space. No DPoP auth is needed — the credential itself authenticates the request.

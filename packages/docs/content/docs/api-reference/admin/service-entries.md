---
title: "Service Entries"
---

Manage the service entries published in the instance's DID document. Each entry declares an XRPC service type (e.g. `AtprotoLabeler`, `BskyFeedGenerator`) and controls which lexicons route to HappyView's endpoint. All endpoints require the `settings:manage` permission.

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

## List service entries

```
GET /admin/service-entries
```

```ts tab="TypeScript" tab-group="language"
interface ServiceEntry {
  id: number;
  fragment_id: string;
  service_type: string;
  access_mode: string;
  created_at: string;
  updated_at: string;
}

const response = await fetch("http://127.0.0.1:3000/admin/service-entries", {
  headers,
});
const data: ServiceEntry[] = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/service-entries", {
  headers,
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let client = reqwest::Client::new();
let response = client
    .get("http://127.0.0.1:3000/admin/service-entries")
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/service-entries", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/service-entries -H "$AUTH"
```

**Response**: `200 OK`

```json
[
  {
    "id": 1,
    "fragment_id": "#happyview",
    "service_type": "AtprotoLabeler",
    "access_mode": "all",
    "created_at": "2026-05-09T12:00:00Z",
    "updated_at": "2026-05-09T12:00:00Z"
  }
]
```

## Create a service entry

```
POST /admin/service-entries
```

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/service-entries", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    fragment_id: "#feed",
    service_type: "BskyFeedGenerator",
  }),
});
const data: ServiceEntry = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/service-entries", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    fragment_id: "#feed",
    service_type: "BskyFeedGenerator",
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("http://127.0.0.1:3000/admin/service-entries")
    .bearer_auth(token)
    .json(&serde_json::json!({
        "fragment_id": "#feed",
        "service_type": "BskyFeedGenerator"
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{"fragment_id": "#feed", "service_type": "BskyFeedGenerator"}`)
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/admin/service-entries", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/service-entries \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{ "fragment_id": "#feed", "service_type": "BskyFeedGenerator" }'
```

| Field          | Type   | Required | Description                                                 |
| -------------- | ------ | -------- | ----------------------------------------------------------- |
| `fragment_id`  | string | yes      | DID document fragment (e.g. `#feed`, `#happyview`)          |
| `service_type` | string | yes      | AT Protocol service type (e.g. `BskyFeedGenerator`)         |

The entry is created with `access_mode` set to `all`. The endpoint URL is derived from `PUBLIC_URL`.

**Response**: `201 Created`

## Update a service entry

```
PUT /admin/service-entries/{id}
```

```ts tab="TypeScript" tab-group="language"
await fetch("http://127.0.0.1:3000/admin/service-entries/1", {
  method: "PUT",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    service_type: "AtprotoLabeler",
    access_mode: "allowlist",
  }),
});
```
```js tab="JavaScript" tab-group="language"
await fetch("http://127.0.0.1:3000/admin/service-entries/1", {
  method: "PUT",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    service_type: "AtprotoLabeler",
    access_mode: "allowlist",
  }),
});
```
```rust tab="Rust" tab-group="language"
client
    .put("http://127.0.0.1:3000/admin/service-entries/1")
    .bearer_auth(token)
    .json(&serde_json::json!({
        "service_type": "AtprotoLabeler",
        "access_mode": "allowlist"
    }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{"service_type": "AtprotoLabeler", "access_mode": "allowlist"}`)
req, _ := http.NewRequest("PUT", "http://127.0.0.1:3000/admin/service-entries/1", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X PUT http://127.0.0.1:3000/admin/service-entries/1 \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{ "service_type": "AtprotoLabeler", "access_mode": "allowlist" }'
```

| Field          | Type   | Required | Description                                        |
| -------------- | ------ | -------- | -------------------------------------------------- |
| `fragment_id`  | string | no       | Updated DID document fragment                      |
| `service_type` | string | no       | Updated service type                               |
| `access_mode`  | string | no       | `all`, `allowlist`, or `disabled`                  |

All fields are optional. Only provided fields are updated.

**Response**: `204 No Content`

## Delete a service entry

```
DELETE /admin/service-entries/{id}
```

```ts tab="TypeScript" tab-group="language"
await fetch("http://127.0.0.1:3000/admin/service-entries/1", {
  method: "DELETE",
  headers,
});
```
```js tab="JavaScript" tab-group="language"
await fetch("http://127.0.0.1:3000/admin/service-entries/1", {
  method: "DELETE",
  headers,
});
```
```rust tab="Rust" tab-group="language"
client
    .delete("http://127.0.0.1:3000/admin/service-entries/1")
    .bearer_auth(token)
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("DELETE", "http://127.0.0.1:3000/admin/service-entries/1", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X DELETE http://127.0.0.1:3000/admin/service-entries/1 \
  -H "$AUTH"
```

Returns `404 Not Found` if the entry doesn't exist.

**Response**: `204 No Content`

## List XRPCs for a service entry

```
GET /admin/service-entries/{id}/xrpcs
```

Returns the lexicon IDs (NSIDs) bound to this service entry.

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/service-entries/1/xrpcs", {
  headers,
});
const data: string[] = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/service-entries/1/xrpcs", {
  headers,
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("http://127.0.0.1:3000/admin/service-entries/1/xrpcs")
    .bearer_auth(token)
    .send()
    .await?;
let data: Vec<String> = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/service-entries/1/xrpcs", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/service-entries/1/xrpcs -H "$AUTH"
```

**Response**: `200 OK`

```json
["app.bsky.feed.getFeedSkeleton", "app.bsky.feed.describeFeedGenerator"]
```

## Add XRPCs to a service entry

```
POST /admin/service-entries/{id}/xrpcs
```

```ts tab="TypeScript" tab-group="language"
await fetch("http://127.0.0.1:3000/admin/service-entries/1/xrpcs", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    lexicon_ids: ["app.bsky.feed.getFeedSkeleton"],
  }),
});
```
```js tab="JavaScript" tab-group="language"
await fetch("http://127.0.0.1:3000/admin/service-entries/1/xrpcs", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    lexicon_ids: ["app.bsky.feed.getFeedSkeleton"],
  }),
});
```
```rust tab="Rust" tab-group="language"
client
    .post("http://127.0.0.1:3000/admin/service-entries/1/xrpcs")
    .bearer_auth(token)
    .json(&serde_json::json!({
        "lexicon_ids": ["app.bsky.feed.getFeedSkeleton"]
    }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{"lexicon_ids": ["app.bsky.feed.getFeedSkeleton"]}`)
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/admin/service-entries/1/xrpcs", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/service-entries/1/xrpcs \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{ "lexicon_ids": ["app.bsky.feed.getFeedSkeleton"] }'
```

| Field        | Type     | Required | Description                           |
| ------------ | -------- | -------- | ------------------------------------- |
| `lexicon_ids` | string[] | yes      | Lexicon NSIDs to bind to this entry  |

**Response**: `204 No Content`

## Remove XRPCs from a service entry

```
DELETE /admin/service-entries/{id}/xrpcs
```

```ts tab="TypeScript" tab-group="language"
await fetch("http://127.0.0.1:3000/admin/service-entries/1/xrpcs", {
  method: "DELETE",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    lexicon_ids: ["app.bsky.feed.getFeedSkeleton"],
  }),
});
```
```js tab="JavaScript" tab-group="language"
await fetch("http://127.0.0.1:3000/admin/service-entries/1/xrpcs", {
  method: "DELETE",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    lexicon_ids: ["app.bsky.feed.getFeedSkeleton"],
  }),
});
```
```rust tab="Rust" tab-group="language"
client
    .delete("http://127.0.0.1:3000/admin/service-entries/1/xrpcs")
    .bearer_auth(token)
    .json(&serde_json::json!({
        "lexicon_ids": ["app.bsky.feed.getFeedSkeleton"]
    }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{"lexicon_ids": ["app.bsky.feed.getFeedSkeleton"]}`)
req, _ := http.NewRequest("DELETE", "http://127.0.0.1:3000/admin/service-entries/1/xrpcs", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X DELETE http://127.0.0.1:3000/admin/service-entries/1/xrpcs \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{ "lexicon_ids": ["app.bsky.feed.getFeedSkeleton"] }'
```

| Field        | Type     | Required | Description                              |
| ------------ | -------- | -------- | ---------------------------------------- |
| `lexicon_ids` | string[] | yes      | Lexicon NSIDs to unbind from this entry |

**Response**: `204 No Content`

## List services for a lexicon

```
GET /admin/lexicons/{id}/services
```

Returns service entries that grant access to the specified lexicon.

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/lexicons/app.bsky.feed.getFeedSkeleton/services", {
  headers,
});
const data: ServiceEntry[] = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/lexicons/app.bsky.feed.getFeedSkeleton/services", {
  headers,
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("http://127.0.0.1:3000/admin/lexicons/app.bsky.feed.getFeedSkeleton/services")
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/lexicons/app.bsky.feed.getFeedSkeleton/services", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/lexicons/app.bsky.feed.getFeedSkeleton/services -H "$AUTH"
```

**Response**: `200 OK` — returns an array of `ServiceEntry` objects.

## Sync to PLC directory

These endpoints publish your service entries to the PLC directory so they appear in the DID document. The sync method depends on the [service identity mode](../../getting-started/service-identity.md).

### Direct sync (did:plc mode)

```
POST /admin/service-entries/sync-plc
```

Signs and submits a PLC update operation using the stored rotation key. Only available when the identity mode is `did_plc`.

```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/service-entries/sync-plc -H "$AUTH"
```

Returns `400 Bad Request` if the identity mode is not `did_plc` or no DID is configured.

**Response**: `204 No Content`

### Request PLC token (attach_account mode)

```
POST /admin/service-entries/sync-plc/request
```

Requests a PLC operation signature token via the attached account's PDS. This sends an email confirmation code to the account holder. Only available when the identity mode is `attach_account`.

```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/service-entries/sync-plc/request -H "$AUTH"
```

Returns `400 Bad Request` if the identity mode is not `attach_account`.

**Response**: `204 No Content`

### Submit PLC token (attach_account mode)

```
POST /admin/service-entries/sync-plc/submit
```

Submits the email confirmation token to sign and publish the PLC operation via the attached account's PDS.

```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/service-entries/sync-plc/submit \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{ "token": "123456" }'
```

| Field   | Type   | Required | Description                          |
| ------- | ------ | -------- | ------------------------------------ |
| `token` | string | yes      | Email confirmation code from the PDS |

Returns `400 Bad Request` if the identity mode is not `attach_account`.

**Response**: `204 No Content`

---
title: "Lexicons"
---

Manage lexicons and network lexicons. See the [Lexicons guide](../../guides/lexicons.md) for background on how lexicons drive indexing and XRPC routing.

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

## Upload / upsert a lexicon

```
POST /admin/lexicons
```

```ts tab="TypeScript" tab-group="language"
interface LexiconResult {
  id: string;
  revision: number;
  backfill_job_id: string | null;
}

const response = await fetch("http://127.0.0.1:3000/admin/lexicons", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    lexicon_json: {
      lexicon: 1,
      id: "xyz.statusphere.status",
      defs: {
        main: {
          type: "record",
          key: "tid",
          record: {
            type: "object",
            required: ["status", "createdAt"],
            properties: {
              status: { type: "string", maxGraphemes: 1 },
              createdAt: { type: "string", format: "datetime" },
            },
          },
        },
      },
    },
    backfill: true,
    target_collection: null,
  }),
});
const data: LexiconResult = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/lexicons", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    lexicon_json: {
      lexicon: 1,
      id: "xyz.statusphere.status",
      defs: {
        main: {
          type: "record",
          key: "tid",
          record: {
            type: "object",
            required: ["status", "createdAt"],
            properties: {
              status: { type: "string", maxGraphemes: 1 },
              createdAt: { type: "string", format: "datetime" },
            },
          },
        },
      },
    },
    backfill: true,
    target_collection: null,
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("http://127.0.0.1:3000/admin/lexicons")
    .bearer_auth(token)
    .json(&serde_json::json!({
        "lexicon_json": {
            "lexicon": 1,
            "id": "xyz.statusphere.status",
            "defs": {
                "main": {
                    "type": "record",
                    "key": "tid",
                    "record": {
                        "type": "object",
                        "required": ["status", "createdAt"],
                        "properties": {
                            "status": { "type": "string", "maxGraphemes": 1 },
                            "createdAt": { "type": "string", "format": "datetime" }
                        }
                    }
                }
            }
        },
        "backfill": true,
        "target_collection": null
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "lexicon_json": {
    "lexicon": 1,
    "id": "xyz.statusphere.status",
    "defs": {
      "main": {
        "type": "record",
        "key": "tid",
        "record": {
          "type": "object",
          "required": ["status", "createdAt"],
          "properties": {
            "status": { "type": "string", "maxGraphemes": 1 },
            "createdAt": { "type": "string", "format": "datetime" }
          }
        }
      }
    }
  },
  "backfill": true,
  "target_collection": null
}`)
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/admin/lexicons", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/lexicons \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{
    "lexicon_json": { "lexicon": 1, "id": "xyz.statusphere.status", "defs": { "main": { "type": "record", "key": "tid", "record": { "type": "object", "required": ["status", "createdAt"], "properties": { "status": { "type": "string", "maxGraphemes": 1 }, "createdAt": { "type": "string", "format": "datetime" } } } } } },
    "backfill": true,
    "target_collection": null
  }'
```

| Field               | Type    | Required | Description                                                           |
| ------------------- | ------- | -------- | --------------------------------------------------------------------- |
| `lexicon_json`      | object  | yes      | Raw lexicon JSON (must have `lexicon: 1` and a valid NSID `id`)       |
| `backfill`          | boolean | no       | Whether the first upload triggers a historical backfill (default `true`) |
| `target_collection` | string  | no       | For query/procedure lexicons, the record collection they operate on   |
| `token_cost`        | integer | no       | Token cost for query/procedure endpoints (overrides instance default) |
| `indexed`           | boolean | no       | For record lexicons, whether HappyView ingests the collection. Defaults to `true` for a new lexicon; left out on an update, the stored setting is kept. An unindexed lexicon starts no backfill |

The `id` must be a valid [NSID](https://atproto.com/specs/nsid) — it is the
lexicon's primary key, the XRPC method it is served at, and, for record
lexicons, the collection HappyView filters Jetstream on. An `id` that is empty
or malformed is rejected with `400 Bad Request`.

**Response**: `201 Created` (new) or `200 OK` (upsert)

```json
{
  "id": "xyz.statusphere.status",
  "revision": 1,
  "backfill_job_id": "9f1c8e2a-7b40-4d1e-9f3a-2c5b8d6e1a04"
}
```

`backfill_job_id` is the id of the [backfill job](backfill.md) this upload
started, or `null` if it started none — because `backfill` was `false`, because
this was an upsert rather than a first upload, because the lexicon is not a
record type, or because the caller lacks `backfill:create`. Poll it via
`GET /admin/backfill/status`.

## List lexicons

```
GET /admin/lexicons
```

```ts tab="TypeScript" tab-group="language"
interface Lexicon {
  id: string;
  revision: number;
  lexicon_type: string;
  backfill: boolean;
  // "project" when a file in the instance's project defines this lexicon,
  // otherwise "manual" or "network". `source` carries the same value.
  layer: "project" | "manual" | "network";
  // Project-relative path of the file, for a project lexicon only.
  path: string | null;
  // A dashboard or network row sits under this project file. It is kept and
  // answers again if the file is removed.
  shadows_stored: boolean;
  // null for a project lexicon, which has no stored row.
  created_at: string | null;
  updated_at: string | null;
}

const response = await fetch("http://127.0.0.1:3000/admin/lexicons", {
  headers,
});
const data: Lexicon[] = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/lexicons", {
  headers,
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("http://127.0.0.1:3000/admin/lexicons")
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/lexicons", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/lexicons -H "$AUTH"
```

**Response**: `200 OK`

```json
[
  {
    "id": "xyz.statusphere.status",
    "revision": 1,
    "lexicon_type": "record",
    "backfill": true,
    "indexed": true,
    "created_at": "2025-01-01T00:00:00Z",
    "updated_at": "2025-01-01T00:00:00Z"
  }
]
```

## Get a lexicon

```
GET /admin/lexicons/{id}
```

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/lexicons/xyz.statusphere.status",
  { headers },
);
const data = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/lexicons/xyz.statusphere.status",
  { headers },
);
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("http://127.0.0.1:3000/admin/lexicons/xyz.statusphere.status")
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/lexicons/xyz.statusphere.status", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/lexicons/xyz.statusphere.status -H "$AUTH"
```

**Response**: `200 OK` with full lexicon details including raw JSON.

The response carries the same `indexed` boolean as the list (`true` for every lexicon that is not an unindexed record type).

## Delete a lexicon

```
DELETE /admin/lexicons/{id}
```

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/lexicons/xyz.statusphere.status",
  {
    method: "DELETE",
    headers,
  },
);
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/lexicons/xyz.statusphere.status",
  {
    method: "DELETE",
    headers,
  },
);
```
```rust tab="Rust" tab-group="language"
let response = client
    .delete("http://127.0.0.1:3000/admin/lexicons/xyz.statusphere.status")
    .bearer_auth(token)
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("DELETE", "http://127.0.0.1:3000/admin/lexicons/xyz.statusphere.status", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X DELETE http://127.0.0.1:3000/admin/lexicons/xyz.statusphere.status -H "$AUTH"
```

**Response**: `204 No Content`

## Lexicons a project file defines

When the instance runs a project, a lexicon defined by a file under `src/` answers ahead of any stored row with the same id. Uploading, deleting, adding as a network lexicon or removing as one is refused with `409 Conflict`:

    { "error": "defined by the project file src/lexicons/com/example/thing.json; change that file instead" }

A stored row the file shadows is kept as it is and answers again when the file is removed.

## Network Lexicons

Network lexicons are fetched from the atproto network via DNS TXT resolution and kept updated via the Jetstream subscription. See [Lexicons - Network lexicons](../../guides/lexicons.md#network-lexicons) for background.

### Add a network lexicon

```
POST /admin/network-lexicons
```

```ts tab="TypeScript" tab-group="language"
interface NetworkLexiconResult {
  nsid: string;
  authority_did: string;
  revision: number;
}

const response = await fetch("http://127.0.0.1:3000/admin/network-lexicons", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    nsid: "xyz.statusphere.status",
    target_collection: null,
  }),
});
const data: NetworkLexiconResult = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/network-lexicons", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    nsid: "xyz.statusphere.status",
    target_collection: null,
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("http://127.0.0.1:3000/admin/network-lexicons")
    .bearer_auth(token)
    .json(&serde_json::json!({
        "nsid": "xyz.statusphere.status",
        "target_collection": null
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "nsid": "xyz.statusphere.status",
  "target_collection": null
}`)
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/admin/network-lexicons", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/network-lexicons \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{
    "nsid": "xyz.statusphere.status",
    "target_collection": null
  }'
```

| Field               | Type   | Required | Description                                                         |
| ------------------- | ------ | -------- | ------------------------------------------------------------------- |
| `nsid`              | string | yes      | The NSID of the lexicon to watch                                    |
| `target_collection` | string | no       | For query/procedure lexicons, the record collection they operate on |
| `indexed`           | boolean | no     | Whether HappyView ingests the record collection. Defaults to `true` for a new lexicon; left out on an update, the stored setting is kept |

HappyView resolves the NSID authority via DNS TXT, fetches the lexicon from the authority's PDS, parses it, and stores it.

**Response**: `201 Created`

```json
{
  "nsid": "xyz.statusphere.status",
  "authority_did": "did:plc:authority",
  "revision": 1
}
```

### List network lexicons

```
GET /admin/network-lexicons
```

```ts tab="TypeScript" tab-group="language"
interface NetworkLexicon {
  nsid: string;
  authority_did: string;
  target_collection: string | null;
  last_fetched_at: string;
  created_at: string;
  declared: boolean;
}

const response = await fetch("http://127.0.0.1:3000/admin/network-lexicons", {
  headers,
});
const data: NetworkLexicon[] = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/network-lexicons", {
  headers,
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("http://127.0.0.1:3000/admin/network-lexicons")
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/network-lexicons", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/network-lexicons -H "$AUTH"
```

**Response**: `200 OK`

```json
[
  {
    "nsid": "xyz.statusphere.status",
    "authority_did": "did:plc:authority",
    "target_collection": null,
    "last_fetched_at": "2025-01-01T00:00:00Z",
    "created_at": "2025-01-01T00:00:00Z"
  }
]
```

### Remove a network lexicon

```
DELETE /admin/network-lexicons/{nsid}
```

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/network-lexicons/xyz.statusphere.status",
  {
    method: "DELETE",
    headers,
  },
);
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/network-lexicons/xyz.statusphere.status",
  {
    method: "DELETE",
    headers,
  },
);
```
```rust tab="Rust" tab-group="language"
let response = client
    .delete("http://127.0.0.1:3000/admin/network-lexicons/xyz.statusphere.status")
    .bearer_auth(token)
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("DELETE", "http://127.0.0.1:3000/admin/network-lexicons/xyz.statusphere.status", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X DELETE http://127.0.0.1:3000/admin/network-lexicons/xyz.statusphere.status \
  -H "$AUTH"
```

Removes the network lexicon tracking and also deletes the lexicon from the `happyview_lexicons` table and in-memory registry.

A network lexicon listed under `[network_lexicons] include` in the project's `happyview.toml` reports `declared: true`, and removing it returns `409 Conflict`; remove it from the file instead.

**Response**: `204 No Content`

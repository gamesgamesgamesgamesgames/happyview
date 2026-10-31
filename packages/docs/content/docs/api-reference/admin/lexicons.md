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

### When the lexicon is not valid

The document is checked against the lexicon spec before anything is stored. A
document that cannot work is refused with `400 Bad Request` and **one entry per
field**, so an editor can point at each place rather than showing one sentence:

    {
      "error": "the lexicon is not valid: defs.main.key: unknown record key 'uuid': must be tid, nsid, any, or literal:<value> (and 2 more)",
      "issues": [
        {
          "path": "defs.main.key",
          "message": "unknown record key 'uuid': must be tid, nsid, any, or literal:<value>",
          "severity": "error"
        },
        {
          "path": "defs.main.record.properties.a.ref",
          "message": "'#missing' names no def in this lexicon ('missing' is not defined)",
          "severity": "error"
        }
      ]
    }

`path` is a dotted path into the submitted document, with array entries as
`required[0]`; it is the empty string for a problem with the document as a
whole, such as its size. `error` still carries one sentence for anything that
reads only that field.

**`severity` matters.** Only an `error` refuses the upload. A `warning` is a
document that works but reads oddly — an `array` with no `items`, a
`minLength` above its `maxLength`, a def name that is not camelCase — and it is
reported alongside the errors so one answer says everything.

An upload whose only issues are warnings **succeeds**, and carries them in
`issues` on the success response, so nothing is accepted in silence:

    {
      "id": "com.example.events",
      "revision": 1,
      "backfill_job_id": null,
      "issues": [
        {
          "path": "defs.main.type",
          "message": "this is a valid 'subscription' lexicon, but HappyView does not serve subscriptions yet: it will be stored and no endpoint will be generated for it",
          "severity": "warning"
        }
      ]
    }

That example is the warning most worth knowing about. A `subscription` lexicon
is valid, and HappyView serves no subscriptions
([#7](https://github.com/gamesgamesgamesgamesgames/happyview/issues/7)): it is
stored and no endpoint is generated. Accepting one quietly would read as
support, so the upload says what will happen instead. `record`, `query` and
`procedure` warn about nothing.

What is checked: the `lexicon` version and the `id` NSID; that `defs` is a
non-empty object; that every def names a known type; that `record`, `query`,
`procedure` and `subscription` appear only as `main`; that a record says how
its rkey is chosen (`tid`, `nsid`, `any` or `literal:<value>`) and carries an
object schema; that a `ref` or a `union` member resolves to a def this document
defines, when it names one (a ref into another lexicon is checked only for
shape, since that document is not here); that an object's `required` and
`nullable` name properties it actually has; and that the document is at most
256 KiB.

### The same rules in a project

A project's lexicon files go through the same function, so a file under `src/`
cannot carry a lexicon this endpoint would accept and then fail on. It renders
them as build diagnostics rather than per field.

**Boot is deliberately more forgiving than a deploy.** `happyview check`, a
project deploy and `happyview test` refuse a lexicon that violates the spec.
Boot only *warns* about it, names the file, and goes on to serve, because
upgrading HappyView must never turn an instance that served yesterday into one
that exits 1 over a file it already accepted. The next check or deploy refuses
it, which is where someone is watching.

Two things stay fatal at boot, because neither is new behaviour and the build
cannot proceed without them: an `id` that is missing or is not an NSID, and an
unsupported `lexicon` version.

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

### Also deleting the collection's records

Deleting a lexicon stops live indexing; on its own it leaves every record
already indexed under that collection in place. Add `delete_records=true` to
delete those too:

```
DELETE /admin/lexicons/{id}?delete_records=true
```

```sh tab="cURL" tab-group="language"
curl -X DELETE "http://127.0.0.1:3000/admin/lexicons/xyz.statusphere.status?delete_records=true" -H "$AUTH"
```

**Response**: `202 Accepted`

    { "job_id": "0f9f...", "collection": "xyz.statusphere.status" }

The records are deleted by a `happyview.delete-collection` job, so a large
collection can be followed, paused and cancelled through
[the jobs API](jobs.md). The lexicon itself is removed before the job is
enqueued, so live indexing cannot put records back while it runs.

Requires `records:delete-collection` and `records:delete` in addition to
`lexicons:delete` — the same permissions as
[deleting a collection directly](records.md).

Only a **record** lexicon has records of its own. A query or procedure lexicon
operates on another lexicon's collection through `target_collection`, so
`delete_records=true` for one is refused with `400 Bad Request` naming that
collection rather than deleting records the request did not describe.

Records a record script kept after the source deleted them are included: an
operator deleting the collection overrides what a script chose to do with an
upstream delete.

## How many records a collection holds

```
GET /admin/lexicons/{id}/record-count
```

```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/lexicons/xyz.statusphere.status/record-count -H "$AUTH"
```

**Response**: `200 OK`

    { "collection": "xyz.statusphere.status", "count": 1204 }

A separate route rather than a field on the lexicon because it counts the
collection, which is far more expensive than reading the lexicon. The dashboard
asks for it when the delete confirmation opens. Record lexicons only; anything
else is a `400`. Requires `lexicons:read` and `records:read`.

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

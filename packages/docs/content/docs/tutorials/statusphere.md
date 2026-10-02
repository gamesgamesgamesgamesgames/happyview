---
title: "Statusphere"
---

[Statusphere](https://github.com/bluesky-social/statusphere-example-app) is an example atproto application where users set their current status as a single emoji. It's a great way to learn how HappyView works because the data model is simple but the queries are interesting.

In this tutorial, you'll set up HappyView to act as the AppView for Statusphere. By the end, you'll have indexed records and working XRPC endpoints.

<Callout type="idea">
This tutorial assumes you have a running HappyView instance. If you don't, start with the [Quickstart](../getting-started/deployment/railway.md) or one of the local development guides ([Docker](../getting-started/deployment/docker.md), [from source](../getting-started/deployment/other.md)).
</Callout>

## The Statusphere lexicon

Statusphere uses a single record type, `xyz.statusphere.status`. Each record has two fields:

- `status`: a single emoji
- `createdAt`: a timestamp

Users can set their status as many times as they want. Each status is a new record in their repository, keyed by a TID (timestamp-based identifier). The most recent record is their "current" status.

For more background on how the app works, see the [atproto Statusphere guide](https://atproto.com/guides/applications).

## Step 1: Add the record lexicon

First, tell HappyView to start indexing Statusphere records. Since `xyz.statusphere.status` is [published on the atproto network](../guides/lexicons.md#network-lexicons), you can add it directly from the dashboard:

1. Go to **Lexicons > Add Lexicon > Network**
2. Enter `xyz.statusphere.status`
3. HappyView resolves the schema from its authority domain records and shows a preview
4. Enable the **Backfill** toggle so HappyView fetches existing records from the network
5. Click **Add**

HappyView now subscribes to `xyz.statusphere.status` via Jetstream and kicks off a backfill job to index historical records.

<Callout type="idea">
You can also add lexicons via the [admin API](../api-reference/admin/lexicons.md). This is useful for automation or CI/CD workflows:

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/lexicons", {
  method: "POST",
  headers: {
    Authorization: `Bearer ${TOKEN}`,
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
  }),
});
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/lexicons", {
  method: "POST",
  headers: {
    Authorization: `Bearer ${TOKEN}`,
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
  }),
});
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("http://127.0.0.1:3000/admin/lexicons")
    .header("Authorization", format!("Bearer {}", token))
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
        "backfill": true
    }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := `{
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
  "backfill": true
}`
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/admin/lexicons", bytes.NewBufferString(body))
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/lexicons \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
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
    "backfill": true
  }'
```

</Callout>

## Step 2: Verify records are being indexed

Once the backfill starts, you should see records appearing in the dashboard:

1. The **home page** shows a live record count and per-collection breakdown
2. Go to **Records** to browse individual indexed statuses
3. Go to **Backfill** to watch the backfill job progress — you'll see the number of repos processed and records fetched

## Step 3: Create an API client

Before you can call any XRPC endpoint, you need an [API client](../guides/api-clients.md). The client key identifies your application to HappyView and is required on every request.

1. Go to **Settings > API Clients > New client**
2. Set the **Name** to something like "Statusphere Dev"
3. Set the **Client ID URL** and **Client URI** to your app's URL (for local testing, `http://localhost:3000` works)
4. Add a **Redirect URI** (e.g. `http://localhost:3000/oauth/callback`)
5. Click **Create**

Copy the `hvc_`-prefixed **client key** — you'll use it in every request. If you created a confidential client, also save the `hvs_`-prefixed **client secret** immediately; it's only shown once.

For the rest of this tutorial, we'll use `$CLIENT_KEY` to refer to your client key.

## Step 4: Add a query endpoint for listing statuses

Now add a query endpoint to read the indexed data:

1. Go to **Lexicons > Add Lexicon > Local**
2. In the JSON editor, set the `id` to `xyz.statusphere.listStatuses` and change the type to `query`:

```json
{
  "lexicon": 1,
  "id": "xyz.statusphere.listStatuses",
  "defs": {
    "main": {
      "type": "query"
    }
  }
}
```

3. A [Lua script](../guides/lua-scripting.md) editor appears automatically. Replace the default script with:

```lua
collection = "xyz.statusphere.status"

function handle()
  if params.uri then
    local record = db.get(params.uri)
    if not record then
      error("record not found")
    end
    return { record = record }
  end

  return db.query({
    collection = collection,
    did = params.did,
    limit = tonumber(params.limit) or 20,
    cursor = params.cursor,
  })
end
```

The `collection` variable at the top tells the script which record collection to query. The `handle()` function supports single-record lookups by URI and paginated listing with an optional DID filter.

4. Click **Upload**

Try it out:

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/xrpc/xyz.statusphere.listStatuses?limit=5",
  { headers: { "X-Client-Key": CLIENT_KEY } },
);
const data = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/xrpc/xyz.statusphere.listStatuses?limit=5",
  { headers: { "X-Client-Key": CLIENT_KEY } },
);
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("http://127.0.0.1:3000/xrpc/xyz.statusphere.listStatuses")
    .query(&[("limit", "5")])
    .header("X-Client-Key", client_key)
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/xrpc/xyz.statusphere.listStatuses?limit=5", nil)
req.Header.Set("X-Client-Key", clientKey)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl "http://127.0.0.1:3000/xrpc/xyz.statusphere.listStatuses?limit=5" \
  -H "X-Client-Key: $CLIENT_KEY"
```

```json
{
  "records": [
    {
      "uri": "at://did:plc:abc/xyz.statusphere.status/3abc123",
      "status": "😊",
      "createdAt": "2025-01-01T12:00:00Z"
    },
    {
      "uri": "at://did:plc:def/xyz.statusphere.status/3def456",
      "status": "🌟",
      "createdAt": "2025-01-01T11:30:00Z"
    }
  ],
  "cursor": "MjAyNS0wMS0wMVQxMjowMDowMFp8YXQ6Ly9kaWQ6..."
}
```

Filter by a specific user:

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/xrpc/xyz.statusphere.listStatuses?did=did:plc:abc&limit=1",
  { headers: { "X-Client-Key": CLIENT_KEY } },
);
const data = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/xrpc/xyz.statusphere.listStatuses?did=did:plc:abc&limit=1",
  { headers: { "X-Client-Key": CLIENT_KEY } },
);
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("http://127.0.0.1:3000/xrpc/xyz.statusphere.listStatuses")
    .query(&[("did", "did:plc:abc"), ("limit", "1")])
    .header("X-Client-Key", client_key)
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/xrpc/xyz.statusphere.listStatuses?did=did:plc:abc&limit=1", nil)
req.Header.Set("X-Client-Key", clientKey)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl "http://127.0.0.1:3000/xrpc/xyz.statusphere.listStatuses?did=did:plc:abc&limit=1" \
  -H "X-Client-Key: $CLIENT_KEY"
```

Fetch a single record by URI:

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/xrpc/xyz.statusphere.listStatuses?uri=at://did:plc:abc/xyz.statusphere.status/3abc123",
  { headers: { "X-Client-Key": CLIENT_KEY } },
);
const data = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/xrpc/xyz.statusphere.listStatuses?uri=at://did:plc:abc/xyz.statusphere.status/3abc123",
  { headers: { "X-Client-Key": CLIENT_KEY } },
);
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("http://127.0.0.1:3000/xrpc/xyz.statusphere.listStatuses")
    .query(&[("uri", "at://did:plc:abc/xyz.statusphere.status/3abc123")])
    .header("X-Client-Key", client_key)
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/xrpc/xyz.statusphere.listStatuses?uri=at://did:plc:abc/xyz.statusphere.status/3abc123", nil)
req.Header.Set("X-Client-Key", clientKey)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl "http://127.0.0.1:3000/xrpc/xyz.statusphere.listStatuses?uri=at://did:plc:abc/xyz.statusphere.status/3abc123" \
  -H "X-Client-Key: $CLIENT_KEY"
```

## Step 5: Add a procedure endpoint for setting status

Add a write endpoint so users can set their status through your AppView:

1. Go to **Lexicons > Add Lexicon > Local**
2. In the JSON editor, set the `id` to `xyz.statusphere.setStatus` and change the type to `procedure`:

```json
{
  "lexicon": 1,
  "id": "xyz.statusphere.setStatus",
  "defs": {
    "main": {
      "type": "procedure"
    }
  }
}
```

3. A default Lua script is generated — replace it with:

```lua
collection = "xyz.statusphere.status"

function handle()
  local r = Record(collection, {
    status = input.status,
    createdAt = now(),
  })
  r:save()
  return { uri = r._uri, cid = r._cid }
end
```

4. Click **Upload**

This creates a `POST /xrpc/xyz.statusphere.setStatus` endpoint that creates records on the user's PDS and indexes them locally.

## Step 6: Test the procedure endpoint

Set a status. This requires DPoP authentication — the [JavaScript SDK](../sdk/overview.md) handles this for you, but you can test with curl if you have a token:

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/xrpc/xyz.statusphere.setStatus", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    Authorization: `DPoP ${ACCESS_TOKEN}`,
    DPoP: DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({ status: "🚀" }),
});
const data = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/xrpc/xyz.statusphere.setStatus", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    Authorization: `DPoP ${ACCESS_TOKEN}`,
    DPoP: DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({ status: "🚀" }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("http://127.0.0.1:3000/xrpc/xyz.statusphere.setStatus")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", dpop_proof)
    .json(&serde_json::json!({ "status": "🚀" }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := `{ "status": "🚀" }`
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/xrpc/xyz.statusphere.setStatus", bytes.NewBufferString(body))
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/xrpc/xyz.statusphere.setStatus \
  -H "X-Client-Key: $CLIENT_KEY" \
  -H "Authorization: DPoP $TOKEN" \
  -H "DPoP: $DPOP_PROOF" \
  -H "Content-Type: application/json" \
  -d '{ "status": "🚀" }'
```

```json
{
  "uri": "at://did:plc:yourDID/xyz.statusphere.status/3xyz789",
  "cid": "bafyreiabc123..."
}
```

The record is created on your PDS and immediately indexed by HappyView.

## What you've built

With three lexicons and a few lines of Lua, you have a complete Statusphere AppView:

- **Real-time indexing** of `xyz.statusphere.status` records from the entire atproto network
- **Historical backfill** of existing status records
- **A query endpoint** (`xyz.statusphere.listStatuses`) with filtering, pagination, and single-record lookups
- **A write endpoint** (`xyz.statusphere.setStatus`) that creates records on the user's PDS and indexes them locally

Everything was done through the dashboard — no server restarts, no config files, no deploys. For automation and CI/CD, the same operations are available via the [admin API](../api-reference/admin/admin-api.md).

## Next steps

- [API Clients](../guides/api-clients.md): Public vs. confidential clients, DPoP authentication, and rate limiting
- [Lua Scripting](../guides/lua-scripting.md): Explore the full Record and database APIs to build more complex queries
- [Lexicons](../guides/lexicons.md): Learn about network lexicons, the backfill flag, and record collections
- [XRPC API](../api-reference/xrpc-api.md): Understand how the generated endpoints behave
- [Admin API](../api-reference/admin/admin-api.md): Automate lexicon management via the API
- [Statusphere example app](https://github.com/bluesky-social/statusphere-example-app): See the full Statusphere frontend
- [atproto Statusphere guide](https://atproto.com/guides/applications): How the app works at the protocol level

---
title: "XRPC API"
---

[XRPC](https://atproto.com/specs/xrpc) is the HTTP-based RPC protocol used by the atproto. HappyView dynamically registers XRPC endpoints based on your uploaded [lexicons](../guides/lexicons.md): query lexicons become `GET /xrpc/{nsid}` routes, procedure lexicons become `POST /xrpc/{nsid}` routes.

A query or procedure is served by the [Lua script](../guides/lua-scripting.md) attached to it. Without one, a query answers only a single-record lookup by `uri`, and every other call answers `501 Not Implemented` (described below).

## Auth

XRPC routes accept several authentication methods:

- **DPoP auth** — `Authorization: DPoP <token>` + `DPoP` proof header + `X-Client-Key`
- **Service auth JWTs** — `Authorization: Bearer <service_auth_jwt>` (inter-service calls). A token whose `lxm` names a different method than the one called is rejected.
- **Space credentials** — `Authorization: Atproto-Space <credential>` with an `Atproto-Space-Audience` header and an HTTP Message Signature (space routes only). See [Space credentials](../experimental/spaces/credentials.md#using-a-credential).
- **Cookie-based session auth** — signed session cookies (used by the dashboard, falls back when no `Authorization` header is present)
- **Anonymous** — no auth headers (identity is `nil` in Lua scripts)

Bearer API keys (`hv_*`) are rejected on XRPC routes — they are only accepted on the [admin API](admin/admin-api.md).

Default auth behavior:

- **Queries** (`GET /xrpc/{method}`): unauthenticated by default (identity available if provided)
- **Procedures** (`POST /xrpc/{method}`): require authentication (DPoP, session cookie, or service auth)
- **getProfile**: requires auth
- **uploadBlob**: requires auth

## Fixed endpoints

These endpoints are always available regardless of which lexicons are loaded.

### Health check

```
GET /health
```

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/health");
const text = await response.text(); // "ok"
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/health");
const text = await response.text(); // "ok"
```
```rust tab="Rust" tab-group="language"
let client = reqwest::Client::new();

let response = client
    .get("http://127.0.0.1:3000/health")
    .send()
    .await?;

let text = response.text().await?; // "ok"
```
```go tab="Go" tab-group="language"
resp, err := http.Get("http://127.0.0.1:3000/health")
```
```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/health
```

**Response**: `200 OK` with body `ok`

### Get profile

```
GET /xrpc/app.bsky.actor.getProfile
```

Returns the authenticated user's profile, resolved from their PDS via PLC directory lookup.

```ts tab="TypeScript" tab-group="language"
const CLIENT_KEY = "hvc_..."; // your API client key
const TOKEN = "..."; // your access token

interface ProfileResponse {
  did: string;
  handle: string;
  displayName: string;
  description: string;
  avatarURL: string;
}

const response = await fetch(
  "http://127.0.0.1:3000/xrpc/app.bsky.actor.getProfile",
  {
    headers: {
      "X-Client-Key": CLIENT_KEY,
      Authorization: `Bearer ${TOKEN}`,
    },
  },
);

const profile: ProfileResponse = await response.json();
```
```js tab="JavaScript" tab-group="language"
const CLIENT_KEY = "hvc_..."; // your API client key
const TOKEN = "..."; // your access token

const response = await fetch(
  "http://127.0.0.1:3000/xrpc/app.bsky.actor.getProfile",
  {
    headers: {
      "X-Client-Key": CLIENT_KEY,
      Authorization: `Bearer ${TOKEN}`,
    },
  },
);

const profile = await response.json();
```
```rust tab="Rust" tab-group="language"
let client_key = "hvc_..."; // your API client key
let token = "..."; // your access token

let response = client
    .get("http://127.0.0.1:3000/xrpc/app.bsky.actor.getProfile")
    .header("X-Client-Key", client_key)
    .bearer_auth(token)
    .send()
    .await?;

let profile: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
clientKey := "hvc_..." // your API client key
token := "..."         // your access token

req, _ := http.NewRequest("GET",
  "http://127.0.0.1:3000/xrpc/app.bsky.actor.getProfile", nil)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "Bearer "+token)

resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/xrpc/app.bsky.actor.getProfile \
  -H "X-Client-Key: $CLIENT_KEY" \
  -H "Authorization: Bearer $TOKEN"
```

**Response**: `200 OK`

```json
{
  "did": "did:plc:abc123",
  "handle": "user.bsky.social",
  "displayName": "User Name",
  "description": "Bio text",
  "avatarURL": "https://pds.example.com/xrpc/com.atproto.sync.getBlob?did=did:plc:abc123&cid=bafyabc"
}
```

### Upload blob

```
POST /xrpc/com.atproto.repo.uploadBlob
```

Proxies a blob upload to the authenticated user's PDS. Maximum size: 50MB.

```ts tab="TypeScript" tab-group="language"
import { readFile } from "node:fs/promises";

const imageData = await readFile("image.png");

const response = await fetch(
  "http://127.0.0.1:3000/xrpc/com.atproto.repo.uploadBlob",
  {
    method: "POST",
    headers: {
      "X-Client-Key": CLIENT_KEY,
      Authorization: `Bearer ${TOKEN}`,
      "Content-Type": "image/png",
    },
    body: imageData,
  },
);
```
```js tab="JavaScript" tab-group="language"
import { readFile } from "node:fs/promises";

const imageData = await readFile("image.png");

const response = await fetch(
  "http://127.0.0.1:3000/xrpc/com.atproto.repo.uploadBlob",
  {
    method: "POST",
    headers: {
      "X-Client-Key": CLIENT_KEY,
      Authorization: `Bearer ${TOKEN}`,
      "Content-Type": "image/png",
    },
    body: imageData,
  },
);
```
```rust tab="Rust" tab-group="language"
let image_data = std::fs::read("image.png")?;

let response = client
    .post("http://127.0.0.1:3000/xrpc/com.atproto.repo.uploadBlob")
    .header("X-Client-Key", client_key)
    .bearer_auth(token)
    .header("Content-Type", "image/png")
    .body(image_data)
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
imageData, _ := os.ReadFile("image.png")

req, _ := http.NewRequest("POST",
  "http://127.0.0.1:3000/xrpc/com.atproto.repo.uploadBlob",
  bytes.NewReader(imageData))
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "image/png")

resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/xrpc/com.atproto.repo.uploadBlob \
  -H "X-Client-Key: $CLIENT_KEY" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: image/png" \
  --data-binary @image.png
```

**Response**: proxied from the user's PDS.

## Dynamic query endpoints

Query endpoints are generated from lexicons with `type: "query"`. The method's [Lua script](../guides/lua-scripting.md) handles every call and decides what the response is. A query with no script answers one call, a single-record lookup by `uri`; any other call answers [`501`](#method-has-no-script).

### Single record

```
GET /xrpc/{method}?uri={at-uri}
```

```ts tab="TypeScript" tab-group="language"
const CLIENT_KEY = "hvc_..."; // your API client key

const params = new URLSearchParams({
  uri: "at://did:plc:abc/xyz.statusphere.status/abc123",
});

interface RecordResponse {
  record: {
    uri: string;
    $type: string;
    status: string;
    createdAt: string;
  };
}

const response = await fetch(
  `http://127.0.0.1:3000/xrpc/xyz.statusphere.listStatuses?${params}`,
  { headers: { "X-Client-Key": CLIENT_KEY } },
);

const data: RecordResponse = await response.json();
```
```js tab="JavaScript" tab-group="language"
const CLIENT_KEY = "hvc_..."; // your API client key

const params = new URLSearchParams({
  uri: "at://did:plc:abc/xyz.statusphere.status/abc123",
});

const response = await fetch(
  `http://127.0.0.1:3000/xrpc/xyz.statusphere.listStatuses?${params}`,
  { headers: { "X-Client-Key": CLIENT_KEY } },
);

const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let client_key = "hvc_..."; // your API client key

let response = client
    .get("http://127.0.0.1:3000/xrpc/xyz.statusphere.listStatuses")
    .query(&[("uri", "at://did:plc:abc/xyz.statusphere.status/abc123")])
    .header("X-Client-Key", client_key)
    .send()
    .await?;

let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
clientKey := "hvc_..." // your API client key

req, _ := http.NewRequest("GET",
  "http://127.0.0.1:3000/xrpc/xyz.statusphere.listStatuses?uri=at%3A%2F%2Fdid%3Aplc%3Aabc%2Fxyz.statusphere.status%2Fabc123",
  nil)
req.Header.Set("X-Client-Key", clientKey)

resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl "http://127.0.0.1:3000/xrpc/xyz.statusphere.listStatuses?uri=at%3A%2F%2Fdid%3Aplc%3Aabc%2Fxyz.statusphere.status%2Fabc123" \
  -H "X-Client-Key: $CLIENT_KEY"
```

**Response**: `200 OK`

```json
{
  "record": {
    "uri": "at://did:plc:abc/xyz.statusphere.status/abc123",
    "$type": "xyz.statusphere.status",
    "status": "\ud83d\ude0a",
    "createdAt": "2025-01-01T12:00:00Z"
  }
}
```

Media blobs are automatically enriched with a `url` field pointing to the user's PDS.

## Dynamic procedure endpoints

Procedure endpoints are generated from lexicons with `type: "procedure"`. The method's [Lua script](../guides/lua-scripting.md) handles the call: it validates the body, writes whatever records it writes, and returns the response. A procedure with no script answers every call with [`501`](#method-has-no-script); HappyView writes nothing to the caller's PDS on its own.

### Calling a procedure

```
POST /xrpc/{method}
```

```ts tab="TypeScript" tab-group="language"
const CLIENT_KEY = "hvc_..."; // your API client key
const ACCESS_TOKEN = "..."; // DPoP access token
const DPOP_PROOF = "..."; // DPoP proof JWT

const response = await fetch(
  "http://127.0.0.1:3000/xrpc/xyz.statusphere.setStatus",
  {
    method: "POST",
    headers: {
      "X-Client-Key": CLIENT_KEY,
      Authorization: `DPoP ${ACCESS_TOKEN}`,
      DPoP: DPOP_PROOF,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      status: "\ud83d\ude0a",
      createdAt: "2025-01-01T12:00:00Z",
    }),
  },
);
```
```js tab="JavaScript" tab-group="language"
const CLIENT_KEY = "hvc_..."; // your API client key
const ACCESS_TOKEN = "..."; // DPoP access token
const DPOP_PROOF = "..."; // DPoP proof JWT

const response = await fetch(
  "http://127.0.0.1:3000/xrpc/xyz.statusphere.setStatus",
  {
    method: "POST",
    headers: {
      "X-Client-Key": CLIENT_KEY,
      Authorization: `DPoP ${ACCESS_TOKEN}`,
      DPoP: DPOP_PROOF,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      status: "\ud83d\ude0a",
      createdAt: "2025-01-01T12:00:00Z",
    }),
  },
);
```
```rust tab="Rust" tab-group="language"
let client_key = "hvc_..."; // your API client key
let access_token = "..."; // DPoP access token
let dpop_proof = "..."; // DPoP proof JWT

let response = client
    .post("http://127.0.0.1:3000/xrpc/xyz.statusphere.setStatus")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", dpop_proof)
    .json(&serde_json::json!({
        "status": "\ud83d\ude0a",
        "createdAt": "2025-01-01T12:00:00Z"
    }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
clientKey := "hvc_..."  // your API client key
accessToken := "..."    // DPoP access token
dpopProof := "..."      // DPoP proof JWT

body := bytes.NewBufferString(`{"status": "\ud83d\ude0a", "createdAt": "2025-01-01T12:00:00Z"}`)

req, _ := http.NewRequest("POST",
  "http://127.0.0.1:3000/xrpc/xyz.statusphere.setStatus", body)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
req.Header.Set("Content-Type", "application/json")

resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/xrpc/xyz.statusphere.setStatus \
  -H "X-Client-Key: $CLIENT_KEY" \
  -H "Authorization: DPoP $ACCESS_TOKEN" \
  -H "DPoP: $DPOP_PROOF" \
  -H "Content-Type: application/json" \
  -d '{ "status": "\ud83d\ude0a", "createdAt": "2025-01-01T12:00:00Z" }'
```

**Response**: whatever the procedure's script returns.

### Method has no script

A query called without a `uri`, or a procedure called at all, with no script attached answers `501 Not Implemented`:

```json
{
  "error": "MethodNotImplemented",
  "message": "xyz.statusphere.setStatus has no script. Add one for the trigger xrpc.procedure:xyz.statusphere.setStatus"
}
```

For a query the trigger in the message is `xrpc.query:<method>`.

## XRPC proxy

When a request targets an NSID that has no locally registered lexicon, HappyView resolves the NSID's authority via DNS and forwards the request. Admins can restrict which NSIDs are proxied — see [XRPC Proxy settings](admin/xrpc-proxy.md).

## Errors

All error responses return JSON with an `error` field:

```json
{
  "error": "description of what went wrong"
}
```

| Status | Meaning | Common causes |
|--------|---------|---------------|
| `400 Bad Request` | Invalid input | Missing required fields, malformed JSON, invalid AT URI |
| `401 Unauthorized` | Authentication failed | Missing or invalid client identification or DPoP authentication |
| `404 Not Found` | Method or record not found | XRPC method has no matching lexicon, or the requested record doesn't exist |
| `500 Internal Server Error` | Server-side failure | Lua script error, database error, or upstream PDS failure |
| `501 Not Implemented` | Method has no script | A query or procedure lexicon with no script attached; see [Method has no script](#method-has-no-script) |

### Lexicon validation errors

Before a query or procedure's script runs, HappyView checks the call against the method's lexicon: its parameters against `parameters`, and a JSON body against `input.schema`. A call that breaks it is refused with `400` and an `InvalidRequest` body naming the first problem, in the atproto reference implementation's wording:

```json
{ "error": "InvalidRequest", "message": "Params/limit can not be greater than 100" }
```

Required properties, `const`, `enum`, `minLength` and `maxLength` (in UTF-8 bytes), `minGraphemes` and `maxGraphemes`, string `format`s, integer `minimum` and `maximum`, types, array lengths and items, `ref`s and closed `union`s are all checked. `knownValues` is a hint and isn't enforced, and a parameter the lexicon doesn't declare passes through to the script. A `ref` to a lexicon this instance doesn't hold isn't checked. A script calling another method with `xrpc.query` or `xrpc.procedure` meets the same checks.

### Lua script errors

When a Lua script fails, the response is a `script_error` body:

```json
{ "error": "script_error", "errorType": "runtime", "message": "attempt to index a nil value (local 't')", "method": "xyz.statusphere.getStatus", "line": 3, "file": "src/lib/format.lua" }
```

`errorType` is `syntax`, `runtime` or `missing_handle` with status `500`, or `timeout` with status `408` when the script hit the instruction limit or the wall clock, both set in Settings → General. `line` is the script line that raised, or `null`. `file` is the project file `line` is in, relative to the project root: the handler itself, or a local module it imported. It is `null` for a script saved through the dashboard, which has no file.

The same error is logged server-side with the script source and the caller's input. See [Lua Scripting - Debugging](../guides/lua-scripting.md#debugging) for how to diagnose script issues.

### PDS errors

When a procedure proxies a write to the user's PDS and the PDS returns an error, HappyView forwards the PDS response status code and body directly to the client.

## Next steps

- [Lua Scripting](../guides/lua-scripting.md): Write the scripts that serve queries and procedures
- [Lexicons](../guides/lexicons.md): Understand how lexicons generate these endpoints
- [Admin API](admin/admin-api.md): Manage lexicons and monitor your instance

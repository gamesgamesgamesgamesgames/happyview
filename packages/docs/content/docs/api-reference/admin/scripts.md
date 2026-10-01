---
title: "Scripts"
---

Manage trigger-keyed scripts. Scripts run automatically in response to events like record indexing, XRPC calls, or labeler actions. The trigger id (e.g. `record.index:xyz.statusphere.status`) determines when a script fires.

**Permissions:** `scripts:read` for GET endpoints, `scripts:manage` for mutating endpoints.

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

## List scripts

```
GET /admin/scripts
```

Optionally filter by NSID suffix with the `?suffix=` query parameter.

```ts tab="TypeScript" tab-group="language"
interface Script {
  id: string;
  script_type: string;
  body: string;
  description: string | null;
  created_at: string;
  updated_at: string;
  // v2 globals (`db`, `Record`, `now`, `params`, …) this script still
  // references as free names; empty once migrated onto
  // require("happyview.*") / require("internal.*"). Always empty for a
  // non-Lua script. Can instead be ["unparseable"] when a Lua `body` does
  // not parse at all.
  needs_migration: string[];
  // false when no installed interpreter claims `script_type`, so the script
  // is stored but inert. See "A script whose language has no interpreter".
  runnable: boolean;
}

// List all scripts
const response = await fetch("http://127.0.0.1:3000/admin/scripts", {
  headers,
});
const data: Script[] = await response.json();

// Filter by NSID suffix
const filtered = await fetch(
  "http://127.0.0.1:3000/admin/scripts?suffix=xyz.statusphere.status",
  { headers },
);
const filteredData: Script[] = await filtered.json();
```
```js tab="JavaScript" tab-group="language"
// List all scripts
const response = await fetch("http://127.0.0.1:3000/admin/scripts", {
  headers,
});
const data = await response.json();

// Filter by NSID suffix
const filtered = await fetch(
  "http://127.0.0.1:3000/admin/scripts?suffix=xyz.statusphere.status",
  { headers },
);
const filteredData = await filtered.json();
```
```rust tab="Rust" tab-group="language"
// List all scripts
let response = client
    .get("http://127.0.0.1:3000/admin/scripts")
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;

// Filter by NSID suffix
let response = client
    .get("http://127.0.0.1:3000/admin/scripts?suffix=xyz.statusphere.status")
    .bearer_auth(token)
    .send()
    .await?;
let filtered: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
// List all scripts
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/scripts", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)

// Filter by NSID suffix
req, _ = http.NewRequest("GET", "http://127.0.0.1:3000/admin/scripts?suffix=xyz.statusphere.status", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err = http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
# List all scripts
curl http://127.0.0.1:3000/admin/scripts -H "$AUTH"

# Filter by NSID suffix
curl "http://127.0.0.1:3000/admin/scripts?suffix=xyz.statusphere.status" -H "$AUTH"
```

| Parameter | Type   | Required | Description                                                      |
| --------- | ------ | -------- | ---------------------------------------------------------------- |
| `suffix`  | string | no       | Filter to scripts whose id ends with `:<suffix>` (query param)   |

**Response**: `200 OK`

```json
[
  {
    "id": "record.index:xyz.statusphere.status",
    "script_type": "lua",
    "body": "function handle(input, ctx)\n  return input.record\nend",
    "description": "Process indexed statuses",
    "created_at": "2026-01-01T00:00:00Z",
    "updated_at": "2026-01-01T00:00:00Z",
    "needs_migration": [],
    "runnable": true
  }
]
```

## Get a script

```
GET /admin/scripts/{id}
```

The `{id}` path parameter is the trigger string, URL-encoded (e.g. `record.index%3Axyz.statusphere.status`).

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status",
  { headers },
);
const data: Script = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status",
  { headers },
);
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status")
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl "http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status" -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "id": "record.index:xyz.statusphere.status",
  "script_type": "lua",
  "body": "function handle(input, ctx)\n  return input.record\nend",
  "description": "Process indexed statuses",
  "created_at": "2026-01-01T00:00:00Z",
  "updated_at": "2026-01-01T00:00:00Z",
  "needs_migration": [],
  "runnable": true
}
```

## Create or replace a script

```
POST /admin/scripts
```

Creates a new script or replaces an existing one by `id`. The trigger grammar and Lua body are validated at write-time, a body that still references a removed v2 global is [refused](#saving-an-unmigrated-script), and so is a `script_type` [no installed interpreter claims](#a-script-whose-language-has-no-interpreter).

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/scripts", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    id: "record.index:xyz.statusphere.status",
    script_type: "lua",
    body: "function handle(input, ctx)\n  return input.record\nend",
    description: "Process indexed statuses",
  }),
});
const data: Script = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/scripts", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    id: "record.index:xyz.statusphere.status",
    script_type: "lua",
    body: "function handle(input, ctx)\n  return input.record\nend",
    description: "Process indexed statuses",
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("http://127.0.0.1:3000/admin/scripts")
    .bearer_auth(token)
    .json(&serde_json::json!({
        "id": "record.index:xyz.statusphere.status",
        "script_type": "lua",
        "body": "function handle(input, ctx)\n  return input.record\nend",
        "description": "Process indexed statuses"
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "id": "record.index:xyz.statusphere.status",
  "script_type": "lua",
  "body": "function handle(input, ctx)\n  return input.record\nend",
  "description": "Process indexed statuses"
}`)
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/admin/scripts", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/scripts \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{
    "id": "record.index:xyz.statusphere.status",
    "script_type": "lua",
    "body": "function handle(input, ctx)\n  return input.record\nend",
    "description": "Process indexed statuses"
  }'
```

| Field         | Type   | Required | Description                                                    |
| ------------- | ------ | -------- | -------------------------------------------------------------- |
| `id`          | string | yes      | Trigger string (e.g. `record.index:xyz.statusphere.status`)    |
| `script_type` | string | no       | Script language; defaults to `"lua"`. Must be one an installed interpreter claims |
| `body`        | string | yes      | The script source code                                         |
| `description` | string | no       | Human-readable description (max 300 characters)                |

**Response**: `201 Created` (new) or `200 OK` (update)

```json
{
  "id": "record.index:xyz.statusphere.status",
  "script_type": "lua",
  "body": "function handle(input, ctx)\n  return input.record\nend",
  "description": "Process indexed statuses",
  "created_at": "2026-01-01T00:00:00Z",
  "updated_at": "2026-01-01T00:00:00Z",
  "needs_migration": [],
  "runnable": true
}
```

## Partial update a script

```
PATCH /admin/scripts/{id}
```

Updates individual fields of an existing script. At least one field must be provided. Setting `description` to `null` in JSON clears it. If `script_type` is changed, `body` must also be provided so validation can run against the new type. A `body` that still references a removed v2 global is [refused](#saving-an-unmigrated-script), as is a `script_type` [no installed interpreter claims](#a-script-whose-language-has-no-interpreter).

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status",
  {
    method: "PATCH",
    headers: {
      ...headers,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      description: "Updated description for status processing",
    }),
  },
);
const data: Script = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status",
  {
    method: "PATCH",
    headers: {
      ...headers,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      description: "Updated description for status processing",
    }),
  },
);
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .patch("http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status")
    .bearer_auth(token)
    .json(&serde_json::json!({
        "description": "Updated description for status processing"
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "description": "Updated description for status processing"
}`)
req, _ := http.NewRequest("PATCH", "http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X PATCH "http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status" \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{ "description": "Updated description for status processing" }'
```

| Field         | Type         | Required | Description                                                      |
| ------------- | ------------ | -------- | ---------------------------------------------------------------- |
| `script_type` | string       | no       | Script language; requires `body` alongside, and must be one an installed interpreter claims |
| `body`        | string       | no       | New script source; re-validated against `script_type`            |
| `description` | string\|null | no       | New description, or `null` to clear                              |

**Response**: `200 OK`

```json
{
  "id": "record.index:xyz.statusphere.status",
  "script_type": "lua",
  "body": "function handle(input, ctx)\n  return input.record\nend",
  "description": "Updated description for status processing",
  "created_at": "2026-01-01T00:00:00Z",
  "updated_at": "2026-01-01T00:00:00Z",
  "needs_migration": [],
  "runnable": true
}
```

## Delete a script

```
DELETE /admin/scripts/{id}
```

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status",
  {
    method: "DELETE",
    headers,
  },
);
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status",
  {
    method: "DELETE",
    headers,
  },
);
```
```rust tab="Rust" tab-group="language"
let response = client
    .delete("http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status")
    .bearer_auth(token)
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("DELETE", "http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X DELETE "http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status" \
  -H "$AUTH"
```

**Response**: `204 No Content`

## Preview or apply the v3 codemod

```
POST /admin/scripts/{id}/codemod
```

Rewrites a Lua script's body onto the v3 `handle(input, ctx)` contract — the rewrite [Migrating Scripts to v3](../../guides/migrating-scripts.md) describes. Only `script_type: "lua"` is supported. Previewing needs `scripts:read`; `apply: true` needs `scripts:manage` as well, since it stores the rewritten body. Omit the request body entirely, or send `{}`, to preview.

```ts tab="TypeScript" tab-group="language"
interface CodemodResult {
  source: string;
  notes: { line: number; message: string }[];
  changed: boolean;
}

const response = await fetch(
  "http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status/codemod",
  {
    method: "POST",
    headers: { ...headers, "Content-Type": "application/json" },
    body: JSON.stringify({}),
  },
);
const preview: CodemodResult = await response.json();

// Apply once the notes are reviewed; allow_markers is required if any remain
const applied = await fetch(
  "http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status/codemod",
  {
    method: "POST",
    headers: { ...headers, "Content-Type": "application/json" },
    body: JSON.stringify({ apply: true, allow_markers: true }),
  },
);
const data: CodemodResult = await applied.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status/codemod",
  {
    method: "POST",
    headers: { ...headers, "Content-Type": "application/json" },
    body: JSON.stringify({}),
  },
);
const preview = await response.json();

const applied = await fetch(
  "http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status/codemod",
  {
    method: "POST",
    headers: { ...headers, "Content-Type": "application/json" },
    body: JSON.stringify({ apply: true, allow_markers: true }),
  },
);
const data = await applied.json();
```
```rust tab="Rust" tab-group="language"
let preview = client
    .post("http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status/codemod")
    .bearer_auth(token)
    .json(&serde_json::json!({}))
    .send()
    .await?;
let data: serde_json::Value = preview.json().await?;

let applied = client
    .post("http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status/codemod")
    .bearer_auth(token)
    .json(&serde_json::json!({ "apply": true, "allow_markers": true }))
    .send()
    .await?;
let data: serde_json::Value = applied.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{}`)
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status/codemod", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)

applyBody := bytes.NewBufferString(`{"apply": true, "allow_markers": true}`)
req, _ = http.NewRequest("POST", "http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status/codemod", applyBody)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err = http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
# Preview
curl -X POST "http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status/codemod" \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{}'

# Apply, accepting any remaining markers
curl -X POST "http://127.0.0.1:3000/admin/scripts/record.index%3Axyz.statusphere.status/codemod" \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{ "apply": true, "allow_markers": true }'
```

| Field           | Type    | Required | Description                                                                 |
| --------------- | ------- | -------- | ---------------------------------------------------------------------------- |
| `apply`         | boolean | no       | Defaults to `false` (preview only). `true` stores the rewritten body.        |
| `allow_markers` | boolean | no       | Required alongside `apply: true` when the rewrite still leaves `-- codemod:` markers behind. |
| `source`        | string  | no       | Lua text to rewrite in place of the stored body. Preview only.               |

**Response**: `200 OK`

```json
{
  "source": "local log = require(\"internal.logging\")\n\nfunction handle(input, ctx)\n  log.info(\"script fired\", { trigger = ctx.trigger })\n  return input\nend",
  "notes": [],
  "changed": true
}
```

| Field              | Type    | Description                                                                                |
| ------------------ | ------- | ------------------------------------------------------------------------------------------ |
| `source`           | string  | The rewritten script body.                                                                 |
| `notes`            | array   | Constructs the codemod could not rewrite mechanically, each left under a `-- codemod:` comment in `source`. |
| `notes[].line`     | number  | Line number in the **original** body the construct sits on — not the rewritten `source` above. |
| `notes[].message`  | string  | What changed and what to finish by hand.                                                   |
| `changed`          | boolean | `false` when the rewrite equals the stored body, so there is nothing to apply; `source` then equals the stored body. Not a statement that the script is on the v3 contract: one applied with markers left in place reports `false` too. |

`apply: true` on a rewrite that changed the script (`changed: true`) and still has one or more notes returns `409 Conflict` unless `allow_markers: true` is also set. Re-applying when the rewrite changes nothing (`changed: false`) is a no-op and returns `200 OK` regardless of `allow_markers`.

`source` previews the rewrite of text the server doesn't hold, such as an editor's unsaved changes. No stored script is needed, so it works before a script is first saved: the `{id}` in the path then only tells the codemod which kind of script it is rewriting. Nothing is stored, `changed` compares against `source` rather than the stored body, and sending it with `apply: true` returns `400 Bad Request`.

A non-Lua `script_type` returns `400 Bad Request`. A body that doesn't parse as Lua also returns `400 Bad Request` rather than a result — this is different from `needs_migration`'s `"unparseable"`, which is a value on the *script list/get* response, not this endpoint.

### A script whose language has no interpreter

A script's language is run by an interpreter plugin, and which languages an instance can run is therefore whatever is installed on it rather than a fixed list. `POST /admin/scripts` and `PATCH /admin/scripts/{id}` refuse a `script_type` no installed interpreter claims:

```json
{
  "error": "no interpreter installed for 'typescript' scripts; install one from the plugins page"
}
```

**Response**: `400 Bad Request`. Nothing is stored. Nothing on the instance could check the body or run it, so storing it would mean keeping a script nothing has validated — the refusal happens at the edit rather than on the script's first trigger. Installing the interpreter is the whole fix; the same request then succeeds unchanged.

The check runs after the [unmigrated-body refusal](#saving-an-unmigrated-script), so a Lua body with removed globals still gets `removed_globals` and the offer of the codemod.

**Stored rows are unaffected.** A script whose interpreter was never installed, or was uninstalled afterwards, still lists and still reads with its body intact, and reports `runnable: false`. It can still be edited and deleted. What it cannot do is run, and each trigger answers that in the way its own caller needs:

| Trigger | What happens |
| ------- | ------------ |
| `xrpc.query:*`, `xrpc.procedure:*` | `503 Service Unavailable` with `error: "ServerMisconfigured"` and a `message` naming the language. A caller is refused rather than given an answer the script never shaped — in a procedure's case, rather than having its raw input written |
| `record.index:*`, `record.create:*`, `record.update:*`, `record.delete:*` | Fail-open: the event is indexed as if no script had run, and a dead-letter row records the reason. Ingest is not held up by a missing plugin |
| `labeler.apply:*` | Fail-open: the label is persisted unchanged, with a dead-letter row recording the reason |
| `job.run:*` | The job fails, with the same sentence in its `error` column. A job that silently never ran would tell an operator nothing |

The dead-letter rows record **one** attempt rather than the usual four: a missing plugin is not a transient failure, so the retry budget is not spent on it. Retrying such a row through `POST /admin/dead-letters/{id}/retry` answers `500 Internal Server Error` with a correlation id and writes the sentence to the row's `error` column, until the interpreter is installed. (Label dead letters are never retryable, whatever the reason they were written.)

### Saving an unmigrated script

`POST /admin/scripts` and `PATCH /admin/scripts/{id}` refuse a Lua `body` that still references a removed v2 global, the same names `needs_migration` reports for the script's kind and in the same order:

```json
{
  "error": "script references removed globals: db, params -- run the codemod first",
  "removed_globals": ["db", "params"]
}
```

```ts
interface UnmigratedScriptError {
  error: string;
  removed_globals: string[];
}
```

**Response**: `400 Bad Request`. Nothing is stored. `removed_globals` is present on this error and no other, so a client can key on it rather than on the wording of `error`. The check runs before the body is compiled, so a script that reads a removed global at file scope gets this answer rather than a compilation failure; a body that doesn't parse at all still gets the compilation failure. Such a script would fail on its first run, so it is turned away at the edit instead. A `PATCH` that sends no `body` is not checked, so the description of a stored script that is still waiting for the codemod stays editable.

`apply: true` on this endpoint is the one write that can store a body with removed globals left in it: only the constructs it marked `-- codemod:` remain, and `allow_markers: true` is the explicit consent to store them. Saving that same body back through `POST` or `PATCH` is refused until the marked lines are rewritten by hand.

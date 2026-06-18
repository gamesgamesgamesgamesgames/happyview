---
title: "Record & Label Scripts"
---

Record and label scripts are Lua scripts that run in response to events on the AT Protocol network. **Record scripts** fire when a record in a collection is created, updated, or deleted. **Label scripts** fire when a label is applied to a record or actor. Both run **before** the event is indexed, giving you the ability to filter, transform, or trigger side effects.

These scripts are event-driven -- they react to incoming Jetstream events (which include events caused by HappyView's own PDS writes), not to XRPC requests. For scripts that run in response to XRPC queries and procedures, see [Lua Scripting](./lua-scripting.md).

> **Migration note:** Prior to v2.9, record scripts were called "index hooks" and were attached directly to lexicons. They now live in their own `scripts` table and are managed separately. Existing index hooks were automatically migrated.

## Trigger grammar

Every script is identified by a **trigger string** -- the script's `id` in the `scripts` table IS its trigger binding. There is no separate name or host column; the trigger string determines which events the script receives.

### Record event triggers

| Trigger                    | Fires when                                    |
| -------------------------- | --------------------------------------------- |
| `record.index:<nsid>`      | Any record event (create, update, or delete)  |
| `record.create:<nsid>`     | A record is created                           |
| `record.update:<nsid>`     | A record is updated                           |
| `record.delete:<nsid>`     | A record is deleted                           |

**Cascade rule:** When a record event occurs, the dispatcher tries the action-specific trigger first (e.g. `record.create:<nsid>`), then falls back to `record.index:<nsid>` if no action-specific script exists. This means you can use `record.index` as a catch-all and override individual actions when needed.

### XRPC triggers

| Trigger                    | Fires when                                    |
| -------------------------- | --------------------------------------------- |
| `xrpc.query:<nsid>`        | An XRPC query endpoint is called              |
| `xrpc.procedure:<nsid>`    | An XRPC procedure endpoint is called          |

XRPC scripts handle the request and return the response. Without a script, HappyView uses [default query/procedure behavior](../api-reference/xrpc-api.md). See [Lua Scripting](./lua-scripting.md) for the full query/procedure scripting reference.

### Label event triggers

| Trigger                    | Fires when                                         |
| -------------------------- | -------------------------------------------------- |
| `labeler.apply:<nsid>`     | A label arrives whose subject is `at://<did>/<nsid>/<rkey>` |
| `labeler.apply:_actor`     | A label arrives whose subject is a bare DID (actor-level label) |

There is no cascade for label or XRPC triggers -- each trigger string must match exactly.

## Creating scripts

You can create scripts through the [dashboard](../getting-started/dashboard.md) (Settings > Scripts > New) or via the [admin API](../api-reference/admin/scripts.md) (`POST /admin/scripts`).

When creating a script, you provide the trigger string as the script's `id`. For example, a script with id `record.index:xyz.statusphere.status` will fire on every record event for the `xyz.statusphere.status` collection.

## Script structure

Like query and procedure scripts, record and label scripts must define a `handle()` function:

```lua
function handle()
  if action == "delete" then
    log("deleted " .. uri)
  else
    log(action .. " " .. uri)
  end
  return true
end
```

The function is called once per event.

### Record script return values

| Return value | Effect                                                      |
| ------------ | ----------------------------------------------------------- |
| `nil`        | The record is **not** indexed (skipped entirely)            |
| A table      | That table is stored as the record instead                  |
| `true`       | The original record is stored as-is                         |
| *(no script)* | The original record is stored as-is                        |

On **delete** events, returning `nil` skips the delete (the record stays in the database).

**Important:** If your script has side effects (e.g. syncing to a search index) but you want normal indexing to proceed, return `record` or `true` -- not nothing. A missing return statement returns `nil`, which **skips indexing**.

### Label script return values

| Return value | Effect                                                       |
| ------------ | ------------------------------------------------------------ |
| `nil`        | The label is **not** persisted (skipped entirely)            |
| A table      | The returned fields are merged with the original label       |
| `true`       | The original label is stored as-is                           |
| *(no script)* | The original label is stored as-is                          |

When a label script returns a table, any field the script omits falls back to the original value. This means `return event` passes the label through unchanged, while `return { val = "new-value" }` rewrites only the `val` field.

## Context globals

### Record script globals

These globals are set before `handle()` is called for record events:

| Global       | Type   | Description                                        |
| ------------ | ------ | -------------------------------------------------- |
| `action`     | string | `"create"`, `"update"`, or `"delete"`              |
| `uri`        | string | The full AT URI (e.g. `at://did:plc:abc/col/rkey`) |
| `did`        | string | The repo DID                                       |
| `collection` | string | The collection NSID                                |
| `rkey`       | string | The record key                                     |
| `record`     | table? | The full record as a Lua table (nil on delete)     |
| `event`      | table  | The full event payload (see below)                 |

The `event` table contains the same fields as the individual globals (`action`, `uri`, `did`, `collection`, `rkey`, `record`). New scripts can use either style -- `event.action` or the bare `action` global -- both work. The `event` table corresponds to the `RecordEventPayload` struct in the Rust dispatcher.

### Label script globals

These globals are set before `handle()` is called for label events:

| Global  | Type    | Description                                      |
| ------- | ------- | ------------------------------------------------ |
| `src`   | string  | DID of the labeler that issued the label         |
| `uri`   | string  | The label subject (`at://` URI or bare DID)      |
| `val`   | string  | The label value (e.g. `"!hide"`, `"nudity"`)     |
| `neg`   | boolean | `true` if this is a negation (label removal)     |
| `cts`   | string  | Creation timestamp (ISO 8601)                    |
| `exp`   | string? | Expiration timestamp (nil if the label does not expire) |
| `event` | table   | The full label event as a table (same fields)    |

Record and label scripts do **not** have access to `caller_did`, `input`, `params`, or `method`. They run from the event stream, not from a user request.

## Available APIs

Record and label scripts have access to:

- **[Record API](../api-reference/lua/record-api.md)** (no-auth mode) -- `Record.load`, `r:save_local()`, `r:delete_local()`, `Record.delete_local()`. PDS-touching methods (`r:save()`, `r:delete()`) raise an error.
- **[Database API](../api-reference/lua/database-api.md)** -- `db.query`, `db.get`, `db.search`, `db.backlinks`, `db.count`, `db.raw`
- **[HTTP API](../api-reference/lua/http-api.md)** -- `http.get`, `http.post`, `http.put`, `http.patch`, `http.delete`, `http.head`
- **[XRPC Lua API](../api-reference/lua/xrpc-lua-api.md)** -- `xrpc.query`, `xrpc.procedure`
- **[atproto API](../api-reference/lua/atproto-api.md)** -- `atproto.resolve_service_endpoint`, `atproto.get_labels`, `atproto.get_labels_batch`
- **[JSON API](../api-reference/lua/json-api.md)** -- `json.encode`, `json.decode`
- **[Utility globals](./lua-scripting.md#utility-globals)** -- `log()`, `now()`, `TID()`, `toarray()`
- **[Script variables](../api-reference/admin/script-variables.md)** -- `env` table with key-value pairs configured in the dashboard

## Error handling and retries

Record and label scripts are designed to be resilient:

1. If a script fails, it retries up to **4 attempts total** (1 initial + 3 retries) with exponential backoff (1s, 2s, 4s delays).
2. If all attempts are exhausted, the failed event is inserted into the `dead_letter_scripts` table for later inspection.
3. On failure the system **fails open** -- the original record or label is stored as-is so indexing is not permanently blocked. The firehose has no caller to surface errors to.

Failed scripts are logged as errors. Check the [event logs](./event-logs.md) or query the `dead_letter_scripts` table directly to find and replay failures.

### Performance considerations

Because scripts run synchronously before indexing, they block the Jetstream consumer while executing. With retry logic (1s + 2s + 4s backoff), a persistently failing script could block for ~7 seconds per event. Keep scripts fast and ensure external services they depend on are reliable.

### Dead letter table

The `dead_letter_scripts` table stores events that failed all retry attempts:

| Column       | Type      | Description                                           |
| ------------ | --------- | ----------------------------------------------------- |
| `id`         | BIGSERIAL | Primary key                                           |
| `script_ref` | text      | The trigger id of the script that failed              |
| `host_kind`  | text      | `'record'` or `'label'`                               |
| `host_id`    | text      | Identifies the specific event source                  |
| `collection` | text      | The collection NSID of the failed event               |
| `payload`    | jsonb     | The full event payload                                |
| `error`      | text      | The error message from the last attempt               |
| `attempts`   | int       | Total number of attempts made                         |
| `created_at` | text      | When the failure was recorded (ISO 8601)              |
| `resolved_at`| text      | When the failure was resolved (null until resolved)   |

## Examples

### Filter out records missing a required field

Create a script with trigger `record.index:your.collection.nsid` to skip indexing any record that doesn't have a `title` field:

```lua
function handle()
  if action == "delete" then
    return record  -- allow deletes to proceed
  end

  if record.title == nil or record.title == "" then
    return nil  -- skip: no title
  end

  return record
end
```

### Transform a record before storage

Enrich a record with a computed field before it is stored:

```lua
function handle()
  if action == "delete" then
    return record
  end

  record.slug = string.lower(string.gsub(record.title or "", "%s+", "-"))
  return record
end
```

### Post to a webhook

```lua
function handle()
  http.post("https://hooks.example.com/records", {
    headers = { ["Content-Type"] = "application/json" },
    body = json.encode({
      action = action,
      uri = uri,
      did = did,
      record = record
    })
  })
  return record
end
```

### Sync to Algolia

Push records to an Algolia search index on create/update, and remove them on delete:

```lua
function handle()
  local headers = {
    ["X-Algolia-API-Key"] = "your-api-key",
    ["X-Algolia-Application-Id"] = "your-app-id",
    ["Content-Type"] = "application/json"
  }

  if action == "delete" then
    http.delete("https://YOUR-APP.algolia.net/1/indexes/records/" .. uri, {
      headers = headers
    })
  else
    http.put("https://YOUR-APP.algolia.net/1/indexes/records/" .. uri, {
      headers = headers,
      body = json.encode({
        objectID = uri,
        collection = collection,
        did = did,
        record = record
      })
    })
  end

  return record
end
```

See the full [Algolia sync reference](../reference/script-examples/algolia-sync.md) for more detail.

### Sync to Meilisearch

Push records to a self-hosted Meilisearch index on create/update, and remove them on delete:

```lua
function handle()
  local headers = {
    ["Authorization"] = "Bearer " .. env.MEILISEARCH_API_KEY,
    ["Content-Type"] = "application/json"
  }

  if action == "delete" then
    http.delete(env.MEILISEARCH_URL .. "/indexes/records/documents/" .. uri, {
      headers = headers
    })
  else
    http.post(env.MEILISEARCH_URL .. "/indexes/records/documents", {
      headers = headers,
      body = json.encode(toarray({
        {
          id = uri,
          collection = collection,
          did = did,
          record = record
        }
      }))
    })
  end

  return record
end
```

See the full [Meilisearch sync reference](../reference/script-examples/meilisearch-sync.md) for more detail.

### Filter labels by value

Create a script with trigger `labeler.apply:your.collection.nsid` to only persist specific label values:

```lua
function handle()
  local allowed = { ["!hide"] = true, ["nudity"] = true, ["spam"] = true }

  if not allowed[val] then
    return nil  -- skip: label value not in allowlist
  end

  return event
end
```

### Rewrite a label field

Normalize the label value before it is stored:

```lua
function handle()
  return { val = string.lower(val) }
end
```

Fields not returned fall back to their original values, so only `val` is changed here.

## Next steps

- [Lua Scripting](./lua-scripting.md): Full reference for the sandbox, APIs, and debugging (covers query and procedure scripts)
- [Admin API -- Scripts](../api-reference/admin/scripts.md): Create and manage scripts via the API
- [Lexicons](lexicons.md): Understand how record, query, and procedure lexicons work together

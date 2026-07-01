---
title: "Meilisearch Sync"
---

Push records to a Meilisearch search index whenever they are created, updated, or deleted on the network.

**Script type:** record event (e.g. `record.index:<nsid>`)

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

## How it works

1. On **create** or **update**: sends a `POST` request to Meilisearch's document API with the record data wrapped in an array. Meilisearch upserts by `id` — if a document with the same AT URI already exists, it's replaced.
2. On **delete**: sends a `DELETE` request to remove the document from the index by its AT URI.

The `toarray()` function ensures the table is encoded as a JSON array (Meilisearch expects an array of documents). See [JSON API](../../api-reference/lua/json-api.md).

## Configuration

This script uses [script variables](../../guides/lua-scripting.md) instead of hardcoded values. Set these via the [admin API](../../api-reference/admin/admin-api.md) or dashboard:

| Variable              | Value                                                                          |
| --------------------- | ------------------------------------------------------------------------------ |
| `MEILISEARCH_URL`     | Your Meilisearch instance URL (e.g. `http://meilisearch.railway.internal:7700`) |
| `MEILISEARCH_API_KEY` | A Meilisearch API key with write permissions                                    |

Script variables are stored in the `happyview_script_variables` table and accessible as `env.*` in Lua.

## Use case

This hook keeps an external search index in sync with your indexed records in real time. Users searching through Meilisearch get results that reflect the latest state of the network without polling or scheduled jobs.

Meilisearch is a good fit for self-hosted deployments — colocate it alongside HappyView (e.g. on the same Railway project) for sub-millisecond network latency.

Combine this with a [query script](../../guides/lua-scripting.md) that searches Meilisearch instead of the local database for a full-text search experience that goes beyond what `db.search` offers.

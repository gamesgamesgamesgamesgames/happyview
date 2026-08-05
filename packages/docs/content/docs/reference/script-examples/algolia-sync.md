---
title: "Algolia Sync"
---

Push records to an Algolia search index whenever they are created, updated, or deleted on the network.

**Script type:** record event (e.g. `record.index:<nsid>`)

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

  return record or true  -- `record` is nil on delete; `true` lets it proceed
end
```

## How it works

1. On **create** or **update**: sends a `PUT` request to Algolia's index API with the record data, using the AT URI as the `objectID`. Algolia upserts the object — if it already exists, it's replaced.
2. On **delete**: sends a `DELETE` request to remove the object from the index by its AT URI.

The `json.encode()` function converts the Lua table into a JSON string for the request body. See [JSON API](../../api-reference/lua/json-api.md).

## Configuration

Replace the placeholder values:

| Placeholder              | Value                                                                 |
| ------------------------ | --------------------------------------------------------------------- |
| `your-api-key`           | Your Algolia Admin API key (with write permissions)                   |
| `your-app-id`            | Your Algolia Application ID                                           |
| `YOUR-APP`               | Your Algolia application subdomain (same as the Application ID)       |
| `records`                | The Algolia index name (choose any name you like)                     |

## Use case

This hook keeps an external search index in sync with your indexed records in real time. Users searching through Algolia get results that reflect the latest state of the network without polling or scheduled jobs.

Combine this with a [query script](../../guides/lua-scripting.md) that searches Algolia instead of the local database for a full-text search experience that goes beyond what `db.search` offers.

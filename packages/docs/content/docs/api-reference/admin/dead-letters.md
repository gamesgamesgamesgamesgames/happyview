---
title: "Dead Letters"
---

Events that failed all retry attempts are stored as dead letters for inspection and manual resolution. Dead letters come from two sources: legacy index hooks (`happyview_dead_letter_hooks`) and trigger-keyed scripts (`happyview_dead_letter_scripts`). Both tables are surfaced through a single unified API.

Read endpoints require `dead-letters:read`. Action endpoints (dismiss, retry, reindex) require `dead-letters:manage`.

```sh tab="cURL" tab-group="language"
# All examples assume $TOKEN is an API key (hv_...)
AUTH="Authorization: Bearer $TOKEN"
```

## List dead letters

```
GET /admin/dead-letters
```

Paginated list of dead letters from both tables, merged and sorted by `created_at` descending.

| Param        | Type   | Required | Description                                           |
| ------------ | ------ | -------- | ----------------------------------------------------- |
| `collection` | string | no       | Filter by collection NSID                             |
| `resolved`   | string | no       | `"true"`, `"false"` (default), or omit for all        |
| `cursor`     | string | no       | Pagination cursor from a previous response            |
| `limit`      | number | no       | Max results per page (default 50, max 100)            |

```sh tab="cURL" tab-group="language"
curl "http://127.0.0.1:3000/admin/dead-letters?limit=10" -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "dead_letters": [
    {
      "id": "42",
      "lexicon_id": "xyz.statusphere.status",
      "uri": "at://did:plc:abc/xyz.statusphere.status/3k...",
      "did": "did:plc:abc",
      "collection": "xyz.statusphere.status",
      "rkey": "3k...",
      "action": "create",
      "error": "script error: attempt to index nil value",
      "attempts": 4,
      "created_at": "2026-06-01T12:00:00Z"
    }
  ],
  "cursor": "2026-06-01T11:00:00Z"
}
```

Resolved dead letters include a `resolved_at` timestamp. `cursor` is omitted when there are no more results.

## Count dead letters

```
GET /admin/dead-letters/count
```

Returns the total count of dead letters across both tables.

| Param        | Type   | Required | Description                                    |
| ------------ | ------ | -------- | ---------------------------------------------- |
| `collection` | string | no       | Filter by collection NSID                      |
| `resolved`   | string | no       | `"true"`, `"false"` (default), or omit for all |

```sh tab="cURL" tab-group="language"
curl "http://127.0.0.1:3000/admin/dead-letters/count" -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "count": 7
}
```

## Get dead letter detail

```
GET /admin/dead-letters/{id}
```

Returns the full dead letter including the record body.

```sh tab="cURL" tab-group="language"
curl "http://127.0.0.1:3000/admin/dead-letters/42" -H "$AUTH"
```

**Response**: `200 OK`

The response includes all fields from the list view plus a `record` field containing the original record data (if available). Returns `404` if the dead letter is not found.

## Dismiss a dead letter

```
POST /admin/dead-letters/{id}/dismiss
```

Marks a dead letter as resolved without retrying it.

```sh tab="cURL" tab-group="language"
curl -X POST "http://127.0.0.1:3000/admin/dead-letters/42/dismiss" -H "$AUTH"
```

**Response**: `200 OK` — `{"ok": true}`

Returns `404` if the dead letter is not found.

## Retry a dead letter

```
POST /admin/dead-letters/{id}/retry
```

Re-runs the dead letter's script with the original event payload. On success the dead letter is marked resolved. On failure the error and attempt count are updated.

Label-arrival dead letters cannot be retried — the upstream label event is gone. These return `400 Bad Request`.

```sh tab="cURL" tab-group="language"
curl -X POST "http://127.0.0.1:3000/admin/dead-letters/42/retry" -H "$AUTH"
```

**Response**: `200 OK` — `{"ok": true}`

Returns `404` if no matching script binding is found, or if the dead letter is not found or already resolved.

## Reindex a dead letter

```
POST /admin/dead-letters/{id}/reindex
```

Fetches the record fresh from the author's PDS and re-runs the full record indexing pipeline. On success the dead letter is marked resolved. Label-arrival dead letters cannot be reindexed and return `400 Bad Request`.

```sh tab="cURL" tab-group="language"
curl -X POST "http://127.0.0.1:3000/admin/dead-letters/42/reindex" -H "$AUTH"
```

**Response**: `200 OK` — `{"ok": true}`

## Bulk dismiss

```
POST /admin/dead-letters/bulk/dismiss
```

Dismiss multiple dead letters at once.

| Field        | Type     | Required | Description                                           |
| ------------ | -------- | -------- | ----------------------------------------------------- |
| `ids`        | string[] | no       | List of dead letter IDs to dismiss                    |
| `all`        | boolean  | no       | Set to `true` to dismiss all unresolved dead letters  |
| `collection` | string   | no       | When `all` is true, limit to this collection          |

One of `ids` or `all: true` is required.

```sh tab="cURL" tab-group="language"
curl -X POST "http://127.0.0.1:3000/admin/dead-letters/bulk/dismiss" \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{"ids": ["42", "43", "44"]}'
```

**Response**: `200 OK` — `{"ok": true}`

## Bulk retry

```
POST /admin/dead-letters/bulk/retry
```

Retry multiple dead letters. Accepts the same input as [bulk dismiss](#bulk-dismiss).

```sh tab="cURL" tab-group="language"
curl -X POST "http://127.0.0.1:3000/admin/dead-letters/bulk/retry" \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{"all": true, "collection": "xyz.statusphere.status"}'
```

**Response**: `200 OK` — `{"ok": true}`

## Bulk reindex

```
POST /admin/dead-letters/bulk/reindex
```

Reindex multiple dead letters. Accepts the same input as [bulk dismiss](#bulk-dismiss).

```sh tab="cURL" tab-group="language"
curl -X POST "http://127.0.0.1:3000/admin/dead-letters/bulk/reindex" \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{"all": true}'
```

**Response**: `200 OK` — `{"ok": true}`

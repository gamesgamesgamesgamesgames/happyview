---
title: "Records"
---

Browse and manage indexed records. All endpoints require the appropriate `records:*` permission.

```sh tab="cURL" tab-group="language"
# All examples assume $TOKEN is an API key (hv_...)
AUTH="Authorization: Bearer $TOKEN"
```

## List records

```
GET /admin/records
```

Paginated list of records in a collection, ordered by `indexed_at` descending.

| Param        | Type   | Required | Description                               |
| ------------ | ------ | -------- | ----------------------------------------- |
| `collection` | string | yes      | Collection NSID to list records from      |
| `limit`      | number | no       | Max results per page (default 20, max 100)|
| `cursor`     | string | no       | Pagination cursor from a previous response|

```sh tab="cURL" tab-group="language"
curl "http://127.0.0.1:3000/admin/records?collection=xyz.statusphere.status&limit=10" -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "records": [
    {
      "uri": "at://did:plc:abc/xyz.statusphere.status/3k...",
      "did": "did:plc:abc",
      "collection": "xyz.statusphere.status",
      "rkey": "3k...",
      "cid": "bafyrei...",
      "indexed_at": "2025-01-01T00:00:00Z",
      "record": { "...": "..." },
      "labels": []
    }
  ],
  "cursor": "20"
}
```

`cursor` is omitted when there are no more results.

## List collections

```
GET /admin/records/collections
```

Returns the list of collection NSIDs from registered record-type lexicons. This is a fast lookup (no record counting).

```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/records/collections -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "collections": [
    "xyz.statusphere.status",
    "app.bsky.feed.post"
  ]
}
```

## Delete a record

```
DELETE /admin/records
```

Delete a single record by AT URI.

| Param | Type   | Required | Description                          |
| ----- | ------ | -------- | ------------------------------------ |
| `uri` | string | yes      | AT URI of the record to delete       |

```sh tab="cURL" tab-group="language"
curl -X DELETE "http://127.0.0.1:3000/admin/records?uri=at://did:plc:abc/xyz.statusphere.status/3k..." -H "$AUTH"
```

**Response**: `204 No Content`

Returns `404` if the record is not found.

## Delete all records in a collection

```
DELETE /admin/records/collection
```

Delete all indexed records for a given collection. Requires both `records:delete` and `records:delete-collection` permissions.

| Param        | Type   | Required | Description                          |
| ------------ | ------ | -------- | ------------------------------------ |
| `collection` | string | yes      | Collection NSID to delete from       |

```sh tab="cURL" tab-group="language"
curl -X DELETE "http://127.0.0.1:3000/admin/records/collection?collection=xyz.statusphere.status" -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "deleted": 42
}
```

## Reconcile a collection with its PDSes

```
POST /admin/records/collection/reconcile
```

Checks a collection's stored records against each account's PDS and, unless it is a dry run, deletes the records the PDS no longer holds. Use it when records were deleted on the network while HappyView wasn't tracking the collection, so the deletes never reached the index. Returns `202 Accepted` with a job id; the job's result is the report below. A dry run needs `records:read`; deleting needs `records:delete`.

| Field                     | Type     | Required | Description |
| ------------------------- | -------- | -------- | ----------- |
| `collection`              | string   | yes      | A record collection this instance has a lexicon for |
| `dry_run`                 | boolean  | no       | Report only. Defaults to `true` |
| `dids`                    | string[] | no       | Check only these accounts. Defaults to every account holding records of the collection |
| `pds_requests_per_second` | integer  | no       | Pace of PDS requests, 1–20. Defaults to 5 |
| `allow_suspicious`        | string[] | no       | Accounts whose suspicious listing (below) is acted on anyway |

```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/records/collection/reconcile -H "$AUTH" \
  -H 'Content-Type: application/json' \
  -d '{"collection": "xyz.statusphere.status"}'
```

**Response**: `202 Accepted`

```json
{ "job_id": "0b9a7c1e-..." }
```

For each account, the job reads the records it stores, resolves the account's PDS from its DID document, lists the collection there with `com.atproto.repo.listRecords`, and confirms each stored record the listing lacks with `com.atproto.repo.getRecord`. Only a record the PDS confirms is gone counts as missing. An account is skipped, and nothing of it is deleted, when:

- its PDS can't be resolved, a listing page fails, or the PDS is still rate limiting after five tries (`pds_unresolved`, `listing_failed`, `rate_limited`);
- the PDS says the repo is missing, deactivated or taken down (`repo_unavailable`);
- the listing is suspicious: empty while records are stored, or missing at least 10 stored records and more than half of them, as an account's old PDS or a broken one can answer. Name the account in `allow_suspicious` to act on it (`suspicious_listing`).

In a run that deletes, each missing record goes through the normal delete path, so the collection's delete hook runs and can keep the record. A record whose stored CID differs from its PDS's is reported and left alone. The job's result:

```json
{
  "collection": "xyz.statusphere.status",
  "dry_run": true,
  "dids_checked": 412,
  "dids_skipped": [
    { "did": "did:plc:abc", "reason": "repo_unavailable", "detail": "listRecords answered 400 Bad Request: ..." }
  ],
  "missing": [{ "uri": "at://did:plc:def/xyz.statusphere.status/3k...", "cid": "bafyrei..." }],
  "missing_total": 1,
  "unconfirmed_total": 0,
  "deleted": 0,
  "kept_by_hook": 0,
  "stale_cid": [],
  "stale_cid_total": 0
}
```

`missing` lists the first 1,000 and `stale_cid` the first 100; the totals count them all. On an instance whose collection is served from a shared record store, the route answers `400`: a stale record there belongs to the store.

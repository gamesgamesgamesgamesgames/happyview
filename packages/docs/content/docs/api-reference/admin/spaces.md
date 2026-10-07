---
title: "Spaces"
---

Browse the [spaces](../../experimental/spaces/index.md) on this instance for moderation. These endpoints give operators access to spaces they are not members of, so they live in the admin API rather than the space XRPC API.

Listing spaces and reading their metadata and members requires `spaces:read`. Reading records and blobs requires `spaces:manage-records`, and each read is written to the [event log](../../guides/event-logs.md#space-events) as a `space.moderator_read` event.

```sh tab="cURL" tab-group="language"
# All examples assume $TOKEN is an API key (hv_...)
AUTH="Authorization: Bearer $TOKEN"
```

## List spaces

```
GET /admin/spaces
```

Every space on the instance, newest first.

| Param    | Type   | Required | Description                                 |
| -------- | ------ | -------- | ------------------------------------------- |
| `limit`  | number | no       | Max results per page (default 50, max 100)  |
| `cursor` | string | no       | Pagination cursor from a previous response  |

```sh tab="cURL" tab-group="language"
curl "http://127.0.0.1:3000/admin/spaces?limit=20" -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "spaces": [
    {
      "id": "0b6c1f0e-...",
      "uri": "at://did:web:happyview.example.com/space/com.example.forum/main",
      "did": "did:web:happyview.example.com",
      "authority_did": "did:web:happyview.example.com",
      "creator_did": "did:plc:creator123",
      "type": "com.example.forum",
      "skey": "main",
      "display_name": "Forum",
      "description": null,
      "read_policy": { "$type": "com.atproto.simplespace.defs#memberListPolicy" },
      "write_policy": { "$type": "com.atproto.simplespace.defs#memberListPolicy" },
      "app_access": { "$type": "com.atproto.simplespace.defs#open" },
      "config": { "membership_public": false, "records_public": false },
      "revision": "3k...",
      "created_at": "2026-01-01T00:00:00Z",
      "updated_at": "2026-01-01T00:00:00Z"
    }
  ],
  "cursor": "20"
}
```

`cursor` is omitted when there are no more results.

## Get a space

```
GET /admin/spaces/{id}
```

A space's metadata, its resolved member list (including members added through delegation), and the record count of each collection in it. `{id}` is the space's `id` from [List spaces](#list-spaces).

```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/spaces/0b6c1f0e-... -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "space": { "id": "0b6c1f0e-...", "uri": "at://...", "...": "..." },
  "members": [
    { "did": "did:plc:creator123", "read": true, "write": true }
  ],
  "collections": [
    { "collection": "com.example.forum.post", "count": 42 }
  ]
}
```

Returns `404 Not Found` if no space has that ID.

## List records in a space

```
GET /admin/spaces/{id}/records
```

Records in a space, newest first. Requires `spaces:manage-records`.

| Param        | Type   | Required | Description                                 |
| ------------ | ------ | -------- | ------------------------------------------- |
| `repo`       | string | no       | Only records authored by this DID           |
| `collection` | string | no       | Only records in this collection             |
| `limit`      | number | no       | Max results per page (default 20, max 100)  |
| `cursor`     | string | no       | Pagination cursor from a previous response  |

```sh tab="cURL" tab-group="language"
curl "http://127.0.0.1:3000/admin/spaces/0b6c1f0e-.../records?collection=com.example.forum.post" -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "records": [
    {
      "uri": "at://did:web:happyview.example.com/space/com.example.forum/main/did:plc:abc/com.example.forum.post/3k...",
      "did": "did:plc:abc",
      "collection": "com.example.forum.post",
      "rkey": "3k...",
      "cid": "bafyrei...",
      "indexed_at": "2026-01-01T00:00:00Z",
      "record": { "...": "..." }
    }
  ],
  "cursor": "..."
}
```

`cursor` is omitted when there are no more results.

## Get a blob in a space

```
GET /admin/spaces/{id}/blob
```

A blob referenced by a record in the space, fetched from its author's PDS. The response body is the blob, with the content type the PDS reports. Requires `spaces:manage-records`.

| Param | Type   | Required | Description  |
| ----- | ------ | -------- | ------------ |
| `cid` | string | yes      | The blob CID |

```sh tab="cURL" tab-group="language"
curl "http://127.0.0.1:3000/admin/spaces/0b6c1f0e-.../blob?cid=bafkrei..." -H "$AUTH" -o blob
```

Returns `404 Not Found` if no record in the space references the blob, and `502 Bad Gateway` if the author's PDS cannot serve it.

---
title: "Spaces"
---

Browse the [spaces](../../experimental/spaces/index.md) on this instance for moderation. These endpoints give operators access to spaces they are not members of, so they live in the admin API rather than the space XRPC API.

While the space inspector is off (see [Configuration](../../getting-started/configuration.md)), listing spaces and accounts, reading a space's metadata or contents, and creating access grants return `403 SpaceInspectorDisabled`. [Space inspector status](#space-inspector-status), listing your own grants, revoking a grant, and listing a grant's reads stay available, so access can still be ended and audited. Listing spaces and reading their metadata requires `spaces:read`. Reading a space's records or blobs also requires `spaces:inspect` and an active [access grant](#access-grants) covering the request. Every grant, its revocation, and every read made under it is written to the [event log](../../guides/event-logs.md#space-events) as a protected event that can't be purged by hand. The grant or revocation and its event are written together, and content is returned only after its read event is written: if the event can't be written, the request fails with `500` and nothing changes or is returned.

```sh tab="cURL" tab-group="language"
# All examples assume $TOKEN is an API key (hv_...)
AUTH="Authorization: Bearer $TOKEN"
```

## Space inspector status

```
GET /admin/spaces/inspector
```

Whether the space inspector is on, and how long an access grant can last. Requires `spaces:read`. This route answers while the inspector is off, so the dashboard can show why the other routes are closed.

```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/spaces/inspector -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "enabled": true,
  "default_grant_minutes": 60,
  "max_grant_minutes": 120
}
```

`default_grant_minutes` is what a grant gets when [`duration_minutes`](#create-an-access-grant) is omitted: 60 minutes, or `max_grant_minutes` if that's shorter.

## Access grants

A grant names one space or one account, records why, and expires after a limited time (5 minutes to `max_grant_minutes`, 60 by default — see [Configuration](../../getting-started/configuration.md)).

| Grant scope | Target     | Covers                                                                        |
| ----------- | ---------- | ------------------------------------------------------------------------------ |
| `space`     | A space id | Records and blobs in that space, regardless of author                          |
| `account`   | A DID      | That account's own records in any space, and blobs its own records reference  |

[List records in a space](#list-records-in-a-space) also accepts an account grant when the request's `repo` filter matches the grant's target DID. [Get a blob in a space](#get-a-blob-in-a-space) accepts an account grant only for a blob that account's own record references, even when another author's record shares the same CID. [List an account's records](#list-an-accounts-records) always needs an account grant for that DID — a space grant never covers it.

### Create an access grant

```
POST /admin/spaces/access-grants
```

Requires `spaces:inspect` and the space inspector turned on.

| Field              | Type   | Required | Description                                                                          |
| ------------------ | ------ | -------- | -------------------------------------------------------------------------------------- |
| `scope`            | string | yes      | `space` or `account`                                                                   |
| `target`           | string | yes      | A space id for `space` scope, a DID for `account` scope                                |
| `reason`           | string | yes      | Why access is needed. Trimmed, 1–2000 characters                                       |
| `duration_minutes` | number | no       | Grant length in minutes, clamped to 5–`max_grant_minutes`. Defaults to 60 or `max_grant_minutes`, whichever is shorter |

```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/spaces/access-grants \
  -H "$AUTH" -H "Content-Type: application/json" \
  -d '{"scope": "space", "target": "0b6c1f0e-...", "reason": "Investigating report #482", "duration_minutes": 30}'
```

**Response**: `201 Created`

```json
{
  "id": "6f1ecb2a-...",
  "user_id": "u_123",
  "user_did": "did:plc:moderator",
  "scope": "space",
  "target": "0b6c1f0e-...",
  "reason": "Investigating report #482",
  "created_at": "2026-01-01T00:00:00+00:00",
  "expires_at": "2026-01-01T00:30:00+00:00",
  "revoked_at": null,
  "revoked_by": null
}
```

Returns `403 SpaceInspectorDisabled` if the inspector is off, `400 Bad Request` for a missing, blank, or over-length reason or an invalid DID, and `404 Not Found` if `target` is a `space`-scope id that doesn't exist.

### List access grants

```
GET /admin/spaces/access-grants
```

The caller's own grants, newest first. Requires `spaces:inspect`.

| Param    | Type    | Required | Description                                       |
| -------- | ------- | -------- | -------------------------------------------------- |
| `active` | boolean | no       | Only grants that haven't expired or been revoked   |

```sh tab="cURL" tab-group="language"
curl "http://127.0.0.1:3000/admin/spaces/access-grants?active=true" -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "grants": [
    { "id": "6f1ecb2a-...", "scope": "space", "target": "0b6c1f0e-...", "...": "..." }
  ]
}
```

### Revoke an access grant

```
DELETE /admin/spaces/access-grants/{id}
```

Ends a grant early. Ending your own grant requires `spaces:inspect`; ending someone else's requires `users:update`.

```sh tab="cURL" tab-group="language"
curl -X DELETE http://127.0.0.1:3000/admin/spaces/access-grants/6f1ecb2a-... -H "$AUTH"
```

**Response**: `200 OK` with the grant, `revoked_at` and `revoked_by` set. Revoking a grant that's already expired or revoked is a no-op that returns it unchanged. Returns `404 Not Found` if no grant has that ID.

### List a grant's reads

```
GET /admin/spaces/access-grants/{id}/reads
```

The `space.moderator_read` events logged under a grant, oldest first, a page at a time. Requires `events:read`.

| Param    | Type   | Required | Description                                 |
| -------- | ------ | -------- | ------------------------------------------- |
| `limit`  | number | no       | Max results per page (default 100, max 500) |
| `cursor` | string | no       | Pagination cursor from a previous response  |

```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/spaces/access-grants/6f1ecb2a-.../reads -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "events": [
    {
      "id": "...",
      "event_type": "space.moderator_read",
      "severity": "info",
      "actor_did": "did:plc:moderator",
      "subject": "at://did:web:happyview.example.com/space/com.example.forum/main",
      "detail": { "action": "list_records", "grant_id": "6f1ecb2a-...", "scope": "space", "...": "..." },
      "created_at": "2026-01-01T00:05:00Z"
    }
  ],
  "cursor": "..."
}
```

`cursor` is omitted when there are no more reads.

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

A space's metadata, its resolved member list (including members added through delegation), every DID with records in it (`authors`, which can include accounts that are no longer members), and the record count of each collection in it. `{id}` is the space's `id` from [List spaces](#list-spaces).

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
  "authors": ["did:plc:creator123", "did:plc:formermember"],
  "collections": [
    { "collection": "com.example.forum.post", "count": 42 }
  ]
}
```

Returns `403 SpaceInspectorDisabled` if the space inspector is off, and `404 Not Found` if no space has that ID.

## List records in a space

```
GET /admin/spaces/{id}/records
```

Records in a space, newest first. Requires `spaces:inspect`, the space inspector on, and an [access grant](#access-grants) covering the space — or, when `repo` is set, an account grant for that DID.

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

Returns `403 SpaceInspectorDisabled` if the space inspector is off, `403 SpaceAccessGrantRequired` if no active grant covers the space (or the `repo` filter), and `404 Not Found` if no space has that ID.

## Get a blob in a space

```
GET /admin/spaces/{id}/blob
```

A blob referenced by a record in the space, fetched from its author's PDS. The response body is the blob, with the content type the PDS reports. Requires `spaces:inspect`, the space inspector on, and a covering access grant.

| Param | Type   | Required | Description  |
| ----- | ------ | -------- | ------------ |
| `cid` | string | yes      | The blob CID |

```sh tab="cURL" tab-group="language"
curl "http://127.0.0.1:3000/admin/spaces/0b6c1f0e-.../blob?cid=bafkrei..." -H "$AUTH" -o blob
```

A space grant covering this space opens any blob a record in it references, regardless of author. Without one, each active account grant is checked in turn, longest-expiring first, and opens the blob only when that account's own record references it — if another author's record happens to share the CID, the account grant doesn't cover it.

Returns `403 SpaceAccessGrantRequired` if no grant covers the space and the caller holds no account grant at all. If the caller holds an account grant but none of them reference this blob, the response is `404 Not Found` instead, the same as a CID no record references — an account grant that misses looks identical to the blob not existing. Also returns `502 Bad Gateway` if the author's PDS cannot serve it.

## List an account's spaces

```
GET /admin/accounts/{did}/spaces
```

Where an account has membership or records. Metadata only — no access grant required. Requires `spaces:read`. Returns `403 SpaceInspectorDisabled` if the space inspector is off.

```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/accounts/did:plc:abc/spaces -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "spaces": [
    { "space": { "id": "0b6c1f0e-...", "uri": "at://...", "...": "..." }, "record_count": 12 }
  ]
}
```

## List an account's records

```
GET /admin/accounts/{did}/space-records
```

One account's records across every space, newest first. Requires `spaces:inspect`, the space inspector on, and an account-scoped [access grant](#access-grants) for `{did}` — a space grant never covers this endpoint, even for a space the account belongs to.

| Param        | Type   | Required | Description                                 |
| ------------ | ------ | -------- | ------------------------------------------- |
| `space`      | string | no       | Only records in this space                  |
| `collection` | string | no       | Only records in this collection             |
| `limit`      | number | no       | Max results per page (default 20, max 100)  |
| `cursor`     | string | no       | Pagination cursor from a previous response  |

```sh tab="cURL" tab-group="language"
curl "http://127.0.0.1:3000/admin/accounts/did:plc:abc/space-records?collection=com.example.forum.post" -H "$AUTH"
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
      "record": { "...": "..." },
      "space_id": "0b6c1f0e-...",
      "space_uri": "at://did:web:happyview.example.com/space/com.example.forum/main"
    }
  ],
  "cursor": "..."
}
```

Returns `403 SpaceInspectorDisabled` if the space inspector is off, and `403 SpaceAccessGrantRequired` if the caller has no account grant for `{did}`.

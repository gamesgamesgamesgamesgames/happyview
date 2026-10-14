---
title: "Lexicon Publishing"
---

Admin API endpoints for publishing lexicons as `com.atproto.lexicon.schema` records and managing the defaults that decide where they go. For the concepts, DNS requirements and dashboard, see [Publishing Lexicons](../../guides/publishing-lexicons.md).

All endpoints are under `/admin/lexicon-publishing`. Reads need `lexicons:read` and writes need `lexicons:publish`. Errors use the usual body, `{"error": "<message>"}`.

```sh
# All examples assume $TOKEN is an API key (hv_...)
AUTH="Authorization: Bearer $TOKEN"
```

## Shared shapes

A **target** is a place a lexicon can be published:

```json
{
  "kind": "linked_repo",
  "linked_repo_id": "550e8400-e29b-41d4-a716-446655440000",
  "did": "did:plc:abc123",
  "handle": "partner.example.com",
  "available": true,
  "unavailable_reason": null,
  "can_unpublish": true
}
```

`kind` is `instance` or `linked_repo`. `available` is false when nothing can be written there now, and `unavailable_reason` says why. `can_unpublish` is true when the target can also delete records.

A **rule** (a publishing default):

| Field            | Type        | Description                                                                 |
| ---------------- | ----------- | --------------------------------------------------------------------------- |
| `id`             | string      | Rule ID                                                                     |
| `pattern`        | string      | `*`, a lowercased prefix such as `com.example.*`, or an exact NSID          |
| `target_kind`    | string      | `instance` or `linked_repo`                                                 |
| `linked_repo_id` | string/null | The linked repo; `null` for the instance, or after its linked repo is removed |
| `auto_publish`   | boolean     | Publish matching lexicons without a click                                   |
| `created_by`     | string      | DID of the admin who created it                                             |
| `created_at`     | string      | RFC 3339                                                                    |
| `updated_at`     | string      | RFC 3339                                                                    |

Rules returned by the API also carry a `target` object beside these fields.

## Get the overview

```http
GET /admin/lexicon-publishing
```

**Permission:** `lexicons:read`

**Response:**

```json
{
  "targets": [],
  "rules": [],
  "lexicons": [
    {
      "nsid": "com.example.notes.getNote",
      "layer": "manual",
      "rule_id": "7c1e0f3a-0f7e-4c35-9f43-6d2b8d1a9f10",
      "rule_pattern": "com.example.*",
      "auto_publish": true,
      "target": {},
      "status": "outdated",
      "publication": {
        "nsid": "com.example.notes.getNote",
        "did": "did:plc:abc123",
        "uri": "at://did:plc:abc123/com.atproto.lexicon.schema/com.example.notes.getNote",
        "cid": "bafyrei...",
        "published_at": "2026-10-13T09:00:00+00:00",
        "last_error": null,
        "last_attempt_at": "2026-10-13T09:00:00+00:00"
      }
    }
  ]
}
```

- `targets`: the instance first, then one per linked repo.
- `rules`: most specific first.
- `lexicons`: every publishable lexicon, sorted by NSID. `layer` is `manual` or `project`; network lexicons never appear. `rule_id`, `rule_pattern` and `target` are `null` when no default matches, and `auto_publish` is then `false`. `status` is one of `no_rule`, `target_unavailable`, `unpublished`, `moved`, `outdated` or `current`. `publication` is `null` until something has been attempted; it carries only the error columns when every attempt has failed.

The overview computes each lexicon's CID locally, so it makes no network calls.

## Check DNS

```http
GET /admin/lexicon-publishing/dns
```

**Permission:** `lexicons:read`

Looks up `_lexicon.<authority>` for every distinct authority among the publishable lexicons, eight at a time with a five-second timeout each. It's separate from the overview because DNS is slow.

```json
[
  {
    "authority": "notes.example.com",
    "record_name": "_lexicon.notes.example.com",
    "did": "did:plc:abc123",
    "error": null
  }
]
```

`did` is `null` when there's no TXT record. `error` is set when the lookup itself failed, such as a timeout. Compare `did` with each lexicon's target DID to see what needs adding.

## Create a default

```http
POST /admin/lexicon-publishing/rules
```

**Permission:** `lexicons:publish`

**Body:**

| Field            | Type    | Description                                                           |
| ---------------- | ------- | --------------------------------------------------------------------- |
| `pattern`        | string  | Required. `*`, a prefix ending in `.*`, or an exact NSID              |
| `target_kind`    | string  | Required. `instance` or `linked_repo`                                 |
| `linked_repo_id` | string  | Required for `linked_repo`; ignored for `instance`                    |
| `auto_publish`   | boolean | Optional, defaults to `false`                                         |

```json
{
  "pattern": "com.example.*",
  "target_kind": "linked_repo",
  "linked_repo_id": "550e8400-e29b-41d4-a716-446655440000",
  "auto_publish": true
}
```

Returns `201` with the rule and its target.

| Status | Cause                                                                                     |
| ------ | ----------------------------------------------------------------------------------------- |
| `400`  | A malformed pattern, an unknown `target_kind`, a missing or unknown `linked_repo_id`, or a target that is unavailable (the body gives its reason) |
| `409`  | A default for this pattern already exists. Patterns are compared after normalisation, so `COM.EXAMPLE.*` collides with `com.example.*` |

After a successful create, HappyView starts an auto-publish run in the background.

## Update a default

```http
PATCH /admin/lexicon-publishing/rules/{id}
```

**Permission:** `lexicons:publish`

Any of `target_kind`, `linked_repo_id` and `auto_publish`. The pattern can't change; delete the default and add another. The target is re-validated only when it changes, so you can still turn off auto-publish on a default whose linked repo has gone away. Switching to `instance` clears `linked_repo_id`.

Returns `200` with the rule and its target, or `404` for an unknown ID.

## Delete a default

```http
DELETE /admin/lexicon-publishing/rules/{id}
```

**Permission:** `lexicons:publish`

Returns `204`, or `404` for an unknown ID. Records already published are left alone.

## Publish

```http
POST /admin/lexicon-publishing/publish
```

**Permission:** `lexicons:publish`

**Body:** `{ "nsids": ["com.example.notes.getNote"] }`, between 1 and 500 NSIDs. An empty or longer list is a `400`.

NSIDs are published one after another, in the order given. The response is `200` even when some fail:

```json
{
  "results": [
    {
      "nsid": "com.example.notes.getNote",
      "ok": true,
      "uri": "at://did:plc:abc123/com.atproto.lexicon.schema/com.example.notes.getNote",
      "cid": "bafyrei...",
      "error": null
    },
    {
      "nsid": "com.example.other",
      "ok": false,
      "uri": null,
      "cid": null,
      "error": "not a lexicon HappyView can publish"
    }
  ]
}
```

## Unpublish

```http
POST /admin/lexicon-publishing/unpublish
```

**Permission:** `lexicons:publish`

**Body:** `{ "nsid": "com.example.notes.getNote" }`

Deletes the record from the account it was published to and forgets the publication. Returns `204`.

| Status | Cause                                                                                  |
| ------ | -------------------------------------------------------------------------------------- |
| `404`  | The NSID isn't published                                                               |
| `409`  | No linked repo or attached account holds the DID the record was published to; nothing is changed |
| `401`  | The attached account needs to sign in again, or a linked repo needs reauthorization |
| `403`  | The linked repo's grant lacks the `delete` scope |
| `502`  | The attached account's PDS refused the delete                                          |

## Events

These actions are written to the event log: `lexicon_publishing.rule_created`, `lexicon_publishing.rule_updated`, `lexicon_publishing.rule_deleted`, `lexicon.published`, `lexicon.publish_failed` and `lexicon.unpublished`.

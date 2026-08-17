---
title: "Linked Repos"
---

Admin API endpoints for managing linked repos and their invites. The whole lifecycle is available here, so the feature can be driven headlessly without the dashboard. For a conceptual overview, see [Linked Repos](../../guides/linked-repos.md).

The repo write operations themselves are not part of this API. The admin API manages grants; [Lua scripts](../lua/linked-repos-api.md) use them.

## List grants

```http
GET /admin/linked-repos
```

Returns every grant, newest first. Never includes session data or tokens.

**Permission:** `linked-repos:read`

**Response:**

```json
{
  "linked_repos": [
    {
      "id": "550e8400-e29b-41d4-a716-446655440000",
      "did": "did:plc:abc123",
      "handle": "partner.example.com",
      "reason": "Mirror published notes",
      "scopes": "atproto repo:com.example.note?action=create&action=update",
      "status": "active",
      "last_error": null,
      "last_refreshed_at": "2026-07-30T12:00:00Z",
      "authorized_at": "2026-07-29T09:14:00Z",
      "created_by": "did:plc:admin",
      "created_at": "2026-07-29T09:12:00Z"
    }
  ]
}
```

`did` is `null` for an open grant that hasn't been authorized yet. `status` is one of `pending`, `active`, or `needs_reauth`.

## Create a grant

```http
POST /admin/linked-repos
```

**Permission:** `linked-repos:create`

**Body:**

| Field    | Type   | Description                                                                    |
| -------- | ------ | ------------------------------------------------------------------------------ |
| `handle` | string | Optional. A handle or DID to pin the grant to. Omit for an open grant.          |
| `reason` | string | Optional. Why access is being requested; shown to the recipient of an invite. |
| `scopes` | string | Required. Whitespace-separated scope string.                                    |

```json
{
  "handle": "partner.example.com",
  "reason": "Mirror published notes",
  "scopes": "repo:com.example.note?action=create&action=update"
}
```

Returns the created grant, in the same shape as the list endpoint.

Scopes are validated on the way in: a malformed scope or an invalid NSID is rejected with a `400`. `atproto` is added automatically if the string doesn't contain it, so the stored scopes may differ from what you sent. Scopes are immutable afterwards; there is no update endpoint.

Pinning to a DID that already has a grant returns a `409`.

## Start an authorization

```http
POST /admin/linked-repos/{id}/authorize
```

Returns a URL to send the browser to. This only works for a grant that names a handle or DID. An open grant has no identity to authorize against, so it returns a `400` telling you to use an invite link instead.

**Permission:** `linked-repos:create`

**Response:**

```json
{
  "authorize_url": "https://pds.example.com/oauth/authorize?..."
}
```

<Callout type="warn">
A `400` mentioning cached client metadata is expected occasionally right after creating a grant. Authorization servers cache the client metadata document, so a freshly added scope may not be visible yet. Retrying after a minute usually clears it. The error text distinguishes this from a scope the server genuinely won't accept.
</Callout>

## Mint an invite

```http
POST /admin/linked-repos/{id}/invite
```

Mints a single-use invite link for the grant.

**Permission:** `linked-repos:create`

**Body:** optional.

| Field        | Type   | Description                                                       |
| ------------ | ------ | ----------------------------------------------------------------- |
| `expires_in` | number | Optional. Lifetime in seconds, 60 to 2592000 (30 days). Default 7 days. |

**Response:**

```json
{
  "invite_url": "https://appview.example.com/auth/linked-repo/start?token=...",
  "expires_at": "2026-08-06T09:12:00Z"
}
```

<Callout type="warn">
This is the **only** response that ever contains the invite URL, and it cannot be recovered afterwards: only a hash is stored. A headless caller must capture it here. If it's lost, revoke the invite and mint another.
</Callout>

## List invites

```http
GET /admin/linked-repos/{id}/invites
```

Returns the grant's outstanding, unexpired invites. Expired invites are not listed.

**Permission:** `linked-repos:read`

**Response:**

```json
{
  "invites": [
    {
      "invite_id": "a5e55c20844abe8068d142a815c6730d4e4ae73309c145dc1c5b451d9e7e45c8",
      "expires_at": "2026-08-06T09:12:00Z"
    }
  ]
}
```

`invite_id` is the stored SHA-256 of the token. It's safe to display and is what you pass to the revoke endpoint, but it cannot be turned back into a usable link.

## Revoke an invite

```http
DELETE /admin/linked-repos/{id}/invites/{invite_id}
```

**Permission:** `linked-repos:create`

Revoking uses the same permission as minting, since they're one lifecycle: whoever can send a link should be able to take it back.

**Response:**

```json
{ "revoked": true }
```

Returns `404` if no matching invite exists for that grant, which includes an invite that has already been used, revoked, or expired.

## Delete a grant

```http
DELETE /admin/linked-repos/{id}
```

Deletes the grant and its stored OAuth session together.

**Permission:** `linked-repos:delete`

**Response:**

```json
{ "deleted": true }
```

Scopes are immutable, so this is also how you change what a grant can do: delete it and create a new one. Deleting on HappyView's side does not revoke the authorization at the PDS.

## Headless flow

Creating a grant and getting it authorized without the dashboard:

```bash
# 1. Create an open grant
GRANT=$(curl -s -X POST https://appview.example.com/admin/linked-repos \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"reason":"Mirror notes","scopes":"repo:com.example.note?action=create"}' \
  | jq -r .id)

# 2. Mint an invite, capturing the URL — it can't be read back later
curl -s -X POST "https://appview.example.com/admin/linked-repos/$GRANT/invite" \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"expires_in":3600}' | jq -r .invite_url

# 3. Send that URL to the repo owner. Poll until they've completed it.
curl -s https://appview.example.com/admin/linked-repos \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  | jq --arg id "$GRANT" '.linked_repos[] | select(.id == $id) | .status'
```

## See also

- [Linked Repos guide](../../guides/linked-repos.md): Concepts, scopes, and the invite flow
- [Linked Repos Lua API](../lua/linked-repos-api.md): Writing to a linked repo from a script
- [Permissions](../../guides/permissions.md): The full permission list

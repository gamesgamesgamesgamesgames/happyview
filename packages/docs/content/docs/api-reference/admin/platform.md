---
title: "Platform"
---

Endpoints for a managed-hosting provider that runs this instance on someone else's behalf. Self-hosted instances don't use them.

## Enabling the platform principal

Set `PLATFORM_API_KEY_HASH` to the hex SHA-256 of a key in the `hv_` format:

```sh
KEY="hv_$(openssl rand -hex 16)"
printf '%s' "$KEY" | sha256sum | cut -d' ' -f1
```

On macOS, use `shasum -a 256` in place of `sha256sum`.

A request with `Authorization: Bearer <key>` is then authenticated as the **platform principal**. It isn't stored with other API keys, so it can't be revoked from the dashboard; change or remove the variable and restart to rotate or disable it. When it's set, the dashboard shows "Managed by HappyProto".

The platform principal can:

- list, add, remove and promote [domains](./domains.md); while it's set, it is the only caller that can add, remove or promote one
- read [stats](./stats.md)
- read script and ingest [metrics](./metrics.md), but not the live view
- read the status of a `TOKEN_ENCRYPTION_KEY` rotation ([`GET /admin/encryption/rotation`](./encryption.md#rotation-status)): how many stored values each store still has sealed under the previous key, and whether the previous key can be removed. It sees counts, never a value, and can't start a re-seal
- call the endpoints on this page

Every other admin endpoint refuses it with `403`.

## Set the super user

```
PUT /admin/platform/super-user
```

Makes `did` the instance's only super user, creating the user if needed and demoting any previous super user. Safe to repeat.

| Field | Type   | Required | Description         |
| ----- | ------ | -------- | ------------------- |
| `did` | string | yes      | The new owner's DID |

```sh
curl -X PUT http://127.0.0.1:3000/admin/platform/super-user \
  -H "Authorization: Bearer $KEY" \
  -H "Content-Type: application/json" \
  -d '{ "did": "did:plc:abc123" }'
```

**Response**: `200 OK`

```json
{ "user_id": "550e8400-e29b-41d4-a716-446655440000", "did": "did:plc:abc123" }
```

Returns `400` if `did` isn't a DID and `403` for any caller other than the platform principal.

Once a super user exists, the first person to sign in is no longer made super user automatically. A provider should call this endpoint before the instance is publicly reachable.

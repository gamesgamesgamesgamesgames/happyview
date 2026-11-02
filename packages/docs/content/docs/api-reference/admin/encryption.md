---
title: "Encryption rotation"
---

Endpoints for following a `TOKEN_ENCRYPTION_KEY` rotation. [Rotating the encryption key](../../guides/rotating-the-encryption-key.md) walks through the whole procedure.

## Rotation status

```
GET /admin/encryption/rotation
```

Requires `settings:manage`. Each call reads every stored value, so the counts are never stale, and `previous_key_can_go` is never `true` while any value still needs the previous key.

```sh
curl -s http://127.0.0.1:3000/admin/encryption/rotation \
  -H "Authorization: Bearer $TOKEN"
```

**Response**: `200 OK`, part way through a re-seal (`stores` trimmed to three of its fourteen entries):

```json
{
  "key_configured": true,
  "previous_key_configured": true,
  "previous_key_can_go": false,
  "remaining": 3,
  "unsealed": 0,
  "unreadable": 0,
  "stores": [
    { "store": "oauth_client_keys", "previous": 1, "unsealed": 0, "unreadable": 0 },
    { "store": "attestation_key", "previous": 1, "unsealed": 0, "unreadable": 0 },
    { "store": "delegation_key", "previous": 1, "unsealed": 0, "unreadable": 0 }
  ],
  "job": { "id": "ffd3922d-db8d-4fd9-a287-be128500c3be", "status": "running", "progress": null }
}
```

| Field | Type | Description |
| --- | --- | --- |
| `key_configured` | boolean | `TOKEN_ENCRYPTION_KEY` is set. When it isn't, the response has only `key_configured`, `previous_key_configured` and `previous_key_can_go` (all `false`), `remaining` (`0`), `stores` (empty) and `job` (`null`) |
| `previous_key_configured` | boolean | `TOKEN_ENCRYPTION_KEY_PREVIOUS` is set to a key other than the current one |
| `previous_key_can_go` | boolean | The previous key is set and no stored value needs it, so it can be removed |
| `remaining` | integer | Stored values only the previous key opens |
| `unsealed` | integer | Values stored in plaintext, from before the instance had a key. They're sealed at the next start |
| `unreadable` | integer | Values neither key opens. Nothing can re-seal them |
| `stores` | array | The same three counts for every store, as `{store, previous, unsealed, unreadable}`: `dpop_keys`, `dpop_sessions`, `oauth_client_keys`, `external_account_tokens`, `service_identity`, `verification_methods`, `space_dids`, `attestation_key`, `delegation_key`, `script_variables`, `oauth_sessions`, `linked_repo_sessions`, `oauth_state`, `plugin_secrets` |
| `job` | object or `null` | The `happyview.reseal` job that is pending, running, pausing or paused: its `id`, `status` and `progress`, which is `null` until the job finishes. `null` when no such job exists |

## Start a re-seal

```
POST /admin/encryption/rotation
```

Requires `settings:manage`. Starts a `happyview.reseal` job, or answers with the one already under way. HappyView starts one at each start when anything needs re-sealing, so this is only needed to resume one that stopped.

```sh
curl -s -X POST http://127.0.0.1:3000/admin/encryption/rotation \
  -H "Authorization: Bearer $TOKEN"
```

**Response**: `202 Accepted`

```json
{ "job_id": "ffd3922d-db8d-4fd9-a287-be128500c3be" }
```

Returns `400` when `TOKEN_ENCRYPTION_KEY` isn't set.

---
title: "Overview"
---

The admin API lets you manage lexicons, monitor records, run backfill jobs, and control user access. All endpoints live under `/admin` and require authentication from a DID that exists in the `happyview_users` table, with the appropriate [permissions](../../guides/permissions.md) for the endpoint being called. You can also manage all of this through the [web dashboard](../../getting-started/dashboard.md).

## Auth

The admin API supports three authentication methods:

1. **API keys** — read/write tokens starting with `hv_`, passed as `Authorization: Bearer hv_...`. See the [API Keys guide](../../guides/api-keys.md) for details.
2. **Service auth JWT** — atproto inter-service authentication via signed JWTs.
3. **Cookie-based session auth** — signed session cookies set during the dashboard OAuth login flow. The [web dashboard](../../getting-started/dashboard.md) uses this method.

In all cases the resolved DID is checked against the `happyview_users` table, and the user's permissions are loaded to authorize the request.

**Auto-bootstrap**: If the `happyview_users` table is empty, the first authenticated request automatically creates the caller as the **super user** with all permissions granted.

Non-user DIDs receive a `403 Forbidden` response. Users without the required permission for a specific endpoint also receive `403 Forbidden`.

## Errors

All error responses return JSON with an `error` field:

```json
{
  "error": "description of what went wrong"
}
```

| Status             | Meaning                                                                                |
| ------------------ | -------------------------------------------------------------------------------------- |
| `400 Bad Request`  | Invalid input (missing required fields, malformed lexicon JSON)                        |
| `401 Unauthorized` | Missing or invalid API key or service auth JWT                                         |
| `403 Forbidden`    | Authenticated DID is not in the users table, or user lacks the required permission     |
| `404 Not Found`    | Lexicon, user, or backfill job not found                                               |

```sh
# All examples assume $TOKEN is an API key (hv_...)
AUTH="Authorization: Bearer $TOKEN"
```

## Endpoint groups

| Group | Description |
| ----- | ----------- |
| [Lexicons](lexicons.md) | Upload, list, get, and delete lexicons and network lexicons |
| [Stats](stats.md) | Record counts by collection |
| [Backfill](backfill.md) | Create and monitor historical backfill jobs |
| [Event Logs](events.md) | Query the audit trail of system events |
| [API Keys](api-keys.md) | Create, list, and revoke API keys |
| [Users](users.md) | Create, list, update, and delete admin users |
| [Labelers](labelers.md) | Manage external labeler subscriptions |
| [Records](records.md) | List and delete indexed records |
| [Instance Settings](settings.md) | Configure app name, logo, policy URLs, and concurrency settings |
| [Domains](domains.md) | Manage domains and their OAuth client identities |
| [Scripts](scripts.md) | Create, list, update, and delete Lua scripts |
| [Script Variables](script-variables.md) | Encrypted key/value pairs for Lua scripts |
| [API Clients](api-clients.md) | Register and manage third-party XRPC clients |
| [Plugins](plugins.md) | Install, configure, and manage WASM plugins |
| [Dead Letters](dead-letters.md) | List, inspect, dismiss, retry, and reindex dead-lettered events |
| [Service Identity](service-identity.md) | Get and update service identity configuration |
| [Service Entries](service-entries.md) | Manage service entries and their XRPC bindings |
| [Verification Methods](verification-methods.md) | Create, list, and delete DID verification methods |
| [Feature Flags](feature-flags.md) | List feature flag status |
| [Permissions](permissions.md) | List available permission definitions |

## Permissions

Each admin API endpoint requires a specific permission. See the [Permissions guide](../../guides/permissions.md) for the full list of permissions and templates.

| Endpoint                                 | Required Permission        |
| ---------------------------------------- | -------------------------- |
| `POST /admin/lexicons`                   | `lexicons:create`          |
| `GET /admin/lexicons`                    | `lexicons:read`            |
| `GET /admin/lexicons/{id}`               | `lexicons:read`            |
| `DELETE /admin/lexicons/{id}`            | `lexicons:delete`          |
| `POST /admin/network-lexicons`           | `lexicons:create`          |
| `GET /admin/network-lexicons`            | `lexicons:read`            |
| `DELETE /admin/network-lexicons/{id}`    | `lexicons:delete`          |
| `GET /admin/stats`                       | `stats:read`               |
| `POST /admin/backfill`                   | `backfill:create`          |
| `GET /admin/backfill/status`             | `backfill:read`            |
| `POST /admin/backfill/{id}/cancel`       | `backfill:create`          |
| `POST /admin/backfill/{id}/pause`        | `backfill:create`          |
| `POST /admin/backfill/{id}/resume`       | `backfill:create`          |
| `GET /admin/backfill/{id}/events`        | `backfill:read`            |
| `GET /admin/backfill/{id}/repos`         | `backfill:read`            |
| `GET /admin/backfill/{id}/pds-summary`   | `backfill:read`            |
| `DELETE /admin/backfill/{id}/details`    | `backfill:create`          |
| `DELETE /admin/backfill/details`         | `backfill:create`          |
| `GET /admin/events`                      | `events:read`              |
| `POST /admin/api-keys`                   | `api-keys:create`          |
| `GET /admin/api-keys`                    | `api-keys:read`            |
| `DELETE /admin/api-keys/{id}`            | `api-keys:delete`          |
| `POST /admin/users`                      | `users:create`             |
| `GET /admin/users`                       | `users:read`               |
| `GET /admin/users/{id}`                  | `users:read`               |
| `PATCH /admin/users/{id}/permissions`    | `users:update`             |
| `DELETE /admin/users/{id}`               | `users:delete`             |
| `POST /admin/users/transfer-super`       | Super user only            |
| `GET /admin/script-variables`            | `script-variables:read`    |
| `POST /admin/script-variables`           | `script-variables:create`  |
| `DELETE /admin/script-variables/{key}`   | `script-variables:delete`  |
| `GET /admin/scripts`                     | `scripts:read`             |
| `POST /admin/scripts`                    | `scripts:manage`           |
| `GET /admin/scripts/{id}`                | `scripts:read`             |
| `PATCH /admin/scripts/{id}`              | `scripts:manage`           |
| `DELETE /admin/scripts/{id}`             | `scripts:manage`           |
| `POST /admin/labelers`                   | `labelers:create`          |
| `GET /admin/labelers`                    | `labelers:read`            |
| `PATCH /admin/labelers/{did}`            | `labelers:create`          |
| `DELETE /admin/labelers/{did}`           | `labelers:delete`          |
| `GET /admin/records`                     | `records:read`             |
| `GET /admin/records/collections`         | `records:read`             |
| `DELETE /admin/records`                  | `records:delete`           |
| `DELETE /admin/records/collection`       | `records:delete-collection`|
| `GET /admin/settings`                    | `settings:manage`          |
| `GET /admin/settings/db-info`            | `settings:manage`          |
| `PUT /admin/settings/{key}`              | `settings:manage`          |
| `DELETE /admin/settings/{key}`           | `settings:manage`          |
| `PUT /admin/settings/logo`              | `settings:manage`          |
| `DELETE /admin/settings/logo`           | `settings:manage`          |
| `GET /admin/plugins`                     | `plugins:read`             |
| `POST /admin/plugins`                    | `plugins:create`           |
| `POST /admin/plugins/preview`            | `plugins:read`             |
| `GET /admin/plugins/official`            | `plugins:read`             |
| `DELETE /admin/plugins/{id}`             | `plugins:delete`           |
| `POST /admin/plugins/{id}/reload`        | `plugins:create`           |
| `POST /admin/plugins/{id}/check-update`  | `plugins:read`             |
| `GET /admin/plugins/{id}/secrets`        | `plugins:read`             |
| `PUT /admin/plugins/{id}/secrets`        | `plugins:create`           |
| `GET /admin/domains`                     | `settings:manage`          |
| `POST /admin/domains`                    | `settings:manage`          |
| `DELETE /admin/domains/{id}`             | `settings:manage`          |
| `POST /admin/domains/{id}/primary`       | `settings:manage`          |
| `GET /admin/api-clients`                 | `api-clients:view`         |
| `POST /admin/api-clients`                | `api-clients:create`       |
| `GET /admin/api-clients/{id}`            | `api-clients:view`         |
| `PUT /admin/api-clients/{id}`            | `api-clients:edit`         |
| `DELETE /admin/api-clients/{id}`         | `api-clients:delete`       |
| `GET /admin/dead-letters`                | `dead-letters:read`        |
| `GET /admin/dead-letters/count`          | `dead-letters:read`        |
| `GET /admin/dead-letters/{id}`           | `dead-letters:read`        |
| `POST /admin/dead-letters/{id}/dismiss`  | `dead-letters:manage`      |
| `POST /admin/dead-letters/{id}/retry`    | `dead-letters:manage`      |
| `POST /admin/dead-letters/{id}/reindex`  | `dead-letters:manage`      |
| `POST /admin/dead-letters/bulk/dismiss`  | `dead-letters:manage`      |
| `POST /admin/dead-letters/bulk/retry`    | `dead-letters:manage`      |
| `POST /admin/dead-letters/bulk/reindex`  | `dead-letters:manage`      |
| `GET /admin/service-identity`            | `settings:manage`          |
| `PUT /admin/service-identity`            | `settings:manage`          |
| `GET /admin/service-entries`             | `settings:manage`          |
| `POST /admin/service-entries`            | `settings:manage`          |
| `PUT /admin/service-entries/{id}`        | `settings:manage`          |
| `DELETE /admin/service-entries/{id}`     | `settings:manage`          |
| `GET /admin/service-entries/{id}/xrpcs`  | `settings:manage`          |
| `POST /admin/service-entries/{id}/xrpcs` | `settings:manage`          |
| `DELETE /admin/service-entries/{id}/xrpcs`| `settings:manage`         |
| `POST /admin/service-entries/sync-plc`   | `settings:manage`          |
| `POST /admin/service-entries/sync-plc/request` | `settings:manage`    |
| `POST /admin/service-entries/sync-plc/submit`  | `settings:manage`    |
| `GET /admin/lexicons/{id}/services`      | `settings:manage`          |
| `GET /admin/verification-methods`        | `settings:manage`          |
| `POST /admin/verification-methods`       | `settings:manage`          |
| `DELETE /admin/verification-methods/{fragment_id}` | `settings:manage` |
| `GET /admin/feature-flags`               | `settings:manage`          |
| `GET /admin/network-lexicons/resolve/{nsid}` | `lexicons:read`        |
| `GET /admin/permissions`                 | `users:read`               |
| `GET /admin/settings/xrpc-proxy`         | `settings:manage`          |
| `PUT /admin/settings/xrpc-proxy`         | `settings:manage`          |

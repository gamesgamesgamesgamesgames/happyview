---
title: "Overview"
---

The admin API lets you manage lexicons, monitor records, run backfill jobs, and control user access. All endpoints live under `/admin` and require authentication from a DID that exists in the `happyview_users` table, with the appropriate [permissions](../../guides/permissions.md) for the endpoint being called. You can also manage all of this through the [web dashboard](../../getting-started/dashboard.md).

## Auth

The admin API supports four authentication methods:

1. **API keys** — read/write tokens starting with `hv_`, passed as `Authorization: Bearer hv_...`. See the [API Keys guide](../../guides/api-keys.md) for details.
2. **Service auth JWT** — atproto inter-service authentication via signed JWTs. The token must be bound to the method `dev.happyview.admin.manage` (its `lxm` claim). A PDS mints such a token for an OAuth app only when the user granted it that method, which is what the next method's permission set does.
3. **Cookie-based session auth** — signed session cookies set during the dashboard OAuth login flow. The [web dashboard](../../getting-started/dashboard.md) uses this method.
4. **App sessions granted `dev.happyview.adminFull`** — an API client's [DPoP session](../oauth/api-clients.md), when the user approved the permission set `dev.happyview.adminFull` at sign-in. The app asks for it with the scope `include:dev.happyview.adminFull`. The session then acts with the user's own permissions.

HappyView decides whether a session was granted the set by what the user's authorization server says, never by the scope the app registered. On the session's first admin request it refreshes the session itself, after checking that the user's PDS names the session's authorization server, and reads the granted scope from the token response. It reads it again once that access token expires, so revoking the grant at the PDS ends admin access within one token lifetime. Any other API client session is refused with `403 Forbidden`, because those sessions belong to apps built on the instance, and an admin signing in to one must not hand it the instance.

hpy, the HappyView CLI, signs in this way through a client every instance has built in: `hvc_hpy`, which `GET /config` publishes as `cli_client_key`. An operator who wants another app to manage the instance registers it with `include:dev.happyview.adminFull` among its scopes.

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
| [Lexicon Publishing](lexicon-publishing.md) | Publish lexicons to the network and manage publishing rules |
| [Stats](stats.md) | Record counts by collection, admin and client counts |
| [Backfill](backfill.md) | Create and monitor historical backfill jobs |
| [Event Logs](events.md) | Query the audit trail of system events |
| [API Keys](api-keys.md) | Create, list, and revoke API keys |
| [Users](users.md) | Create, list, update, and delete admin users |
| [Labelers](labelers.md) | Manage external labeler subscriptions |
| [Records](records.md) | List and delete indexed records |
| [Spaces](spaces.md) | Browse spaces and their records for moderation |
| [Instance Settings](settings.md) | Configure app name, logo, policy URLs, and concurrency settings |
| [Domains](domains.md) | Manage domains and their OAuth client identities |
| [OAuth Keys](oauth-keys.md) | Rotate and revoke the instance's OAuth client-assertion signing key |
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
| `GET /admin/lexicon-publishing`          | `lexicons:read`            |
| `GET /admin/lexicon-publishing/dns`      | `lexicons:read`            |
| `POST /admin/lexicon-publishing/rules`   | `lexicons:publish`         |
| `PATCH /admin/lexicon-publishing/rules/{id}` | `lexicons:publish`     |
| `DELETE /admin/lexicon-publishing/rules/{id}` | `lexicons:publish`    |
| `POST /admin/lexicon-publishing/publish` | `lexicons:publish`         |
| `POST /admin/lexicon-publishing/unpublish` | `lexicons:publish`       |
| `GET /admin/stats`                       | `stats:read`               |
| `GET /admin/stats/counts`                | `stats:read`               |
| `GET /admin/jetstream/status`            | `stats:read`               |
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
| `GET /admin/scripts/languages`           | `scripts:read`             |
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
| `GET /admin/spaces`                      | `spaces:read`              |
| `GET /admin/spaces/inspector`            | `spaces:read` or `spaces:inspect` |
| `GET /admin/spaces/access-grants`        | `spaces:inspect`           |
| `POST /admin/spaces/access-grants`       | `spaces:inspect`           |
| `DELETE /admin/spaces/access-grants/{id}`| `spaces:inspect` (own) or `users:update` (others) |
| `GET /admin/spaces/access-grants/{id}/reads` | `events:read`          |
| `GET /admin/spaces/{id}`                 | `spaces:read`              |
| `GET /admin/spaces/{id}/access`          | `spaces:inspect`           |
| `GET /admin/spaces/{id}/records`         | `spaces:inspect`           |
| `GET /admin/spaces/{id}/blob`            | `spaces:inspect`           |
| `GET /admin/accounts/{did}/access`        | `spaces:inspect`           |
| `GET /admin/accounts/{did}/spaces`       | `spaces:read`              |
| `GET /admin/accounts/{did}/space-records`| `spaces:inspect`           |
| `GET /admin/database/status`             | `settings:manage`          |
| `POST /admin/database/vacuum/schedule`   | `settings:manage`          |
| `DELETE /admin/database/vacuum/schedule` | `settings:manage`          |
| `POST /admin/storage/retrain`           | `settings:manage`          |
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
| `GET /admin/oauth/instance-key`          | `settings:manage`          |
| `POST /admin/oauth/instance-key/rotate`  | `settings:manage`          |
| `DELETE /admin/oauth/instance-key/{kid}` | `settings:manage`          |
| `GET /admin/api-clients`                 | `api-clients:view`         |
| `POST /admin/api-clients`                | `api-clients:create`       |
| `GET /admin/api-clients/{id}`            | `api-clients:view`         |
| `PUT /admin/api-clients/{id}`            | `api-clients:edit`         |
| `DELETE /admin/api-clients/{id}`         | `api-clients:delete`       |
| `POST /admin/api-clients/{id}/auth-key`  | `api-clients:edit`         |
| `GET /admin/api-clients/{id}/auth-key`   | `api-clients:view`         |
| `GET /admin/api-clients/{id}/auth-keys`  | `api-clients:view`         |
| `POST /admin/api-clients/{id}/auth-key/recheck` | `api-clients:edit`  |
| `POST /admin/api-clients/{id}/auth-key/rotate`  | `api-clients:edit`  |
| `DELETE /admin/api-clients/{id}/auth-key/{kid}` | `api-clients:edit`  |
| `DELETE /admin/api-clients/{id}/auth-keys` | `api-clients:edit`       |
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

### Jetstream status

`GET /admin/jetstream/status` reports the live [Jetstream](https://github.com/bluesky-social/jetstream) consumer since the process started. A platform API key can read it. Unknown values are an explicit `null`: `protocol`, `host`, `dictionary_id`, `applied_seq`, `applied_time_us` and `lag_ms` are all `null` before the first connection or event, and the two worker arrays are empty until the worker pool starts.

| Field | Meaning |
| --- | --- |
| `protocol` | `v2` (`subscribeEvents`) or `v1` (`/subscribe`); `null` before the first connection |
| `host` | The Jetstream host from `JETSTREAM_URL` |
| `connected` | Whether a subscription is open now |
| `compression` | Whether frames arrive zstd-compressed |
| `dictionary_id` | The zstd dictionary in use, or `null` |
| `applied_seq` | The newest event that it and every event before it have been applied: the position saved for a restart. `null` on v1, which has no seq |
| `applied_time_us` | When Jetstream witnessed that event, in microseconds since the Unix epoch |
| `lag_ms` | Milliseconds from `applied_time_us` to now |
| `worker_queue_depths` | Events waiting in each worker's queue, one entry per `JETSTREAM_WORKERS`. The event a worker is applying or retrying is not counted |
| `now_ms` | Server time when the snapshot was taken, in milliseconds since the Unix epoch. Subtract `worker_busy_since_ms` from it rather than using your own clock |
| `worker_busy_since_ms` | One entry per worker: when its current event started, in milliseconds since the Unix epoch, or `null` when idle. An old value with an empty queue means the worker is stuck retrying |

### Database disk usage and vacuum

`GET /admin/database/status` reports the database and write-ahead log size, free disk space, and the status of the one-time SQLite vacuum described in [Configuration](../../getting-started/configuration.md). It also reports the periodic SQLite WAL checkpoint: `checkpoint_interval_secs` is the `SQLITE_CHECKPOINT_INTERVAL_SECS` in effect (`0` when disabled), `wal_checkpoint_bytes` is the `SQLITE_WAL_CHECKPOINT_BYTES` in effect (`0` when disabled, and always on Postgres), and `checkpoint` is the latest run since the process started, or `null` before the first one and always on Postgres. A run has `at` (RFC 3339), `busy` (`true` when a reader or writer kept it from finishing), `wal_frames` (frames in the WAL when it ran) and `checkpointed_frames` (frames copied back into the database file). `POST /admin/database/vacuum/schedule` arms the vacuum to run at the next restart; `DELETE /admin/database/vacuum/schedule` disarms it. These only apply to SQLite — on a Postgres-backed instance, scheduling returns `400 Bad Request` rather than silently doing nothing.

### Record compression

`POST /admin/storage/retrain` trains a new zstd dictionary for one collection from up to 20,000 of its most recently stored records, as the background task does when a collection first reaches `ZSTD_DICT_TRAIN_AT` records (see [the compact record format](../../guides/database/database-setup.md#after-the-upgrade)). Records written from then on are compressed with it. Records already compressed keep the dictionary they were compressed with, and records the background task has not reached yet get the new one. One retrain runs at a time. SQLite only.

```sh
curl -X POST "http://127.0.0.1:3000/admin/storage/retrain" -H "$AUTH" \
  -H 'content-type: application/json' -d '{"collection": "app.bsky.feed.post"}'
```

```json
{ "collection": "app.bsky.feed.post", "dict_id": 3 }
```

| Status | When |
| --- | --- |
| `200 OK` | The dictionary was trained and is in use |
| `400 Bad Request` | The instance runs on Postgres, or `RECORD_COMPRESSION=none` |
| `404 Not Found` | The instance holds no records in that collection |
| `409 Conflict` | Another retrain is still running on this instance; try again when it finishes |
| `422 Unprocessable Entity` | zstd could not train a dictionary from the records (`TrainingFailed`) |

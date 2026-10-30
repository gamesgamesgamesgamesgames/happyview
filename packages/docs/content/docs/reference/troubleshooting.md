---
title: "Troubleshooting"
---

Common issues and how to resolve them.

## XRPC endpoint returns 404

**Symptom**: `GET /xrpc/your.method.name` returns `{"error": "method not found"}`.

**Causes**:

- The lexicon hasn't been uploaded yet. Check with `GET /admin/lexicons` or the [dashboard](../getting-started/dashboard.md).
- The lexicon's `defs.main.type` doesn't match the HTTP method. Queries are `GET`, procedures are `POST`.
- The NSID in the URL doesn't match the `id` field in the uploaded lexicon JSON.

## Queries return empty results

**Symptom**: The XRPC query endpoint returns `{"records": []}` even though records should exist.

**Causes**:

- The query lexicon is missing a `target_collection`. Without it, the query doesn't know which records to read. See [Lexicons - target_collection](../guides/lexicons.md#target-collection).
- The record-type lexicon hasn't finished backfilling. Check backfill status with `GET /admin/backfill/status` or the dashboard.
- Records exist on the network but HappyView hasn't indexed them yet. Jetstream only delivers events from after the collection was added to the filter. Use [backfill](../guides/backfill.md) to import historical records.

## Procedure returns 401 Unauthorized

**Symptom**: `POST /xrpc/your.method.name` returns `{"error": "..."}` with status 401.

**Causes**:

- No `Authorization: DPoP` header or `X-Client-Key` header is present.
- The DPoP proof is invalid or expired.
- The API client key is not registered or is inactive.

## Admin endpoints return 403 Forbidden

**Symptom**: Admin API calls return `{"error": "forbidden"}`.

**Causes**:

- Your DID is not in the users table. Ask an existing user with `users:create` permission to add you via `POST /admin/users`.
- If this is a fresh deployment with no users, the first authenticated request to any admin endpoint automatically bootstraps you as the super user. Make sure you're logged in via the dashboard or using a valid API key.
- You may be in the users table but lack the required permission for the endpoint you're calling. Check your permissions with `GET /admin/users` or ask a user with `users:update` permission to grant the permission you need.

## Permission denied errors

**Symptom**: Admin API calls return `{"error": "insufficient permissions"}` with status 403, even though you can access other endpoints.

**Causes**:

- Your user account doesn't have the specific permission required by the endpoint. Each endpoint requires a specific permission — see the [permissions table](../api-reference/admin/admin-api.md#permissions).
- If using an API key, the key's effective permissions are the intersection of the key's permissions and your user permissions. A key can never have more access than the user who created it.
- Only the super user can call `POST /admin/users/transfer-super`. This endpoint cannot be accessed with any permission — it requires super user status.

## Lua script errors

**Symptom**: An XRPC endpoint returns `{"error": "script execution failed"}` or `{"error": "script exceeded execution time limit"}`.

**What to do**:

1. Check the server logs: the full error message is logged at error level but not exposed to the client.
2. Use `require("internal.logging")` in your script to trace execution. `log.debug` output appears in server logs at debug level (requires `RUST_LOG` to include debug); `log.info` and above are also recorded as events.
3. If you hit the execution limit, your script likely has an infinite loop or is processing too much data. See [Lua Scripting - Sandbox](../guides/lua-scripting.md#sandbox).

See [Lua Scripting - Debugging](../guides/lua-scripting.md#debugging) for more.

## Backfill job stuck in "pending" or "running"

**Symptom**: A backfill job doesn't progress or stays in `pending`.

**Causes**:

- The backfill worker processes one job at a time. If another job is running, yours will wait.
- The relay (`RELAY_URL`) may be unreachable or slow to respond. Check connectivity.
- Individual PDS fetches can fail silently. The worker logs warnings and continues. Check server logs for details.

See [Backfill](../guides/backfill.md) for how the process works.

## Records not appearing in real time

**Symptom**: New records created on the network don't show up in queries.

**Causes**:

- HappyView receives real-time events via [Jetstream](https://github.com/bluesky-social/jetstream). Verify the `JETSTREAM_URL` is reachable and check server logs for `jetstream.disconnected` events.
- No record-type lexicon exists for the collection. HappyView only indexes collections that have a corresponding record-type lexicon.
- The Jetstream subscription hasn't reconnected with the new collection filter after a lexicon change. This should happen automatically. Check server logs for connection errors.

## Lua script can't find records

**Symptom**: a `happyview.db` chain or `db.get` returns empty results inside a Lua script, even though the admin dashboard shows records exist.

**Causes**:

- `ctx.collection` is only set when the lexicon has a `target_collection`. If you're using a hardcoded collection name, double-check the spelling matches exactly.
- `db.get` expects a full AT URI (`at://did:plc:abc/collection/rkey`), not just an rkey.
- If querying by DID, make sure you're passing the full DID string including the `did:plc:` or `did:web:` prefix.

## Plugin secrets not working

**Symptom**: A plugin fails with authentication errors even though you've configured its secrets.

**Causes**:

- `TOKEN_ENCRYPTION_KEY` is not set. Plugin secrets are encrypted at rest and cannot be read without this key. See [Plugins - Configuration](../guides/plugins.md#plugin-configuration).
- If `TOKEN_ENCRYPTION_KEY` changed since the secrets were saved, the existing encrypted values are unreadable. Re-enter the secrets via the dashboard or `PUT /admin/plugins/{id}/secrets`.
- Environment variable secrets (`PLUGIN_<ID>_<KEY>`) are overridden by dashboard-configured secrets. If you've set both, the dashboard values take precedence.

## OAuth or login issues

HappyView handles atproto OAuth internally via the `atrium-oauth` library. If users can't log in:

1. Verify `PUBLIC_URL` is set correctly and the URL is publicly accessible (required for OAuth callbacks).
2. Check that the user's PDS authorization server is reachable.
3. Verify `SESSION_SECRET` hasn't changed since sessions were created (changing it invalidates all existing dashboard sessions).
4. Check server logs for OAuth-specific error messages.

## Third-party app can't authenticate

**Symptom**: A third-party app using DPoP authentication gets 401 errors on XRPC endpoints.

**Causes**:

- The app hasn't registered an API client. Every XRPC request needs an `X-Client-Key` header with a valid `hvc_`-prefixed client key. Register one via **Settings > API Clients** or `POST /admin/api-clients`.
- The DPoP proof is malformed or expired. Proofs include a timestamp and are valid for a short window.
- The API client has been deactivated (`is_active: false`). Re-enable it via the dashboard or `PUT /admin/api-clients/{id}`.

## Database connection errors

**Symptom**: HappyView fails to start or returns 500 errors.

**Causes**:

- `DATABASE_URL` is not set or points to an unreachable Postgres instance.
- The database user doesn't have sufficient permissions. HappyView needs to create tables (migrations run automatically on startup).
- Postgres version is too old. HappyView requires Postgres 17+.

See [Configuration](../getting-started/configuration.md) for environment variable details.

## Switching databases loses data

**Symptom**: After changing `DATABASE_URL` from SQLite to Postgres (or vice versa), all records, lexicons, and users are gone.

**Explanation**: Each database is independent. Switching `DATABASE_URL` points HappyView at a fresh database. Your old data is still in the previous database file or Postgres instance.

**Recovery**: Re-upload your lexicons and run backfills to re-index records from the network. Admin settings, users, and API keys need to be re-created manually. See the [SQLite → Postgres](../guides/database/sqlite-to-postgres-migration.md) or [Postgres → SQLite](../guides/database/postgres-to-sqlite-migration.md) migration guides.

## Jetstream disconnects frequently

**Symptom**: Server logs show repeated `jetstream.disconnected` / `jetstream.connected` events.

**Causes**:

- Network instability between HappyView and the Jetstream server. Verify `JETSTREAM_URL` is reachable.
- The default Jetstream instance may be under heavy load. Consider pointing `JETSTREAM_URL` at a different instance if available.
- HappyView reconnects automatically and resumes from its last cursor, so brief disconnections don't cause data loss. Prolonged outages may require a backfill to catch up on missed records.

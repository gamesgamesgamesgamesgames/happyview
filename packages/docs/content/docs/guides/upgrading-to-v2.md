---
title: "Migrating from v1"
---

v2 consolidates HappyView, [Tap](https://github.com/bluesky-social/indigo/tree/main/cmd/tap), and [AIP](https://tangled.org/gamesgamesgamesgames.games/aip) into a single binary. Real-time indexing, backfill, and OAuth are now built in — there are no companion services to deploy.

This guide covers every breaking change and the steps to migrate.

## Architecture changes

| v1 | v2 |
|----|-----|
| HappyView + Tap + AIP (3 services) | Single HappyView binary |
| Postgres only | SQLite (default) or Postgres |
| AIP handles OAuth | Built-in atproto OAuth with DPoP |
| Tap handles indexing + backfill | Built-in Jetstream streaming + backfill |
| Offset-based pagination | Cursor-based pagination |
| Admin bootstrapping via config | First authenticated user becomes admin |

## 1. Remove Tap and AIP

Tap and AIP are no longer needed. Remove them from your `docker-compose.yml` (or equivalent) and delete any associated containers/volumes.

**Environment variables to remove:**

| Variable | Reason |
|----------|--------|
| `AIP_URL` | OAuth is now built in |
| `AIP_PUBLIC_URL` | No longer needed |
| `TAP_URL` | Indexing is now built in |
| `TAP_ADMIN_PASSWORD` | No longer needed |
| `TAP_DATABASE_URL` | Tap no longer exists |
| `TAP_RELAY_URL` | Use `RELAY_URL` on HappyView directly |
| `TAP_PLC_URL` | Use `PLC_URL` on HappyView directly |
| `TAP_COLLECTION_FILTERS` | Collection filtering now uses lexicon config |
| `TAP_SIGNAL_COLLECTIONS` | Collection filtering now uses lexicon config |

## 2. Update environment variables

**New required variables:**

| Variable | Description |
|----------|-------------|
| `PUBLIC_URL` | Public-facing URL (e.g. `https://happyview.example.com`). Used for OAuth callbacks |
| `SESSION_SECRET` | Secret key for signing session cookies (at least 64 chars). **Must be set in production** |

**New optional variables:**

| Variable | Default | Description |
|----------|---------|-------------|
| `DATABASE_BACKEND` | auto-detected | Force `sqlite` or `postgres` |
| `JETSTREAM_URL` | `wss://jetstream1.us-east.bsky.network` | Replaces Tap's Jetstream connection |
| `STATIC_DIR` | `./web/out` | Dashboard static assets directory |
| `TOKEN_ENCRYPTION_KEY` | --- | Base64-encoded 32-byte key for encrypting stored OAuth tokens. **Strongly recommended in production** |
| `DEFAULT_RATE_LIMIT_CAPACITY` | `100` | Default token bucket capacity for new API clients |
| `DEFAULT_RATE_LIMIT_REFILL_RATE` | `2.0` | Default refill rate (tokens/sec) for new API clients |
| `APP_NAME` | --- | App name shown on OAuth screens |
| `LOGO_URI` | --- | Logo URL for OAuth screens |
| `TOS_URI` | --- | Terms of service URL |
| `POLICY_URI` | --- | Privacy policy URL |

**Unchanged variables:** `DATABASE_URL`, `HOST`, `PORT`, `RELAY_URL`, `PLC_URL`, `EVENT_LOG_RETENTION_DAYS`, `RUST_LOG`.

See [Configuration](../getting-started/configuration.md) for the full reference.

## 3. Choose your database

v2 defaults to SQLite. If you're running Postgres in v1, you have two options:

**Keep Postgres** — no changes needed. Your `DATABASE_URL` stays the same and v2 auto-detects the backend from the connection string.

**Migrate to SQLite** — follow the [Postgres to SQLite migration guide](database/postgres-to-sqlite-migration.md). SQLite is simpler to operate (no separate database server) and is the recommended default for most deployments.

## 4. Update Lua scripts

### Cursor-based pagination (breaking change)

`db.query` no longer supports `offset`. Replace offset-based pagination with cursors:

**Before (v1):**

```lua
local result = db.query({
  collection = collection,
  limit = limit,
  offset = page * limit,
})
```

**After (v2):**

```lua
local result = db.query({
  collection = collection,
  limit = limit,
  cursor = params.cursor,
})

-- result.cursor is an opaque string; pass it back as ?cursor= for the next page
```

Clients should pass the `cursor` value from the response as a query parameter to fetch the next page. Don't parse or construct cursors — they're opaque.

### New APIs available

v2 adds several new Lua APIs that you can optionally adopt:

- [`atproto.resolve_service_endpoint`](../api-reference/lua/libraries.md) — resolve a DID to its PDS endpoint
- [`atproto.get_labels`](../api-reference/lua/libraries.md) / [`atproto.get_labels_batch`](../api-reference/lua/libraries.md) — fetch content labels from subscribed labelers
- [`os.time`](../api-reference/lua/standard-libraries.md), `os.date`, `os.difftime`, `os.clock` — safe `os` subset

## 5. Update API key prefixes

v1 API keys used the `hv_` prefix. v2 keeps existing `hv_` keys working but new keys use the `hv_` prefix as well. No migration needed.

v2 also adds **API clients** for third-party OAuth apps, which use the `hvc_` prefix. These are separate from API keys — see the [API Clients guide](api-clients.md).

## 6. Update the dashboard URL

The dashboard has moved from the root path to `/dashboard`:

| v1 | v2 |
|----|-----|
| `/` | `/dashboard` |
| `/lexicons` | `/dashboard/lexicons` |
| `/records` | `/dashboard/records` |
| `/settings` | `/dashboard/settings` |

Update any bookmarks or internal links.

## 7. User permissions

v2 introduces granular user permissions. After upgrading:

1. The first user to authenticate becomes the **super user** (full access).
2. Additional users are created with no permissions by default.
3. Assign permissions or use a template (Viewer, Operator, Manager, Full Access).

See the [Permissions guide](permissions.md) for details.

## 8. Docker Compose (example)

**Before (v1):**

```yaml
services:
  postgres:
    image: postgres:17
    # ...

  tap:
    image: ghcr.io/bluesky-social/indigo/tap:latest
    environment:
      TAP_DATABASE_URL: postgres://...
      TAP_RELAY_URL: https://bsky.network
      TAP_ADMIN_PASSWORD: secret
    depends_on: [postgres]

  happyview:
    image: ghcr.io/gamesgamesgamesgamesgames/happyview:latest
    environment:
      DATABASE_URL: postgres://...
      AIP_URL: http://aip:8080
      TAP_URL: http://tap:2480
      TAP_ADMIN_PASSWORD: secret
    depends_on: [postgres, tap]
```

**After (v2):**

```yaml
services:
  happyview:
    image: ghcr.io/gamesgamesgamesgamesgames/happyview:latest
    environment:
      DATABASE_URL: sqlite://data/happyview.db?mode=rwc
      PUBLIC_URL: https://happyview.example.com
      SESSION_SECRET: your-64-char-secret
    volumes:
      - data:/app/data

volumes:
  data:
```

Or with Postgres:

```yaml
services:
  postgres:
    image: postgres:17
    # ...

  happyview:
    image: ghcr.io/gamesgamesgamesgamesgames/happyview:latest
    environment:
      DATABASE_URL: postgres://happyview:happyview@postgres/happyview
      PUBLIC_URL: https://happyview.example.com
      SESSION_SECRET: your-64-char-secret
    depends_on: [postgres]
```

## Checklist

- [ ] Remove Tap and AIP services
- [ ] Remove old environment variables (`AIP_URL`, `TAP_URL`, `TAP_ADMIN_PASSWORD`, etc.)
- [ ] Add `PUBLIC_URL` and `SESSION_SECRET`
- [ ] Add `TOKEN_ENCRYPTION_KEY` (recommended for production)
- [ ] Decide on SQLite (default) or keep Postgres
- [ ] Update Lua scripts to use cursor-based pagination instead of offsets
- [ ] Update any bookmarks/links to use `/dashboard` prefix
- [ ] Set up user permissions after first login

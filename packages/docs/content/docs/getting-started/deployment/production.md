---
title: "Production"
---

This page covers what to change when taking a HappyView instance from local development to production. For setup instructions, see [Deployment](railway.md). This page assumes you already have a working deployment and focuses on hardening and operational concerns.

## Session secret

Set `SESSION_SECRET` to a random string of at least 64 characters. This signs the session cookies issued during OAuth login; rotating it invalidates every existing session.

```sh
openssl rand -base64 48
```

Never commit the secret to source control. Store it in your platform's secret manager (Railway variables, Docker secrets, Kubernetes secrets, etc.).

## Token encryption key

If you use [plugins](../../guides/plugins.md) that require secrets (API keys, OAuth credentials), set `TOKEN_ENCRYPTION_KEY` to a base64-encoded 32-byte key. This encrypts plugin secrets at rest using AES-256-GCM:

```sh
openssl rand -base64 32
```

Without this variable, the dashboard's plugin secret fields are disabled and plugins can only read secrets from environment variables.

## TLS and `PUBLIC_URL`

HappyView does not terminate TLS. Put it behind a reverse proxy (nginx, Caddy, Cloudflare Tunnel, a platform-managed load balancer) and set `PUBLIC_URL` to the public HTTPS URL:

```sh
PUBLIC_URL=https://happyview.example.com
```

`PUBLIC_URL` is used to construct OAuth redirect URIs, so it must exactly match the URL users hit — including scheme. A mismatch breaks OAuth login.

## Reverse proxy subpath

If you need HappyView to share a domain with other services, set `BASE_PATH` to mount it at a subpath. For example, to serve the dashboard at `https://example.com/hv/`:

```sh
PUBLIC_URL=https://example.com
BASE_PATH=/hv
```

`PUBLIC_URL` should **not** include the base path — HappyView appends it automatically when constructing OAuth callbacks and other external URLs.

The base path is applied at container startup without rebuilding the image, so prebuilt Docker images (including Railway deployments) work with any subpath.

### XRPC endpoints

atproto clients expect XRPC endpoints at `/xrpc/*` on the domain root. When using a base path, configure your reverse proxy to rewrite `/xrpc/*` requests so they reach HappyView under its base path. Here's an example using Caddy:

```
example.com {
    # Dashboard and API under the base path
    handle /hv/* {
        reverse_proxy happyview:3000
    }

    # Redirect bare /hv to /hv/ for cleaner URLs
    handle /hv {
        redir /hv/ permanent
    }

    # atproto XRPC — rewrite to base path before proxying
    handle /xrpc/* {
        rewrite * /hv{uri}
        reverse_proxy happyview:3000
    }

    # Everything else goes to another service
    handle {
        reverse_proxy other-app:3001
    }
}
```

The `rewrite * /hv{uri}` directive prepends `/hv` to the request path before proxying, so `/xrpc/com.atproto.sync.getRecord` becomes `/hv/xrpc/com.atproto.sync.getRecord` — which matches HappyView's nested routes.

### Health checks with a base path

`GET /health` is always served at the domain root, even when `BASE_PATH` is set. This keeps load balancer probes working without routing changes.

## Database

SQLite is fine for small to medium instances and is the default. Switch to Postgres if you need:

- Multiple HappyView replicas sharing one database
- Larger-than-memory working sets
- External tools that need direct read access to the records table

See the [database setup guide](../../guides/database/database-setup.md) for configuration details and [Postgres → SQLite migration](../../guides/database/postgres-to-sqlite-migration.md) if you're moving the other direction. Migrations run automatically on startup regardless of backend.

## Rate limits

HappyView has a per-client token-bucket rate limiter for XRPC endpoints. The defaults (set via `DEFAULT_RATE_LIMIT_CAPACITY` and `DEFAULT_RATE_LIMIT_REFILL_RATE`) apply to any [API client](../../guides/api-keys.md) that doesn't have per-client overrides. Raise the defaults cautiously — they exist so one misbehaving integrator can't saturate the server.

Per-client overrides are set at client creation or via `PUT /admin/api-clients/{id}` (see [Admin API — API Clients](../../api-reference/admin/api-clients.md)).

## Logging

The default `RUST_LOG` setting (`happyview=debug,tower_http=debug`) is noisy. For production, drop the verbosity:

```sh
RUST_LOG=happyview=info,tower_http=info
```

Structured logs go to stdout, so any platform that captures container stdout (Railway, Fly, ECS, Kubernetes) will ingest them without further configuration. For retention and querying, ship stdout to your usual log aggregator.

## Event log retention

The admin [event log](../../guides/event-logs.md) is stored in the same database as records. `EVENT_LOG_RETENTION_DAYS` (default `30`) controls automatic cleanup. Set to `0` to keep events indefinitely — useful for compliance-sensitive deployments, but plan for database growth.

## Health checks

`GET /health` returns `200 ok` when HappyView can bind its HTTP listener. Use it as the readiness/liveness probe for your platform.

For a deeper check, hit `GET /xrpc/com.atproto.server.describeServer` — this exercises the database and lexicon registry, and only returns `200` if HappyView can actually serve requests.

## Backups

- **SQLite**: back up the database file (e.g. `data/happyview.db`) plus its `-wal` and `-shm` sidecar files. Use `sqlite3 happyview.db ".backup '/path/backup.db'"` for a consistent snapshot while HappyView is running.
- **Postgres**: standard `pg_dump` / managed-Postgres snapshots.

Most of what HappyView stores is derivable from the network — lost records can be re-indexed via [backfill](../../guides/backfill.md). You can't recover from the network: user accounts and permissions, API keys, API clients, plugin secrets, and the Jetstream cursor. Prioritize those in your backup plan.

## Next steps

- [Configuration](../configuration.md) — full environment variable reference
- [Permissions](../../guides/permissions.md) — lock down admin access before exposing the dashboard publicly
- [Troubleshooting](../../reference/troubleshooting.md) — diagnose issues with a running instance

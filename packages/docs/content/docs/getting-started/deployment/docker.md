---
title: "Docker"
---

This guide deploys HappyView from the prebuilt images published to GitHub Container Registry, using one of the two production Compose files in the repository. Nothing is compiled locally — the image already contains the server and the dashboard.

If you want to run the repository's development stack with hot reloading, see [Local Development](local-development.md) instead. To run the server directly with `cargo run`, see [From Source](other.md).

## Prerequisites

- [Docker](https://docs.docker.com/get-docker/) and Docker Compose
- A domain and a reverse proxy that terminates TLS — HappyView does not. See [step 4](#4-put-a-reverse-proxy-in-front)

## The image

```
ghcr.io/gamesgamesgamesgamesgames/happyview
```

Multi-arch manifests are published for `linux/amd64` and `linux/arm64` on every release.

| Tag           | Moves                            | Use for                    |
| ------------- | -------------------------------- | -------------------------- |
| `latest`      | Every stable release             | Trying HappyView out       |
| `2.13.0`      | Never                            | Production                 |
| `2.13`        | Every stable patch in that minor | Automatic patch updates    |
| `2`           | Every stable minor in that major | Automatic minor updates    |
| `sha-abc1234` | Never                            | Pinning to an exact commit |

Prereleases cut from the `dev` branch (e.g. `2.13.0-dev.1`) are published under their full version only. They never move `latest`, `2.13`, or `2`.

<Callout type="idea" title="Pin a version in production">
Every Compose file below reads `HAPPYVIEW_VERSION` and defaults to `latest`. Set it to an exact version so a redeploy can't pull a different build than the one you tested.
</Callout>

## 1. Choose a database

Pick one Compose file. Both live at the repository root:

| File                               | Database | Best for                                                                                                                              |
| ---------------------------------- | -------- | ------------------------------------------------------------------------------------------------------------------------------------- |
| `docker-compose.prod.sqlite.yml`   | SQLite   | Small to medium instances. One container, one volume, no database to operate                                                          |
| `docker-compose.prod.postgres.yml` | Postgres | Multiple HappyView replicas sharing a database, larger-than-memory working sets, or external tools reading the records table directly |

See the [database setup guide](../../guides/database/database-setup.md) for the full comparison. Migrations run automatically on startup on either backend, and both directions are [migratable](../../guides/database/sqlite-to-postgres-migration.md) later.

Make a directory for the deployment and download the one you picked as `docker-compose.yml`, so `docker compose` finds it without a `-f` flag:

```sh
mkdir -p happyview && cd happyview
```

```sh
# SQLite
curl -o docker-compose.yml https://raw.githubusercontent.com/gamesgamesgamesgamesgames/happyview/main/docker-compose.prod.sqlite.yml
```

```sh
# Postgres
curl -o docker-compose.yml https://raw.githubusercontent.com/gamesgamesgamesgamesgames/happyview/main/docker-compose.prod.postgres.yml
```

## 2. Generate secrets

Create a `.env` next to it. Compose auto-loads that filename, so these need no flag either:

```sh
cat > .env <<EOF
PUBLIC_URL=https://happyview.example.com
SESSION_SECRET=$(openssl rand -base64 48)
TOKEN_ENCRYPTION_KEY=$(openssl rand -base64 32)
HAPPYVIEW_VERSION=latest
EOF
```

For the Postgres stack, add a database password as well:

```sh
echo "POSTGRES_PASSWORD=$(openssl rand -hex 32)" >> .env
```

Compose refuses to start without these:

- **`PUBLIC_URL`** — the public HTTPS URL users actually hit, scheme included. It's used to build OAuth redirect URIs, so a mismatch breaks login. Do **not** include `BASE_PATH` here.
- **`SESSION_SECRET`** — signs the dashboard session cookie. An unset or too-short value doesn't stop the server booting; it silently disables cookie login, which is why the Compose files require it explicitly.
- **`TOKEN_ENCRYPTION_KEY`** — the AES-256-GCM key protecting OAuth tokens, DPoP private keys, and plugin secrets at rest. Without it, DPoP sessions, [spaces](../../experimental/spaces/index.md), and service identity are disabled. Rotating it makes everything already encrypted unreadable.

<Callout type="warn" title="Deploying from a clone of the repository instead">
The steps above assume a directory containing nothing but the Compose file and your `.env`, which is why the plain filenames are safe. In a clone of the repository they are not: a development `.env` left beside the Compose files is auto-loaded just the same, and will quietly supply these values — including the `POSTGRES_USER`/`POSTGRES_PASSWORD`/`POSTGRES_DB` triplet that `.env.example` ships. If you deploy from a clone, keep production values in their own file and name both explicitly on every command:

```sh
docker compose --env-file .env.prod -f docker-compose.prod.sqlite.yml up -d
```

</Callout>

<Callout type="warn" title="Percent-encode Postgres passwords">
`POSTGRES_PASSWORD` is interpolated into a connection URL. A password containing `@`, `/`, `:`, or `#` truncates it, because those are URL delimiters. `openssl rand -hex 32` avoids the problem entirely.
</Callout>

## 3. Start the stack

```sh
docker compose up -d
```

HappyView runs its migrations on first boot, then starts serving on port 3000 inside the container.

Watch it come up:

```sh
docker compose logs -f
```

## 4. Put a reverse proxy in front

HappyView does not terminate TLS, and both Compose files publish to **loopback only** by default (`127.0.0.1:3000`) — the proxy in front is what should be exposed.

### Using the bundled Caddy service

Each Compose file ends with a commented-out `caddy` service that handles TLS with certificates obtained and renewed automatically. Uncomment it, along with the `caddy-data` and `caddy-config` entries in the `volumes` block at the bottom, then create a `Caddyfile` beside the Compose file:

```
{$CADDY_DOMAIN} {
    reverse_proxy happyview:3000
}
```

Then, in `.env` and the Compose file:

- Add `CADDY_DOMAIN` to `.env` — the hostname from `PUBLIC_URL` with the scheme stripped, so `happyview.example.com` for `https://happyview.example.com`. `PUBLIC_URL` itself stays the full URL.
- Delete the `ports` block from the `happyview` service. Caddy reaches it over the Compose network, so publishing it to the host as well only widens the exposure. Leave `HTTP_BIND` unset.

Point DNS at the host _before_ the first `up`. Caddy requests a certificate on startup, and a challenge for a hostname that doesn't resolve to this host fails and retries with backoff.

<Callout type="warn" title="Port 80 has to stay open">
It serves the HTTP→HTTPS redirect *and* the ACME HTTP-01 challenge — which is also how renewal works. Closing it once the first certificate has issued makes renewal fail silently about 60 days later. DNS-01, the usual alternative, isn't available here: the stock `caddy` image ships no DNS provider modules, and adding one means building a custom image with xcaddy.
</Callout>

<Callout type="error" title="The certificate volume must persist">
`caddy-data` holds the issued certificates and the ACME account key. Without it, every recreate re-issues from scratch. Let's Encrypt allows only 5 duplicate certificates per week before it starts refusing, which locks the site out of HTTPS for days.
</Callout>

### Other options

- **A proxy on the host** — leave the default `ports` block and point nginx, Caddy, or Traefik at `127.0.0.1:3000`
- **Cloudflare Tunnel** — no inbound port needs opening at all. See [Local Development](local-development.md#exposing-your-instance-with-a-cloudflare-tunnel) for how the tunnel is wired up, then set `PUBLIC_URL` to the tunnel's hostname
- **`HTTP_BIND=0.0.0.0:3000`** — publishes on all interfaces. Only do this if something else is firewalling the port, since `ports` bypasses the host firewall on most Docker installs

Whatever you use, `PUBLIC_URL` must match the resulting public URL exactly. To serve HappyView at a subpath alongside other services, see [Reverse proxy subpath](production.md#reverse-proxy-subpath). `BASE_PATH` is applied at container start, so prebuilt images work at any subpath without a rebuild.

## 5. Log in

Open `PUBLIC_URL` in a browser. The image serves the dashboard at the root, and the first handle to authenticate on a fresh instance is automatically bootstrapped as the [super user](../../guides/permissions.md) with all permissions — so use the handle you want to own the instance.

From there, follow the [Quickstart](../quickstart.md) from step 3 to add your first lexicon.

## Configuration

Beyond the required secrets, both Compose files expose these with production defaults. Override them in `.env`.

| Variable                         | Default                                    | Notes                                                           |
| -------------------------------- | ------------------------------------------ | --------------------------------------------------------------- |
| `HAPPYVIEW_VERSION`              | `latest`                                   | Image tag to deploy                                             |
| `HTTP_BIND`                      | `127.0.0.1:3000`                           | Host address the container publishes on                         |
| `BASE_PATH`                      | _(none)_                                   | Subpath prefix, e.g. `/hv`. `PUBLIC_URL` must not include it    |
| `RUST_LOG`                       | `happyview=info,tower_http=info,sqlx=warn` | The dev default is very noisy in production                     |
| `JETSTREAM_URL`                  | `wss://jetstream1.us-east.bsky.network`    | Real-time record stream                                         |
| `RELAY_URL`                      | `https://bsky.network`                     | Used for [backfill](../../guides/backfill.md) repo discovery    |
| `PLC_URL`                        | `https://plc.directory`                    | DID resolution                                                  |
| `EVENT_LOG_RETENTION_DAYS`       | `30`                                       | `0` keeps [event logs](../../guides/event-logs.md) indefinitely |
| `DEFAULT_RATE_LIMIT_CAPACITY`    | `100`                                      | Per-client token bucket capacity                                |
| `DEFAULT_RATE_LIMIT_REFILL_RATE` | `2.0`                                      | Tokens per second                                               |

SQLite stack only:

| Variable                    | Default             | Notes                                                                                                                                          |
| --------------------------- | ------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| `SQLITE_JOURNAL_SIZE_LIMIT` | `67108864` (64 MiB) | Caps the `-wal` file after a checkpoint. Deleting rows grows the WAL for the duration of the delete, so this bounds a delete's peak disk usage |

Postgres stack only:

| Variable                   | Default     | Notes                                         |
| -------------------------- | ----------- | --------------------------------------------- |
| `POSTGRES_VERSION`         | `17`        | Image tag for the `postgres` service          |
| `POSTGRES_USER`            | `happyview` |                                               |
| `POSTGRES_DB`              | `happyview` |                                               |
| `POSTGRES_MAX_CONNECTIONS` | `200`       | Raised from the stock 100; see the note below |
| `DATABASE_MAX_CONNECTIONS` | `32`        | Main pool ceiling                             |

The full environment variable reference is in [Configuration](../configuration.md). A few that aren't wired into the Compose files — `ATTESTATION_PRIVATE_KEY`, `APP_NAME`, `LOGO_URI`, `TOS_URI`, `POLICY_URI`, and the `BACKFILL_CONCURRENT_*` tuning knobs — are present as commented-out lines you can uncomment.

<Callout type="warn" title="Postgres connection limits">
HappyView opens two pools: the main one (`DATABASE_MAX_CONNECTIONS`) and a separate backfill pool sized `(BACKFILL_CONCURRENT_PDS × BACKFILL_CONCURRENT_DIDS_PER_PDS) + BACKFILL_CONCURRENT_RESOLUTION + 4`, which is 134 on the defaults. Both are lazy, so an idle instance holds almost nothing, but a running backfill peaks near 166, and Postgres's stock limit of 100 would fail it with `sorry, too many clients already`. Raise `POSTGRES_MAX_CONNECTIONS` further if you raise the backfill concurrency or add replicas.
</Callout>

<Callout type="error" title="Don't override the SQLite DATABASE_URL through the environment">
The SQLite stack sets `DATABASE_URL` to the literal absolute path `sqlite:///data/happyview.db?mode=rwc`, deliberately not interpolated. A relative path (as in `.env.example`) resolves against the image's `/app` working directory, which is the container's writable layer. The database would live outside the volume and vanish on the next `up`. Change the path in the Compose file, not through the environment.
</Callout>

## Operations

### Upgrading

Bump `HAPPYVIEW_VERSION` in `.env` (or leave it on a moving tag), then:

```sh
docker compose pull
docker compose up -d
```

Migrations run automatically on the new container's first boot. Back up the volume first; see [Backups](production.md#backups). If you're coming from HappyView 1.x, read [Upgrading to v2](../../guides/upgrading-to-v2.md) first.

### Health checks

Both stacks ship a container healthcheck. The runtime image carries no `curl` or `wget`, so it probes `/health` over bash's `/dev/tcp`. The SQLite stack allows a 120-second start period, because a `VACUUM` scheduled from the dashboard runs at boot before the server binds. The Postgres stack uses 60 seconds, which covers migrations on a first boot. Scheduling a vacuum is rejected there, so there's nothing longer to wait out.

<Callout type="warn" title="A scheduled SQLite VACUUM takes the instance offline">
Reclaiming SQLite disk space is scheduled from **Settings > Database** in the dashboard and runs on the next boot. `VACUUM` rebuilds the database into a new file, and it runs before the server starts listening, so on a large database that's minutes to hours during which nothing is serving. Behind a container healthcheck this can get the process killed mid-rebuild, which the next boot then reports as a crashed attempt. Raise or disable the healthcheck for that one restart.
</Callout>

For an external probe, `GET /health` returns `200 ok` once HappyView can bind its listener, and stays at the domain root even when `BASE_PATH` is set. For a deeper check that exercises the database and lexicon registry, use `GET /xrpc/com.atproto.server.describeServer`.

### Stopping and restarting

Both stacks set `init: true`. The entrypoint `exec`s the server, making it PID 1, where the kernel drops signals that have only their default disposition, and the server installs no `SIGTERM` handler. Without an init, every `stop`, `restart`, and `down` would hang for the full grace period and end in `SIGKILL` (exit 137). `init: true` puts tini at PID 1 to forward the signal so the server exits promptly.

This is _prompt_, not _graceful_: there is no graceful-shutdown handler, so in-flight requests are dropped either way, and an interrupted [job](../../guides/background-jobs.md) is re-queued on the next boot.

The Postgres service additionally uses `stop_signal: SIGINT`. Docker's default `SIGTERM` is a Postgres _smart_ shutdown, which waits for every client to disconnect, so it would hang for the full grace period and take a `SIGKILL`, leaving the next boot to run crash recovery. `SIGINT` is the fast shutdown: roll back open transactions, checkpoint, exit.

### Logs

Container stdout is the only log sink, capped at 5 × 10 MB per service so `json-file` logs can't grow unbounded. Ship stdout to your usual aggregator if you need retention.

### Backups

The SQLite stack keeps everything in the `happyview-data` volume; the Postgres stack in `pgdata`. Those are the only stateful paths. See [Backups](production.md#backups) for what is and isn't recoverable from the network: most records can be re-indexed via [backfill](../../guides/backfill.md), but user accounts, permissions, API keys, plugin secrets, and the Jetstream cursor cannot.

## Next steps

- [Production](production.md) — the full hardening and operations checklist
- [Hardening](../../guides/hardening.md) — locking down a public instance
- [Configuration](../configuration.md) — every environment variable
- [Statusphere tutorial](../../tutorials/statusphere.md) — upload lexicons, add query logic, index records

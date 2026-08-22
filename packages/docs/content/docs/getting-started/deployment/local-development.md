---
title: "Local Development"
---

This guide runs the full HappyView stack locally with hot reloading, using the `docker-compose.yml` at the repository root. The Rust server rebuilds on save via `cargo watch`, and the dashboard runs the Next.js dev server.

This is not the production path — nothing here uses the published image. To deploy HappyView from prebuilt images, see [Docker](docker.md). To run the server directly on your machine without containers, see [From Source](other.md).

## Prerequisites

- [Docker](https://docs.docker.com/get-docker/) and Docker Compose

## 1. Clone and configure

```sh
git clone git@github.com:gamesgamesgamesgamesgames/happyview
cd happyview
cp .env.example .env
```

Set `SESSION_SECRET` in `.env` to a random value — `openssl rand -base64 48` works. The defaults cover everything else. See [Configuration](../configuration.md) for the full list.

## 2. Start the stack

```sh
docker compose up
```

The first run takes several minutes: it compiles the Rust workspace from scratch inside the container and installs `cargo-watch`. Subsequent runs reuse the cached `cargo-target` volume, so only changed crates rebuild.

Four services come up:

| Service       | Port   | Description                                                                 |
| ------------- | ------ | --------------------------------------------------------------------------- |
| **caddy**     | 3080   | Unified origin — routes to the backend or the dashboard                    |
| **happyview** | 3000   | Rust API server, rebuilt on save by `cargo watch`                           |
| **web**       | 3001   | Next.js dashboard dev server, hot-reloads on changes to `web/`              |
| **tunnel**    | ---    | `cloudflared`, giving the stack a public HTTPS URL                          |

<Callout type="info" title="Use :3080, not :3000 or :3001">
`caddy` is what stitches the two halves into one origin, and it's what the tunnel points at. `scripts/dev-Caddyfile` sends `/api`, `/admin`, `/auth`, `/xrpc`, `/oauth`, `/external-auth`, `/.well-known`, `/health`, `/config`, and `/settings` to the Rust server, and everything else to Next.js. Hitting the two ports separately splits your session cookie across origins and skips that routing.
</Callout>

Port 3000 does serve a dashboard, but not a useful one in this stack. The dev `happyview` service sets no `STATIC_DIR`, so the server falls back to its default of `./web/out` — which, under the source bind-mount, is whatever your last `npm run build` left on your host. That directory is gitignored and usually stale or missing. Use `:3080` (or the tunnel URL) for the dashboard.

## Exposing your instance with a Cloudflare Tunnel

The `tunnel` service runs `cloudflared` against `http://caddy:80` and writes the resulting URL to a shared volume. `scripts/entrypoint.sh` in the `happyview` service reads that file and **exports it as `PUBLIC_URL`**, overriding whatever you set in `.env`.

This exists because atproto OAuth needs a publicly reachable HTTPS URL. With a tunnel, your local instance gets one, so you can sign in with a real account and test flows that a loopback URL can't reach.

<Callout type="warn" title="The tunnel overrides PUBLIC_URL">
`docker-compose.yml` sets `TUNNEL_URL_FILE` on the `happyview` service unconditionally, so the entrypoint always waits for a tunnel URL — up to 30 seconds — before starting the server. The `PUBLIC_URL` in your `.env` is only used as the fallback if no URL appears in that window, which is why a fresh stack reports a `trycloudflare.com` address.
</Callout>

### Quick tunnel (default)

Leave `CLOUDFLARE_TUNNEL_TOKEN` blank and you get a free ephemeral tunnel — no Cloudflare account needed. Find the URL in the logs:

```sh
docker compose logs tunnel | grep trycloudflare
```

The hostname is random and **changes on every restart**, so anything that pins your instance's URL (an OAuth client registration, a lexicon's service entry) has to be updated each time.

### Named tunnel (stable hostname)

For a hostname that survives restarts, create a tunnel in the Cloudflare dashboard and set both variables in `.env`:

```sh
CLOUDFLARE_TUNNEL_TOKEN=<token from the Cloudflare dashboard>
TUNNEL_HOSTNAME=happyview-dev.example.com
```

The token starts the named tunnel, but `TUNNEL_HOSTNAME` is what actually gets written to the shared URL file. Set the token alone and nothing is written, so the server waits the full 30 seconds and then falls back to the `PUBLIC_URL` in your `.env`. That works, but only if you've already set it to match the tunnel's hostname.

### Running without a tunnel

Start only the three local services:

```sh
docker compose up happyview web caddy
```

The server still waits 30 seconds for a URL that never arrives, then falls back to your `.env` `PUBLIC_URL`. To skip the wait, comment out the `TUNNEL_URL_FILE` line in the `happyview` service.

<Callout type="warn" title="Use 127.0.0.1, not localhost">
Without a tunnel you're an atproto OAuth loopback client, and those are registered with `127.0.0.1`. Setting `PUBLIC_URL` to `http://localhost:3000` makes OAuth sign-in fail, because the redirect URI won't match the loopback client ID.
</Callout>

## Using Postgres instead of SQLite

SQLite is the default and needs no extra container. To use Postgres, uncomment the `postgres` service in `docker-compose.yml`, its `pgdata` volume, and the `depends_on` block on the `happyview` service, then point `DATABASE_URL` at it in `.env`:

```sh
DATABASE_URL=postgres://happyview:happyview@postgres/happyview
POSTGRES_USER=happyview
POSTGRES_PASSWORD=happyview
POSTGRES_DB=happyview
```

See the [database setup guide](../../guides/database/database-setup.md) for the differences between backends.

## The other Compose files

The repository root holds five Compose files, each declaring its own project name so they can run simultaneously without sharing containers, networks, or volumes:

| File                                | Project name             | Purpose                                        |
| ----------------------------------- | ------------------------ | ---------------------------------------------- |
| `docker-compose.yml`                | `happyview`              | This dev stack                                 |
| `docker-compose.test.yml`           | `happyview-test`         | Postgres for the Rust integration tests         |
| `docker-compose.e2e.yml`            | `happyview-e2e`          | Full stack plus a local PLC directory and PDS for Playwright |
| `docker-compose.prod.sqlite.yml`    | `happyview-prod-sqlite`  | [Production, SQLite](docker.md)                |
| `docker-compose.prod.postgres.yml`  | `happyview-prod-postgres`| [Production, Postgres](docker.md)              |

<Callout type="info" title="Containers named plain happyview-postgres-1 are orphans">
The explicit project names are recent. Before them, every file defaulted to the directory name and shared containers, so a `down` in one stack tore down the others, including a running dev stack. Containers left over from before that change are orphans and can be removed.
</Callout>

## Next steps

- [Statusphere tutorial](../../tutorials/statusphere.md) — upload lexicons, add query logic, index records from the network
- [Configuration](../configuration.md) — every environment variable
- [Lua Scripting](../../guides/lua-scripting.md) — custom query and procedure logic
- [Docker](docker.md) — deploying from published images

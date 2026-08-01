---
title: "Docker"
---

This guide runs HappyView and the dashboard locally using Docker Compose.

## Prerequisites

- [Docker](https://docs.docker.com/get-docker/) and Docker Compose

## 1. Clone and configure

```sh
git clone git@tangled.org:gamesgamesgamesgames.games/happyview
cd happyview
cp .env.example .env
```

Edit `.env` and set at least `PUBLIC_URL` (e.g. `http://127.0.0.1:3000`) and `SESSION_SECRET` (at least 64 characters). The defaults work for everything else. See [Configuration](../configuration.md) for the full list of environment variables.

<Callout type="warn" title="Use 127.0.0.1, not localhost">
atproto OAuth loopback clients are registered with `127.0.0.1`. If you set `PUBLIC_URL` to `http://localhost:3000`, OAuth sign-in will fail because the redirect URI won't match the loopback client ID. Always use `http://127.0.0.1:3000` for local development.
</Callout>

## 2. Start the stack

```sh
docker compose up
```

This starts:

| Service       | Port | Description          |
| ------------- | ---- | -------------------- |
| **happyview** | 3000 | HappyView API server |
| **web**       | 3001 | Next.js dashboard    |

HappyView runs migrations automatically on startup. The first build will take a few minutes while Rust compiles.

The `happyview` container serves its own bundled dashboard at `http://127.0.0.1:3000`, but that copy is baked in at container build time and only updates when you rebuild the image. For day-to-day development, use the dev dashboard at `http://127.0.0.1:3001` — it hot-reloads on changes to the `web/` source.

<Callout type="idea">
SQLite is the default and requires no extra services. To use Postgres instead, uncomment the `postgres` service in `docker-compose.yml` and update `DATABASE_URL` in `.env`. See the [database setup guide](../../guides/database/database-setup.md).
</Callout>

## Next steps

Your HappyView stack is running. Follow the [Statusphere tutorial](../../tutorials/statusphere.md) to upload lexicons, add custom query logic, and start indexing records from the network.

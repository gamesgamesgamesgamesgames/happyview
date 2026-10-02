---
title: "From Source"
---

This guide runs HappyView directly with `cargo run`. If you'd rather run the whole stack in containers, see [Local Development](local-development.md). To deploy from prebuilt images, see [Docker](docker.md).

## Prerequisites

- Rust (stable)
- (Optional) PostgreSQL 17+ if you prefer Postgres over the default SQLite

## 1. Clone and configure

```sh
git clone git@github.com:gamesgamesgamesgamesgames/happyview
cd happyview
cp .env.example .env
```

Edit `.env` to point at your running services:

```sh
# SQLite (default — no setup needed, file created automatically)
DATABASE_URL=sqlite://data/happyview.db?mode=rwc
PUBLIC_URL=http://127.0.0.1:3000
SESSION_SECRET=change-me-in-production
```

<Callout type="warn" title="Use 127.0.0.1, not localhost">
atproto OAuth loopback clients are registered with `127.0.0.1`. If you set `PUBLIC_URL` to `http://localhost:3000`, OAuth sign-in will fail because the redirect URI won't match the loopback client ID. Always use `http://127.0.0.1:3000` for local development.
</Callout>

Or if you prefer Postgres:

```sh
DATABASE_URL=postgres://happyview:happyview@localhost/happyview
```

See [Configuration](../configuration.md) for all available variables and the [database setup guide](../../guides/database/database-setup.md) for details on both backends.

## 2. Create the database (Postgres only)

If using SQLite, skip this step — HappyView creates the database file automatically.

If using Postgres:

```sh
createdb happyview
```

Or if using a Postgres user with a password:

```sh
psql -c "CREATE DATABASE happyview;" -U postgres
```

HappyView runs migrations automatically on startup, so no manual migration step is needed.

## 3. Start HappyView

```sh
cargo run
```

HappyView starts on port 3000 (configurable via the `PORT` environment variable).

## Next steps

Your HappyView instance is running. Follow the [Statusphere tutorial](../../tutorials/statusphere.md) to upload lexicons, add custom query logic, and start indexing records from the network.

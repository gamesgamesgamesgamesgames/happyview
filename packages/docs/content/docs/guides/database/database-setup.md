---
title: "Database Setup"
---

HappyView supports two database backends: **SQLite** (default) and **PostgreSQL**. The backend is auto-detected from your `DATABASE_URL` scheme, or you can set `DATABASE_BACKEND` explicitly.

## SQLite (default)

SQLite requires zero setup. HappyView creates the database file automatically on first startup.

```sh
DATABASE_URL=sqlite://data/happyview.db?mode=rwc
```

The `?mode=rwc` parameter tells SQLite to create the file if it does not exist. The path is relative to the working directory (or use an absolute path).

**When to use SQLite:**

- Getting started or local development
- Small to medium deployments
- Single-server setups where simplicity is preferred

## PostgreSQL (optional)

For larger deployments or when you need concurrent write scalability, use Postgres.

```sh
DATABASE_URL=postgres://happyview:happyview@localhost/happyview
```

You need to create the database before starting HappyView:

```sh
createdb happyview
```

HappyView runs migrations automatically on startup for both backends.

**When to use Postgres:**

- High write concurrency from many simultaneous users
- You need Postgres-specific features (e.g., advanced JSON queries in Lua scripts)
- You already have a Postgres infrastructure

## Environment variables

| Variable | Description |
|----------|-------------|
| `DATABASE_URL` | Connection string. `sqlite://...` for SQLite, `postgres://...` for Postgres |
| `DATABASE_BACKEND` | Optional. Force `sqlite` or `postgres`. Auto-detected from `DATABASE_URL` if not set |

## Docker Compose

The default `docker-compose.yml` ships with the Postgres service commented out. To use Postgres:

1. Uncomment the `postgres` service and `pgdata` volume in `docker-compose.yml`
2. Uncomment the `depends_on: postgres` block in the `happyview` service
3. Update `DATABASE_URL` in `.env`:
   ```sh
   DATABASE_URL=postgres://happyview:happyview@postgres/happyview
   ```
4. Set the Postgres credentials:
   ```sh
   POSTGRES_USER=happyview
   POSTGRES_PASSWORD=happyview
   POSTGRES_DB=happyview
   ```

## Lua scripts

Both backends support the same Lua libraries. A `happyview.db` chain is portable: the host generates the SQL for whichever backend is running. `happyview.sql`'s `raw` is not: the statement goes to the backend untranslated, so its placeholders (`?` on SQLite, `$1` on Postgres) and functions must be the running backend's. A script that has to run on both branches on `db.backend()`.

If you are migrating existing Lua scripts from Postgres SQL syntax to SQLite syntax, see the [Postgres to SQLite migration guide](postgres-to-sqlite-migration.md).

## Next steps

- [SQLite → Postgres migration](sqlite-to-postgres-migration.md) — switch an existing instance from SQLite to Postgres
- [Postgres → SQLite migration](postgres-to-sqlite-migration.md) — switch an existing instance from Postgres to SQLite
- [Lua scripting](../lua-scripting.md) — write queries that target either backend
- [Configuration](../../getting-started/configuration.md) — `DATABASE_URL` and related variables

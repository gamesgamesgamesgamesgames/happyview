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

## Migrations at startup

HappyView applies any pending migrations every time it starts, before it serves requests. Most take milliseconds. One that builds an index over `happyview_records` scales with that table, and so does the boot that applies it.

**SQLite** builds an index inside the migration's transaction, which holds the database's write lock. On a records table of several gigabytes, expect a boot that takes minutes rather than seconds, during which HappyView is not serving. Plan for free disk of roughly two to three times the size of the new index while it runs: the build sorts every row, spilling to temporary storage, and writes the index into the write-ahead log, which is copied into the database file at the next checkpoint before the log is truncated. Let it finish: stopping the process rolls the migration back, and the next boot starts the build over.

**Postgres** builds these indexes with `CREATE INDEX CONCURRENTLY`, which keeps reads and writes flowing while it builds but still has to scan the whole table. If a build fails partway, Postgres leaves an invalid index behind. HappyView drops it on the next boot and builds it again, so a restart is the fix.

**Multiple replicas on Postgres.** HappyView takes a database-wide lock while it migrates. A replica that starts while another holds it waits for that migration to finish, which takes as long as the slowest index build. When an upgrade carries a migration like this, roll it out to one replica first and start the rest once it is serving. Replicas still on an older release take the lock differently, and one that starts during a concurrent index build can deadlock with it. Postgres then aborts one side or the other, and that can be the upgraded replica's index build. That build leaves an invalid index, which HappyView drops and rebuilds from scratch on its next boot. An orchestrator that keeps restarting old replicas can therefore keep the build from ever finishing. While the first upgraded replica migrates, scale to that single replica, or at least keep replicas on the older release from starting, then bring the rest up once it is serving.

The `happyview_records` index on `(collection, created_at DESC, uri DESC)` is one of these. It replaces the `idx_records_created_at_uri` and `idx_records_collection` indexes, which the same upgrade drops.

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

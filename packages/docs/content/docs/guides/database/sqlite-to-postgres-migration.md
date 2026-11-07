---
title: "SQLite to Postgres Migration"
---

This guide covers migrating an existing HappyView deployment from SQLite to Postgres. If you are staying on SQLite, no action is required.

## Overview

HappyView writes all internal SQL in SQLite syntax and translates to Postgres automatically at runtime. This means your **Lua scripts do not need any changes** when switching from SQLite to Postgres — they continue to work as-is.

The main steps are: set up the Postgres database, update your environment variables, and re-index your data.

## Step 1: Set up Postgres

Create a Postgres database for HappyView:

```sh
createdb happyview
```

If you are using Docker Compose, uncomment the `postgres` service and `pgdata` volume in your `docker-compose.yml`. See the [database setup guide](database-setup.md#docker-compose) for details.

## Step 2: Back up your SQLite database

Copy your SQLite database file before making any changes:

```sh
cp data/happyview.db data/happyview.db.backup
```

## Step 3: Update environment variables

Change your `.env` to use Postgres:

```sh
# Before
DATABASE_URL=sqlite://data/happyview.db?mode=rwc

# After
DATABASE_URL=postgres://happyview:happyview@localhost/happyview
```

If you had `DATABASE_BACKEND` set, update it as well:

```sh
DATABASE_BACKEND=postgres
```

## Step 4: Start HappyView

Start HappyView with the new `DATABASE_URL`. It will connect to Postgres and run migrations automatically, creating all necessary tables.

## Step 5: Re-index your data

Since HappyView indexes records from the atproto network, the simplest way to populate your new Postgres database is to re-run the backfill:

1. Upload your lexicons via the dashboard or admin API (or they will already be there if you exported and re-imported them)
2. Run a backfill for each collection (dashboard or `POST /admin/backfill`)

Backfill fetches all records fresh from the network, so no data transfer between databases is needed.

<Callout type="idea">
If you have many lexicons, you can export them from the old instance before switching. Use `GET /admin/lexicons` to list them and `POST /admin/lexicons` to re-upload after switching to Postgres.
</Callout>

## Step 6: Re-create admin settings

Instance settings (app name, logo, TOS/privacy URIs), API keys, users, and labeler subscriptions are stored in the database and are not carried over automatically. Re-create these via the dashboard or admin API after switching.

## Lua scripts

`happyview.db` chains need no changes; the host generates Postgres SQL for them. Statements passed to `happyview.sql`'s `raw` do, because raw SQL reaches the backend untranslated:

- `?` placeholders become `$1`, `$2`, etc.
- `json_extract()` becomes Postgres JSON operators (`->`, `->>`)
- `datetime('now')` becomes `NOW()`
- Boolean literals `1`/`0` work in both backends

Rewrite them before switching, or branch on `db.backend()` in a script that has to run on both.

## Rollback

To switch back to SQLite, revert your `DATABASE_URL` to the SQLite connection string. Your SQLite database file remains unchanged — HappyView does not modify it during the migration to Postgres.

## Next steps

- [Postgres → SQLite migration](postgres-to-sqlite-migration.md) — migrate in the opposite direction
- [Database setup](database-setup.md) — choose between SQLite and Postgres for new instances
- [Backfill](../backfill.md) — re-index records from the network after switching backends
- [Lua scripting](../lua-scripting.md) — write SQL that works against either backend

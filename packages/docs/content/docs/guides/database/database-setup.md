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

Before the compact record format, the `happyview_records` table's index on `(collection, created_at DESC, uri DESC)` was one of these. It replaced the `idx_records_created_at_uri` and `idx_records_collection` indexes, which the same upgrade dropped. The compact format's records table has its own indexes, and `happyview_records` is now a view (see below).

## The compact record format

This release moves records into a compact format. Each DID and collection is stored once and referenced by number, CIDs are stored as bytes and timestamps as integers. On SQLite, record bodies are also compressed with a zstd dictionary trained for each collection. `happyview_records` and `happyview_record_refs` become views with the same columns, so `happyview.sql`'s `raw`, Lua scripts and plugin SQL keep reading them, and `record` is still the exact JSON HappyView received. Writes through the views work with a few differences, listed under [raw SQL on the record views](#raw-sql-on-the-record-views).

Measured on 67,437 Bluesky posts and their 105,095 refs:

| | Before | After the upgrade | After compression and a vacuum |
|---|---|---|---|
| SQLite database file | 164 MB | 58 MB | 31 MB |
| Postgres record tables | 155 MB | 77 MB | not compressed |

A synthetic set of a million records built from the same posts gave the same proportions: 2,285 MB, 855 MB and 451 MB on SQLite, and 2,047 MB and 1,061 MB for the Postgres record tables. On Postgres the saving comes from storing DIDs, collections, CIDs and timestamps compactly. Postgres's own compression only reaches bodies over about 2 KB, so post-sized records stay about the size they were.

### Before you upgrade

**Take a backup before you upgrade. The only way back to the previous release is to restore that backup.** The upgrade drops the old tables once it has checked the copy. See [Backups](../../getting-started/deployment/production.md#backups).

Make sure there is enough free disk:

- **SQLite**: about half the size of the database file. The copy sits beside the old tables, at about a third of their size, and once the old tables are dropped HappyView returns their pages to the filesystem in steps of 32 MB, emptying the write-ahead log after each. In the measurements above the most extra disk in use was 0.40 times the database file. On a database that has never had incremental vacuum (`PRAGMA auto_vacuum` reports `0`), the pages are not returned: the file keeps its size, and SQLite reuses the freed pages for new records.
- **Postgres**: about the current size of `happyview_records` and `happyview_record_refs` together. The copy takes about half of that inside the database. It also generates about the tables' size in write-ahead log, of which Postgres keeps up to `max_wal_size` (1 GB by default), or more when WAL archiving or a replication slot holds it back.

**On Postgres, stop every HappyView instance before you start the new release.** An instance still running the old release can write to the old tables while the new one copies them. The copy picks up those writes, and the final check locks the old tables so that it sees every one, but if writes keep arriving, the upgrade stops with an error rather than drop them. Stop the old instances and start the new release again.

**Your own indexes, triggers and views on the old tables.** The upgrade drops `happyview_records` and `happyview_record_refs` and puts views of the same names in their place, so what you built on the old tables goes with them:

- **Indexes and triggers** you created on either table are dropped with it, on both backends. The upgrade logs each one first, at warn level, with its full definition (`record store upgrade: dropping an object on the old record tables that HappyView did not create`, with `kind`, `name` and `definition` fields), and then carries on. On Postgres, recreate an index on the table behind the view, where queries on the view use it; see [JSON indexes on Postgres](#after-the-upgrade):

  ```sql
  CREATE INDEX ON happyview_records_v2 ((record::jsonb->>'via'));
  ```

  On SQLite a JSON index such as `json_extract(record, '$.via')` has no replacement: the body behind the view is compressed, so there is no column to index. Queries on JSON fields still work through the view, as a scan of the collection.

  A trigger cannot be recreated on the view as it was: write through the view and react in your own code, or put the trigger on `happyview_records_v2`, whose columns are the compact ones.
- **Views on SQLite** keep working. They name `happyview_records`, which is the new view after the upgrade.
- **Views, materialized views, rules and foreign keys on Postgres** that depend on either table would stop the old table from being dropped. The upgrade checks for them before it copies anything and stops with an error that names each one, such as `record store upgrade: these objects depend on happyview_records or happyview_record_refs, which the upgrade replaces with views of the same names: view my_witchsky; …`. Nothing is copied or dropped. Save their definitions, drop them and start HappyView again so the upgrade runs. Then recreate the views and rules: they read the new views the same way they read the tables. A foreign key cannot point at a view, so a foreign key to either table cannot come back; check the reference in your own code instead.

**On SQLite, start one instance.** A second process started on the same file while the first is upgrading can exit with `database is locked`. Start it again once the first is serving: it finds the upgrade done.

### What happens at the first start

The upgrade runs once, after the migrations and before HappyView serves requests. On Postgres it holds the migration lock, so another instance that starts waits for it, as it waits for a migration.

1. HappyView copies `happyview_records` into the new tables in batches of 10,000, then `happyview_record_refs`. Each batch is committed with its position, and the log reports `record store upgrade: copying records` and `copying refs` with the count so far. If the process stops partway, the next start resumes from the last batch.
2. It compares every record and every ref with the copy: the body byte for byte, the CID, the rev and the timestamps, and the number of records in each collection. If anything differs, it drops nothing, discards the copy and stops with an error naming up to five differences. Fix those rows or restore the backup, then start again; the copy starts over.
3. It replaces the old tables with the views and drops them, in the same transaction as the comparison. On SQLite it then returns the freed pages to the filesystem. The log reports `record store upgrade complete` with the number of records and refs.

On an Apple M4 Max laptop, the 67,437-post set took 3 seconds on SQLite and 43 seconds on Postgres 17. The million-record set took 102 seconds on SQLite, 65 of them returning the freed pages, and 224 seconds on Postgres. Expect a slower disk to take longer.

A record whose `uri` is not `at://{did}/{collection}/{rkey}` stops the upgrade with an error naming it. Nothing is dropped. Fix or delete that row, then start again.

Some rows are copied differently, with a warning in the log:

- A record whose `did`, `collection` or `rkey` column disagrees with its `uri` is stored under its `uri`.
- A timestamp that cannot be read is stored as 1970-01-01 (Unix time 0). An unreadable `created_at` takes the row's `indexed_at` instead, when that can be read.
- A ref whose source record does not exist is not copied.

### After the upgrade

**Page cursors.** A pagination cursor issued before the upgrade is refused once with `400` and `invalid cursor: request the first page again`. Clients that start again from the first page get cursors in the new form. A WASM plugin that pages records with `host_records_query` gets an `INVALID_SPEC` error for such a cursor and chooses the status it answers with.

**Compression on SQLite.** A collection's bodies are compressed once it holds `ZSTD_DICT_TRAIN_AT` records (5,000 by default). A background task, which runs within 30 seconds of startup and then every `RECORD_COMPRESSION_INTERVAL_SECS` seconds (600 by default), trains a dictionary for the collection from up to 20,000 of its most recently stored records. From then on new records in that collection are compressed as they are written, and the task compresses the existing ones in batches of 1,000. Bodies under 64 bytes or over 16 MiB, and bodies that compression would not make smaller, stay as they are. `RECORD_COMPRESSION=none` turns compression off; bodies already compressed stay readable. [`POST /admin/storage/retrain`](../../api-reference/admin/admin-api.md#record-compression) trains a new dictionary for one collection, and records keep the dictionary they were compressed with. See [Configuration](../../getting-started/configuration.md) for the settings.

**Reclaiming the compressed space on SQLite.** Compressing the records the upgrade copied leaves their pages partly empty, and SQLite does not shrink the file for that. In the measurement above, the file went from 58 MB to 57 MB when the task finished, and to 31 MB after a `VACUUM`. To get that space back, schedule the one-time vacuum from **Settings > Database** once the task has caught up. It runs at the next restart, takes the instance offline while it runs, and needs free disk about the size of the database. Records written after the upgrade are compressed as they are written and don't leave this gap.

**The `sqlite3` command-line tool cannot read `happyview_records`.** Its `record`, `cid` and timestamp columns are computed by functions that only HappyView registers, so any query on that view from another program fails with `no such function: hv_record`, even one that selects only `uri`. `happyview_record_refs` uses no such function and reads normally. Query the records table behind the view instead:

```sql
SELECT d.did, c.nsid AS collection, r.rkey
  FROM happyview_records_v2 r
  JOIN happyview_dids d ON d.id = r.did_id
  JOIN happyview_collections c ON c.id = r.collection_id;
```

`body` holds the record's JSON. When `body_dict` is `NULL` or `0` it is plain text (`CAST(body AS TEXT)`); otherwise it is compressed with the dictionary of that id in `happyview_zstd_dicts`, and only HappyView reads it. Backups with `sqlite3 happyview.db ".backup ..."` work as before.

**JSON indexes on Postgres.** You can index a field of `record` on the table behind the view, connected with `psql` or another client (raw SQL from scripts and plugins cannot reach that table), and queries on the view use it:

```sql
CREATE INDEX ON happyview_records_v2 ((record::jsonb->>'via'));

SELECT uri FROM happyview_records WHERE record::jsonb->>'via' = 'example';
```

Write the expression the same way in the index and the query. `record` is text, so cast it to `jsonb`: `record->>'via'` is an error, as it was on the old table.

### Raw SQL on the record views

Reads return the same columns as before: `uri`, `did`, `collection`, `rkey`, `record`, `cid`, `indexed_at`, `created_at` and `rev`. `json_extract(record, '$.field')` on SQLite and `record::jsonb->>'field'` on Postgres work as before. Timestamps read back as RFC 3339 in UTC with six fractional digits, such as `2026-01-02T03:04:05.123456+00:00`, whatever form they were written in, so a value can read back as different text from what was written. Compare timestamps as timestamps (`datetime(created_at)` on SQLite, `created_at::timestamptz` on Postgres) rather than as text.

Writes go through triggers on the views, with these differences:

- Upserts don't work. On SQLite, `INSERT … ON CONFLICT` on a view is refused. On Postgres, `ON CONFLICT` with a target, such as `ON CONFLICT (uri)`, is refused, and `ON CONFLICT DO NOTHING` without one inserts a new row but raises a duplicate-key error for an existing one instead of skipping it. A script tested only on fresh data can pass and fail later. Use `UPDATE`, or `DELETE` then `INSERT`. On SQLite, `INSERT OR REPLACE` works, and as before it drops the replaced record's refs.
- A `uri` you write must be `at://{did}/{collection}/{rkey}` of the same row. A ref's `source_uri` must be a stored record, and its `collection` must be that record's collection. Updating a ref that the upgrade copied with a different `collection` is refused unless you also set `collection` to its source record's.
- `indexed_at` left out or `NULL` is stored as `NULL` on both backends; on SQLite it used to default to the current time. `NULL` marks a record HappyView wrote itself and has not yet seen on Jetstream, so set it when you write a record. `created_at` left out is the current time; on Postgres that is `clock_timestamp()` for each row, not the transaction's start time as `now()` would give.
- A timestamp has to be RFC 3339 or SQLite's `datetime('now')` form. Anything else makes the write fail.
- `INSERT … RETURNING` returns the values the statement supplied, not the stored row, on both backends: a `created_at` you left out comes back `NULL`, and timestamps come back as you wrote them. On SQLite `uri` is `NULL` if you left it out; Postgres fills it in. On SQLite, a write through either view also reports 0 rows changed. Read the stored row back with a `SELECT`.
- On Postgres, an `INSERT` of several rows through a view can deadlock with records arriving from Jetstream. Postgres aborts it with `deadlock detected`; retry it, or insert one row per statement.

Some queries are slower through the views, because `uri`, `source_uri` and `target_uri` are computed when they are read:

- Filtering on `uri`, `source_uri` or `target_uri` reads every row. Filter `happyview_records` by `did`, `collection` and `rkey` together instead, which use an index. For the records that link to a URI, use the [`happyview.backlinks`](../../api-reference/lua/libraries.md) library, or `host_backlinks_query` from a plugin.
- Filtering by `collection` alone can read every record on a large Postgres database, even with a `LIMIT`. To read one collection, use `happyview_records_by_collection` (below).
- `ORDER BY` or a range on `created_at`, `indexed_at` or `uri` sorts or scans the whole collection.
- On Postgres, `cid`, `indexed_at` and `created_at` are converted back to text as they are read, about 6.6 µs per row for the three (about 7 seconds per million rows). A query that reads every row, such as an export, is faster when it selects only the columns it needs.

HappyView's own queries read the tables directly and are not affected.

### Reading one collection

`happyview_records_by_collection` is a read-only view with `happyview_records`' columns plus `collection_id`, `created_at_us` and `indexed_at_us` (the timestamps in microseconds) and `id`. Filter it with `hv_collection_id`, which gives a collection's id, and order it by `created_at_us` and `id`, which is the order of the index on the records table:

```sql
SELECT uri, record FROM happyview_records_by_collection
 WHERE collection_id = hv_collection_id('xyz.statusphere.status')
 ORDER BY created_at_us DESC, id DESC
 LIMIT 50;
```

That reads only the collection's newest rows, however large the database. For the next page, add `AND (created_at_us, id) < (?, ?)` with the last row's values. `hv_collection_id` answers `NULL` for a collection the database doesn't have, so the query returns nothing. Raw SQL may read this view and call the function; it may not read `happyview_collections` itself.

`happyview_records` keeps its columns unchanged, so `SELECT *` and positional `INSERT`s on it work as before. Add the view to existing queries only where they filter by collection.

#### When record storage offers a raw read route

When an instance keeps some collections in external [record storage](../record-providers.md) that offers a raw read route, every read of the two views runs on that route instead of this database:

- Read-only SQL that reads only `happyview_records` and `happyview_record_refs` runs on the route, and so does `sql.from` on either view. It sees the storage's records.
- The route uses its own backend's dialect and placeholders, which can differ from this instance's: a SQLite instance can have its record views served by Postgres. A failed routed query's error names the backend that ran it.
- A query must read only the two views, or neither. One that joins them with other tables is refused, as is a read of them that HappyView can't confirm is read-only, such as one with `FOR SHARE`. Split it into two queries.
- While the storage is down, a routed query fails with `STORAGE_UNAVAILABLE`.
- The views on the route show only the storage's records. Records of other collections held in this database can't be read through them.
- Writes through the views run on this database as before and see only its rows, even a write that reads the views, such as `INSERT … SELECT`. Writes to the storage's collections are refused.

## Rolling back the record tombstones

The release with the Jetstream v2 consumer adds two migrations: `20261010000000_records_rev` (a `rev` column on `happyview_records`) and `20261010000001_record_tombstones` (the `happyview_record_tombstones` and `happyview_did_tombstones` tables). An older release refuses to start on a database with migrations it doesn't know. To roll back, stop HappyView and delete their rows:

```sql
DELETE FROM _sqlx_migrations WHERE version IN (20261010000000, 20261010000001);
```

The column and the two tables stay. The older release ignores them, and the next upgrade reuses the tables and what they hold. On Postgres, nothing else is needed to upgrade again. On SQLite, which cannot add a column only when it is missing, drop the `rev` column before upgrading again:

```sql
ALTER TABLE happyview_records DROP COLUMN rev;
```

Dropping the column rewrites `happyview_records`, so on a large database it takes a while and needs free disk about the size of the table. It also discards the stored revs: records written before the drop have none until a new commit to them arrives, so until then they behave as records from before the upgrade.

The upgraded release resumes from the time cursor the older one saved, not from its own older seq.

Once a database has the [compact record format](#the-compact-record-format), this rollback no longer applies: `happyview_records` is a view, and the way back is restoring the backup taken before that upgrade.

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

Both backends support the same Lua libraries. A `happyview.db` chain is portable: the host generates the SQL for whichever backend is running. `happyview.sql`'s `raw` is not: the statement goes to the backend untranslated, so its placeholders (`?` on SQLite, `$1` on Postgres) and functions must be the running backend's, or for reads of the record views, [the raw read route's](#when-record-storage-offers-a-raw-read-route) when there is one. A script that has to run on both branches on `db.backend()`.

`happyview_records` and `happyview_record_refs` are views; see [raw SQL on the record views](#raw-sql-on-the-record-views) for what changes when a `raw` statement writes them or filters them by `uri`.

If you are migrating existing Lua scripts from Postgres SQL syntax to SQLite syntax, see the [Postgres to SQLite migration guide](postgres-to-sqlite-migration.md).

## Next steps

- [SQLite → Postgres migration](sqlite-to-postgres-migration.md) — switch an existing instance from SQLite to Postgres
- [Postgres → SQLite migration](postgres-to-sqlite-migration.md) — switch an existing instance from Postgres to SQLite
- [Lua scripting](../lua-scripting.md) — write queries that target either backend
- [Configuration](../../getting-started/configuration.md) — `DATABASE_URL` and related variables

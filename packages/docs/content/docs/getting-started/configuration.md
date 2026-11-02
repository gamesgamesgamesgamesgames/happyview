---
title: "Configuration"
---

HappyView is configured via environment variables. Serving with a plain `happyview` loads a `.env` file on startup, from the working directory or the nearest parent that has one. `happyview check`, `happyview dev`, `happyview test` and `happyview types` do not: under `happyview dev` the project's `.env` supplies script variables (`ctx.env`) only, as [Running locally](#running-locally-happyview-dev) explains, and under `happyview test` they come from `.env.test` instead, as [Testing](../guides/testing.md) explains. See [Local Development](deployment/local-development.md) for local setup, [Docker](deployment/docker.md) for deploying from published images, [Native binary](deployment/binary.md) for a released binary, or [Production](deployment/production.md) for the hardening checklist.

## Environment variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `DATABASE_URL` | yes | --- | Database connection string. SQLite (`sqlite://path/to/db?mode=rwc`) or Postgres (`postgres://user:pass@host/db`) |
| `DATABASE_BACKEND` | no | auto-detected | Force `sqlite` or `postgres`. Auto-detected from `DATABASE_URL` scheme if not set |
| `SQLITE_JOURNAL_SIZE_LIMIT` | no | `67108864` (64 MiB) | Bytes. Caps how large the SQLite write-ahead log is allowed to grow before it's truncated back down after a checkpoint, bounding disk usage from WAL growth. SQLite-only; ignored on Postgres |
| `SQLITE_SYNCHRONOUS` | no | `NORMAL` | SQLite `synchronous` level for every connection: `NORMAL` or `FULL`. In WAL mode `NORMAL` cannot corrupt the database, but a crash or power loss can lose the last few transactions. `FULL` syncs the write-ahead log on every commit. SQLite-only; ignored on Postgres |
| `SQLITE_CHECKPOINT_INTERVAL_SECS` | no | `60` | Seconds between `wal_checkpoint(TRUNCATE)` runs, which wait briefly for readers, then empty the write-ahead log. `0` disables them. The latest result is shown on **Settings > Database**. SQLite-only; ignored on Postgres |
| `SQLITE_WAL_CHECKPOINT_BYTES` | no | `67108864` (64 MiB) | Bytes. Runs the same checkpoint as soon as the write-ahead log passes this size, checked every second, so a heavy backfill cannot fill the log between two intervals. If the checkpoint is busy, the next try waits twice as long, up to 10 seconds. `0` disables it. SQLite-only; ignored on Postgres |
| `RECORD_COMPRESSION` | no | `zstd` | `zstd` compresses record bodies with a dictionary trained for each collection; `none` stores new bodies uncompressed and stops the background compression. Bodies already compressed stay readable either way. Any other value uses `zstd`, with a warning. See [the compact record format](../guides/database/database-setup.md#after-the-upgrade). SQLite-only; ignored on Postgres |
| `ZSTD_DICT_TRAIN_AT` | no | `5000` | Records a collection needs before HappyView trains its compression dictionary. `0` or an invalid value uses `5000`. SQLite-only; ignored on Postgres |
| `RECORD_COMPRESSION_INTERVAL_SECS` | no | `600` | Seconds between the background passes that train dictionaries and compress records stored uncompressed. The first pass runs within 30 seconds of startup. `0` or an invalid value uses `600`; to stop compressing, set `RECORD_COMPRESSION=none`. SQLite-only; ignored on Postgres |
| `PUBLIC_URL` | yes | --- | Public-facing URL for HappyView (used for OAuth callbacks, e.g. `https://happyview.example.com`). **For local development, use `http://127.0.0.1:3000` — not `localhost`** (see note below). Do **not** include the base path — see `BASE_PATH` |
| `BASE_PATH` | no | _(none)_ | Subpath prefix for mounting HappyView behind a reverse proxy (e.g. `/hv`). Must start with `/` and have no trailing slash. When set, all routes are served under this prefix and the dashboard is accessible at `https://example.com/hv/`. See [Reverse proxy subpath](deployment/production.md#reverse-proxy-subpath) |
| `SESSION_SECRET` | no | dev default | Secret key for signing session cookies (at least 64 characters). **Must be set in production** |
| `HOST` | no | `0.0.0.0` | Bind host |
| `PORT` | no | `3000` | Bind port |
| `JETSTREAM_URL` | no | `wss://jetstream1.us-east.bsky.network` | Jetstream host for real-time record streaming. The scheme can be `ws`, `wss`, `http` or `https`, and a v1 or v2 subscribe path, a trailing slash and a query string are ignored, so all of them name the same host |
| `JETSTREAM_PROTOCOL` | no | `auto` | `auto` subscribes to the v2 stream (`/xrpc/network.bsky.jetstream.subscribeEvents`) and falls back to v1 (`/subscribe`) when the host answers 404. A fallback lasts until HappyView restarts. `v2` or `v1` uses only that one. Set `v1` for a v1-only host behind a proxy that answers the v2 path with something other than 404 (a 400, 403 or 502, say): `auto` does not fall back on those |
| `JETSTREAM_COMPRESSION` | no | `true` | Receive v2 events zstd-compressed with the host's dictionary. When the dictionary cannot be fetched, or the host keeps refusing it, HappyView subscribes uncompressed instead. `false` always subscribes uncompressed. v1 is always uncompressed |
| `JETSTREAM_WORKERS` | no | `8` | Workers applying events in parallel, 1 to 64. Larger values are capped at 64, and 0 or an invalid value uses 8 with a warning. A repo's events always go to the same worker, in order. A worker waits for each event's [record script](../guides/record-scripts.md), so slow scripts limit throughput; on Postgres, raise this to apply more repos at once. On SQLite writes take turns, so more workers help less. See [Real-time indexing](../reference/architecture.md#real-time-indexing) |
| `JETSTREAM_API_KEY` | no | _(none)_ | Key for the Jetstream archive (`planSnapshot`, `getSegment`, `getBlock`), sent as `Authorization: Bearer`. Never sent to the live stream. Bluesky's hosted archive requires one: without it, `auto` backfills crawl instead of replaying. Only used when `BACKFILL_SOURCE` or a job asks for replay |
| `RELAY_URL` | no | `https://bsky.network` | Relay URL for [backfill](../guides/backfill.md) repo discovery |
| `PLC_URL` | no | `https://plc.directory` | [PLC directory](https://github.com/did-method-plc/did-method-plc) URL for DID resolution |
| `STATIC_DIR` | no | _(embedded)_ | Directory to serve the dashboard from instead of the copy built into release binaries. Set but empty is the same as unset. A binary built from source without the `embedded-dashboard` feature falls back to `./web/out` |
| `PLUGIN_DIR` | no | `./plugins` | Directory whose subdirectories (`<id>/manifest.json` beside the `.wasm`) are loaded as plugins at boot. `happyview dev` sets it to `<project>/.happyview/plugins` |
| `HAPPYVIEW_DEFAULT_PLUGINS` | no | _(none)_ | Comma-separated plugin sources to install once, on the first boot that lists them: anything **Add plugin** takes as a URL (a `manifest.json`, a `.wasm`, or another URL that answers with a manifest). Listing a source consents to every capability its manifest declares. Each applied source is recorded, so a plugin the owner uninstalls isn't installed again; one that fails to install is retried at the next boot. Installs run in the background and never delay or stop boot. See [Plugins](../guides/plugins.md#default-plugins) |
| `HAPPYVIEW_SCRIPT_ENV_FILE` | no | _(none)_ | A dotenv file whose variables reach scripts as `ctx.env`, over the ones stored in the dashboard. Read on every run. `happyview dev` sets it to the project's `.env`, and `happyview test` to the project's `.env.test` |
| `HAPPYVIEW_DEV_BACKFILL` | no | `[dev] backfill`, else `false` | Under `happyview dev` only: whether a backfill may start on its own when a record lexicon with `backfill` is added or declared. Exactly `true` or `false`; any other value stops `happyview dev` with an error. A backfill created from the dashboard's Backfill page always runs; the backfill option on a lexicon upload follows this setting |
| `HAPPYVIEW_NETWORK` | no | _(none)_ | Under `happyview dev` only: `local` suppresses the warning that writes reach the real network. `hpy happyview dev --network local` sets it |
| `HAPPYVIEW_WATCH_POLL_MS` | no | _(native file events)_ | Under `happyview dev` and `happyview test --watch` only: watch the project by polling every N milliseconds (50 to 60000) instead of with file-system events, for mounts that don't deliver events, such as a Docker Desktop bind mount. Any other value stops the command with an error |
| `PLUGIN_REGISTRY_URL` | no | `https://api.happyproto.at` | Registry the plugin catalogue is read from. Empty or `off` turns the catalogue off entirely, so no registry is contacted |
| `SCRIPT_INSTRUCTION_LIMIT` | no | `1000000` | Lua VM instructions a query, procedure, record or label script may execute per run, from loading the chunk to `handle` returning, before it fails with `timeout`. An integer from `1000` to `1000000000`. Job scripts are not limited. The **Instruction limit** setting in Settings → General takes precedence when set. See [Lua Scripting](../guides/lua-scripting.md#sandbox) |
| `SCRIPT_WALL_CLOCK_SECONDS` | no | `10` | Seconds a query, procedure, record or label script may spend running, before it fails with `timeout`. Time spent awaiting the host — an HTTP request, a database read — is not charged against it. An integer from `1` to `300`. Job scripts are not clocked. The **Request wall clock** setting in Settings → General takes precedence when set |
| `SCRIPT_METRICS_ENABLED` | no | `true` | Keep per-script run counts, latency, errors and resource use in memory, read through [`/admin/metrics`](../api-reference/admin/metrics.md). `false` turns recording off. The `script_metrics_enabled` instance setting takes precedence when set |
| `HAPPYVIEW_INSTANCE_ID` | no | the hostname, else a random id for each boot | This instance's name in the stored script metrics, at most 128 characters (longer values are cut, and surrounding whitespace is trimmed). Instances sharing a database each keep their own rows under it, and the `7d` and `30d` windows add them all up. Give each replica a stable, distinct value, so a restart reads its own history back. See [several instances on one database](../api-reference/admin/metrics.md#several-instances-on-one-database) |
| `EVENT_LOG_RETENTION_DAYS` | no | `30` | Number of days to keep event logs, job logs and stored script metrics before automatic cleanup. Set to `0` to disable cleanup. [Protected events](../guides/event-logs.md#protected-events) follow `SPACE_ACCESS_LOG_RETENTION_DAYS` instead |
| `SPACE_ACCESS_LOG_RETENTION_DAYS` | no | `365` | Days to keep [protected events](../guides/event-logs.md#protected-events) — space access grants and revocations, the reads made under them, event log purges, and retention changes — before cleanup. Access grants that ended (expired or were revoked) longer ago than this are deleted on the same schedule. Set to `0` to keep them forever. When unset, it follows an `EVENT_LOG_RETENTION_DAYS` of `0` and is `365` otherwise. The events tied to an access grant that still exists (its grant and revocation events and the reads made under it) are kept regardless. Overridden by database setting if set via admin API |
| `SPACE_INSPECTOR_ENABLED` | no | `false` | Whether users with `spaces:inspect` can request time-limited access to read records and blobs in private [spaces](../experimental/spaces/index.md#moderating-spaces). In this build the space inspector stays off whatever is set: a value of `true`, here or stored as the `feature.space_inspector_enabled` setting, is ignored with a warning logged at startup, and the admin settings API refuses to turn it on. A stored setting is kept and takes effect once the inspector's dashboard is available |
| `SPACE_INSPECTOR_MAX_GRANT_MINUTES` | no | `60` | The longest an access grant can last. Requests default to 60 minutes or this value, whichever is shorter; unparseable values fall back to `60`, anything below `5` is raised to `5`, and anything above `525600` (one year) is lowered to it. Overridden by database setting if set via admin API |
| `DEAD_LETTER_RETENTION_DAYS` | no | `30` | Days to keep script dead letters after they are resolved; the hourly cleanup deletes older ones. Unresolved dead letters are never deleted. `0` keeps resolved ones indefinitely |
| `RECORD_TOMBSTONE_RETENTION_HOURS` | no | `72` | Hours to remember a deleted record or account, so that a late or replayed copy of it cannot bring it back. The hourly cleanup deletes older tombstones. `0` keeps them indefinitely |
| `JOB_WORKER_CONCURRENCY` | no | `1` | Number of background jobs that run at once. Values below 1 fall back to 1; values above 32 are capped at 32. Each running job uses database connections, so keep this below `DATABASE_MAX_CONNECTIONS`. |
| `PLATFORM_API_KEY_HASH` | no | — | Hex SHA-256 of a key that a managed-hosting provider uses to administer this instance. Leave unset when self-hosting. See [Platform API](../api-reference/admin/platform.md) |
| `TELEMETRY_MANAGED_BY` | no | --- | Name of the managed-hosting platform that requires this instance to send telemetry. When set, the instance sends a report every day whatever **System → Telemetry** says, the report names the platform in `managed`, and the telemetry settings can't be changed from the dashboard or the API (`409`). The report covers the instance only: the contact address and the lexicon names, structures and documents are left out, and setting keys HappyView doesn't define are counted, not named. Lowercase letters, digits and `-`, starting with a letter or digit, at most 64 characters; any other value refuses boot. Leave unset when self-hosting |
| `TOKEN_ENCRYPTION_KEY` | no | --- | Base64-encoded 32-byte key for encrypting stored OAuth tokens. **Strongly recommended in production** |
| `DEFAULT_RATE_LIMIT_CAPACITY` | no | `100` | Default token bucket capacity used when registering a new API client |
| `DEFAULT_RATE_LIMIT_REFILL_RATE` | no | `2.0` | Default token bucket refill rate (tokens/second) for new API clients |
| `ATTESTATION_PRIVATE_KEY` | no | auto-generated | Hex-encoded 32-byte secp256k1 private key for [attestation signing](../guides/attestation-signing.md). Auto-generated and persisted to database on first run |
| `ATTESTATION_KEY_ID` | no | `did:web:{host}#attestation` | Key identifier included in attestation signatures. Derived from `PUBLIC_URL` by default |
| `ATTESTATION_SIG_TYPE` | no | app-specific NSID | `$type` value used in attestation signature objects |
| `BACKFILL_CONCURRENT_PDS` | no | `10` | How many PDS servers to fetch from simultaneously during backfill. Overridden by database setting if set via admin API |
| `BACKFILL_CONCURRENT_DIDS_PER_PDS` | no | `3` | How many repos to fetch concurrently from each PDS during backfill. Overridden by database setting if set via admin API |
| `BACKFILL_CONCURRENT_RESOLUTION` | no | `100` | How many DID document lookups to run in parallel during PDS resolution. Overridden by database setting if set via admin API |
| `BACKFILL_HOOK_CONCURRENCY` | no | `4` | How many of a backfill's records run their record hook at once, 1 to 32. `1` hooks them one at a time. Live records from the firehose are always hooked one at a time. A record storage with a connection limit can lower it, and HappyView logs a warning when it does. See [Record hooks run concurrently](../guides/backfill.md#record-hooks-run-concurrently) |
| `BACKFILL_RETENTION_DAYS` | no | `28` | Days to keep per-repo detail data from completed backfill jobs. `0` to keep indefinitely. Overridden by database setting if set via admin API |
| `BACKFILL_DISCOVERY_WINDOW` | no | `50000` | Most work units a backfill keeps queued at once. A unit is one repo under one collection it was discovered in (a repo found under two collections is two units). Discovery pauses while the queue is full and resumes as units finish, so a network backfill never stores the whole network's repo list |
| `BACKFILL_SOURCE` | no | `crawl` | Where collection backfills read records: `crawl` fetches each PDS; `replay` reads the Jetstream archive; `auto` replays when `JETSTREAM_URL` has an archive it may use and otherwise crawls (an archive that requires a key, like Bluesky's, counts only when `JETSTREAM_API_KEY` is set). Replay can only return what the archive holds, and some repos' history in Bluesky's archive starts later than their first record. Account backfills always crawl. `auto` or `replay` also turns on gap replay. A job can override it. See [Replay and crawl](../guides/backfill.md#replay-and-crawl) |
| `BACKFILL_REPLAY_CONCURRENCY` | no | `8` | Archive blocks a replay job downloads at once, 1 to 64. Bluesky's hosted archive allows each key about 2 MB/s after a 1 GB burst, and HappyView paces to it, so raising this doesn't speed replay up there |
| `BACKFILL_REPLAY_MARKER_TAIL_HOURS` | no | `12` | How many of the archive's newest hours a collection replay sweeps for account and sync events. Jetstream's compaction has already removed what deletions and resyncs supersede in the older part. `0` sweeps the whole archive, for a Jetstream with compaction turned off. Takedowns and suspensions older than this aren't applied. See [Replay and crawl](../guides/backfill.md#replay-and-crawl) |
| `BACKFILL_REPLAY_DIR` | no | system temp dir + `/happyview-replay` | Where replay jobs spool archive segments, one at a time per job. A segment is capped at 1 GiB. Files older than 24 hours are removed when a replay job starts |
| `BACKFILL_DATABASE_MAX_CONNECTIONS` | no | auto-calculated | Override the backfill connection pool size. Auto-calculated from concurrency settings if not set, capped at 16 on SQLite and 256 on Postgres |
| `VERBOSE_EVENT_LOGGING` | no | `false` | Log every record index, hook execution, and hook skip to the event log. High write volume — recommended only for debugging. Overridden by database setting if set via admin API |
| `RUST_LOG` | no | `happyview=debug,tower_http=debug,sqlx=warn` | Log filter (uses `tracing_subscriber::EnvFilter`) |
| `APP_NAME` | no | --- | Application name shown on OAuth authorization screens. Overridden by database setting if set via admin API |
| `SUPERADMIN_DID` | no | --- | DID seeded as a super user at every boot, before the server accepts requests. Setting it turns off the first-sign-in admin bootstrap, even if seeding fails (restart to retry). A value that is not a valid DID refuses boot. Additive: other super users keep their role, and removing it later demotes nobody. A platform super-user transfer demotes the other super users, and the configured superadmin is promoted again at the next boot |
| `HAPPYVIEW_PROJECT` | no | --- | Path to a project directory. Its `happyview.toml` is read at boot; see [Project file](#project-file-happyviewtoml). A path that does not exist stops boot |
| `LOGO_URI` | no | --- | URL to application logo for OAuth screens. Overridden by database setting or logo upload |
| `TOS_URI` | no | --- | URL to terms of service. Overridden by database setting if set via admin API |
| `POLICY_URI` | no | --- | URL to privacy policy. Overridden by database setting if set via admin API |

<Callout type="warn" title="Use 127.0.0.1, not localhost">
atproto OAuth loopback clients are registered with `127.0.0.1`. If you set `PUBLIC_URL` to `http://localhost:3000`, OAuth sign-in will fail because the redirect URI won't match the loopback client ID. Always use `http://127.0.0.1:3000` for local development.
</Callout>

## Example `.env`

```sh
# SQLite (default — zero setup required)
DATABASE_URL=sqlite://data/happyview.db?mode=rwc
PUBLIC_URL=http://127.0.0.1:3000
SESSION_SECRET=change-me-in-production

# Or use Postgres instead:
# DATABASE_URL=postgres://happyview:happyview@localhost/happyview

# Optional overrides
# BASE_PATH=/hv
# HOST=0.0.0.0
# PORT=3000
# SQLITE_JOURNAL_SIZE_LIMIT=67108864
# SQLITE_SYNCHRONOUS=NORMAL
# SQLITE_CHECKPOINT_INTERVAL_SECS=60
# SQLITE_WAL_CHECKPOINT_BYTES=67108864
# RECORD_COMPRESSION=zstd
# ZSTD_DICT_TRAIN_AT=5000
# RECORD_COMPRESSION_INTERVAL_SECS=600
# JETSTREAM_URL=wss://jetstream1.us-east.bsky.network
# JETSTREAM_PROTOCOL=auto
# JETSTREAM_COMPRESSION=true
# JETSTREAM_WORKERS=8
# JETSTREAM_API_KEY=
# BACKFILL_SOURCE=crawl
# BACKFILL_REPLAY_CONCURRENCY=8
# BACKFILL_REPLAY_DIR=/var/tmp/happyview-replay
# BACKFILL_REPLAY_MARKER_TAIL_HOURS=12
# RELAY_URL=https://bsky.network
# PLC_URL=https://plc.directory
# STATIC_DIR=./web/out
# EVENT_LOG_RETENTION_DAYS=30
# SPACE_ACCESS_LOG_RETENTION_DAYS=365
# SPACE_INSPECTOR_ENABLED=false
# SPACE_INSPECTOR_MAX_GRANT_MINUTES=60
# DEAD_LETTER_RETENTION_DAYS=30
# RECORD_TOMBSTONE_RETENTION_HOURS=72
# TOKEN_ENCRYPTION_KEY=base64-encoded-32-byte-key
# DEFAULT_RATE_LIMIT_CAPACITY=100
# DEFAULT_RATE_LIMIT_REFILL_RATE=2.0
# BACKFILL_CONCURRENT_PDS=10
# BACKFILL_CONCURRENT_DIDS_PER_PDS=3
# BACKFILL_CONCURRENT_RESOLUTION=100
# BACKFILL_RETENTION_DAYS=28
# BACKFILL_DISCOVERY_WINDOW=50000
# VERBOSE_EVENT_LOGGING=false
# HAPPYVIEW_DEFAULT_PLUGINS=https://example.com/plugins/lua/manifest.json
# RUST_LOG=happyview=debug,tower_http=debug,sqlx=warn
# APP_NAME=My App
# LOGO_URI=https://example.com/logo.png
# TOS_URI=https://example.com/tos
# POLICY_URI=https://example.com/privacy
```

## Project file (`happyview.toml`)

When `HAPPYVIEW_PROJECT` names a directory, HappyView reads `happyview.toml` from it at boot. The file is optional, and **every environment variable wins over it**, including one set to an empty string. Instance settings saved in the dashboard win over both.

```toml
[happyview]
version = "3.0.0"            # warns at boot when the running binary differs

[instance]
app_name = "HappyProto Registry"           # stands in for APP_NAME
superadmin = "did:plc:..."                 # stands in for SUPERADMIN_DID

[interpreters]
lua = { did = "did:plc:mo7vq4jbioxwafy5zkcc3eux", name = "happyview-lua", channel = "latest" }

[libraries]
"happyview.http" = { did = "did:plc:mo7vq4jbioxwafy5zkcc3eux", name = "happyview-http", version = "1.0.3" }

[dev]
port = 3000                  # stands in for PORT, under `happyview dev` only
backfill = false             # stands in for HAPPYVIEW_DEV_BACKFILL, under `happyview dev` only

[network_lexicons]
include = ["app.bsky.actor.profile"]

[labels]
subscribe = ["did:plc:ar7c4by46qjdydhdevvrndac"]

[jetstream]
protocol = "auto"            # stands in for JETSTREAM_PROTOCOL: auto, v2 or v1
compression = true           # stands in for JETSTREAM_COMPRESSION

[lexicons."com.example.thing"]
backfill = true              # backfill this record collection once
indexed = false              # keep the lexicon for validation and refs, without ingesting its collection
```

- `[network_lexicons] include` lexicons are tracked at boot, with no target collection and no backfill. One whose id a dashboard lexicon already uses is skipped with a warning; the dashboard lexicon is kept.
- `[labels] subscribe` labelers are subscribed at boot. Pausing one from the dashboard sticks across restarts.
- Neither can be removed from the dashboard or the admin API (`409`); remove the line from the file. A line removed from the file leaves its subscription or lexicon in place, now removable from the dashboard.
- `backfill = true` on a record lexicon in the project's `src/lexicons/` starts a backfill only the first time. Under `happyview dev`, only when `[dev] backfill = true`; dev also checks after every rebuild, so a lexicon added while dev runs gets its backfill then. At boot, HappyView starts one for the collection unless a network backfill of that collection already exists, whatever its outcome: completed, failed, cancelled or still running. A backfill of named accounts does not count, and neither does a backfill of all collections, so either of those leaves the declared one to start. Restarting does not crawl the network again, even if the first backfill failed or was cancelled. To run it again, open **Backfill** in the dashboard, choose **Create Backfill Job** and select the collection, or call [`POST /admin/backfill`](../api-reference/admin/backfill.md) with `{"collection": "..."}`.
- `indexed` (default `true`): set `false` to keep a record lexicon for validation and refs without ingesting its collection from the network. An unindexed lexicon starts no backfill, even with `backfill = true`. Toggling it takes effect on the next build. The admin API's `indexed` field on [`POST /admin/lexicons`](../api-reference/admin/lexicons.md) does the same for a dashboard lexicon.
- `[interpreters]` and `[libraries]` name the plugins a project uses, keyed by language id or library namespace. Each value is a table naming a registry project: `did` is the publisher's DID and `name` the project name, followed by exactly one of `channel` (`latest`, `beta` or `alpha`) or `version` (a pinned release, such as `1.2.0` or `1.2.0-rc.1`). `hpy happyview add` writes these entries. The older `lua = "1.4.0"` form is refused with a message showing the table form.
- Unknown sections and keys are refused, and every error names the file, line and column: `happyview.toml:3:1: unknown field ...`.

## Checking a project

`happyview check <root>` builds the project at `<root>` the way boot does, without serving it, and prints what it found. It needs no database and no `DATABASE_URL`, so it can run in CI on every pull request.

```sh
happyview check ./my-project
happyview check ./my-project --plugin ./plugins/lua
```

Each problem is printed on its own line, with paths relative to the project root. An error reads `path:line: message`, or `path: message` when there is no line; a warning reads the same with `warning: ` before the message:

```text
src/lexicons/com/example/getThing.json: the lexicon's id is 'com.example.getOther', ...
src/lexicons/com/example/thing.index.lua: warning: overrides the dashboard script ...
```

A project that fails prints its problems followed by a count of errors. A project that builds prints its warnings and a summary such as `ok: 4 lexicons, 6 scripts, 1 module`.

| Exit code | Meaning |
| --- | --- |
| `0` | The project builds |
| `1` | The project does not build; the reasons are printed |
| `2` | The check could not run, for example because an interpreter plugin failed to load |

Scripts are checked by the interpreter that claims their file extension, so `check` needs the interpreter plugins. It loads them from, in order:

1. each `--plugin <dir>`, where the directory holds one plugin's `manifest.json`, when any are given;
2. otherwise every plugin directory inside `$PLUGIN_DIR`;
3. otherwise every plugin directory inside `<root>/.happyview/plugins`.

### For editors (`--json`)

`happyview check <root> --json` runs the same check and prints one JSON object instead of lines, for an editor to read. The exit codes are the same.

```json
{
  "ok": false,
  "diagnostics": [
    { "path": "src/lexicons/com/example/getThing.lua", "line": 3,
      "severity": "error", "message": "..." }
  ],
  "files": [
    { "path": "src/lexicons/com/example/getThing.lua", "role": "handler",
      "trigger": "xrpc.query:com.example.getThing",
      "lexicon": "com.example.getThing", "language": "lua" }
  ],
  "lexicons": [
    { "nsid": "com.example.getThing",
      "path": "src/lexicons/com/example/getThing.json", "kind": "query" }
  ],
  "languages": [ { "language_id": "lua", "extensions": ["lua"] } ]
}
```

- **`diagnostics`** holds a successful build's warnings, or a failed build's errors and warnings. `line` is `null` for a problem with the whole file.
- **`files`** gives every file under `src/` a role: `lexicon`, `handler`, `module` or `test`.
  - Files HappyView ignores, and files it refuses, are left out. A refused file's reason is in `diagnostics`.
  - A handler carries its `trigger`, plus `lexicon` when the trigger names one of the project's lexicons.
  - `language` is the interpreter that runs the file.
- **`lexicons`** lists the lexicon files that load, with their `kind`: `record`, `query`, `procedure`, `space` or `definitions`.
- **`files` and `lexicons` are filled in even when the project does not build.**
- **A check that cannot start** prints `{"ok": false, "error": "..."}` and exits `2`.

## Running locally (`happyview dev`)

`happyview dev` serves a project from its own directory with nothing to configure. It listens on `127.0.0.1` at `[dev] port` (default `3000`) and skips the setup wizard, so the dashboard is usable on first start. An unfinished wizard is left alone, not overwritten.

State lives in `<project>/.happyview/`:

- `.happyview/` itself is kept at `0700`; one that is looser is tightened.
- `happyview.db` is the SQLite database.
- `plugins/` holds the plugins, and the compiled-module cache sits beside the database.
- `secrets` holds the generated session secret and token encryption key. It is created once with mode `0600`, and a file that grants any permission to group or other, or that another user owns, is refused. A damaged file is never regenerated over. Deleting it generates new keys, and sessions and tokens stored under the old ones stop working.

Precedence is the same as everywhere: environment variables, then `happyview.toml`, then these defaults.

**`.env` is for scripts, not the server.** The project's `.env` supplies `ctx.env` only, and is re-read on every script run. It never sets server configuration, so a `DATABASE_URL` or `PORT` in it is just a script variable. `$VAR` references in it are expanded from the process environment first, then from lines above it in the same file. The process environment holds only what was exported in the shell that started HappyView, not the defaults dev supplies. So `X=$DATABASE_URL` gives `ctx.env` the `DATABASE_URL` you exported, else the value of a `DATABASE_URL=` line earlier in `.env`, else an empty value.

**Saving rebuilds.** The watcher covers `src/`, `happyview.toml` and the modules in `.happyview/build/`. Each build prints one line listing what changed, followed by a line for each warning. A build that fails prints every error, then `build failed; still serving the previous build`, and the last good build keeps serving until you fix it. Dev also starts when the project's `src/` is already broken, and waits for the fix. A `happyview.toml` that does not parse at startup stops it; the same breakage while dev runs is reported and survived. If file events don't arrive (a Docker Desktop bind mount, say), set `HAPPYVIEW_WATCH_POLL_MS`.

**Editing `happyview.toml` while dev runs:**

- Added declarations apply at once.
- Removed declarations are reported, not applied. They stay until you delete them from the dashboard after a restart.
- A setting that is read only at startup prints `restart required to apply: <names>`, naming the variables, such as `PORT` or `PUBLIC_URL`.
- An edit that is rejected leaves the running configuration untouched.

**Writes reach the real network.** Dev prints a warning once at startup: writes from the instance go to your real PDS and the live network, where any instance indexing the same collections, including your production one, will index them. Set `HAPPYVIEW_NETWORK=local`, or run `hpy happyview dev --network local`, to silence it.

## Next steps

- [Authentication](authentication.md) — set up OAuth and admin users
- [Dashboard](dashboard.md) — explore the admin dashboard
- [Production deployment](deployment/production.md) — deploy HappyView to production

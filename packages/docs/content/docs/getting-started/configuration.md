---
title: "Configuration"
---

HappyView is configured via environment variables. A `.env` file in the project root is loaded automatically on startup. See [Local Development](deployment/local-development.md) for local setup, [Docker](deployment/docker.md) for deploying from published images, or [Production](deployment/production.md) for the hardening checklist.

## Environment variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `DATABASE_URL` | yes | --- | Database connection string. SQLite (`sqlite://path/to/db?mode=rwc`) or Postgres (`postgres://user:pass@host/db`) |
| `DATABASE_BACKEND` | no | auto-detected | Force `sqlite` or `postgres`. Auto-detected from `DATABASE_URL` scheme if not set |
| `SQLITE_JOURNAL_SIZE_LIMIT` | no | `67108864` (64 MiB) | Bytes. Caps how large the SQLite write-ahead log is allowed to grow before it's truncated back down after a checkpoint, bounding disk usage from WAL growth. SQLite-only; ignored on Postgres |
| `SQLITE_SYNCHRONOUS` | no | `NORMAL` | SQLite `synchronous` level for every connection: `NORMAL` or `FULL`. In WAL mode `NORMAL` cannot corrupt the database, but a crash or power loss can lose the last few transactions. `FULL` syncs the write-ahead log on every commit. SQLite-only; ignored on Postgres |
| `SQLITE_CHECKPOINT_INTERVAL_SECS` | no | `60` | Seconds between `wal_checkpoint(TRUNCATE)` runs, which wait briefly for readers, then empty the write-ahead log. `0` disables them. The latest result is shown on **Settings > Database**. SQLite-only; ignored on Postgres |
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
| `STATIC_DIR` | no | `./web/out` | Directory containing the built dashboard static assets |
| `SCRIPT_INSTRUCTION_LIMIT` | no | `1000000` | Lua VM instructions a query, procedure, record or label script may execute per run, from loading the chunk to `handle` returning, before it fails with `timeout`. An integer from `1000` to `1000000000`. Job scripts are not limited. The **Instruction limit** setting in Settings → General takes precedence when set. See [Lua Scripting](../guides/lua-scripting.md#sandbox) |
| `SCRIPT_WALL_CLOCK_SECONDS` | no | `10` | Seconds a query, procedure, record or label script may spend running, before it fails with `timeout`. Time spent awaiting the host — an HTTP request, a database read — is not charged against it. An integer from `1` to `300`. Job scripts are not clocked. The **Request wall clock** setting in Settings → General takes precedence when set |
| `EVENT_LOG_RETENTION_DAYS` | no | `30` | Number of days to keep event logs before automatic cleanup. Set to `0` to disable cleanup |
| `DEAD_LETTER_RETENTION_DAYS` | no | `30` | Days to keep script dead letters after they are resolved; the hourly cleanup deletes older ones. Unresolved dead letters are never deleted. `0` keeps resolved ones indefinitely |
| `RECORD_TOMBSTONE_RETENTION_HOURS` | no | `72` | Hours to remember a deleted record or account, so that a late or replayed copy of it cannot bring it back. The hourly cleanup deletes older tombstones. `0` keeps them indefinitely |
| `JOB_WORKER_CONCURRENCY` | no | `1` | Number of background jobs that run at once. Values below 1 fall back to 1; values above 32 are capped at 32. Each running job uses database connections, so keep this below `DATABASE_MAX_CONNECTIONS`. |
| `PLATFORM_API_KEY_HASH` | no | — | Hex SHA-256 of a key that a managed-hosting provider uses to administer this instance. Leave unset when self-hosting. See [Platform API](../api-reference/admin/platform.md) |
| `TOKEN_ENCRYPTION_KEY` | no | --- | Base64-encoded 32-byte key for encrypting stored OAuth tokens. **Strongly recommended in production** |
| `DEFAULT_RATE_LIMIT_CAPACITY` | no | `100` | Default token bucket capacity used when registering a new API client |
| `DEFAULT_RATE_LIMIT_REFILL_RATE` | no | `2.0` | Default token bucket refill rate (tokens/second) for new API clients |
| `ATTESTATION_PRIVATE_KEY` | no | auto-generated | Hex-encoded 32-byte secp256k1 private key for [attestation signing](../guides/attestation-signing.md). Auto-generated and persisted to database on first run |
| `ATTESTATION_KEY_ID` | no | `did:web:{host}#attestation` | Key identifier included in attestation signatures. Derived from `PUBLIC_URL` by default |
| `ATTESTATION_SIG_TYPE` | no | app-specific NSID | `$type` value used in attestation signature objects |
| `BACKFILL_CONCURRENT_PDS` | no | `10` | How many PDS servers to fetch from simultaneously during backfill. Overridden by database setting if set via admin API |
| `BACKFILL_CONCURRENT_DIDS_PER_PDS` | no | `3` | How many repos to fetch concurrently from each PDS during backfill. Overridden by database setting if set via admin API |
| `BACKFILL_CONCURRENT_RESOLUTION` | no | `100` | How many DID document lookups to run in parallel during PDS resolution. Overridden by database setting if set via admin API |
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
# RUST_LOG=happyview=debug,tower_http=debug,sqlx=warn
# APP_NAME=My App
# LOGO_URI=https://example.com/logo.png
# TOS_URI=https://example.com/tos
# POLICY_URI=https://example.com/privacy
```

## Next steps

- [Authentication](authentication.md) — set up OAuth and admin users
- [Dashboard](dashboard.md) — explore the admin dashboard
- [Production deployment](deployment/production.md) — deploy HappyView to production

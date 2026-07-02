---
title: "HappyView v2.9"
description: "Trigger-keyed scripts, backfill concurrency, db.query filters, multi-device DPoP sessions, plus bugs fixes and performance improvements."
date: 2026-05-27
author:
  name: "Trezy"
  avatar: "/authors/trezy.webp"
tags:
  - announcements
atUri: "at://did:plc:qneu5uamqs6cug7sjobbaexm/site.standard.document/3mpokm3b2ua2c"
---

2.9 is a big one. Scripts got an overhaul, backfill got way faster, and there's a mountain of bug fixes and performance improvements across the board.

## Trigger-keyed scripts

The biggest conceptual change in 2.9: scripts are no longer embedded with lexicons, and lexicons are no longer limited to a single script.

| Trigger                 | Fires when...                                                     |
| ----------------------- | ----------------------------------------------------------------- |
| `record.index:<nsid>`   | Any record event (create, update, delete) — the wildcard fallback |
| `record.create:<nsid>`  | A record is created                                               |
| `record.update:<nsid>`  | A record is updated                                               |
| `record.delete:<nsid>`  | A record is deleted                                               |
| `xrpc.query:<nsid>`     | An XRPC query is called                                           |
| `xrpc.procedure:<nsid>` | An XRPC procedure is called                                       |
| `labeler.apply:<nsid>`  | A label arrives on a record of this type                          |
| `labeler.apply:_actor`  | A label arrives on a bare DID (actor-level)                       |

For record events, the dispatcher tries the action-specific trigger first (e.g. `record.create:com.example.post`), then falls back to the wildcard `record.index:com.example.post`. This means you can have one general-purpose script that handles everything, or surgical scripts for specific actions — or both.

Scripts are managed through the dashboard under **Settings > Scripts**, or via the new [`/admin/scripts`](/api-reference/admin/scripts) API endpoints. The lexicon detail page also shows which scripts target each lexicon, with links to create or edit them.

**If you're upgrading from v2.x to v2.9:** existing index hooks and lexicon scripts will be migrated to the new system automatically.

Full docs: [Record & Label Scripts](/guides/label-scripts), [Lua Scripting](/guides/lua-scripting), [Admin API — Scripts](/api-reference/admin/scripts).

## Backfill, but concurrent

The biggest change is that PDS resolution and record fetching now run concurrently. Previously, HappyView resolved every DID's PDS endpoint before it started fetching any records. For large backfills with hundreds of thousands of DIDs, that meant the fetcher sat idle for potentially hours. Now fetching starts as soon as the first DIDs are resolved and runs alongside resolution for the rest of the job.

On top of that:

- **Pause and resume** — you can now manually pause a running backfill and pick it back up later. No lost progress.
- **Concurrency settings** — new settings in the dashboard let you tune PDS concurrency, DID concurrency per PDS, and PLC directory concurrency. HappyView will recommend a restart if the settings require a larger connection pool than the one currently running.
- **Concurrent collection discovery** — the repo discovery phase now runs multiple collection queries in parallel instead of sequentially.
- **Batch record inserts** — record inserts are now batched, significantly reducing database round trips during the fetch phase.
- **Separate connection pool** — backfill jobs now use their own database connection pool so they can't starve the main app of connections during heavy backfills.

The backfill details view got a significant overhaul: progress indicators are more detailed and more accessible.

## `db.query` filters

You can now filter records directly in `db.query` without writing raw SQL or post-processing in Lua:

```lua
local result = db.query({
  collection = "com.example.post",
  filter = { field = "status", value = "published" },
})
```

Filters support comparison operators (`=`, `!=`, `>`, `<`, `>=`, `<=`), `AND`/`OR` groups, and nesting up to 5 levels deep. Field paths use the same dot notation and array indices as `sort` (e.g. `author.handle`, `scores[0]`).

```lua
local result = db.query({
  collection = "com.example.post",
  filter = {
    combine = "AND",
    { field = "status", value = "published" },
    { field = "views", op = ">", value = 100 },
  },
})
```

Full docs are in the [Database API reference](/api-reference/lua/database-api).

## Auth fixes

These were bugs that have been making my life hard, but I _finally_ figured out what was causing them.

First, users were basically limited to one auth session per client. If you signed into [Cartridge](https://cartridge.dev) from a second device, it would kill your other auth session. Whoops.

Second, there were scenarios where the PDS may refresh the auth session while a HappyView XRPC was in-progress. If that happened, HappyView would handle it internally so any other requests in that XRPC worked, but _it didn't return the refreshed tokens to the client._ Follow up requests from the client would break. Double whoops.

Both of these are fixed properly now, AND I added a couple new endpoints so clients can allow users to see and manage their active sessions:

- `GET /oauth/sessions/{did}/devices` — list all active sessions
- `DELETE /oauth/sessions/{did}/devices/{session_id}` — revoke a session

The existing `DELETE /oauth/sessions/{did}` endpoint still works: confidential clients revoke all device sessions for the user, and public clients revoke the session matching their DPoP key. Full details in the [Authentication guide](/getting-started/authentication#6-managing-device-sessions).

## SDK fix

If you tried to use `@happyview/oauth-client` with the latest versions of the `@atproto/*` SDKs, things would break because of a missing parameter. `@happyview/oauth-client` now provides that parameter and should also be backwards-compatible.

## CI & infrastructure

- **Binary releases** — Rust binaries are now published to GitHub Releases alongside Docker images, so you can grab a prebuilt binary directly if you're not into Docker.

## Contributors

**Huge thanks** to [Chris Pardy](https://bsky.app/profile/chris.pardy.family) for doing the bulk of the work for the new script system. 🥰

## Go play

Full changelog is on [GitHub](https://github.com/gamesgamesgamesgamesgames/happyview/releases/tag/v2.9.0). If you have questions, feature requests, or just need a little help, join the [Cartridge](https://cartridge.dev) [Discord Server](https://discord.gg/BUPnjaBwRZ) and hop into the `#happyview` channel.

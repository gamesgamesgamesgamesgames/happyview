---
title: "HappyView v2.8"
description: "Backfill cancellation, a rebuilt permission system, and a brand new docs site."
date: 2026-05-14
author:
  name: "Trezy"
  avatar: "/authors/trezy.webp"
tags:
  - announcements
atUri: "at://did:plc:qneu5uamqs6cug7sjobbaexm/site.standard.document/3mpokm3dl4t2j"
---

v2.8 is mostly bug fixes, but Backfills did get a serious overhaul. The permissions page was also rebuilt to be much more useful, and the docs site has a whole new look.

## Backfills, but like way better

Backfills have always _worked_, but at the same time they've been a little quirky. v2.8 rewrites most of the backfill pipeline:

- **Cancellation** — you can now cancel a running backfill. Cancellation is two-phase: the UI requests it, and the worker picks it up at the next checkpoint so nothing gets left in a half-finished state.
- **Resume** — backfills that get interrupted (server restart, crash) now properly resume where they left off instead of silently failing.
- **Retry logic** — rate limits from PDS endpoints and the PLC directory are handled with proper backoff using `RateLimit-Reset` headers, with a capped `retry-after` fallback.
- **Progress tracking** — the dashboard does a better job of communicating a backfill job's current stage (discovering repos, resolving PDS endpoints, fetching records), and it updates much more frequently than it used to.

## Permission system rebuild

The permission system had a few rough edges. Ghost permissions ([#23](https://github.com/gamesgamesgamesgamesgames/happyview/issues/23)), and some permissions couldn't be toggled after user creation ([#24](https://github.com/gamesgamesgamesgamesgames/happyview/issues/24)). Both are fixed, and the dashboard for managing permissions got a massive refresh.

## New docs site

The documentation [happyview.dev](https://happyview.dev) has been completely rebuilt on [Fumadocs](https://fumadocs.vercel.app). It's faster, has proper search, multi-language code examples, and a vaporwave theme because fuck yeah. There's also a [blog](/blog) now. You're reading it, and you're welcome.

Oh, the vaporwave theme can be... well it's a lot. The animations, gradients, glows... It does, however, respects `prefers-reduced-motion`, and I've added a toggle to the sidebar to disable motion if you can't or don't otherwise want to enable your `prefers-reduced-motion` setting.

## Roll that beautiful ~~bean~~ HappyView footage

Full changelog is on [GitHub](https://github.com/gamesgamesgamesgamesgames/happyview/releases/tag/v2.8.0). If you have questions, feature requests, or just need a little help, join the [Cartridge](https://cartridge.dev) [Discord Server](https://discord.gg/BUPnjaBwRZ) and hop into the `#happyview` channel.

---
title: "HappyView v2.6 + v2.7"
description: "Base path support, a Node.js SDK, security hardening, and a pile of quality-of-life fixes."
date: 2026-05-13
author:
  name: "Trezy"
  avatar: "/authors/trezy.webp"
tags:
  - announcements
atUri: "at://did:plc:qneu5uamqs6cug7sjobbaexm/site.standard.document/3mpokm3g6zm22"
---

Two releases in two days. v2.6, followed closely by v2.7. Neither one has a single marquee feature like [Permissioned Spaces](/blog/happyview-2.5), but together they make the whole system noticeably more solid.

## Base path support

HappyView can now run on a subpath to make it easier to use behind a reverse proxy. If your setup already serves something at the root and you want HappyView at `/appview` or `/api`, that just works now. The dashboard, OAuth flows, and XRPC routes all respect the configured base path.

## Node.js SDK

v2 shipped with browser and generic OAuth client packages. v2.6 added the code for a Node.js-specific OAuth client, and v2.7 published it to npm:

- [**`@happyview/oauth-client-node`**](https://npmx.dev/package/@happyview/oauth-client-node) — For server-side Node.js applications that need to authenticate against a HappyView instance.

This rounds out the SDK story. You've got [`@happyview/oauth-client-browser`](https://npmx.dev/package/@happyview/oauth-client-browser) for the browser, [`@happyview/oauth-client`](https://npmx.dev/package/@happyview/oauth-client) for custom or low-level work, and now `oauth-client-node` for server-side apps. All three SDKs were also updated in v2.6 to more closely match their [`@atproto`](https://github.com/bluesky-social/atproto) counterparts, so if you're already using the official SDK the APIs should feel familiar.

## Security hardening

v2.6 includes a batch of security fixes:

- **Privilege escalation prevention** — closed a path that could allow unauthorized permission changes.
- **JWT expiry precision** — tokens are now rejected at the exact expiry second, not after.
- **Rate limiting before rejection** — unauthenticated procedure requests are rate-limited before being rejected, preventing abuse of error responses.
- **Client key enforcement** — rate limiting is now enforced on all XRPC routes, not just a subset.
- **Space credential scoping** — Bearer space credentials are now restricted to space XRPC routes only.

## Everything else

- **TID functions** in Lua scripting for generating and working with [TIDs](https://atproto.com/specs/record-key#record-key-type-tid).
- **XRPC proxy settings** in the dashboard for controlling proxy behavior.
- **Experiments page** in the dashboard for toggling feature flags (like the one behind Permissioned Spaces).
- **Dead letter fixes** — dead letters now always get an ID, fixing an issue where some could be created without one ([#20](https://github.com/gamesgamesgamesgamesgames/happyview/issues/20)).
- **Space pagination** — cursors now work correctly when paginating through spaces.

## Go get some

Full changelogs: [v2.6.0](https://github.com/gamesgamesgamesgamesgames/happyview/releases/tag/v2.6.0), [v2.7.0](https://github.com/gamesgamesgamesgamesgames/happyview/releases/tag/v2.7.0). If you have questions, feature requests, or just need a little help, join the [Cartridge](https://cartridge.dev) [Discord Server](https://discord.gg/BUPnjaBwRZ) and hop into the `#happyview` channel.

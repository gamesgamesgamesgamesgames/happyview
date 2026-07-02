---
title: "Releasing HappyView 2 Into the Wild"
description: "One binary, no companion services, and a lot fewer moving parts."
date: 2026-04-24
author:
  name: "Trezy"
  avatar: "/authors/trezy.webp"
tags:
  - announcements
atUri: "at://did:plc:qneu5uamqs6cug7sjobbaexm/site.standard.document/3mpokm3ld5m2v"
---

I originally built HappyView because I needed it for my own [atproto](https://atproto.com) project, [Cartridge](https://cartridge.dev). v2 is still built for my needs, but now I can hand it to someone else without apologizing.

## If you're new here

HappyView is a general-purpose [AppView](https://atproto.com/guides/glossary#app-view) for atproto. Upload your [lexicons](https://atproto.com/guides/glossary#lexicon) and it gives you a fully functional [XRPC](https://atproto.com/guides/glossary#xrpc) API. It also provides a scripting interface that can be used to process records during ingestion, as well as customizing your XRPC handlers.

## What's new

HappyView v2 is a _massive_ upgrade. Bug fixes, new features, and an interface overhaul across the board.

### Fewer moving parts

HappyView v1 required setting up HappyView itself, a [Tap](https://github.com/bluesky-social/indigo/tree/main/cmd/tap) server for ingestion, and an [AIP](https://github.com/graze-social/aip) server for handling auth. All three had to be deployed, configured, and kept in sync, which was a lot of moving parts. HappyView v2 completely absorbs Tap and AIP. Real-time indexing and backfill were fully owned by Tap in v1, but now run as background tasks inside v2. AIP used to handle auth, but now OAuth with DPoP is built natively into v2.

There are no more companion services to stand up.

### More databases

SQLite is now the default so most deployments only require a single binary to run the entire AppView. Postgres is still fully supported for large-scale deployments. A single adapter translates between the two at runtime so your scripts work against either database without modification. That means a fresh HappyView deployment needs no external database, no connection string, and no compose file to run.

### Authorize meeeeeee

HappyView auth has its own system for managing DPoP-bound access tokens, which makes it easy for developers to use the existing atproto SDKs to integrate with HappyView.

That's right: apps built against atproto don't need a separate auth integration to work with a HappyView instance.

Three new npm packages ship alongside to allow existing atproto SDKs and [`@atproto/lex`](https://npmx.dev/package/@atproto/lex) to integrate directly with HappyView:

- [**`@happyview/lex-agent`**](https://npmx.dev/package/@happyview/lex-agent) - Allows [`@atproto/lex`](https://npmx.dev/package/@atproto/lex) to work directly with HappyView.
- [**`@happyview/oauth-client-browser`**](https://npmx.dev/package/@happyview/oauth-client-browser) - Integrates directly with [`@atproto/oauth-client-browser`](https://npmx.dev/package/@atproto/oauth-client-browser) for applications using the classic SDK.
- [**`@happyview/oauth-client`**](https://npmx.dev/package/@happyview/oauth-client) - For use with [`@atproto/oauth-client`](https://npmx.dev/package/@atproto/oauth-client) when building custom clients or doing more low-level work.

### Plugins: The Beginning

v2 introduces a WebAssembly plugin system for extending HappyView without the need to fork it. Plugins run sandboxed with fuel limits, declare their required secrets and auth type up front, and can be installed from anywhere, including an official registry. Plugins currently only support third-party auth, but they'll eventually be expanded to support many more features, like caching, backfill and real-time, and ingestion. The goal is to keep HappyView small and focused at its core while letting operators build exactly the AppView they need.

### And much more...

Also new:

- **Labeler integration** built in. Subscribe to labelers over WebSocket, store labels locally.
- **Dead letter queue** for failed index hooks, with retry, re-index, and dismiss actions.
- **Rate limiting** configurable per API client with token buckets.
- **Permission system** with 29 granular permissions across four templates.
- **API client management**, **instance branding**, and **multi-domain support** in the dashboard.
- **Expanded Lua scripting** with new APIs for database queries, HTTP and XRPC calls, and atproto operations.

## Go get some

If you're currently running HappyView v1, breaking changes are listed in the [changelog](https://happyview.dev/reference/changelog). Full documentation is at [happyview.dev](https://happyview.dev). If you're running v1, the [migration guide](https://happyview.dev/guides/upgrading-to-v2) walks through the process.

I hope y'all enjoy the release, and make sure to ping me if you're building something cool with it! ✌️

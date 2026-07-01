---
title: "HappyView v2.10"
description: "Service identity, permissioned spaces, and new blob utilities."
date: 2026-06-30
author:
  name: "Trezy"
  avatar: "/authors/trezy.webp"
tags:
  - announcements
---

This one's been a long time coming. HappyView finally has a real AT Protocol identity, can do service proxying, and permissioned spaces got a big update to match the official spec.

## Service identity

When a user's PDS routes a request to your AppView, it resolves the destination by looking up your DID. Without a service identity, that lookup fails and standard atproto routing can't reach you.

HappyView offers three modes:

- **Domain identity (did:web)** - Your domain name becomes your identity. HappyView generates a signing keypair and serves a DID document at `/.well-known/did.json` automatically. The simplest option.
- **Network identity (did:plc)** - Registers a new identity in the PLC directory. This is the most durable option — it survives domain changes if you ever need to migrate.
- **Linked account** - Link your AppView to an existing AT Protocol account.

With a service identity in place, HappyView can act as a service proxy. A PDS sends a request with an `atproto-proxy` header pointing at your AppView, HappyView verifies the caller via service auth, runs your XRPC handler, and responds. This is how atproto apps are _supposed_ to work! Up to this point HappyView only supported direct connections via DPoP.

Full docs: [Service Identity](/getting-started/service-identity).

## Permissioned spaces alignment

This is the big one. The spaces implementation now aligns with [Dan's proposal](https://github.com/bluesky-social/proposals/pull/94), and if you were using the experimental spaces API before, this is a breaking change.

### Namespace split

Endpoints moved from `dev.happyview.space.*` to two namespaces:

- **`com.atproto.space.*`** - protocol-level routes (queries, data access, credentials)
- **`com.atproto.simplespace.*`** - management routes (create/update/delete spaces, membership)

The old `dev.happyview.space.*` endpoints will work as aliases until HappyView v3.

### Access model rewrite

The old `accessMode` / `appAllowlist` / `appDenylist` system is gone.

**Mint policy** controls who can create permissioned repos in a space:

- `member-list` (default) - only members
- `public` - anyone
- `managing-app` - only the managing app

**App access** controls which third-party apps can interact with the space:

- `open` (default) - any app
- `allowList` - only explicitly listed apps

Also, `getMemberGrant` is now `getDelegationToken` (and it's a `GET`, not a `POST`).

### New concepts

- **Authority DID** replaces `owner_did`. There's also a new `creator_did` for tracking who originally created the space
- **`read_self` access level** - members can only read their own data within the space
- **Deniable commit signatures** - per-user repo state uses LtHash (homomorphic set-hash) with deniable signatures. The user signs context (space + rev + random input keying material), not content
- **Record operation log** - `listRepoOps` returns the oplog for sync
- **Write notifications** - `registerNotify`, `notifyWrite`, `notifySpaceDeleted`

Full docs: [Permissioned Spaces](/experimental/spaces/).

## Blob utilities for Lua

Two new Lua functions — `atproto.blob_download` and `atproto.blob_upload` — let scripts perform some new magic. For example, you can migrate a blob one PDS to another in just a couple of lines:

```lua
local downloaded = atproto.blob_download(source_did, old_cid)
local uploaded = atproto.blob_upload(downloaded.handle, downloaded.mimeType)
local new_blob_ref = uploaded.blob
```

Full docs: [atproto API (`blob_download` / `blob_upload`)](/api-reference/lua/atproto-api#atprotoblob_download).

## Prefixed database tables

All HappyView tables are now prefixed with `happyview_` (e.g. `records` -> `happyview_records`) so they won't collide with your own tables if you're sharing a database. Existing databases are migrated automatically.

If you use `db.raw()` in Lua scripts to query HappyView tables directly, you'll need to update your queries to use the prefixed names.

## Everything else

- **Setup wizard hardening** - the setup flow handles edge cases better, especially around re-auth and preventing unauthenticated redirects
- **Dynamic cookie security** - cookies now set their security flags based on the request context, which fixes some issues with service proxying behind a reverse proxy
- **Bluesky PDS scope handling** - fixed a compat issue with the scope format Bluesky's PDS returns during OAuth

## Go play

Full changelog is on [GitHub](https://github.com/gamesgamesgamesgamesgames/happyview/releases/tag/v2.10.0). If you have questions, feature requests, or just need a little help, join the [Cartridge](https://cartridge.dev) [Discord Server](https://discord.gg/BUPnjaBwRZ) and hop into the `#happyview` channel.

---
title: "HappyView v2.12"
description: "Linked repos, SQLite disk reclamation, and catching up on Permissioned Data."
date: 2026-08-04
author:
  name: "Trezy"
  avatar: "/authors/trezy.webp"
tags:
  - announcements
---

Two big things this time. Your instance can now act on behalf of other atproto accounts without having to give it an app password, and SQLite instances can finally get their disk space back after a big delete. We're also caught up with the latest permissioned data diary, and there's a healthy pile of fixes to top it all off.

## New feature: ✨ Linked Repos ✨

**Linked Repos** allows any number of atproto accounts to be linked to a HappyView instance, giving the instance permission to act on behalf of those repos from scripts.

You may be wondering why you would ever want to link a repo to your instance, and it's a fair question. To be honest I built the feature specifically for my own project, [Cartridge](https://cartridge.dev), so let me lay out what I'm using it for there:

- **Verifications.** I'm building Cartridge to pick up where my [Games Industry Labeler (@ozone.birb.house)](https://bsky.app/profile/ozone.birb.house) left off. Cartridge issues proper verifications for atproto accounts, which need to be created in the [@cartridge.dev](https://taproot.at/uri/at://did:plc:4mrwcmxk266itsdn33leqljq) repo.
- **Pentaract central repository.** Cartridge pulls games data from several sources and stores it all in the [@gamesgamesgamesgames.games](https://taproot.at/uri/at://did:web:gamesgamesgamesgames.games) repo.
- **Cartridge community contributions.** Cartridge supports wiki-style contributions from the community. Those contribution records are created in the user's repo, then reviewed by the Cartridge moderation team. Once a contribution is approved, it's merged into the Pentaract central repo. This whole process requires access to several different repos at different points in time.

All of this functionality is possible today, but it requires saving an atproto id and app password in HappyView, then creating and managing a session every time a script runs. Worse, the repos are completely vulnerable because they're using app passwords, giving complete control to any script.

Linked Repos allows those accounts to be authorized over OAuth, with their sessions refreshed automatically in the background. Better yet, because it's using OAuth, you can limit the blast radius of a destructive script by adding limited scopes when linking the account.

Full docs: [Linked Repos](/guides/linked-repos), [Admin API — Linked Repos](/api-reference/admin/linked-repos), [Lua API — `linked_repos`](/api-reference/lua/linked-repos-api).

## Permissioned data update

[Dan](https://bsky.app/profile/dholms.at) released [_Permissioned Data Diary 7_](https://dholms.leaflet.pub/3mqtqvjidqs2p) a couple of weeks ago, and we're now officially caught up! The only major change for HappyView was the removal of asymmetric signatures.

Also, it seems we're still unsure of whether the final spec will use `at://` or `ats://` URIs, so HappyView will support both for the foreseeable future.

## SQLite optimization

I really didn't know much about SQLite when I first built HappyView. I knew that it seemed a lot simpler than PostgreSQL, and I knew a lot of folks building on atproto really loved it. I also knew that using it would allow me to reduce HappyView v2 to a single binary. We've now had support for both databases since HappyView v2.0.0.

Last week, [@bnb.im](https://bsky.app/profile/bnb.im) reached out to me about the ballooning SQLite file behind their HappyView instance. They were seeing a steady growth in disk usage, even though they were mass deleting records via the HappyView dashboard.

I dug in and learned that SQLite is not particularly awesome at optimizing its space on disk, _especially_ when you have a huge SQLite database. Deleting records returns those pages to an internal freelist, but it doesn't release that space back to the system. The result is that deleting 20GB of records in a 30GB database means you still have a 30GB database, just writing to the free space for a while.

HappyView now implements several SQLite-specific features to resolve this:

- **Incremental vacuum.** SQLite has a feature called `VACUUM`. It's basically an old school defrag: it rebuilds the database file and releases unused space back to the system. Fortunately, it also includes support for _incremental vacuum_, which will handle cleaning up after deletes without needing to rebuild the entire database.
- **Batched deletes.** Deletes were previously run as a single, massive query. This is fine in Postgres where it handles a lot of the hard parts for you, but it causes a lot of issues in large SQLite databases: every page the delete dirties piles up in the write-ahead log until the whole thing commits, so a huge delete actually _inflates_ your disk usage before it reduces it, and it holds the write lock the entire time. Deletes now run in batches of 5,000, each committing on its own. That lets the WAL checkpoint between batches, and it lets Jetstream keep ingesting instead of queueing up behind a delete that runs for an hour. When the delete finishes — or when you cancel it partway through — HappyView runs an incremental vacuum to hand the freed pages back to the system.
- **`VACUUM` anyway.** Here's the catch: `PRAGMA auto_vacuum = INCREMENTAL` only takes effect on an _empty_ database. So on a HappyView instance created before this update, incremental vacuum does nothing at all — not for old deletes, and not for new ones — until a full `VACUUM` has been run once. However, a full `VACUUM` rebuilds the entire database into a new file, so you need room for a second copy: roughly 1.2× your database size, or 2.2× if your temp directory lives on the same filesystem as the database. It's also an intensive operation that locks up the whole database. To make it feasible anyway, I've added a one-time prompt that will take you to the database settings page, help you understand if you're in a place to run a `VACUUM`, and then allow you to schedule the process for your next restart.

<Callout type="warn">
Keep in mind that the time it takes for `VACUUM` to run scales with the size of your database. If your database is large, or you have lots of users, I _strongly_ recommend scheduling and restarting your AppView only during a low-traffic time to minimize the impact of an outage.
</Callout>

## Other stuff

### Minor polish

- **Huge updates to scope builder.**
  The scope builder in the dashboard now includes helpful controls for setting a scope prefix, as well as setting `action` values on `repo:` scopes.
- **More descriptive errors.**
  When writing to an unauthorized repo, HappyView will now return descriptive errors instead of the confusing DPoP token error.
- **`verify_signature` no longer conflates "invalid" with "uncheckable".**
  `atproto.verify_signature` still returns `false` when it checked a signature and the signature didn't match. But when it _couldn't_ check — malformed signature bytes, a missing field, a record that won't encode — it now raises instead of quietly returning `false`, so a fault in your data can never present itself to a script as "this user forged their records". If you want the old behavior, wrap the call in `pcall`.

### Bug fixes

- **Fixed a deadlock that could occur during backfill.**
  [insert "nobody understands semaphores" meme here]
- **Fixed the jobs system when using SQLite.**
  An issue with `inherit_auth` caused all jobs to stall on HappyView instances using SQLite.
- **Permissioned spaces now use proper CIDs.**
  It turns out our original CID generation code had some minor issues, so the CIDs being generated weren't strictly valid. This has been fixed, and HappyView will automatically backfill your local indices with valid CIDs.
- **Prevent Jetstream deletes from being skipped.**
  If a lexicon didn't have a custom script, deletes would be skipped.
- **Prevent CBOR encoding from invalidating attestations.**
  The way we generated attestations was incorrect, tho our attestation validation code was written to handle it appropriately. Old attestations will not be verifiable by other applications, but HappyView will validate both old and new attestations.
- **Fix base64 attestation verification.**
  It turns out base64 padding is optional in atproto, and Jetstream is returning base64 bytes unpadded (e.g. without `==` on the end). HappyView now supports base64 bytes either padded _or_ unpadded, so attestation verification works properly.
- **Only update `indexed_at` when it comes from the network.**
  HappyView maintains an `indexed_at` field on all records in the local index. When creating a new record, HappyView saves it to the local index with `indexed_at = null`. However, update operations were setting `indexed_at` in the local index when they saved. These have been fixed, so now `indexed_at` is only ever updated when a record is received from the network.
- **Logouts work again.**
  There were some scenarios where log out would just fail. This is fixed, and the `logout` methods in the client SDKs have been updated to make an effort to cleanup auth details even when logout fails.

### Security fixes

Three advisories, all in dependencies:

- **[RUSTSEC-2026-0222](https://rustsec.org/advisories/RUSTSEC-2026-0222) / [GHSA-hgjw-h833-99q9](https://github.com/bytecodealliance/wasmtime/security/advisories/GHSA-hgjw-h833-99q9)** - "Stores can mix up type indices between engines" (CVSS 3.8, low) in `wasmtime`. Fixed by moving to 36.0.13, which also bumps everything that ships alongside it: the `cranelift` crates, `pulley-interpreter`, `pulley-macros`, `winch-codegen`, and the `wasmtime-internal-*` crates.
- **[RUSTSEC-2026-0221](https://rustsec.org/advisories/RUSTSEC-2026-0221)** - an unsoundness in `event-listener` where a `!Send` tag could cross a thread boundary via `StackSlot` and cause a data race in safe code. Fixed in 5.4.2, which also drops `concurrent-queue` from HappyView's dependency tree entirely.

## Contributors

Huge thanks to the following users and contributors that helped me identify various issues:

- [Tierney (@bnb.im)](https://bsky.app/profile/bnb.im) for putting HappyView through its paces and identifying several issues, including the ballooning SQLite issue and several backfill scenarios that weren't well-handled.
- [Florian (@flo-bit.dev)](https://bsky.app/profile/flo-bit.dev) for helping identify the backfill deadlock issue.

## Go play

Full changelog is on [GitHub](https://github.com/gamesgamesgamesgamesgames/happyview/releases/tag/v2.12.0). If you have questions, feature requests, or just need a little help, join the [Cartridge](https://cartridge.dev) [Discord Server](https://discord.gg/BUPnjaBwRZ) and hop into the `#happyview` channel.

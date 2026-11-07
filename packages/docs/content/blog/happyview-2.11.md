---
title: "HappyView v2.11"
description: "Background jobs, repo export, and spaces refinements."
date: 2026-07-07
author:
  name: "Trezy"
  avatar: "/authors/trezy.webp"
tags:
  - announcements
---

Big one this time. HappyView has a proper background job system now, spaces can export repos as CAR files, and the whole spaces implementation got a round of refinements to better match the proposal.

## Background jobs

Sometimes a script needs to run for longer than a single request. Maybe you're processing a big batch of records, running a migration, or polling an external service. Until now you'd have to hack around that with clever scripting, but not anymore!

Any script can now queue a background job with `jobs.create()`, and HappyView's worker picks it up and executes the matching `job.run:<type>` script. Jobs support pause, resume, and cancel, **and** they survive server restarts. The worker reclaims orphaned jobs on startup so nothing gets lost.

Inside a job script you get a `job` global with everything you need:

```lua
job.progress({ processed = count, total = total })

if job.should_stop() then
  return
end

job.wait(5)
```

Jobs can also inherit the caller's PDS auth session by passing `{ auth = true }` to `jobs.create()`, so they can make authenticated atproto calls on behalf of the user who kicked them off.

There's a new dashboard page at `/dashboard/jobs` where you can filter by status, inspect input/progress/result, and manage running jobs. Scripts are authored in the script editor by selecting "Job" as the trigger source and adding a job type.

Full docs: [Background Jobs](/guides/background-jobs).

## Space repo export

As per the [Permissioned Data proposal](https://github.com/bluesky-social/proposals/tree/main/0016-permissioned-data), we've added a new endpoint for spaces: `com.atproto.space.getRepo`. It exports a user's permissioned repo as a CAR v1 file with two roots: the signed commit and a DRISL index of all records. The container format is standard CAR v1, but the internal structure is different from a regular atproto repo. Spaces use a flat sorted index instead of a Merkle Search Tree, so standard repo tooling won't parse the tree structure out of the box.

The related `getRepoState` has been renamed to `getLatestCommit` to better reflect what it actually returns. The old name still works as an alias.

`listRepoOps` now inlines record values by default. If you only need metadata, pass `excludeValues=true` to skip the join and get a lighter response.

## Spaces refinements

### `at://` URIs

Space URIs switched from the `ats://` scheme to standard `at://` URIs with a `space` path segment:

```
at://{did}/space/{type}/{skey}/{author}/{collection}/{rkey}
```

The old `ats://` format is still accepted and automatically rewritten, so nothing breaks. But going forward, spaces use the same URI scheme as everything else in the AT Protocol. You should still update your code and migrate old ats:// URIs, because this automatic rewriting _will be removed_ in HappyView v3.

### Commit versioning

Signed commits now carry a `ver` field (currently `1`) and the signature context includes the author DID alongside the space URI, rev, and IKM. This means commits from v2.10 are _not_ compatible; existing spaces will need their repo state regenerated.

### Better validation

Space routes validate more aggressively now. Stricter checks on credential verification, membership requirements, and CAR serialization inputs. Edge cases that previously returned confusing 500s now return proper 400-level errors with messages that actually tell you what went wrong.

## Go play

Full changelog is on [GitHub](https://github.com/gamesgamesgamesgamesgames/happyview/releases/tag/v2.11.0). If you have questions, feature requests, or just need a little help, join the [Cartridge](https://cartridge.dev) [Discord Server](https://discord.gg/BUPnjaBwRZ) and hop into the `#happyview` channel.

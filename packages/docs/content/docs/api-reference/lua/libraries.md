---
title: "Libraries"
---

A library is a WASM plugin a script loads with `require`. Each has a namespace under `happyview.`, declares the [capabilities](../../guides/developing-plugins.md#capabilities) it needs, and documents its own surface in its README. HappyView ships none of them: every library is installed from the [plugins repository](https://github.com/happyproto/plugins) through **Settings > Plugins** or the [admin API](../admin/plugins.md), and a script that requires one that is not installed fails to load.

| `require` | Capabilities | Purpose | Surface |
| --- | --- | --- | --- |
| `happyview.db` | `records:read` | Read indexed records: a chainable query per collection, one record by URI, substring search, and the backend name | [README](https://github.com/happyproto/plugins/tree/main/plugins/happyview/db) |
| `happyview.sql` | `database:read`, `database:write` | Raw SQL, and a chainable builder over the operator's own tables | [README](https://github.com/happyproto/plugins/tree/main/plugins/happyview/sql) |
| `happyview.backlinks` | `records:read` | Records whose strong refs point at a given AT URI | [README](https://github.com/happyproto/plugins/tree/main/plugins/happyview/backlinks) |
| `happyview.record` | `caller:write`, `records:read`, `records:write` | Record writes as the calling user, blob uploads, direct local-index writes, and lexicon validation | [README](https://github.com/happyproto/plugins/tree/main/plugins/happyview/record) |
| `happyview.xrpc` | `caller:read`, `caller:call` | XRPC queries and procedures as the calling user | [README](https://github.com/happyproto/plugins/tree/main/plugins/happyview/xrpc) |
| `happyview.atproto` | `atproto:read`, `attest:sign` | Service resolution, blob download, label lookup, and attestation signing and verification | [README](https://github.com/happyproto/plugins/tree/main/plugins/happyview/atproto) |
| `happyview.spaces` | `spaces:read`, `spaces:write` | Permissioned spaces: records, membership and invites | [README](https://github.com/happyproto/plugins/tree/main/plugins/happyview/spaces) |
| `happyview.linked_repos` | `linked_repos:use` | Record writes, blob uploads and XRPC calls through repos an admin has linked | [README](https://github.com/happyproto/plugins/tree/main/plugins/happyview/linked-repos) |
| `happyview.jobs` | `jobs:create` | Enqueue background jobs | [README](https://github.com/happyproto/plugins/tree/main/plugins/happyview/jobs) |
| `happyview.http` | `network:request:unrestricted` | Outbound HTTP: `get`, `post`, `put`, `patch`, `delete`, `head` | [README](https://github.com/happyproto/plugins/tree/main/plugins/happyview/http) |

The capability column is what an operator grants when installing the library, and it bounds what any script can do through it: `happyview.sql` with `database:write` can run any statement, `happyview.xrpc` with `caller:call` can invoke any procedure as the user. Install only what your scripts use.

## Conventions every library follows

**Envelopes.** Every read of an indexed record answers an envelope rather than the bare body:

```lua
{ uri, did, collection, rkey, cid, indexed_at, record }
```

`record` is the stored body verbatim. `cid` is `nil` for a row written only with `save_local`, and `indexed_at` is `nil` until the network has echoed the record; a write through `happyview.record` is in the index at once, with the PDS's `cid`. A page is `{ records, cursor }`, where `cursor` is an opaque string present only when more records exist.

**Chains.** A constructor such as `db.records(collection)` or `backlinks.to(uri)` returns an object whose lazy steps (`where`, `sort`, `limit`, `cursor`, `did`, `collection`) each return the same object, so they chain, and whose immediate calls (`run`, `count`, `first`) make the library call. A lazy step given `nil` is skipped, so `:limit(input.limit)` reads as no limit when the input has none. An object holds nothing between calls, so a library cannot keep a transaction open, and a script cannot hand a library a function.

**Acting as the caller.** A library acts as `ctx.caller_did`, and a call that needs the caller's PDS session succeeds only when [`ctx.has_pds_auth`](script-contract.md#ctx) is `true`. `happyview.linked_repos` is the exception: it acts as the linked repo, from any script kind, and the [grant's scopes](../../guides/linked-repos.md) bound it instead.

**Errors.** A library failure raises a Lua error prefixed with the library and function, `happyview-db.get: …`, carrying the error code the README lists (`BAD_INPUT`, `NO_SESSION`, `PDS_ERROR`, …). Catch one with `pcall` when the failure is expected.

**Protected tables.** `happyview.sql` runs against the same database as the index, so `raw` and `from` are refused on HappyView's internal `happyview_*` tables other than the public AppView data. The allowlist is in [Developing Plugins](../../guides/developing-plugins.md#database-access).

## Next steps

- [Script Contract](script-contract.md): `handle(input, ctx)` and the `require` order
- [Built-in Modules](built-in-modules.md): the `internal.*` modules the runtime provides itself
- [Developing Plugins](../../guides/developing-plugins.md#library-plugins): how a library is built and how `require` renders its surface

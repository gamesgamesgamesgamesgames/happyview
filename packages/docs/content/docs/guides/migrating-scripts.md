---
title: "Migrating Scripts to v3"
---

v3 removes the globals (`params`, `caller_did`, `event`, `db`, `Record`, `now()`, …) a v2 script reached HappyView through. Everything moves onto the `handle(input, ctx)` contract and `require("internal.*")` / `require("happyview.*")` modules.

A codemod does the rewrite for you — from the script editor or a CLI — and leaves anything it can't rewrite mechanically as a `-- codemod:` comment for you to finish by hand.

## What changes

**Contract, every kind:** `caller_did` → `ctx.caller_did`; `delegate_did` → `ctx.delegate_did`; `method` → `ctx.method`; `env` / `env.X` → `ctx.env` / `ctx.env.X`; `space.space` → `ctx.space.uri`, `space.space_id` → `ctx.space.id`, and every other `space.K` → `ctx.space.K`.

**Per kind**, the same free name means something different, so the rewrite depends on the script's trigger:

| Kind | Old | New |
|---|---|---|
| Procedure | `input` | `input` — the name is unchanged, but it is now `handle`'s first parameter rather than a global, so a read outside `handle` is [hoisted](#hoisting-input-and-ctx) |
| Procedure | `params` | `ctx.params` |
| Procedure, Query | `collection` | `ctx.collection` |
| Query | `params` | `input` |
| Record event | `event` | `input` |
| Record event | `record`, `action`, `uri`, `did`, `collection`, `rkey` | `input.record`, `input.action`, `input.uri`, `input.did`, `input.collection`, `input.rkey` |
| Label | `event` | `input` |
| Label | `src`, `uri`, `val`, `neg`, `cts`, `exp` | `input.src`, `input.uri`, `input.val`, `input.neg`, `input.cts`, `input.exp` |
| Job | `job.input` | `input` |
| Job | `job.id`, `job.progress`, `job.should_stop`, `job.wait` | `ctx.job.id`, `ctx.job.progress`, `ctx.job.should_stop`, `ctx.job.wait` |
| Job | `job.log(x)`, `job.warn(x)` | `log.info(x)`, `log.warn(x)` |

### Hoisting `input` and `ctx`

A contract rewrite sometimes needs to read `input` or `ctx` outside `handle`'s body — a module-level helper, say. When that happens the codemod adds `local input, ctx` right after the requires block (after any polyfills), and makes `handle`'s first line `input, ctx = handle_input, handle_ctx`, renaming `handle`'s own parameters to `handle_input, handle_ctx` so they don't collide with the hoisted locals. When every rewritten use stays inside `handle`, nothing is hoisted. A `handle` already declared `(input, ctx)` is hoisted the same way when a helper outside it needs the context, and a `handle(handle_input, handle_ctx)` whose `local input, ctx` declaration is missing gets it back.

A `handle` declared with parameters other than exactly `(input, ctx)` gets those parameters renamed: `function handle(evt)` becomes `function handle(input, ctx)` (or `function handle(handle_input, handle_ctx)` when a helper outside `handle` forces the hoist), and every use of `evt` in the body becomes `input`. A `handle` with more than two parameters is left as a marker instead — there's no place to route the extra parameters.

The hoist covers a read inside a function that runs after `handle` has been called. A read at file scope runs while the script loads, when even a hoisted `input` and `ctx` are still `nil`, so it gets [marker 16](#markers) instead. A function that reads them and is itself called at file scope fails the same way and is not marked, so check for one by hand.

**Built-ins** move to [`internal.*` modules](../api-reference/lua/built-in-modules.md): `log(x)` → `log.info(x)`; `TID()` → `tids.create()`; `json.K` → `json.K` on `require("internal.json")`; `toarray(x)` → `json.to_array(x)`. `now()` is the one exception — see below. `TID` conversions other than construction are markers; see below.

**Libraries** move to `happyview.*` modules with a chained, table-free call shape:

- `db.backend` is unchanged. Every v3 read answers an envelope, `{uri, did, collection, rkey, cid, indexed_at, record}`, where v2 answered the record body with `uri` written onto it, so each read passes through the inlined row-shape polyfill (see [Polyfills](#polyfills)): `db.get(u)` → `__codemod_flat(db.get(u))`; `db.search{...}` → `({ records = __codemod_flat_rows(db.search(collection, field, query, limit)) })`, the wrapper keeping v2's `.records` shape (a `db.get` or `db.search` call standing alone as a statement has no result to shape and is left unwrapped); `db.count(c[, did])` → `db.records(c):count()` / `db.records(c):did(did):count()`; `db.query{...}` → `__codemod_flat_page(db.records(collection):where(field, op, value):sort(sort, direction):limit(n):cursor(c):did(d):run())`, each step present only if the corresponding key was given. A lazy step (`where`, `sort`, `limit`, `cursor`, `did`, and backlinks' `collection`) given `nil` is dropped rather than kept as a no-op call, so `:limit(input.limit)` reads as no limit when the input has none.
- `db.raw` → `require("happyview.sql").raw`
- `db.backlinks{...}` → `__codemod_flat_page(require("happyview.backlinks").to(uri):collection(c):did(d):limit(n):cursor(c):run())`
- `Record.load`, `Record.delete_local`, and a bare `Record(collection, table):save()` are handled by an inlined polyfill rather than a rename — see [Polyfills](#polyfills) below.
- `atproto.blob_upload` → `require("happyview.record").upload_blob`; `atproto.blob_download`'s result changes shape (see [Blob fields](#blob-fields) below); every other `atproto.*`, `linked_repos.*`, `jobs.*`, `http.*` name is unchanged, just moved behind `require("happyview.*")`
- `xrpc.query`/`xrpc.procedure` are also handled by an inlined polyfill, because the v3 library's success/failure shape differs from v2's — see [Polyfills](#polyfills).
- `atproto.spaces.*` → `require("happyview.spaces")`, with `is_member`/`get_access`/`list_members` becoming `spaces.get(uri):is_member(did)` / `:access(did)` / `:members()`, and a space handle's `:query{...}` becoming `:records{...}`
- `atproto.spaces.query{...}` → `spaces.query{...}`, with its `space_uri` filter key becoming `uri`
- `atproto.spaces.create{...}` and `atproto.spaces.accept_invite{...}` return a bare `uri` in v3 rather than the v2 handle, so both rewrite to `spaces.get((spaces.create{...}).uri)` / `spaces.get((spaces.accept_invite{...}).uri)` to keep the handle a script can call methods on.

A `local NAME = require("MODULE")` line is inserted once per module actually used, grouped after any leading comment. Running the codemod on an already-migrated script changes nothing. A re-run reports only markers on constructs it left untouched; a marker placed beside a completed rewrite keys on the v2 name that rewrite consumed, so it is not reported again. Treat exit `2` as a first-run signal and search the source for `-- codemod:` to find what is still open.

### The `now()` exception

`now()` becomes `time.to_iso8601(time.now())` — the same instant, but as millisecond-precision RFC 3339 with a `Z` suffix (`"2026-09-16T15:04:05.123Z"`) where the global wrote nanoseconds and a `+00:00` offset (`"2026-09-16T15:04:05.123456789+00:00"`). If a script, another system, or a test compares a stored timestamp against a freshly-generated one as a string, this format change can break that comparison even though the instant is identical.

## Polyfills

Three things in v2 have no direct v3 equivalent: two globals whose return shapes differ enough that a rename alone would be wrong, and the row shape every `db` read answered. For these, the codemod inlines a small shim reproducing the v2 behavior on top of the v3 library, rather than leaving a marker on every call site. Each shim is written once, at the top of the script, under a comment naming it as a polyfill to retire when convenient:

```lua
-- codemod polyfill: v2 row shape over happyview.db; replace with the library API when convenient
```

```lua
-- codemod polyfill: v2 Record over happyview.record; replace with the library API when convenient
```

```lua
-- codemod polyfill: v2 xrpc over happyview.xrpc; replace with the library API when convenient
```

- **row shape** — binds `__codemod_flat`, `__codemod_flat_page` and `__codemod_flat_rows`, and is written when the script reads through `db.get`, `db.query`, `db.search` or `db.backlinks`; its header names the library or libraries those reads go through, `happyview.db`, `happyview.backlinks`, or `happyview.db and happyview.backlinks`. A v3 read answers an envelope, `{uri, did, collection, rkey, cid, indexed_at, record}`, with the stored body verbatim under `record`; v2 answered the body with `uri` written onto it. `__codemod_flat(row)` returns the envelope's `record` with `uri` set to the record's own URI (`nil` for `nil`); `__codemod_flat_page(page)` maps `page.records` through it and keeps `cursor`; `__codemod_flat_rows(rows)` maps an array. Rows are replaced inside the array they arrived in, so an empty page still encodes as `[]`. To retire it, read `row.uri` and `row.record.title` off the envelope instead. As in v2, a stored top-level `uri` field is hidden on the flattened row by the record's own URI; the envelope's `record.uri` is where a script that needs it finds it.
- **`Record`** — reproduces the v2 `Record` API: the `Record(collection, fields)` constructor and its `Record.new(collection, fields)` spelling; `Record.load(uri)`, `Record.load_all(uris)`, `Record.save_all(records)`, `Record.delete_local(uri)`; and the instance methods `:save()`, `:delete()`, `:save_local()`, `:delete_local()`, `:set_rkey(r)`, `:set_repo(did)`, `:set_key_type(t)`, `:generate_rkey()`. It's built on `require("happyview.record")` and `require("internal.tids")`, so prefer those directly in new code — the shim exists only to keep an existing script's `Record` calls working unchanged. Easy to miss:
  - `:save()` and `Record.save_all` see their write in the local index at once, as in v2, because the v3 library mirrors every `create`, `put` and `delete` into the index itself; the row holds the PDS's `cid` and no `indexed_at` until Jetstream echoes it. `Record.save_all` answers `{uri, cid}` per record, which is what the v3 library returns, rather than the PDS's full response.
  - `:delete()` raises without a caller, and always removes the local row whatever else the PDS answered; a refusal or failure there is neither raised nor logged.
  - A write sends only what v2 sent: `_`-prefixed fields are dropped, and when the collection's lexicon declares `properties` (read through `record.lexicon(collection)`), so is every field it doesn't declare. The lexicon's record key also sets the key type `:generate_rkey()` and `:save_local()` mint from, so a `literal:` key is honoured.
  - `Record.load` reads the envelope: `_cid` is the envelope's `cid`, or `""` where the envelope carries none (a row written only with `:save_local()`), which is what v2 read off that row's empty cid column; `_collection` and `_uri` come from the envelope, and the stored body's fields, a top-level `uri` field included, land on the record. `:save()` sets `_cid` from that write; `:save_local()` leaves it as it was.
  - `Record.save_all` saves one record at a time, where v2 issued the writes concurrently.
  - The shim needs the `happyview-record` shipped with v3, the one that exports `lexicon`; on an older library the first `Record(...)` or `Record.load(...)` raises a message naming that export.
- **`xrpc`** — reproduces v2's `xrpc.query(nsid, params)` / `xrpc.procedure(nsid, input[, params])`, which returned `{status, body}` rather than raising. The shim calls `require("happyview.xrpc")` inside a `pcall` and translates: success becomes `{status = 200, body = json.encode(result)}`; failure becomes `{status = S, body = message}`, where `S` is read off an `XRPC_ERROR`/`PDS_ERROR` code in the error message when present, else `500`. On failure `body` is the error message, where v2's was the far side's response bytes, so a script that decoded a non-200 body has to change.

## Blob fields

A name bound from `atproto.blob_download(...)` changes shape: `.handle` becomes `.bytes`, and `.mimeType` becomes `.mime_type`. `atproto.blob_upload(a, b)` keeps its argument order — it becomes `record.upload_blob(a, b)` unchanged.

## Filter comparisons

A record filter compares by the **filter value's own JSON type**, and the codemod cannot rewrite this for you because whether a change is needed depends on what your records hold.

`{ field = "score", op = ">", value = 100 }` asks a numeric question and gets a numeric answer. `value = "100"` asks a different one: it matches a field holding the *string* `"100"`, not the number. A record body carries no schema, so the value is the only thing that can say which comparison was meant.

Two things to check in a migrated script:

- **A number quoted as a string.** `value = "150"` against a field your records store as a number now matches nothing. Drop the quotes. This is the change most likely to bite, because a mismatch is an **empty result, not an error** — there is nothing in the log to tell you the filter was the problem.
- **An ordering comparison.** `op = ">"` on a numeric field now compares numerically, so `> 100` no longer matches a record scoring 50. If a script relied on the old answer, it was relying on a text comparison in which `'50'` sorts above `'100'`.

Both readings are now the same on SQLite and Postgres. Before v3 they were not: a string filter against a stored number matched nothing on SQLite and everything on Postgres, so the strict rule is what SQLite already did and Postgres is the backend whose answer changes.

`like`, `not like` and `ilike` compare as text on both backends whatever the field holds, and are unaffected.

A table filter (`db.table`) compares against the **column's** type instead, which HappyView reads from the table rather than guessing. A value the column cannot hold — `"abc"` against an integer column — is now refused with an error naming the column, where v2 coerced it to something arbitrary on SQLite and failed inside the driver on Postgres.

## Markers

Some constructs have no mechanical equivalent. The codemod leaves them exactly as written, precedes them with a `-- codemod:` comment naming what to do, and lists them in its notes. **A script with markers is not finished** until you rewrite those lines by hand and remove the comment. Every marker reads `-- codemod: <what changed> -- <what to do>`.

These are the only markers the codemod emits, each substituting the specific name, dotted path, module, or trigger kind for the placeholder shown:

1. `db.query has options this rewrite cannot map -- rebuild it as a db.records(...) chain from require("happyview.db")` — a filter using `combine`, a nested list, or another key the `where`/`sort`/`limit`/`cursor`/`did` chain doesn't cover.
2. `db.search has options this rewrite cannot map -- rewrite it as db.search(collection, field, query, limit) from require("happyview.db")`
3. `db.backlinks has options this rewrite cannot map -- rebuild it as a backlinks.to(...) chain from require("happyview.backlinks")`
4. `a linked-repos handle carries no fields -- read this one from linked_repos.list()` — a `repo.did`/`.handle`/`.status`/`.scopes` read off a `linked_repos` result, which has no chained-call form.
5. `v3 space handles carry no fields -- read them from spaces.info(uri)` — a field read (`s.uri`, say) off a `spaces.get`/`spaces.create`/`spaces.accept_invite` result; the v3 handle carries methods only.
6. `v3 space handles carry no fields, so returning one answers nothing -- return spaces.info(uri) or the fields you need` — a `return` of such a handle, bare or as a table value; a v3 handle serialises to nothing useful.
7. `v3 space records spell it author_did -- rename this read to author_did` — an `.authorDid` read in a script that calls the module-level `atproto.spaces.query`, and a `return` that hands a page of space records straight back, whether from a handle's `:query{...}` or the module's (the page itself, a name bound to one, its `.records`, or a table holding either), since every caller of that method then sees the renamed key.
8. `'{name}' is a handle and v3 handles are never nil -- test existence with spaces.info(uri) or linked_repos.list()` — a nil test (`if not x`, `x == nil`, `x ~= nil`, `x and ...`) on a `spaces.get(...)`, `spaces.create`/`accept_invite`, or `linked_repos.get(...)` result.
9. `handle takes exactly (input, ctx) in v3 -- cut this parameter list down to two, then re-run the codemod` — `handle` declared with more than two parameters.
10. `input and ctx are handle's parameters in v3 -- declare function handle(input, ctx) and read them there`
11. `'{name}' is bound by this script, so this use cannot be rewritten -- rename the binding, then rewrite this use by hand` — a script that already has a local called `db`, `time`, `record`, etc.; the codemod won't shadow or rename an existing binding to make room for the rewrite.
12. `{dotted} has no mechanical equivalent -- rewrite it using require("{module}")` — a use of a removed global's member that has a library counterpart but no mechanical form, such as a global read as a value (`local l = log`, `local get = db.get`) or a `db.*`/`atproto.*` member outside the mapped set.
13. `{dotted} has no mechanical equivalent -- rewrite it by hand` — the same case when there is no library to point at. The `TID` conversions land here (`TID.toISO8601`, `TID.fromISO8601`, `TID.toUnixMicroseconds`, `TID.fromUnixMicroseconds`, `TID.toNumber`, `TID.fromNumber`): `internal.tids` carries millisecond precision, so none of them round-trip losslessly, and each is left for a by-hand decision rather than a lossy automatic rewrite.
14. `{name} depends on the script's trigger kind -- re-run the codemod with that kind, or rewrite it by hand` — a kind-dependent name (`record`, `uri`, `did`, `collection`, …) in a script whose trigger kind the codemod couldn't determine.
15. `{name} is not a global in a {kind} script -- rewrite it by hand` — a removed global referenced in a script where that name never meant anything for its kind (e.g. `params` in a record script).
16. `this runs while the script loads, before handle receives input and ctx -- move the read into handle, or into a function handle calls` — a read of `input` or any context global (`env`, `params`, `caller_did`, …) at file scope, outside every function, such as `local BASE = env.API_URL`. v2 set its globals before loading the script; v3 passes `input` and `ctx` to `handle`, which runs after the load, so nothing the codemod could write there would have a value yet.

## What happens to an unmigrated script

A script stored before the upgrade stays in the table untouched, and the scripts list flags it with the globals it still references. It fails the first time it reads one of them, with an error that names the global:

```
the 'params' global was removed in v3; run the script codemod (Settings → Scripts → Migrate, or happyview-codemod) -- see the Migrating scripts guide
```

Only the removed names raise. Any other undefined name is `nil`, as in plain Lua, and a script may still assign its own globals, including one that reuses a removed name.

Saving is stricter than running. Creating or editing a script whose body still references a removed global is refused, whether from the script editor or through [`POST`/`PATCH /admin/scripts`](../api-reference/admin/scripts.md#saving-an-unmigrated-script):

```
script references removed globals: db, params -- run the codemod first
```

The editor shows that message with a **Migrate this script** button beside it, which rewrites the text in the editor rather than the stored script, so unsaved edits survive. The codemod's own Apply is the one exception to the refusal: it may store a script with `-- codemod:` markers left in it, once you confirm, because the markers say exactly which lines are unfinished. Editing that script afterwards means finishing those lines, since the save is refused until they are.

## Running the codemod

### From the script editor

Open a Lua script and click **Migrate**. It shows a diff of the current body against the rewritten one, with any notes listed underneath. Nothing is saved until you click **Apply** — which stores the rewritten body and reloads the script. Previewing only needs permission to view scripts; applying needs permission to manage them. If the rewrite still has markers, Apply stays disabled until you confirm applying with them left in place.

Migrate works on the stored script. When a save is refused for referencing a removed global, the **Migrate this script** button next to the error opens the same preview on what the editor currently holds, and **Use rewritten script** puts the result back into the editor for you to save.

### From the CLI

```sh
# A single file or stdin, given the kind (input's shape depends on it)
happyview-codemod --file path/to/script.lua --kind procedure
cat script.lua | happyview-codemod --stdin --kind query

# KIND is one of: procedure, query, record, label, job

# Every stored script, or one by id, printing per-script notes
happyview-codemod --database-url "$DATABASE_URL"
happyview-codemod --database-url "$DATABASE_URL" --script "xrpc.procedure:app.example.create"

# Add --apply to write the rewritten body back; a script that still has
# markers is skipped unless --allow-markers is also given
happyview-codemod --database-url "$DATABASE_URL" --apply
happyview-codemod --database-url "$DATABASE_URL" --apply --allow-markers
```

File and stdin modes print the rewritten source to stdout and any notes to stderr. Database mode prints notes per script and, without `--apply`, changes nothing — it's a dry run by default. Exit code is `2` if any script still has markers after the rewrite, `1` on error, `0` otherwise. A `0` on a re-run does not mean the markers are gone, for the reason above.

## Before / after

```lua
-- before (a query)
function handle()
  local dl = atproto.blob_download(params.did, params.cid)
  return atproto.blob_upload(dl.handle, dl.mimeType)
end
```

```lua
-- after
local record = require("happyview.record")
local atproto = require("happyview.atproto")

function handle(input, ctx)
  local dl = atproto.blob_download(input.did, input.cid)
  return record.upload_blob(dl.bytes, dl.mime_type)
end
```

`handle()` gains its `(input, ctx)` parameters, a query's `params` becomes `input`, the two modules it uses get `require`d in the fixed order, `atproto.blob_upload` — the one library name that changes namespace — moves to `record.upload_blob`, and the downloaded blob's fields follow it (`.handle` → `.bytes`, `.mimeType` → `.mime_type`). Everything else on `atproto.*` stays put.

## Next steps

- [Built-in Modules](../api-reference/lua/built-in-modules.md): the `internal.*` replacements for `log`, `now`, `TID`, `json`
- [Developing Plugins](developing-plugins.md): the `happyview.*` libraries reached the same way, via `require`
- [Lua Scripting](lua-scripting.md): the `handle(input, ctx)` contract itself

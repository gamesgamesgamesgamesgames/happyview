---
title: "Linked Repos API"
---

Lua API for writing to repos an admin has linked to this instance. For a conceptual overview, see [Linked Repos](../../guides/linked-repos.md).

## `linked_repos` table

The `linked_repos` table is available in **all** script contexts (procedures, queries, record scripts, label scripts, and job scripts).

### `linked_repos.list()`

List every linked repo, including ones that aren't usable yet.

**Returns:** `table` — an array of tables with `id`, `did`, `handle`, `reason`, `status`, and `scopes`.

```lua
for _, repo in ipairs(linked_repos.list()) do
  log(repo.did .. " is " .. repo.status)
end
```

### `linked_repos.get(did)`

Get a handle for a linked repo.

**Parameters:**

| Parameter | Type   | Description                |
| --------- | ------ | -------------------------- |
| `did`     | string | The DID of the linked repo |

**Returns:** a `Repo` handle, or `nil` if no repo with that DID is linked.

**Raises:** if the grant exists but isn't usable, either it has never been authorized (`pending`) or its session failed to refresh (`needs_reauth`). The error names which.

```lua
local repo = linked_repos.get("did:plc:abc123")
if not repo then
  return { error = "that repo isn't linked" }
end
```

## `Repo` handle

Every method re-reads the grant from the database before acting. A grant deleted or flipped to `needs_reauth` mid-script is refused on the next call; the handle you're holding can't outlive it.

Errors raise rather than returning `nil, err`, matching `atproto.spaces`.

**Fields:**

| Field    | Type    | Description                            |
| -------- | ------- | -------------------------------------- |
| `did`    | string  | The linked repo's DID                  |
| `handle` | string? | The handle, when one is known          |
| `status` | string  | `active`, `pending`, or `needs_reauth` |
| `scopes` | string  | The grant's scope string               |

### Scope checks

The typed methods below check the grant's scopes **locally, before any network call**. A missing scope fails immediately with a message naming the grant and the scope it needs, so you never have to work backwards from an opaque `403`.

`repo:call()` is the exception: it can't be checked generically, so it goes straight to the PDS, which enforces the token's real scope.

### Local indexing

`create_record`, `put_record`, and `delete_record` work the same as the [`Record` API](http://localhost:3000/api-reference/lua/record-api).

`repo:call()` allows the use of arbitrary XRPCs against the repo's PDS. Note that using this instead of the built-in methods (e.g. `repo:call('com.atproto.repo.createRecord'))` instead of `repo:create_record()`) bypasses HappyView's local indexing. The records will still be indexed when they come from Jetstream.

### `repo:create_record{...}`

Create a record.

**Options:**

| Key          | Type    | Description                     |
| ------------ | ------- | ------------------------------- |
| `collection` | string  | Required. The collection NSID.  |
| `record`     | table   | Required. The record body.      |
| `rkey`       | string? | Optional. Generated if omitted. |

**Returns:** `table` — `{ uri, cid }`.

**Requires scope:** `repo:<collection>?action=create`

```lua
local result = repo:create_record{
  collection = "com.example.note",
  record = { text = "hello", createdAt = now() },
}
log("wrote " .. result.uri)
```

### `repo:put_record{...}`

Write a record at a known rkey, or creates the record if it doesn't exist.

**Options:**

| Key          | Type    | Description                                  |
| ------------ | ------- | -------------------------------------------- |
| `collection` | string  | Required. The collection NSID.               |
| `rkey`       | string  | Required. The record key.                    |
| `record`     | table   | Required. The record body.                   |
| `swap_cid`   | string? | Optional. Compare-and-swap against this CID. |

**Returns:** `table` — `{ uri, cid }`.

**Requires scope:** both `action=create` and `action=update` — or only `action=update` when `swap_cid` is supplied.

<Callout type="warn">
`putRecord` is an upsert: it creates the record when `rkey` doesn't already exist. A grant holding only `update` therefore can't call this, because the call might create something the grant was never given permission to create. For `putRecord` to be reliable, the linked repo must be given both `update` **and** `create`.

Passing `swap_cid` narrows `putRecord` to updates only. The PDS then requires the record to already exist with that CID to guarantee no create can happen, so only `update` is needed. The trade-off is that you must know the current CID, which usually means reading the record first.
</Callout>

```lua
-- Needs create + update
repo:put_record{
  collection = "com.example.note",
  rkey = "self",
  record = { text = "hello" },
}

-- Needs only update
repo:put_record{
  collection = "com.example.note",
  rkey = "self",
  record = { text = "hello" },
  swap_cid = "bafyreib2rxk3rh6kzwq...",
}
```

### `repo:delete_record{...}`

Delete a record.

**Options:**

| Key          | Type   | Description                    |
| ------------ | ------ | ------------------------------ |
| `collection` | string | Required. The collection NSID. |
| `rkey`       | string | Required. The record key.      |

**Returns:** `true`.

**Requires scope:** `repo:<collection>?action=delete`

```lua
repo:delete_record{ collection = "com.example.note", rkey = "3k2j..." }
```

### `repo:upload_blob(bytes, mime)`

Upload a blob to the linked repo's PDS.

**Parameters:**

| Parameter | Type   | Description                      |
| --------- | ------ | -------------------------------- |
| `bytes`   | string | The raw bytes. Binary-safe.      |
| `mime`    | string | The mime type, e.g. `image/png`. |

**Returns:** `table` — the blob reference, for embedding in a record.

**Requires scope:** a `blob:` scope matching the mime type, e.g. `blob:image/*` or `blob:*/*`.

```lua
local blob = repo:upload_blob(bytes, "image/png")
repo:create_record{
  collection = "com.example.photo",
  record = { image = blob, alt = "a cat" },
}
```

### `repo:call(nsid[, opts])`

Call any XRPC method as the linked repo. This is the escape hatch for anything the typed methods don't cover, including `rpc:` scopes.

**Parameters:**

| Parameter | Type   | Description                             |
| --------- | ------ | --------------------------------------- |
| `nsid`    | string | The XRPC method to call.                |
| `opts`    | table? | `params` for a GET, `input` for a POST. |

**Returns:** `table` — the decoded response.

**Requires scope:** not checked locally. The PDS enforces the token's actual scope.

```lua
-- GET
local out = repo:call("com.atproto.repo.listRecords", {
  params = { repo = repo.did, collection = "com.example.note", limit = 10 },
})

-- POST
repo:call("com.example.doThing", { input = { value = 42 } })
```

<Callout type="warn">
Because `call` isn't scope-checked locally, it can reach operations the typed methods would refuse. This includes `com.atproto.repo.createRecord`. The local checks exist to fail fast with a useful message, not as the security boundary. The real enforcement is the PDS's own scope check on the token.
</Callout>

## Error handling

All methods raise on failure. Use `pcall` if a script should continue past an error:

```lua
local ok, err = pcall(function()
  repo:create_record{ collection = "com.example.note", record = { text = "hi" } }
end)

if not ok then
  log("write failed: " .. tostring(err))
end
```

A grant whose session can no longer be refreshed flips to `needs_reauth` as a side effect of the failed call, so the dashboard reflects the problem even if your script swallows the error.

## See also

- [Linked Repos guide](../../guides/linked-repos.md): Concepts, scopes, and the invite flow
- [Linked Repos admin API](../admin/linked-repos.md): Managing grants and invites
- [Lua Scripting](../../guides/lua-scripting.md): Script contexts and triggers

---
title: "atproto API"
---

The `atproto` table provides atproto utility functions. Available in all [Lua scripts](../../guides/lua-scripting.md) — queries, procedures, and [record/label scripts](../../guides/label-scripts).

## atproto.resolve_service_endpoint

```lua
local endpoint = atproto.resolve_service_endpoint(did)
```

Resolves a DID to its atproto service endpoint URL by fetching the DID document. Supports both `did:plc:*` (via the PLC directory) and `did:web:*` (via `.well-known/did.json`).

| Parameter | Type   | Description        |
| --------- | ------ | ------------------ |
| `did`     | string | The DID to resolve |

**Returns:** The service endpoint URL as a string, or `nil` if resolution fails (DID not found, no PDS service in document, network error).

### Examples

```lua
-- Resolve a did:plc DID
local endpoint = atproto.resolve_service_endpoint("did:plc:abc123")
-- endpoint = "https://pds.example.com"

-- Resolve a did:web DID
local endpoint = atproto.resolve_service_endpoint("did:web:example.com")
-- endpoint = "https://example.com"

-- Handle resolution failure
local endpoint = atproto.resolve_service_endpoint("did:plc:unknown")
if not endpoint then
  return { error = "Could not resolve DID" }
end

-- Use with HTTP API to call a remote XRPC endpoint
local endpoint = atproto.resolve_service_endpoint(did)
if endpoint then
  local resp = http.get(endpoint .. "/xrpc/com.example.method")
  local data = json.decode(resp.body)
end
```

## atproto.get_labels

```lua
local labels = atproto.get_labels(uri)
```

Returns an array of labels for a single AT URI. Merges external labels (from subscribed labelers) with self-labels (from the record's `labels.values[]` field).

| Parameter | Type   | Description                   |
| --------- | ------ | ----------------------------- |
| `uri`     | string | AT URI of the record to query |

Each label in the array is a table with:

| Field | Type   | Description                           |
| ----- | ------ | ------------------------------------- |
| `src` | string | DID of the labeler (or record author) |
| `uri` | string | AT URI this label applies to          |
| `val` | string | Label value (e.g. "nsfw", "!hide")    |
| `cts` | string | Timestamp when the label was created  |

Expired labels are automatically filtered out. Returns an empty array if no labels exist.

## atproto.get_labels_batch

```lua
local labels_by_uri = atproto.get_labels_batch(uris)
```

Batch version of `get_labels`. Takes an array of AT URIs and returns a table keyed by URI, where each value is an array of labels.

| Parameter | Type  | Description             |
| --------- | ----- | ----------------------- |
| `uris`    | table | Array of AT URI strings |

**Returns:** A table keyed by URI. Each value is an array of label tables (same shape as `get_labels`). URIs with no labels have an empty array.

### Label examples

```lua
-- Get labels for a single game
local labels = atproto.get_labels("at://did:plc:abc/games.gamesgamesgamesgames.game/rkey1")
for _, label in ipairs(labels) do
  if label.val == "!hide" then
    -- skip this game in feed results
  end
end

-- Batch fetch labels for multiple games (efficient for feed hydration)
local uris = {}
for _, item in ipairs(skeleton) do
  uris[#uris + 1] = item.game
end

local labels_by_uri = atproto.get_labels_batch(uris)
for _, uri in ipairs(uris) do
  local labels = labels_by_uri[uri]
  for _, label in ipairs(labels) do
    if label.val == "!hide" then
      -- filter out this game
    end
  end
end
```

## atproto.blob_download

```lua
local result = atproto.blob_download(did, cid)
```

Downloads a blob from any DID's PDS via the public `com.atproto.sync.getBlob` endpoint. No authentication is required. The blob bytes are held on the Rust side as an opaque `BlobHandle` — binary data never enters the Lua VM.

| Parameter | Type   | Description                        |
| --------- | ------ | ---------------------------------- |
| `did`     | string | DID of the repo that owns the blob |
| `cid`     | string | CID of the blob to download        |

**Returns:** A table with:

| Field      | Type       | Description                                              |
| ---------- | ---------- | -------------------------------------------------------- |
| `handle`   | BlobHandle | Opaque handle to the blob bytes (pass to `blob_upload`)  |
| `mimeType` | string     | Content type from the PDS response (e.g. `"image/png"`)  |
| `size`     | number     | Size of the blob in bytes                                |

If the content-type header is missing from the PDS response, `mimeType` defaults to `"application/octet-stream"`.

**Throws** on any non-2xx response from the PDS, including 404 (blob not found) and 429 (rate limited). Retry logic is the script's responsibility.

**Availability:** All script contexts (queries, procedures, record scripts).

### BlobHandle methods

The `BlobHandle` userdata exposes two methods:

| Method        | Returns | Description                       |
| ------------- | ------- | --------------------------------- |
| `:size()`     | number  | Size of the blob in bytes         |
| `:mime_type()` | string  | MIME type of the blob             |

### Examples

```lua
-- Download a blob and inspect it
local result = atproto.blob_download("did:plc:abc123", "bafyreie...")
log("downloaded " .. result.size .. " bytes, type: " .. result.mimeType)

-- The handle can also be queried directly
log("handle size: " .. result.handle:size())
log("handle mime: " .. result.handle:mime_type())
```

## atproto.blob_upload

```lua
local response = atproto.blob_upload(handle, content_type)
```

Uploads blob bytes to the caller's PDS via authenticated `com.atproto.repo.uploadBlob`. The `handle` must be a `BlobHandle` from `blob_download`.

| Parameter      | Type       | Description                                  |
| -------------- | ---------- | -------------------------------------------- |
| `handle`       | BlobHandle | Opaque blob handle from `blob_download`      |
| `content_type` | string     | MIME type for the upload (e.g. `"image/png"`) |

**Returns:** The PDS `uploadBlob` response, which contains a `blob` field with the new blob reference:

```lua
{
  blob = {
    ["$type"] = "blob",
    ref = { ["$link"] = "<new-cid>" },
    mimeType = "image/png",
    size = 12345
  }
}
```

**Throws** on any error, including 429 (rate limited) and authentication failures. Retry logic is the script's responsibility.

**Availability:** Procedure scripts only. Returns `nil` in query and record script contexts (no PDS auth available).

### Examples

```lua
-- Copy a blob from one repo to another
local downloaded = atproto.blob_download(source_did, old_cid)
local uploaded = atproto.blob_upload(downloaded.handle, downloaded.mimeType)

-- Use the new blob ref in a record
local new_cid = uploaded.blob.ref["$link"]

-- Migrate all blobs in a media array
for _, item in ipairs(record.media) do
  if item.blob and item.blob.ref then
    local dl = atproto.blob_download(source_did, item.blob.ref["$link"])
    local ul = atproto.blob_upload(dl.handle, dl.mimeType)
    item.blob = ul.blob
  end
end
```

## atproto.sign

```lua
local sig = atproto.sign(record)
```

Signs a record and returns the inline signature object. Only available when an attestation signer is configured — if no signer is configured, `atproto.sign` is `nil`.

> **Privileged capability.** This signs *exactly* the content you pass with the instance's key; it proves only that this instance signed the content, not that the content is authentic. Only sign data you have verified, and be careful in scripts that run on untrusted input (record-event/label scripts, anonymous queries). See [Attestation Signing — Security considerations](../../guides/attestation-signing.md#security-considerations).

| Parameter | Type  | Description             |
| --------- | ----- | ----------------------- |
| `record`  | table | The record data to sign |

**Returns:** A signature table with:

| Field       | Type   | Description                                         |
| ----------- | ------ | --------------------------------------------------- |
| `key`       | string | The signing key ID (e.g. `did:web:example#signing`) |
| `signature` | table  | Contains `$bytes` with the signature                |

### Examples

```lua
-- Sign a record before returning it
local record = { contributionType = "correction", changes = { name = "Test" } }
local sig = atproto.sign(record)
record.signature = sig
return record

-- Check if signing is available
if atproto.sign then
  local sig = atproto.sign(record)
end
```

## atproto.spaces

The `atproto.spaces` sub-table provides access to [Permissioned Spaces](../../experimental/spaces/index.md) from Lua scripts. Every function raises if the `spaces_enabled` feature flag is disabled.

### Read-only functions

These are available in all script contexts, including those that have no `caller_did` (anonymous queries and label scripts).

```lua
atproto.spaces.is_member(space_uri, did) -- boolean
atproto.spaces.get_access(space_uri, did) -- 'read' | 'write' | nil
atproto.spaces.list_members(space_uri) -- [{ did, access }]
atproto.spaces.query({ space_uri, collection?, limit?, cursor? }) -- { records, cursor }
```

| Function      | Parameters                                             | Description                                                          |
| ------------- | ------------------------------------------------------- | --------------------------------------------------------------------- |
| `is_member`   | `space_uri, did`                                        | Whether `did` is a member of the space                                |
| `get_access`  | `space_uri, did`                                        | The member's access level, or `nil` if not a member                   |
| `list_members`| `space_uri`                                              | All resolved members as `{ did, access }`                             |
| `query`       | `{ space_uri, collection?, limit?, cursor? }`            | List records in the space, optionally filtered by collection          |

### Write surface: handles

Three functions return a `Space` userdata handle bound to the calling script's `caller_did`. They require `caller_did` to be set — it is present in authenticated queries, procedure scripts, record-event scripts (where it is the indexed record's author DID), and jobs. Label scripts and anonymous queries have no `caller_did`, so these raise `"this space operation requires an authenticated caller"`.

```lua
atproto.spaces.get(space_uri) -- Space | nil
atproto.spaces.create{ type, skey, display_name?, description?, mint_policy?, app_access?, managing_app_did?, config? } -- Space
atproto.spaces.accept_invite{ token } -- Space
```

`atproto.spaces.get` does not itself require `caller_did` to look up the space, but the returned handle's write methods will still raise without one.

### Space handle methods

| Method | Signature | Description |
| --- | --- | --- |
| `write_record` | `space:write_record{ collection, record }` | Create a record with an auto-generated rkey — returns `{ uri, cid }` |
| `put_record` | `space:put_record{ collection, rkey, record, swap_cid? }` | Create or overwrite a record at a specific rkey — returns `{ uri, cid }` |
| `delete_record` | `space:delete_record{ collection, rkey, swap_cid? }` | Delete a record. Requires write membership *and* record authorship — the caller must be a current write-member of the space and the record's author — returns `true` |
| `add_member` | `space:add_member{ did, access?, is_delegation? }` | Add a member (space-admin only) — returns `{ did, access }` |
| `remove_member` | `space:remove_member{ did }` | Remove a member (space-admin only) — returns `true` |
| `members` | `space:members()` | List resolved members — returns `[{ did, access }]` |
| `is_member` | `space:is_member(did)` | Whether `did` is a member — returns `boolean` |
| `access` | `space:access(did)` | The member's access level, or `nil` — returns `'read' \| 'write' \| nil` |
| `update` | `space:update{ display_name?, description?, mint_policy?, app_access?, managing_app_did?, config? }` | Update space metadata (space-admin only) — returns `true` |
| `delete` | `space:delete()` | Delete the space (space-admin only) — returns `true` |
| `query` | `space:query{ collection?, limit?, cursor? }` | List records in the space — returns `{ records, cursor }` |
| `create_invite` | `space:create_invite{ access?, max_uses?, expires_at? }` | Create an invite token (space-admin only) — returns `{ invite_id, token, access, max_uses, expires_at }` |

**Authorization:** writes act strictly as `caller_did`, using the same authorization the HTTP handlers enforce — a write-member for record operations, space-admin for membership/space/invite management. `delete_record` additionally requires record authorship: the caller must be both a current write-member and the record's author (`author_did == caller_did`). The HTTP handlers and the Lua bindings share the same `src/spaces/service.rs` service layer, so behavior can't drift between them.

**`update` patch semantics:** for the nullable fields (`display_name`, `description`, `managing_app_did`), a string value sets the field, `false` clears it, and omitting the key leaves it unchanged. Lua has no way to represent "explicit null" versus "absent" in a table — a `nil` value simply removes the key — so `false` is used as the clear signal instead.

### Examples

```lua
-- Create a space, write a record, and add a member
local space = atproto.spaces.create{ type = "com.example.chat", skey = "general" }
space:write_record{ collection = "com.example.chat.message", record = { text = "hi" } }
space:add_member{ did = "did:plc:friend", access = "write" }
```

```lua
-- Look up an existing space and write to it if the caller is a member
local space = atproto.spaces.get("at://did:plc:abc123/space/com.example.forum/main")
if space and space:is_member(caller_did) then
  space:write_record{ collection = "com.example.forum.post", record = { text = "hello" } }
end
```

```lua
-- Join a space via an invite token, then write as the new member
local space = atproto.spaces.accept_invite{ token = invite_token }
space:write_record{ collection = "com.example.chat.message", record = { text = "just joined!" } }
```

## atproto.verify_signature

```lua
local valid = atproto.verify_signature(record, signature, repo_did)
```

Verifies that an inline signature was produced by this HappyView instance. Only available when an attestation signer is configured — if no signer is configured, `atproto.verify_signature` is `nil`.

| Parameter   | Type   | Description                                |
| ----------- | ------ | ------------------------------------------ |
| `record`    | table  | The record data                            |
| `signature` | table  | The signature object from `atproto.sign()` |
| `repo_did`  | string | The repo DID                               |

**Returns:** `true` if the signature is valid, `false` if it is not.

**Raises** when the signature cannot be checked at all — signature bytes that aren't valid base64, a missing `key` or `$bytes` field, a record that can't be encoded. These are not the same fact:

| Outcome  | Meaning                                                              |
| -------- | -------------------------------------------------------------------- |
| `true`   | We checked. The signature is genuine.                                 |
| `false`  | We checked. The signature does not match this record — it is forged.  |
| _raises_ | We could not check. This says nothing about the record.               |

The distinction matters because `verify_signature` is almost always used as a guard, and treating "could not check" as "forged" turns any fault in the verification path into an accusation against a user. If your guard rejects records on `false`, wrap the call in `pcall` and handle the raise as a distinct case — retry it, count it, or alert on it, but don't record it as forgery.

### Examples

```lua
-- Verify a signature roundtrip
local record = { contributionType = "correction", changes = { name = "Test" } }
local sig = atproto.sign(record)
local valid = atproto.verify_signature(record, sig, caller_did)
if not valid then
  return { error = "signature verification failed" }
end
```

```lua
-- Guard that keeps "forged" and "unverifiable" apart
local ok, valid = pcall(atproto.verify_signature, record, sig, repo_did)
if not ok then
  -- valid holds the error message; we learned nothing about the record
  return { error = "could not verify signature: " .. tostring(valid) }
elseif not valid then
  return { error = "record signature is not ours" }
end
```

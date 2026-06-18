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

**Returns:** `true` if the signature is valid, `false` otherwise. Returns `false` on failure rather than raising an error.

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

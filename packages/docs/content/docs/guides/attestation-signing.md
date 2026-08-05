---
title: "Attestation Signing"
---

HappyView can sign records with an ECDSA (secp256k1) keypair so their origin can be verified later. Lua scripts call `atproto.sign()` to attach an inline signature to a record and `atproto.verify_signature()` to check one. HappyView's implementation follows the [atproto attestation spec](https://tangled.org/strings/did:plc:cbkjy5n7bk3ax2wplmtjofq2/3m3fy2xuahc22).

## How it works

1. HappyView loads or generates a secp256k1 keypair on startup
2. `atproto.sign(record)` encodes the record to DAG-CBOR, computes its CID, and signs the CID with the private key
3. The signature is added to the record's `signatures` array as an inline object
4. `atproto.verify_signature(record, sig, repo_did)` recomputes the CID and verifies the signature

The repo DID is included in the signed data — a signature for one user's record can't be replayed against another's. Any modification to the record invalidates the signature.

## Setup

Attestation signing is enabled by default — HappyView generates a keypair on first startup and persists it to the `happyview_instance_settings` database table. No configuration is required.

To use an explicit key instead, set the `ATTESTATION_PRIVATE_KEY` environment variable:

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `ATTESTATION_PRIVATE_KEY` | no | auto-generated | Hex-encoded 32-byte secp256k1 private key |
| `ATTESTATION_KEY_ID` | no | `did:web:{host}#attestation` | Key identifier included in signatures. Derived from `PUBLIC_URL` by default |
| `ATTESTATION_SIG_TYPE` | no | app-specific NSID | The `$type` value used in signature objects |

The key ID defaults to a `did:web` derived from your `PUBLIC_URL`. For example, `PUBLIC_URL=https://happyview.example.com` produces a key ID of `did:web:happyview.example.com#attestation`.

### Priority order

HappyView checks for signing configuration in this order:

1. **Environment variables** — if `ATTESTATION_PRIVATE_KEY` is set, it's used
2. **Database** — if previously generated keys exist in `happyview_instance_settings`, they're loaded
3. **Auto-generation** — a new key is generated and persisted to the database

If key loading fails for any reason, signing is disabled and `atproto.sign` / `atproto.verify_signature` will be `nil` in Lua scripts.

## Using in Lua scripts

Available in queries, procedures, and record/label scripts via the [atproto API](../api-reference/lua/atproto-api.md).

### Signing a record

```lua
function handle()
  local r = Record(collection, input)
  r:save()

  local sig = atproto.sign({ text = input.text, createdAt = input.createdAt })
  return { uri = r._uri, cid = r._cid, signature = sig }
end
```

The returned signature object:

```json
{
  "$type": "your.app.attestation",
  "key": "did:web:happyview.example.com#attestation",
  "signature": {
    "$bytes": "base64-encoded-signature"
  }
}
```

### Verifying a signature

```lua
function handle()
  local record = db.get(params.uri)
  if not record then
    return { error = "not found" }
  end

  local sig = record.signatures and record.signatures[1]
  if not sig then
    return { record = record, verified = false }
  end

  local ok, valid = pcall(atproto.verify_signature, record, sig, record.did)
  if not ok then
    -- Couldn't check — that is not the same as "the signature is bad".
    return { record = record, verified = nil, error = tostring(valid) }
  end
  return { record = record, verified = valid }
end
```

`verify_signature` returns `false` only when it checked the signature and it didn't match. It **raises** when it couldn't check — signature bytes that aren't valid base64, a missing field, a record that won't encode. Keep those apart: a script that treats "couldn't check" as `false` will report a fault in its own verification path as a forged record, which is a serious thing to tell a user about their own data.

### Checking availability

Both functions are `nil` when no signer is configured:

```lua
if atproto.sign then
  record.signature = atproto.sign(record)
end
```

## Signature format

Signatures are stored as objects in the record's `signatures` array:

| Field       | Type   | Description                          |
| ----------- | ------ | ------------------------------------ |
| `$type`     | string | Signature type NSID                  |
| `key`       | string | Key identifier (DID with fragment)   |
| `signature` | table  | Contains `$bytes` (base64-encoded)   |

## Security considerations

`atproto.sign` exposes the instance's signing key to Lua scripts. Treat it as a
privileged capability:

- **It signs exactly what you give it.** A signature only proves *"this HappyView
  instance signed this content"* — it does **not** prove the content is authentic,
  is present in anyone's repo, or was authored by any particular DID. Only sign
  content you have already verified.
- **It's available to any script**, including ones that run on untrusted input —
  record-event and label scripts (triggered by arbitrary firehose records) and
  anonymous XRPC queries. In those contexts there is no authenticated caller, so
  the signature's `repository` binding is empty. Don't sign untrusted input (a
  firehose record, an anonymous request parameter) unless you intend the instance
  to vouch for it.
- **Creating scripts requires the `scripts:manage` permission.** The signing key
  is therefore only reachable by operators you have trusted with that permission —
  grant it accordingly, and review scripts that call `atproto.sign`.

If you need signatures scoped to a specific verified subject, have the script
verify the subject itself (e.g. confirm the record's `did` and content against the
source) before calling `atproto.sign`.

## Next steps

- [atproto API reference](../api-reference/lua/atproto-api.md#atprotosign) — `atproto.sign` and `atproto.verify_signature` parameter docs
- [Signed Record](../reference/script-examples/signed-record.md) — save a record with an attestation signature
- [Verify Signed Record](../reference/script-examples/signed-record-verify.md) — fetch a record and verify its signature

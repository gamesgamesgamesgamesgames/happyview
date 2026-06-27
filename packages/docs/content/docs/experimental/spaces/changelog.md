---
title: "Changelog"
---

## Latest — Proposal 0016 Alignment

Major restructuring to align with [AT Protocol Proposal 0016](https://github.com/bluesky-social/proposals) (Permissioned Data).

### Namespace split

- **Protocol routes** now live under `com.atproto.space.*` (queries, data, credentials)
- **Management routes** now live under `com.atproto.simplespace.*` (create/update/delete spaces, membership, config)
- **`dev.happyview.space.*`** endpoints remain as backward-compatible aliases until v3
- Invite endpoints remain under `dev.happyview.space.*` as HappyView extensions

### New terminology

- **`owner_did` → `authority_did`** — the DID that controls the space. A separate `creator_did` tracks who originally created it.
- **`accessMode` → `mintPolicy`** — controls who can create permissioned repos: `member-list` (default), `public`, or `managing-app`
- **`appAllowlist`/`appDenylist` → `appAccess`** — controls third-party app access: `open` (default) or `allowList`
- **`getMemberGrant` → `getDelegationToken`** — renamed and changed from POST to GET. Returns a delegation token (JWT with `typ: atproto-space-delegation+jwt`, ES256K, 60-second TTL)
- **`redeemInvite` → `acceptInvite`** — renamed for clarity
- **Space credential `typ`** — changed from `space_credential` to `atproto-space-credential+jwt`
- **Space credential TTL** — reduced from 4 hours to 2 hours

### New access level

- **`read_self`** — a new membership access level that restricts reads to only the member's own records within the space

### New endpoints

- **`com.atproto.space.getRepoState`** (GET) — returns per-user repo state including LtHash state and signed commit
- **`com.atproto.space.listRepoOps`** (GET) — returns the record operation log for sync
- **`com.atproto.space.listRepos`** (GET) — lists repos (authors) in a space
- **`com.atproto.space.getBlob`** (GET) — retrieves a blob from a space
- **`com.atproto.space.registerNotify`** (POST) — registers for write notifications
- **`com.atproto.space.notifyWrite`** (POST) — pushes a write notification
- **`com.atproto.space.notifySpaceDeleted`** (POST) — pushes a space-deleted notification
- **`com.atproto.simplespace.getConfig`** (GET) — gets space configuration (mint policy, app access, managing app)
- **`com.atproto.simplespace.updateConfig`** (POST) — updates space configuration

### Cryptographic primitives

- **LtHash** — homomorphic set-hash for per-user repo state. 2048-byte state with 1024 little-endian uint16 lanes using BLAKE3 XOF. Supports insert/remove operations for incremental record tracking.
- **Deniable commit signatures** — users sign context (space DID + rev + random IKM) rather than content hash, producing a MAC that proves authorship without binding the user to specific content.

### Data model changes

- New `happyview_space_repo_state` table — per-user LtHash state + signed commit per space
- New `happyview_space_record_oplog` table — ordered record operation log per space
- New `happyview_space_notify_registrations` table — write notification registrations
- Spaces now use `authority_did` and `creator_did` instead of `owner_did`
- `mint_policy` and `app_access` columns replace `access_mode`, `app_allowlist`, `app_denylist`

### Breaking changes

- Feature flag disabled response changed from `501 Not Implemented` to `404` with `FeatureDisabled` error code
- Deleting a space now cascades to all associated data (records, members, repo state, oplog, notifications, credentials)

---

## v2.6.0

### New endpoints

- **`createRecord`:** create a record with an auto-generated TID rkey instead of requiring the caller to supply one
- **`applyWrites`:** batch multiple create, update, and delete operations in a single request

### Optimistic concurrency

- **`swapRecord`:** optional CID-based concurrency guard on `putRecord`, `deleteRecord`, and individual operations within `applyWrites`. Returns `409 Conflict` when the record's current CID doesn't match.
- **`swapCommit`:** optional revision-based concurrency guard on `applyWrites`. Asserts the space's current revision before applying any writes. Returns `409 Conflict` on mismatch.
- Spaces now track a `revision` field (TID) that advances on every write.

### Space DID separation

- Spaces now have their own `did` field, distinct from the `owner_did` of the space creator. For personal spaces these are the same DID; multi-party spaces will have their own DID.
- All URI construction and lookups use the space's DID. Ownership checks use `owner_did`.
- New database migration adds the `did` column to the `spaces` table.

### Two-step credential flow

- Replaced the single `getCredential` endpoint with a two-step flow:
  1. **`getMemberGrant`:** proves membership and returns an HMAC-SHA256 grant (5-minute TTL)
  2. **`getSpaceCredential`:** exchanges the grant for an ES256 space credential JWT (4-hour TTL)
- Removed the `refreshCredential` endpoint (just repeat the two-step flow)

### Bearer auth for space credentials

- Space credentials are now passed as standard `Authorization: Bearer <token>` instead of a custom `X-Space-Credential` header. HappyView distinguishes credentials from other Bearer tokens by checking the JWT `typ` header (`space_credential`), matching Dan's reference implementation.
- No DPoP auth or client key needed when authenticating via space credential.

### Endpoint naming

- Space CRUD endpoints renamed to verbNoun format: `space.create` → `space.createSpace`, `space.get` → `space.getSpace`, `space.list` → `space.listSpaces`, `space.update` → `space.updateSpace`, `space.delete` → `space.deleteSpace`.
- Invite endpoints moved out of the `invite.*` sub-namespace: `invite.create` → `space.createInvite`, `invite.redeem` → `space.redeemInvite`, `invite.revoke` → `space.revokeInvite`, `invite.list` → `space.listInvites`.
- Old endpoint names are still available as legacy aliases and will be removed in a future release.

### Bug fixes

- Fixed `WriteOp` serde deserialization. `swapRecord` fields in `update` and `delete` operations now correctly deserialize from camelCase JSON.
- Credential `iss` claim now uses the space's DID instead of the owner's DID.
- `SpaceUri` parsing updated to use `did` (space DID) instead of `owner_did`.

---

## v2.5.0

_Released 2026-05-05_

Initial release of Permissioned Spaces behind the `feature.spaces_enabled` experimental flag.

### Features

- Space CRUD: `create`, `get`, `list`, `update`, `delete`
- Record operations: `putRecord`, `getRecord`, `listRecords`, `deleteRecord`
- Membership management: `addMember`, `removeMember`, `listMembers`
- Invite system: `invite.create`, `invite.redeem`, `invite.revoke`, `invite.list`
- `ats://` URI scheme for addressing permissioned data
- Access model with `default_allow` / `default_deny` modes and app allowlists/denylists
- Space credentials for cross-service read access via `X-Space-Credential` header
- Delegation: adding a space as a member transitively grants access to its members
- Lua scripting context includes space metadata (`space.did`, `space.owner_did`, `space.type_nsid`, `space.skey`)

---
title: "Changelog"
---

## v2.16 — Space Authority, Signed Credentials, and Sync

Aligns credentials and sync with the latest [Proposal 0016](https://github.com/bluesky-social/proposals/blob/main/0016-permissioned-data/README.md) updates.

### Space authority

- **HappyView is the authority for spaces it creates.** New spaces use the instance's service identity DID (`did:web` or `did:plc`) as both the space URI DID and `authority_did`: `at://<instance DID>/space/<type>/<skey>`. Instances without a published service identity use the creator's DID, as before.
- **The creator administers the space.** `creator_did` controls `putMember`, `removeMember`, `updateSpace`, `deleteSpace`, and invites. `isOwner` in `listSpaces` reflects the creator.
- **Space keys are unique per type across the instance.** A second creator using the same type and `skey` gets `409 Conflict`.
- **`#atproto_space_host` service entry.** HappyView publishes an `AtprotoSpaceHost` entry when a space is created. `did:web` instances serve it at once. `did:plc` operators must sync service entries to the PLC directory for it, and the `#atproto_space` key, to reach their DID document.
- **OAuth scopes must name HappyView as the authority.** Apps requesting `space:` scopes need `authority=<HappyView DID>` or `authority=*`. The default `authority=self` does not cover HappyView-authority spaces.
- **Managing apps are reached through the service entry their identifier names** (e.g. `did:web:forum.example.com#forum`), not only `#atproto_pds`.

### Credentials

- **HTTP Message Signatures replace DPoP for credentials.** `getSpaceCredential` takes the delegation token as `Authorization: Bearer`, with an [RFC 9421](https://www.rfc-editor.org/rfc/rfc9421) `atproto-space` signature by a fresh P-256 key. The body is `{space, clientAttestation?}`. No OAuth session is needed.
- **Credentials are bound to that key** through `cnf.kid`.
- **Credentials use the `Atproto-Space` scheme.** Requests carry `Atproto-Space-Audience` and an `atproto-space` signature over `authorization` and `atproto-space-audience`. The audience is the repo DID for repo reads and the authority DID for space-wide calls. Bearer space credentials are refused.
- **New error:** `BadSpaceSignature` (401).
- **Credentials last 10 minutes** (was 2 hours).
- **Delegation tokens are single-use.** A second exchange fails with `InvalidDelegationToken`. Tokens signed by the account's own key, as a spaces-capable PDS issues, are accepted beside HappyView's own.
- **Credentials for HappyView-authority spaces are signed with the instance's `#atproto_space` key**, so other hosts can verify them.
- **Member-list read policies require the `read` flag.** Write-only members and members limited to their own records cannot obtain credentials.
- **Revocation on read loss.** Removing a member, or setting their `read` to `false`, revokes their credentials. HappyView tells hosts of the space's native repos through `com.atproto.space.notifyCredentialRevoked`.

### Removed

- The `{grant}` body and OAuth caller flow on `getSpaceCredential`
- `dev.happyview.space.getSpaceCredential`
- Verification of credentials issued by other space authorities

### Sync

- **`registerNotify` takes a service identifier.** `{space, service}`, where `service` is a DID with an optional fragment, resolved to its endpoint. Returns `{expiresAt}`. Unresolvable identifiers fail with `ServiceNotResolvable`. Registering again replaces the previous registration.
- **Outbound `notifyWrite` calls.** Registered services receive `com.atproto.space.notifyWrite` once per commit, with service auth from HappyView and `{space, repo, rev, hash, spaceRev, prevSpaceRev?}`. `spaceRev` and `prevSpaceRev` are provisional names.
- **Inbound `notifyWrite` from repo hosts.** HappyView accepts `{space, repo, rev, hash}` with service auth signed by the writing account. The writer must pass the write policy. Stale revisions are ignored, and revisions more than 5 minutes in the future are refused.
- **Space-wide revision.** Every accepted repo update advances a space revision.
- **`listRepos` serves the writer set.** Returns `{repos: [{did, rev, hash}], cursor?, spaceRev}`, with `limit` (default 100, max 1000), `cursor`, and `since`.
- **Fallback sweep.** HappyView re-syncs repos hosted on their authors' PDSes every 5 minutes.
- **Service auth `lxm` is enforced.** Inbound tokens bound to a different method are rejected.

### Backward compatibility (until v3)

- `createSpace` and `updateSpace` accept a single `policy` or `mintPolicy` (`public`, `member-list`, `managing-app`) with a sibling `managingApp` or `managingAppDid`, applied to both reads and writes when `readPolicy` and `writePolicy` are absent.
- `appAccess` accepts `{"type": "open"}` and `{"type": "allowList", "allowed": [...]}`.
- `com.atproto.simplespace.addMember` and `dev.happyview.space.addMember` accept an `access` word.
- `registerNotify` accepts `{space, serviceDid, endpoint}` webhook registrations, returning `{id, expiresAt}`. `notifyWrite` accepts `{space, did, collection, rkey, cid}`.

### Deprecated

- **`config.records_public`.** It has never been enforced: setting it did not change who could read a space. Use a `public` `readPolicy` for a space anyone may read. It is still accepted and stored until v3.

### Lua

- **`space:put_member{did, read, write, is_delegation?}`** sets a member's flags and returns `{did, read, write}`.
- **`space:members()`** entries include `read` and `write` booleans beside `access`.

### Breaking changes

- Credential clients must sign requests and use the `Atproto-Space` scheme.
- Apps must request `space:` scopes naming HappyView's DID as the authority.
- Syncers must accept `notifyWrite` calls in place of webhook payloads, or keep a legacy registration until v3.

---

## v2.11 — Final Proposal 0016 Alignment

Aligns with the merged [Proposal 0016](https://github.com/bluesky-social/proposals/blob/main/0016-permissioned-data/README.md) specification.

### URI scheme change

- **`ats://` → `at://` with `space` path segment** — space URIs now use the standard `at://` scheme with a literal `space` segment: `at://<did>/space/<type>/<skey>`. Record URIs follow: `at://<did>/space/<type>/<skey>/<author>/<collection>/<rkey>`.

### Endpoint changes

- **`getRepoState` → `getLatestCommit`** — renamed to match the proposal. The old name is kept as a backward-compatible alias.
- **`getRepo`** (GET) — new endpoint that exports a user's repo as a CAR v1 file (two roots: signedCommit + DRISL index)
- **`listRepoOps`** now inlines record values by default via LEFT JOIN against `space_records`. Pass `excludeValues=true` for metadata-only responses.

### Commit changes

- **`SignedCommit.ver`** — new version field (currently `1`) for future-proofing
- **Commit context string** now includes the author's DID: `tag || space || author || rev || ikm` (was `tag || space || rev || ikm`)

### Breaking changes

- All space URIs now use `at://` with a `space` segment instead of `ats://`. Clients using the old scheme must update.

---

## v2.10.0 — Proposal 0016 Alignment

Major restructuring to align with [atproto Proposal 0016](https://github.com/bluesky-social/proposals) (Permissioned Data).

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

## v2.8.0

### Bug fixes

- Spaces endpoints now use cursor-based pagination instead of offset-based

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

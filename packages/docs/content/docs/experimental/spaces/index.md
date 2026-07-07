---
title: "Overview"
---

<Callout type="error" title="Experimental">
Permissioned Spaces are experimental and the API will change. This implementation follows [AT Protocol Proposal 0016](https://github.com/bluesky-social/proposals) (Permissioned Data). HappyView uses the `com.atproto.space.*` and `com.atproto.simplespace.*` namespaces. The previous `dev.happyview.space.*` endpoints remain available as backward-compatible aliases until v3.
</Callout>

Spaces are containers for permissioned data in atproto. Unlike regular public records that live in a user's repo, space records are gated by membership — only members can read or write data within a space.

## Concepts

A **space** is identified by three components:

- **Space DID** — the space's own decentralized identifier (for personal spaces, this is the user's DID)
- **Type** — the space type as an NSID, describing the modality (e.g. a forum, a group chat, a photo album)
- **Space key (skey)** — a short string differentiating multiple spaces of the same type

These form the space URI: `at://<space-did>/space/<type>/<skey>`

A **space record** adds three more components to the URI: the author's DID, the collection NSID, and the record key:

```
at://<space-did>/space/<type-nsid>/<skey>/<author-did>/<collection>/<rkey>
```

## Feature flag

In HappyView, spaces are gated behind the `feature.spaces_enabled` instance setting. Enable it in the dashboard under **Settings** or via the admin API:

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/settings/feature.spaces_enabled", {
  method: "PUT",
  headers: {
    "Authorization": `Bearer ${TOKEN}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({ value: "true" }),
});
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/settings/feature.spaces_enabled", {
  method: "PUT",
  headers: {
    "Authorization": `Bearer ${TOKEN}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({ value: "true" }),
});
```
```rust tab="Rust" tab-group="language"
let response = client
    .put("http://127.0.0.1:3000/admin/settings/feature.spaces_enabled")
    .header("Authorization", format!("Bearer {}", token))
    .json(&serde_json::json!({ "value": "true" }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{"value": "true"}`)
req, _ := http.NewRequest("PUT",
  "http://127.0.0.1:3000/admin/settings/feature.spaces_enabled", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X PUT http://127.0.0.1:3000/admin/settings/feature.spaces_enabled \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"value": "true"}'
```

When disabled, all space endpoints return a `404` error with `FeatureDisabled` as the error code.

## Endpoints

Space endpoints are split across two namespaces:

- **`com.atproto.space.*`** — protocol-level routes (queries, data, credentials)
- **`com.atproto.simplespace.*`** — management routes (create/update/delete spaces, membership)

The previous `dev.happyview.space.*` endpoints remain as backward-compatible aliases until v3. All endpoints require [DPoP authentication](../../getting-started/authentication.md) or cookie-based session auth.

| Endpoint                                      | Method | Description                                     |
| --------------------------------------------- | ------ | ----------------------------------------------- |
| `com.atproto.simplespace.createSpace`          | POST   | Create a space                                  |
| `com.atproto.space.getSpace`                   | GET    | Get a space by URI                              |
| `com.atproto.space.listSpaces`                 | GET    | List spaces by membership                       |
| `com.atproto.simplespace.updateSpace`          | POST   | Update space metadata                           |
| `com.atproto.simplespace.deleteSpace`          | POST   | Delete a space                                  |
| `com.atproto.simplespace.getConfig`            | GET    | Get space configuration                         |
| `com.atproto.simplespace.updateConfig`         | POST   | Update space configuration                      |
| `com.atproto.space.createRecord`               | POST   | Create a record (auto-generated rkey)           |
| `com.atproto.space.putRecord`                  | POST   | Write a record                                  |
| `com.atproto.space.getRecord`                  | GET    | Get a record                                    |
| `com.atproto.space.listRecords`                | GET    | List records                                    |
| `com.atproto.space.deleteRecord`               | POST   | Delete a record                                 |
| `com.atproto.space.applyWrites`                | POST   | Batch write operations                          |
| `com.atproto.simplespace.addMember`            | POST   | Add a member                                    |
| `com.atproto.simplespace.removeMember`         | POST   | Remove a member                                 |
| `com.atproto.simplespace.listMembers`          | GET    | List resolved members                           |
| `com.atproto.space.getLatestCommit`             | GET    | Get per-user signed commit                      |
| `com.atproto.space.getRepo`                    | GET    | Export a user's repo as a CAR file               |
| `com.atproto.space.listRepoOps`                | GET    | List record operation log entries                |
| `com.atproto.space.listRepos`                  | GET    | List repos (authors) in a space                 |
| `com.atproto.space.getDelegationToken`         | GET    | Get a delegation token (step 1 of credentials)  |
| `com.atproto.space.getSpaceCredential`         | POST   | Get a space credential (step 2)                 |
| `com.atproto.space.getBlob`                    | GET    | Get a blob from a space                         |
| `com.atproto.space.registerNotify`             | POST   | Register for write notifications                |
| `com.atproto.space.notifyWrite`                | POST   | Push a write notification                       |
| `com.atproto.space.notifySpaceDeleted`         | POST   | Push a space-deleted notification               |
| `dev.happyview.space.createInvite`             | POST   | Create an invite (HappyView extension)          |
| `dev.happyview.space.acceptInvite`             | POST   | Accept an invite (HappyView extension)          |
| `dev.happyview.space.revokeInvite`             | POST   | Revoke an invite (HappyView extension)          |
| `dev.happyview.space.listInvites`              | GET    | List invites (HappyView extension)              |

## Access model

Spaces use two independent controls for access:

**Mint policy** controls who can create permissioned repos in the space:

- **`member-list`** (default) — only members can create repos
- **`public`** — anyone can create repos
- **`managing-app`** — only the managing app can create repos

**App access** controls which third-party apps can interact with the space:

- **`open`** (default) — any app can access
- **`allowList`** — only explicitly listed apps can access

Individual users access spaces through **membership**. Members have one of three access levels:

- **`write`** — can read and write data
- **`read`** — can read all data in the space
- **`read_self`** — can only read their own data within the space

Write access implies read. The space creator is automatically added as a write member.

Spaces also support **delegation** — adding another space as a member, which transitively grants access to all members of the delegated space.

## Alignment with Proposal 0016

HappyView implements [AT Protocol Proposal 0016](https://github.com/bluesky-social/proposals) (Permissioned Data) with some HappyView-specific extensions.

### Protocol features implemented

- **Namespace split** — `com.atproto.space.*` for protocol routes, `com.atproto.simplespace.*` for management
- **Mint policy** — `member-list`, `public`, `managing-app` (replaces `accessMode`)
- **App access** — `open`, `allowList` (replaces `appAllowlist`/`appDenylist`)
- **Delegation tokens** — `getDelegationToken` (GET, 60-second TTL) replaces `getMemberGrant`
- **Space credentials** — `atproto-space-credential+jwt` typ, ES256, 2-hour TTL
- **Deniable commit signatures** — user signs context (space + author + rev + random IKM), not content hash
- **LtHash** — homomorphic set-hash (2048-byte state, 1024 uint16 lanes, BLAKE3 XOF)
- **SignedCommit** — versioned commit struct (`ver: 1`) with hash, ikm, sig, mac, rev
- **Record operation log** — `listRepoOps` returns the oplog for sync (values inlined by default, `excludeValues` to opt out)
- **Latest commit** — `getLatestCommit` returns the signed commit for a user in a space
- **Repo export** — `getRepo` exports a user's repo as a CAR v1 file (signedCommit + DRISL index)
- **Write notifications** — `registerNotify`, `notifyWrite`, `notifySpaceDeleted`
- **Space-scoped blobs** — `getBlob`
- **Authority DID** — spaces use `authority_did` (not `owner_did`) with a separate `creator_did`

### HappyView extensions (not in the protocol spec)

- **Invite system** — `createInvite`, `acceptInvite`, `revokeInvite`, `listInvites` (under `dev.happyview.space.*`)
- **`isDelegation` on members** — allows spaces to be members of other spaces
- **`displayName`, `description` on spaces** — human-readable metadata
- **`config` object** — `membershipPublic`, `recordsPublic`, plus arbitrary extra fields
- **`read_self` access level** — restricts reads to the member's own data

## Next steps

- [Managing Spaces](./managing-spaces.md) — create, update, and delete spaces
- [Members](./members.md) — manage membership and delegation
- [Records](./records.md) — read and write permissioned data
- [Credentials](./credentials.md) — cross-service authentication for spaces
- [Invites](./invites.md) — invite-based membership
- [Changelog](./changelog.md) — version history

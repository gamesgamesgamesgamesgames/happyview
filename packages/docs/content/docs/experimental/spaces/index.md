---
title: "Overview"
---

<Callout type="error" title="Experimental">
Permissioned Spaces are experimental and the API will change. This implementation follows [atproto Proposal 0016](https://github.com/bluesky-social/proposals) (Permissioned Data). HappyView uses the `com.atproto.space.*` and `com.atproto.simplespace.*` namespaces. The `dev.happyview.space.*` endpoints are deprecated aliases, kept until v3.
</Callout>

Spaces are containers for permissioned data in atproto. Unlike regular public records that live in a user's repo, space records are gated by membership — only members can read or write data within a space.

## Concepts

A **space** is identified by three components:

- **Space DID**: the DID of the space's authority. Spaces created through HappyView use HappyView's service identity DID. See [Space authority](./managing-spaces.md#space-authority).
- **Type**: the space type as an NSID, describing the modality (e.g. a forum, a group chat, a photo album)
- **Space key (skey)**: a short string differentiating multiple spaces of the same type

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

- **`com.atproto.space.*`**: protocol-level routes (queries, data, credentials, sync)
- **`com.atproto.simplespace.*`**: management routes (create/update/delete spaces, membership)

The `dev.happyview.space.*` aliases are deprecated and kept until v3. Most endpoints take [DPoP authentication](../../getting-started/authentication.md) or cookie-based session auth. Read and sync endpoints also take a [space credential](./credentials.md), and the notify endpoints take service auth.

| Endpoint                                      | Method | Description                                     |
| --------------------------------------------- | ------ | ----------------------------------------------- |
| `com.atproto.simplespace.createSpace`          | POST   | Create a space                                  |
| `com.atproto.simplespace.getSpace`             | GET    | Get a space and its policies                    |
| `com.atproto.space.listSpaces`                 | GET    | List spaces by membership                       |
| `com.atproto.simplespace.updateSpace`          | POST   | Update a space                                  |
| `com.atproto.simplespace.deleteSpace`          | POST   | Delete a space                                  |
| `com.atproto.simplespace.putMember`            | POST   | Add a member or set their access                |
| `com.atproto.simplespace.removeMember`         | POST   | Remove a member                                 |
| `com.atproto.simplespace.listMembers`          | GET    | List resolved members                           |
| `com.atproto.space.createRecord`               | POST   | Create a record (auto-generated rkey)           |
| `com.atproto.space.putRecord`                  | POST   | Write a record                                  |
| `com.atproto.space.getRecord`                  | GET    | Get a record                                    |
| `com.atproto.space.listRecords`                | GET    | List records                                    |
| `com.atproto.space.deleteRecord`               | POST   | Delete a record                                 |
| `com.atproto.space.applyWrites`                | POST   | Batch write operations                          |
| `com.atproto.space.getLatestCommit`            | GET    | Get per-user signed commit                      |
| `com.atproto.space.getRepo`                    | GET    | Export a user's repo as a CAR file              |
| `com.atproto.space.listRepoOps`                | GET    | List record operation log entries               |
| `com.atproto.space.listRepos`                  | GET    | List the space's writer set                     |
| `com.atproto.space.getBlob`                    | GET    | Get a blob from a space                         |
| `com.atproto.space.listBlobs`                  | GET    | List the blobs a repo's records reference       |
| `com.atproto.space.getDelegationToken`         | GET    | Get a delegation token (step 1 of credentials)  |
| `com.atproto.space.getSpaceCredential`         | POST   | Exchange it for a space credential (step 2)     |
| `com.atproto.space.registerNotify`             | POST   | Register a syncer for write notifications       |
| `com.atproto.space.unregisterNotify`           | POST   | Withdraw a registration                         |
| `com.atproto.space.notifyWrite`                | POST   | Report a repo's new commit                      |
| `com.atproto.space.notifySpaceDeleted`         | POST   | Push a space-deleted notification               |
| `dev.happyview.space.createInvite`             | POST   | Create an invite (HappyView extension)          |
| `dev.happyview.space.acceptInvite`             | POST   | Accept an invite (HappyView extension)          |
| `dev.happyview.space.revokeInvite`             | POST   | Revoke an invite (HappyView extension)          |
| `dev.happyview.space.listInvites`              | GET    | List invites (HappyView extension)              |

`com.atproto.simplespace.addMember` is a deprecated form of `putMember`, kept until v3. See [Legacy addMember](./members.md#legacy-addmember).

## Access model

A space's creator administers it: they update and delete the space and manage its members and invites.

Instance operators can browse any space for moderation through the [admin spaces API](../../api-reference/admin/spaces.md) without being members. Each read of a space's contents is recorded in the event log.

Two independent **policies** decide who can use the space. See [Policies](./managing-spaces.md#policies).

- The **read policy** decides who can obtain a space credential to read the whole space.
- The **write policy** decides whose writes the space tracks and forwards to syncers.

Each policy is **member-list** (the default), **public**, or **managing-app**.

**App access** controls which third-party apps can obtain credentials: **open** (the default) or an **allow list** of OAuth client IDs.

Under member-list policies, each **member** has two flags, `read` and `write`, set independently. The creator is automatically added with both. See [Members](./members.md).

Spaces also support **delegation**: adding another space as a member, which grants its members access to this space.

## Alignment with Proposal 0016

HappyView implements [atproto Proposal 0016](https://github.com/bluesky-social/proposals) (Permissioned Data) with some HappyView-specific extensions.

### Protocol features implemented

- **Namespace split**: `com.atproto.space.*` for protocol routes, `com.atproto.simplespace.*` for management
- **Space authority**: new spaces use the instance's DID, published with `#atproto_space_host` and `#atproto_space` entries
- **Read and write policies**: `memberListPolicy`, `publicPolicy`, `managingAppPolicy`
- **App access**: `open`, `allowList`
- **Member list**: `putMember` with independent `read` and `write` flags
- **Delegation tokens**: `getDelegationToken` (GET, 60-second TTL, single use), or tokens signed by the account's own key
- **Space credentials**: `atproto-space-credential+jwt`, ES256, 10-minute TTL, bound to a key with `cnf.kid` and used with RFC 9421 HTTP Message Signatures
- **Credential revocation**: `notifyCredentialRevoked` sent to hosts of repos in the space
- **Deniable commit signatures**: user signs context (space + author + rev + random IKM), not content hash
- **LtHash**: homomorphic set-hash (2048-byte state, 1024 uint16 lanes, BLAKE3 XOF)
- **SignedCommit**: versioned commit struct (`ver: 1`) with hash, ikm, sig, mac, rev
- **Record operation log**: `listRepoOps` returns the oplog for sync (values inlined by default, `excludeValues` to opt out)
- **Latest commit**: `getLatestCommit` returns the signed commit for a user in a space
- **Repo export**: `getRepo` exports a user's repo as a CAR v1 file (signedCommit + DRISL index)
- **Sync**: `registerNotify` by service identifier, `notifyWrite` in both directions, `listRepos` from the writer set with a space-wide revision
- **Space-scoped blobs**: `getBlob`, `listBlobs`

### HappyView extensions (not in the protocol spec)

- **Invite system**: `createInvite`, `acceptInvite`, `revokeInvite`, `listInvites` (under `dev.happyview.space.*`)
- **`isDelegation` on members**: allows spaces to be members of other spaces
- **`displayName`, `description` on spaces**: human-readable metadata
- **`config` object**: `membership_public`, plus arbitrary extra fields. `records_public` is deprecated and not enforced.
- **`read_self` access**: limits a member's reads to their own records

## Next steps

- [Managing Spaces](./managing-spaces.md): create, update, and delete spaces
- [Members](./members.md): manage membership and delegation
- [Records](./records.md): read and write permissioned data
- [Credentials](./credentials.md): cross-service authentication for spaces
- [Write Notifications](./notifications.md): keep syncers up to date
- [Invites](./invites.md): invite-based membership
- [Changelog](./changelog.md): version history

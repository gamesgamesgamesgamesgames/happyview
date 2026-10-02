---
title: "HappyView v2.16"
description: "HappyView becomes the authority for its own spaces, credentials get signed requests, and space sync goes both ways."
date: 2026-10-02
author:
  name: "Trezy"
  avatar: "/authors/trezy.webp"
tags:
  - announcements
---

Bluesky shipped a new release of the spaces alpha, so... time to catch up again.

This release is almost entirely about atproto spaces. Your HappyView instance is now properly the authority for the spaces it creates, space credentials are bound to a signing key, and sync works in both directions between HappyView and the PDSes that host space repos. There's also a pile of Postgres fixes because apparently the Postgres tests weren't running in CI. (I promise that they do do now. More on that later.)

## RESPECT MAH AUTHORITAH

Every space has an **authority**, which is the DID whose document tells everybody else where the space is hosted and which key signs for it. Until now, a space created on HappyView used the space creator's DID as its authority. That worked fine while HappyView was the only thing that cared, but it doesn't work when other software is involved.

Other hosts look up the authority's DID document to find the space's signing key and its host. With a user's DID as the authority, they'd find the user's PDS, and their DID doc would not cover this. Thanks to that, credentials HappyView minted couldn't be verified anywhere, and updated PDSes sent their write notifications to the creator's PDS instead of to HappyView. Whoops.

New now spaces use your instance's DID as the authority, so a space URI looks like `at://<instance DID>/space/<type>/<skey>`. HappyView publishes an `#atproto_space_host` service entry in its DID document and signs credentials with the instance's `#atproto_space` key, so other hosts can find and verify everything they need.

Important points connected to this change:

- **The creator still runs the space.** Membership, invites, `updateSpace` and `deleteSpace` are all still controlled by whoever created the space, and `isOwner` in `listSpaces` still reflects them.
- **Space keys are unique per type across the whole instance.** If two people try to create a `com.example.forum` space with the same `skey`, the second one gets a `409 Conflict`.
- **Your existing spaces keep working.** Spaces created before v2.16 keep the creator's DID as their authority for now.
- **`did:plc` and linked-account operators have an extra step.** HappyView adds the `#atproto_space_host` entry for you the first time a space is created. `did:web` instances serve it immediately, but if your instance uses `did:plc` or a linked account, you'll need to sync your service entries to the PLC directory so the entry and the `#atproto_space` key show up in your DID document. For linked accounts, that sync goes through the account's PDS, which emails the account holder a confirmation code.
- **No service identity, no change.** If your instance doesn't have a service identity (or doesn't expose it), there's no instance DID for other hosts to resolve, so new spaces keep using the creator's DID as their authority, same as before.
- **Apps need to update their OAuth scopes.** `space:` scopes have to name HappyView as the authority, either with `authority=<your HappyView DID>` or `authority=*`. The default `authority=self` doesn't cover spaces where HappyView is the authority.

## Signed space credentials

Space credentials got a big overhaul to match the spec.

- **HTTP Message Signatures replace DPoP.** To get a credential, your app sends the delegation token as a bearer token, signed with a fresh P-256 key using an [RFC 9421](https://www.rfc-editor.org/rfc/rfc9421) `atproto-space` signature. No OAuth session needed.
- **Credentials are bound to that key.** Every request that uses a credential has to carry a signature from the same key, plus an `Atproto-Space-Audience` header naming who the request is for. That means a host that receives a credential can't turn around and replay it against some other host.
- **Credentials last 10 minutes** instead of 2 hours. HappyView also refuses any credential that lasts longer than an hour, doesn't have a `jti`, or claims to have been issued in the future.
- **Delegation tokens are single-use.** Exchanging the same one twice fails with `InvalidDelegationToken`.
- **Losing read access revokes your credentials.** Removing a member or setting their `read` to `false` revokes their credentials immediately, and HappyView calls `com.atproto.space.notifyCredentialRevoked` on any PDS hosting the space's repos so they know about it too.
- **Errors have names.** `BadSpaceSignature`, `BadSpaceAudience`, `CredentialRevoked`, and `InvalidCredential` replace the old generic failures.

Also, a fun one: HappyView was producing **high-S** ECDSA signatures about half the time. `@atproto/crypto` only accepts low-S signatures, so any peer verifying with it would reject roughly 50% of HappyView's credentials, service auth tokens, and commits, at random. Space credentials, service auth, and commits are all normalized to low-S now.

## Sync goes both ways

With spaces living across HappyView _and_ PDSes, everybody needs to know when something changes. I've filled in all of those gaps:

- **Syncers register by service identifier.** `registerNotify` takes `{space, service}`, where `service` is a DID with an optional fragment (like `did:web:forum.example.com#forum`). HappyView resolves it to an endpoint, so you don't have to pass one.
- **HappyView sends `notifyWrite`.** Registered services get a `com.atproto.space.notifyWrite` call for every commit, authenticated with service auth from HappyView.
- **HappyView accepts `notifyWrite`.** When a PDS hosts a member's space repo, it can tell HappyView about new writes. The call has to be signed by the writing account, and the writer has to pass the space's write policy.
- **Spaces have a revision.** Every accepted repo update advances a space-wide revision, so you can tell exactly what you've already seen.
- **`listRepos` returns the writer set.** It lists every repo in the space in revision order. Pass the last space revision you processed as the `cursor` and you'll only get repos that changed after it.
- **A fallback sweep catches lost notifications.** Every 5 minutes, HappyView re-syncs repos hosted on their authors' PDSes, in case a notification got dropped somewhere along the way.

### Breaking changes

I've tried to keep everything I could backwards compatible (see below), but a few changes couldn't be avoided:

- **Credential clients must sign their requests** and use the `Atproto-Space` scheme. Bearer space credentials are refused.
- **Apps must request `space:` scopes that name the HappyView instance as the authority.**
- **Syncers must accept `notifyWrite` calls** instead of webhook payloads, or keep a legacy registration.
- **The old `getSpaceCredential` flows are gone.** That means the `{grant}` body, the OAuth caller flow, and `dev.happyview.space.getSpaceCredential`. Credentials minted that way were never accepted by anything else anyway.

### Backwards compatibility

HappyView is used by a ton of apps I'm not familiar with and until I have more telemetry enabled, I no way of knowing who's still using older APIs. With that inm mind, every other change in this release keeps the old shape working for now:

- `createSpace` and `updateSpace` still accept a single `policy` or `mintPolicy`, applied to both reads and writes when `readPolicy` and `writePolicy` are missing.
- `addMember` still works alongside `putMember`.
- `appAccess` still accepts the old `{"type": "open"}` and `{"type": "allowList", ...}` shapes.
- `registerNotify` still accepts webhook registrations, and `notifyWrite` still accepts the old payload.
- `type` still works wherever `spaceType` replaced it.
- `getSpaceCredential` responses still include `delegationToken` beside its new name, `token`.

Full details are in the [spaces changelog](/experimental/spaces/changelog).

## Other stuff

### Minor polish

- **`listRecords` can include record values.**
  Pass `includeValues` to a space's `listRecords` and you'll get each record's `value` back with it, the same way `com.atproto.repo.listRecords` does.
- **`listSpaces` can filter by `spaceType`.**
  And `createSpace` takes `spaceType` instead of `type` to match the lexicon.
- **Lua scripts can call `space:put_member`.**
  `space:put_member{did, read, write}` sets a member's flags, and `space:members()` entries now include `read` and `write` booleans.
- **`records_public` is deprecated.**
  This setting was never actually enforced, so setting it didn't change who could read a space. If you want a space anyone can read, use a `public` `readPolicy`. HappyView still accepts and stores `records_public` for the time being.

### Bug fixes

- **Write-only invites stay write-only.**
  Invites stored access as a single word, and `write` was read back as read _and_ write. The create response looked right, but redeeming the invite granted read access that was supposed to be withheld. Invites now store independent `read` and `write` flags, same as members.
- **Expired labels expire on time.**
  Labelers can send an expiry in any timezone offset, and HappyView compared them as text against a UTC cutoff. An expired label could stay live for up to fourteen hours. Expiries are now normalized to UTC on the way in.
- **Write notifications reach syncers for writes from every author**, and syncers are notified when a space is deleted.
- **Managing apps are reached through the service entry their identifier names** (e.g. `did:web:forum.example.com#forum`) instead of always assuming `#atproto_pds`.
- **Service auth tokens bound to a different method are rejected.** HappyView wasn't checking `lxm` on inbound tokens.
- **The record/label scripts page has the right name.**

### Postgres fixes

Several things just didn't work on Postgres. They worked fine on SQLite which is what the majority of operators use, so they were accidentally hidden hidden. 🙃

- **Expired labels are collected.** The cleanup query compared a text column against a timestamp, which Postgres refuses to do, so the sweep failed every time.
- **Plugins can read their key-value store.** Same problem, same fix.
- **Looking up a record by external ID works.**
- **Dead-lettered scripts can be saved and read.** The `payload` column was `JSONB`, which the database driver HappyView uses can't decode. It's `TEXT` now, like every other JSON column.

The Postgres test suite runs in CI now too, so this kind of thing should get caught sooner in the future.

### Security fixes

- **[RUSTSEC-2026-0316](https://rustsec.org/advisories/RUSTSEC-2026-0316) / [GHSA-jqpg-j7w6-42pr](https://github.com/bytecodealliance/wasmtime/security/advisories/GHSA-jqpg-j7w6-42pr)**
  Wasmtime's dynamic record lifting could allocate memory beyond the hostcall fuel limit, so a WebAssembly guest could get the host to allocate more than its budget allowed. Low severity, fixed in Wasmtime 36.0.16.

## Contributors

Thanks to the folks who sent code this cycle:

- [Tyler (@tylersayshi)](https://github.com/tylersayshi) for adding `includeValues` to `listRecords`, and documenting it.
- [noz.am](https://bsky.app/profile/did:plc:lmkzmvv6sdxntwtyxpg7fqqq) for fixing the record/label scripts page name.

## Go play

Full changelog is on [GitHub](https://github.com/gamesgamesgamesgamesgames/happyview/releases/tag/v2.16.0). If you have questions, feature requests, or just need a little help, join the [Cartridge](https://cartridge.dev) [Discord Server](https://discord.gg/BUPnjaBwRZ) and hop into the `#happyview` channel.

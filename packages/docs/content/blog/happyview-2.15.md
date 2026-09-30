---
title: "HappyView v2.15"
description: "Catching up on atproto spaces, interop tests for every PDS that supports them, and automatic space migration."
date: 2026-09-18
author:
  name: "Trezy"
  avatar: "/authors/trezy.webp"
tags:
  - announcements
---

Time for another catch-up! HappyView is once again up-to-date with atproto spaces (formerly known as Permissioned Data and a bunch of other things). Also, HappyView can now automatically move users' spaces into their own repo.

And, as per usual, there's a bunch of quality-of-life updates and fixes.

## Let's talk about spaces

Spaces aren't exactly a spec yet. We have [Proposal 0016](https://github.com/bluesky-social/proposals/tree/main/0016-permissioned-data) as the basis for everything, but most of us are just trying to keep up with [Dan](https://bsky.app/profile/did:plc:yk4dd2qkboz2yv6tpubpc6co)'s work on the [spaces PR](https://github.com/bluesky-social/atproto/pull/5187).

So... HappyView's spaces support has been a **polyfill** from the start. No PDS implemented spaces when I first built it, so HappyView implemented them itself, which is why you could have permissioned data at all. Meanwhile everyone else was choosing between fully public records or building something private from scratch <small>(heh suckers)</small>.

Nowadays spaces are getting support all over the place! We now have a ton of applications building on top of HappyView's spaces implementation, there are several PDS implementations adding early spaces support behind feature flags, and Bluesky even stood up [an alpha PDS for testing spaces](https://atproto.com/blog/atproto-spaces-alpha), with an example app.

### Supporting literally everybody

Since HappyView is an AppView and intended to be generic enough to work across a ton of different applications, I figured it was time to shore up support for all of those different implementations. To that end, I created the [`pds-test`](https://github.com/happyproto/pds-test/) repository.

`pds-test` builds and publishes nightly images for PDSes that support spaces. HappyView now has interop tests for all of these implementations, so those tests are run anytime a new nightly build is created. This allows me to keep up with changes as they happen, like removing fallbacks for PDSes with faulty implementations, or dropping support for out-of-date endpoints that nobody uses anymore.

If you're using HappyView then I'm covering this testing for you, but I'd encourage anybody else building support for spaces into their PDS or AppView or other software to take advantage of `pds-test`. The interop tests are so valuable, and the images are already built, so, like... what are you waiting for?

### A pile of other spaces stuff

- **Conformance**
  - **`space:` OAuth scopes.** Full grammar: `space:<spaceType>[?authority=<did>][&skey=<skey>][&collection=<nsid>…][&action=<action>…][&manage=<op>…]`, with `read`, `read_self`, `create`, `update` and `delete` actions.
  - **Two missing endpoints**: `com.atproto.space.unregisterNotify` and `com.atproto.space.listBlobs`.
    - HappyView still doesn't store blobs. This is mostly for accessing spaces hosted on a PDS.
  - **Credential revocation is keyed on `jti`** instead of a token hash. HappyView shipped revocation before the spec settled on how it should work.
  - **[`community.lexicon.service.describe`](https://discourse.atmosphere.community/t/working-group-service-self-description/) is now supported.** Currently it only advertises spaces endpoints, but in the future it will advertise all of the query and procedure XRPCs your AppView serves.
  - **PDS endpoints resolve through your configured PLC directory** instead of a hardcoded one.
- **Breaking changes**
  - **`mintPolicy` has been split into `readPolicy` and `writePolicy`**, both required on `createSpace`.
  - **Policies are open unions of objects instead of enum strings.** Where you would have sent `"mintPolicy": "managing-app"` plus a sibling `managingApp` field, you must now send `{"$type": "com.atproto.simplespace.defs#managingAppPolicy", "managingApp": "did:web:example.com#forum"}`. The variants are `#publicPolicy`, `#memberListPolicy` and `#managingAppPolicy`. `appAccess` is a union too, either `#open` or `#allowList`.
  - **`addMember` is now `putMember`**, taking required `read` and `write` booleans.
  - **`getConfig` and `updateConfig` are gone** as they were never in the spec. Read configuration from `getSpace`, write it with `updateSpace`.
  - **`getSpace` moved** from `com.atproto.space` to `com.atproto.simplespace`.
  - **Policy and `appAccess` variants HappyView doesn't implement are rejected** at write time with `UnsupportedPolicy` / `UnsupportedAppAccess`, as the spec requires, instead of being stored and ignored.

## Automatic space migration

Technically this is still spaces related, but whatever. It deserves its own section.

So here's where all of this is going. HappyView was never supposed to be the long term solution to spaces. It was a stop gap for applications to build on spaces while the rest of the ecosystem added support. The real goal is that HappyView stops being the repo host for your users' spaces and goes back to just being an AppView. That can't happen in one step, because most PDSes still don't support spaces, and it will likely be some time before all PDSes do.

To solve that, HappyView now supports **automatic space migration**. The next time a user logs in while automatic migration is enabled, HappyView checks if their PDS supports spaces. If it does, their spaces will be migrated to their PDS. If not, everything stays the same.

Part of this is that every permissioned repo now has a mode:

| Mode        | Repo host | Source of truth | HappyView's role                     |
| ----------- | --------- | --------------- | ------------------------------------ |
| `polyfill`  | HappyView | HappyView       | repo host + space host + indexer     |
| `migrating` | both      | HappyView       | as above, but replaying into the PDS |
| `native`    | your PDS  | your PDS        | space host + indexer                 |

A repo's mode is tracked per space _and_ author rather than per user, because whether a space is native depends on the authority's support, not the author's. I want `polyfill` to eventually reach zero so that I can delete all of that code and we can move on with our lives, remembering how awesome HappyView was as a catalyst for the spaces revolution.

All of this sits behind a new `feature.spaces_pds_migration` flag, separate from `feature.spaces_enabled`, so nothing moves until you opt in.

## Other stuff

### Minor polish

- **Backfills can now be multi-DID.**
  When starting a backfill, you're no longer limited to a single DID. Also, you can now add accounts by handle instead of just by DID.
- **Lexicon NSID typeahead, powered by [lexicon.garden](https://lexicon.garden).**
  While typing the NSID for a network lexicon, HV will suggest lexicons that are published across the network. Helpful if you don't know the exact NSID, or you've forgotten how many times to type `games`.
- **API clients can be duplicated.**
  Copy an existing client's configuration into a new one instead of rebuilding it field by field.
- **Better editor navigation.**
  Creating a script from a lexicon page finally takes you to the right place.
- **The linked repo auth pre-page is readable.**
  It was informative and also a mess. Fixed several UI issues so it's ~6,000x easier to parse.
- **Contributing is easier.**
  I added contributor docs and local setup info so it's a little easier to get started if you want to contribute to the project.

### Bug fixes

- **Account-level labels now survive garbage collection.**
  Label GC was removing account-level labels along with the record-level ones it was meant to clean up, so labels applied to an account just disappeared.
- **`@happyview/oauth-client-browser` accepts DIDs in login methods.**
  It originally took handles only, so anything holding a DID had to resolve it back to a handle first, which is, like, so backwards. 😅
- **`prepareLogin` sends a DPoP proof and retries nonce challenges.**
  PARs went out without one, so any authorization server enforcing DPoP at PAR rejected the login before it started.
- **`authority=self` in a space scope resolves to the session's user** instead of being read literally.

### Security fixes

- **[RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285) / [GHSA-2mjx-qc3c-rqvc](https://github.com/rustls/rustls/security/advisories/GHSA-2mjx-qc3c-rqvc)**
  `rustls` accepted TLS 1.3 handshake messages sent at the wrong encryption level when they followed a key-changing message in the same record, where RFC 8446 requires killing the connection. The transcript stays authenticated, so this isn't a path to altering or completing a handshake, but a peer could send messages in plaintext that should have been encrypted without the connection being rejected. Medium severity (5.3), fixed in 0.23.45.

## Contributors

Thanks to the folks who found things this cycle:

- [Tierney (@bnb.im)](https://bsky.app/profile/bnb.im) for creating issues for multi-account backfill, the script editor navigation, and the duplicate client button.
- [Karma (@kzoeps.com)](https://bsky.app/profile/kzoeps.com) for finding the account label garbage collection bug, and both `oauth-client-browser` issues, including the DPoP-at-PAR one that was blocking logins against strict auth servers.

## Go play

Full changelog is on [GitHub](https://github.com/gamesgamesgamesgamesgames/happyview/releases/tag/v2.15.0). If you have questions, feature requests, or just need a little help, join the [Cartridge](https://cartridge.dev) [Discord Server](https://discord.gg/BUPnjaBwRZ) and hop into the `#happyview` channel.

## A little sneaky peaky 👀

If you're interested in development for v3, you can check out the [`next` branch](https://github.com/gamesgamesgamesgamesgames/happyview/tree/next). There's also a `next` branch on the [`happyview-plugins`](https://github.com/gamesgamesgamesgamesgames/happyview-plugins/tree/next) repo.

I'll be publishing another article soon as a sort of State of the Software to make all of the changes easier to understand. 😉

---
title: "HappyView v2.14"
description: "Confidential OAuth clients, opt-in telemetry, and a proxy that finally does what it says."
date: 2026-09-03
author:
  name: "Trezy"
  avatar: "/authors/trezy.webp"
tags:
  - announcements
---

Two big things [again]. First, your instance can now authenticate as a proper confidential OAuth client, taking session lifetime from two weeks to _**two years**_. Second, you can give your instance permission to send me telemetry, which I'll be using to decide what parts of HappyView to focus on next.

Since v2.13 shipped without a post, we're catching up on everything that landed there. The headline from that release is that the XRPC proxy was fundamentally broken and now it's not. Whoops. And also yay!

## New feature: ✨ Confidential OAuth Clients ✨

Until now, HappyView authenticated to PDSes as a **public** OAuth client (regardless of whether you used a public or a confidential HappyView API client). Public clients have no credentials to prove they are who they claim to be. There's a PKCE challenge covering the authorization step, but after that the authorization server has nothing to go on. It then does the sensible thing and caps sessions at two weeks. The result: your users have to login again every 14 days.

However, a **confidential** OAuth client (again, different from a confidential HappyView API client) can sign a `private_key_jwt` assertion with a key it holds, then publish the public half in a JWKS. The authorization server can use that to verify every single request is coming from the real client, and suddenly two weeks becomes _**two years**_.

Now HappyView can generate a JWKS for an API client. Once you plug the `jwks_uri` provided by HappyView into your client metadata doc, it's able to operate as a confidential client, and your users log in roughly 52 times less often.

### This is, like, WAY more machinery than it should be

If you sign a refresh with the wrong key, the reference `@atproto/oauth-provider` doesn't refuse the request — it calls `deleteToken` and **destroys the session**. A `kid` mismatch is not a retryable error. It's a logout, for that user, permanently.

That makes key rotation genuinely dangerous, so HappyView does something magical: it pins sessions to keys. Every session records the ID of the key that established it, and every refresh of that session is signed with **that** key. That means you can retire a key so it can't be used to sign new sessions, but existing sessions continue working. Which gives you two operations with very different blast ~~radiuses~~ ~~radishes~~ radii:

- You can **rotate** an API client's key, which mints a new key and demotes the previous one to `retiring`. A retiring key stays in the JWKS and keeps signing sessions that were established with it. Your users remain logged in, but once the last session signed with that key expires, the key is fully revoked. Rotation is safe, cheap, and you can even do it on a schedule for good security hygiene.
- You can also **revoke** an API client's key, which pulls it out of the JWKS immediately. This also breaks every session pinned to it, so all users get logged out. If your key ever gets leaked, you can pull the trigger and revoke it permanently and immediately.

The dashboard shows you a live session count per key, revoked keys included, so you can confirm a revoke actually worked and see how many people it forced back through login.

You'll find all of this in your dashboard at **Settings → OAuth Keys**, or you can manage it via the Admin API if you'd rather automate the system.

Full docs: [Admin API — OAuth Keys](/api-reference/admin/oauth-keys), [API Clients](/guides/api-clients), [SDK — OAuth Client](/sdk/oauth-client/overview).

### Sessions actually end now

A fun little thing I discovered while doing all of the above: HappyView never called an authorization server's `revocation_endpoint`. I swear on all things holy that I have jumped universes, because I absolutely remember writing the code to revoke sessions, but, uh... it just wasn't there.

Deleting a session locally removes it from HappyView's database and makes HappyView stop using it, but it did nothing at all to the grant on the user's PDS. So a logout, a device revoke, and an account unlink all left a live, working grant sitting on the user's account until the refresh token expired on its own. That was bad at two weeks. At two years it's about fifty times worse (er, fifty two times, I guess).

Now every logout path revokes with the PDS _first_, then deletes locally. Revocation is treated as advisory, tho: if the PDS is down or advertises no revocation endpoint, it's logged and swallowed so that a dead PDS can't make "log out" fail.

Two more session fixes in the same area:

- **Refreshes are now single-flight per session.** If several requests were in flight at the exact moment a token expired, each one sent its own refresh carrying the same token. One of them would win, and the rest came back as `invalid_grant` because HappyView assumed they were replays. HappyView then read that as a dead grant and deleted the session, including the DPoP key row it depends on, which could land _while the winner's refresh was still in flight_ and cause it to fail to save. The rotated token then existed nowhere and the session was unrecoverable. Clients weren't logged out so much as stuck: every subsequent call 401'd, including the logout call that would have cleaned it up. Refreshes now take a per-session lock, and the losing path returns an auth error without deleting anything.
- **Clients retire their own previous session.** Sessions are per-device, so the server can't tell the difference between a browser signing in again and a second, completely different device. As a result, cleaning up on re-registration would sign you out everywhere. The SDK _can_ tell, tho, because it's holding the session that it's about to overwrite, so now it retires that one itself.

## New feature: opt-in telemetry

I have no clue how many HappyView instances are out there right now. There's a handful of people that show up in the Discord, some that file issues on Github, and a few people that talk about their projects on Bluesky and mention that they're using HappyView. Outside of that, I don't know how many instances exist, how big they get, which features actually get used, or what breaks in the wild. Every roadmap decision I make is basically a guess informed by my own needs and whoever complained most recently.

HappyView can now send me some basic telemetry. It is **off by default**, and it's granular so you can choose what you're comfortable sharing:

- **Off.** The default. Nothing is collected, nothing is sent.
- **Manual.** Nothing is sent automatically. You get a button and you can see the complete payload that will be sent before you decide whether to send it.
- **Auto.** One JSON document a day gets sent automatically to my servers. You can still see what will be sent, tho.

Each snapshot contains an instance id (generated by your instance, but completely random so nothing is identifiable), the HappyView version, uptime, a handful of counters (records, jetstream events, requests, that sort of thing), which features are in use, and a broad host shape: CPU count, memory, database size.

Notably absent unless you go turn them on individually: the **names** of your lexicons, their **structures**, and their **full documents**. Those are three separate switches, because a custom NSID usually names your app and I'd rather you decide to hand that over on purpose. Contact info is its own optional field.

In return for your data, my API sends back some benchmarks: how your instance performs next to others of similar size, per metric. That part only works once enough instances of a comparable size are reporting, so early on you'll get "not enough comparable instances yet." I'm hopeful y'all will hook me up with a bunch of cool data, tho, so the missing comparisons are temporary.

You can find the settings at **Settings → Telemetry**, or in the setup wizard on new instances.

## The XRPC proxy, fixed

This is the v2.13.1 headline, and it deserves its own section because the old behavior was actively confusing.

An XRPC method with no registered lexicon and no explicit route falls through to the proxy. There are two entirely independent questions to answer about such a request: **may** it be proxied, and **where** does it go. HappyView had them tangled into a single setting.

The "where" answer was wrong. It resolved the NSID's `_lexicon` DNS record to find the method's authority and forwarded there, but an NSID's authority answers "who defines this schema", not "who serves this request", and for many NSIDs those are wildly different. As an example, let's look at `com.atproto.repo.*`: the authority is whatever account publishes the `com.atproto` lexicons, which has no relationship to your repo whatsoever. Worse, that path forwarded **no credentials at all**. So calling `com.atproto.repo.createRecord` through HappyView sent an anonymous request to a stranger's PDS, which correctly answered `AuthMissing`, which HappyView relayed back verbatim. This resulted in a lot of confused HappyView operators wondering why their proxied requests returned an authentication error when there were definitely credentials attached. It even made PDS issues look like HappyView bugs.

The two axes are now separate settings:

- **Mode** (`disabled` / `open` / `allowlist` / `blocklist`) controls whether a method may be proxied at all.
- **Routing** controls where it goes. The new `serviceproxy` option follows the [service proxying spec](https://atproto.com/specs/xrpc#service-proxying): forward to the caller's own PDS, authenticated as the caller. Absent an `atproto-proxy` header the PDS handles it locally, which is what makes `com.atproto.repo.createRecord` work. With one, the header passes through and the PDS relays onward.

`authority` remains the default for backward compatibility, and switching to `serviceproxy` is opt-in. That's partly because it's a real behavior change, and partly because under `serviceproxy` an anonymous unrecognized query has to be a 401.

## Other stuff

### Minor polish

- **NSID validation now matches the spec.**
  HappyView had its own hand-rolled NSID validation, and it was wrong. It rejected authority segments starting with a digit (like `pics.2bit.feed.getPhotos`, which is perfectly valid and was being refused), and it accepted two-segment NSIDs, segments ending in a hyphen, and arbitrarily long segments, none of which are legal. There's now a Rust crate and a matching [`@happyview/nsid`](https://www.npmjs.com/package/@happyview/nsid) npm package, both holding the spec's canonical regex and both pinned by the same interop corpus, so the server and the dashboard can't disagree about what a valid NSID is. The npm package is standalone and tiny if you want to use it for your own project.
- **A startup audit for NSIDs you already saved.**
  Because that validation only tightened on _write_ paths, a trigger id or proxy pattern stored under the old looser rules will keep working. When you try to save changes to it, tho, you're prompted to fix the NSID. HappyView also scans for those issues at boot and names them in the logs. The scan is read-only and fails open, so a config that currently works won't cause boot failures after an upgrade.
- **New guide: [Hardening an Instance](/guides/hardening).**
  What each auth setting actually grants, why `transition:generic` is broader than you probably want, and how to narrow it. Several of these can't be changed later without re-consent, so getting them right the first time is much cheaper.
- **New guide: [Local Development](/getting-started/deployment/local-development).**
  The full stack locally with hot reloading — `cargo watch` for the server, Next.js dev server for the dashboard.
- **Example production compose files.**
  For when you'd like to deploy without reverse-engineering it from the development compose file.

### Bug fixes

- **did:plc operations no longer fail half the time.**
  P-256 signatures put the S value in either half of the curve order at random, and the PLC directory verifies with `lowS: true`. HappyView wasn't normalizing, so roughly _half_ of all did:plc registrations and service-entry updates came back `400 Invalid signature on op` while the other half worked perfectly. Signatures are now normalized to low S, and there's a _very robust_ test that signs with 64 different keys specifically so this can never hide behind a coin flip again.
- **Giving a user access to your instance based on their handle works.**
  Handles weren't being resolved to DIDs. Whoops.
- **Lexicons can't be uploaded without an ID.**
  It was possible to upload a local lexicon with an empty-string ID, which was stupid. Now it's not stupid.
- **Adding a lexicon with `backfill: true` starts a backfill.**
  The flag was being set, but no job was ever created.
- **The unsaved-changes guard stops firing after you save.**
  The script editor's dirty guard was warning you about unsaved changes immediately after saving them. It was annooooooooying.
- **Event logs can be purged**, and record-skip events no longer flood them. Those are now behind verbose logging.
- **DPoP keys are included with auth requests.**
- **Fixed the dead links in the onboarding docs.**

### Security fixes

Two advisories, both in dependencies:

- **[RUSTSEC-2026-0269](https://rustsec.org/advisories/RUSTSEC-2026-0269) / [GHSA-vqjp-4c8c-hfgg](https://github.com/bytecodealliance/wasmtime/security/advisories/GHSA-vqjp-4c8c-hfgg)** — a filesystem sandbox escape in `wasmtime` when paths or symlinks contain trailing slashes. Fixed by moving to 36.0.14, which brings the `cranelift` crates along with it. This is only reachable if you run plugins, but update anyway.
- **[RUSTSEC-2026-0258](https://rustsec.org/advisories/RUSTSEC-2026-0258) / [GHSA-q83h-524g-xf6h](https://github.com/hyperium/hyper/security/advisories/GHSA-q83h-524g-xf6h)** — `h2` would accept and queue empty DATA frames without limit, which on undrained streams means unbounded memory usage or an overflow panic. Low severity, fixed in 0.4.16.

## Contributors

Thanks to the folks who found things this cycle:

- [Alex (@a2.co)](https://bsky.app/profile/a2.co) for reporting the issue with HappyView auth not reusing existing sessions.
- [Karma (@kzoeps.com)](https://bsky.app/profile/kzoeps.com) for reporting the issue with DPoP not being sent when it should be.
- [Jeff (@tidaltheory.io)](https://bsky.app/profile/tidaltheory.io) for catching that lexicons could be created with an empty ID.
- [Tierney (@bnb.im)](https://bsky.app/profile/bnb.im) for helping test things out and continuing to find every rough edge I've left lying around, like the broken onboarding links and a bunch of issues with the event log.
- [Meri (@meri.garden)](https://bsky.app/profile/meri.garden) and [Erlend (@erlend.sh)](https://bsky.app/profile/erlend.sh) of the [Roomy](https://roomy.space) team for motivating me to add handling for confidential clients.

## Go play

Full changelog is on [GitHub](https://github.com/gamesgamesgamesgamesgames/happyview/releases/tag/v2.14.0). If you have questions, feature requests, or just need a little help, join the [Cartridge](https://cartridge.dev) [Discord Server](https://discord.gg/BUPnjaBwRZ) and hop into the `#happyview` channel.

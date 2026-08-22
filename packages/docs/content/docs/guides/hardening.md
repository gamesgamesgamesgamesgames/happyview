---
title: "Hardening an Instance"
---

A HappyView instance holds OAuth credentials for its users and will act with them on behalf of the applications you register. This guide is about narrowing that authority: what each setting actually grants, and why the narrow option is worth the extra work.

The recurring theme is that **a scope you grant is a scope some application can use**, and several of these settings cannot be changed after the fact without re-consent. Getting them narrow the first time is much cheaper than widening and re-narrowing later.

## Scope your API clients

An API client's registered scopes are the ceiling on what any token issued to it can carry. HappyView refuses to issue a token carrying scopes the client is not registered for, so this is the single most effective control you have.

### Prefer granular scopes to `transition:generic`

`transition:generic` is the broad legacy grant. It covers:

- every `repo` operation, on every collection
- every `blob` upload, of every type
- every `rpc` call whose method is not under `chat.bsky.`

It does **not** cover `identity` or `account` operations — those always need explicit scopes, even for a client holding it. That is a genuine limit, but it is the only one.

Granular scopes name what your app actually does:

```
atproto repo:com.example.post repo:com.example.like blob:image/*
```

- `repo:<nsid>` grants create, update and delete on that collection. Narrow further with repeated `?action=` parameters: `repo:com.example.post?action=create&action=update`.
- `blob:<type>/<subtype>` grants uploads of that media type. `blob:image/*` is a reasonable middle ground; `blob:*/*` is not much better than nothing.
- `rpc:<nsid>?aud=<did>` grants calling a method against a service. The audience is required. `?aud=*` permits any service — prefer a concrete `did:web:example.com#service` where you know it.

`atproto` is required on every request and is always implied.

### Use permission sets for anything non-trivial

A `permission-set` lexicon lets you declare a group of permissions once and reference it as `include:<nsid>`:

```
atproto include:com.example.appAccess
```

Two properties make this safer than it looks:

- **Authority containment.** A permission set may only grant NSIDs under its own authority group. `com.example.appAccess` can grant `com.example.*` and nothing else. Publishing a permission set is therefore not a way to vouch for somebody else's collections.
- **No pinned audiences.** An `rpc` permission inside a set may declare `aud: "*"` or inherit the audience from the `include:` scope, but may not name a concrete service itself. A set cannot quietly aim your users' credentials at a service of its own choosing.

Containment is all-or-nothing per permission entry. An entry listing both your own collection and someone else's grants *neither* — split them into separate entries, and declare the foreign collection directly in the client's registered scopes instead.

## Lock down the XRPC proxy

Any XRPC method with no lexicon registered on your instance falls through to the proxy. By default the proxy is **open**: it will attempt to serve any NSID a caller names.

Under **Settings → XRPC Proxy** you can change this to:

- **Disabled** — only locally registered lexicons are served. The strongest setting, and correct if your instance serves only its own lexicons.
- **Allowlist** — only NSIDs matching a listed pattern are proxied. Patterns may be exact (`com.example.feed.getHot`) or wildcards (`com.example.*`).
- **Blocklist** — everything except the listed patterns.

Allowlist mode is the one to reach for. Enumerate the NSIDs your app actually calls and list those. It is more work up front, and it means a new dependency fails visibly at deploy time rather than silently widening what your instance will fetch on a caller's behalf.

<Callout type="info">
The open default is a carry-over from HappyView 2.0, which shipped before the proxy had any configuration at all. It will change in v3. Setting this explicitly now means the upgrade does not change your instance's behaviour underneath you.
</Callout>

### Choose a routing mode deliberately

Separately from *whether* a method may be proxied, **Routing** decides *where* it goes. The two are independent axes.

- **Lexicon authority** (the default) resolves the NSID's authority via DNS and forwards there, unauthenticated. This answers "who defines this schema", not "who serves this request". For `com.atproto.repo.*` those have different answers: the authority is whoever publishes the `com.atproto` lexicons, which has no access to your users' repos, so such requests arrive unauthenticated and are refused.
- **Service proxy** forwards to the caller's own PDS, authenticated as them, following the [service proxying](https://atproto.com/specs/xrpc#service-proxying) model. Absent an `atproto-proxy` header the PDS handles the request itself — which is what makes `com.atproto.repo.createRecord` work through HappyView. With one, the header is passed through for the PDS to relay onwards.

Service proxy is the correct model, and the default will change in a future release. Two consequences to weigh before switching:

- **Anonymous unrecognized queries stop working.** Under authority routing an unauthenticated caller can hit an unrecognized NSID and get an answer. Under service proxy there is no PDS to forward to, so it returns 401. If clients rely on that, they need to authenticate first.
- **Authenticated clients reach more.** Requests are forwarded with the user's token, so what a client can do is bounded by that token's scopes rather than by which lexicons you have registered. Scope your API clients before switching, per the section above.

HappyView additionally checks repo writes and blob uploads against the session's granted scopes before forwarding. That is a pre-flight check, not the boundary — the PDS enforces scopes regardless — and it deliberately covers only operations that map unambiguously to a permission. It is not a substitute for scoping clients properly.

## Scope linked repos narrowly, because you cannot edit them

A [linked repo](./linked-repos.md) is an admin-managed OAuth grant over somebody's repo. Its scopes are **immutable by design** — changing them means revoking the grant and creating a new one, which requires the account holder to complete the invite again.

That immutability is what makes the advertised scope union safe to recompute live, so it is not going away. The practical consequence: decide the scopes before you send the invite. A grant created with `repo:*` to "get it working" is a grant you will have to ask someone to re-authorize in order to narrow.

Two rules worth knowing before you write one:

- `putRecord` is an upsert, so it needs **both** `create` and `update` unless the caller supplies `swap_cid`. Passing `swap_cid` is a real no-create guarantee, which narrows the requirement to `update` alone.
- Repo reads need no scope at all. There is no read permission in the grammar because repo records are public.

## Understand what `atproto-proxy` forwarding means

When a request carries an `atproto-proxy` header, it is asking to be relayed onwards to a named service. HappyView forwards that header to the user's PDS rather than resolving it — the token that accompanies a relayed request is signed by the *user's own identity key*, which only their PDS holds.

The consequence worth internalising: **the PDS will mint an identity-bearing assertion addressed to whatever service the caller names.** Its audience binding stops that assertion being replayed elsewhere, but the named service still receives proof of that user's identity.

There is no destination allowlist in HappyView for this, and that is deliberate — the decision of whether to sign for a given audience belongs to the party holding the key, which is the PDS. What *is* in your hands is who gets to make such a request at all: registering a third-party API client is the act that grants it. Register clients deliberately, and scope them as above.

## A checklist

- [ ] Every API client registered with granular scopes, or a permission set, rather than `transition:generic`
- [ ] `blob:` scopes narrowed to the media types the app uploads
- [ ] `rpc:` scopes given a concrete `aud` wherever the service is known
- [ ] XRPC proxy set explicitly — allowlist if you can enumerate your NSIDs, disabled if you serve only your own
- [ ] Linked-repo grants scoped before the invite goes out, not after
- [ ] Third-party API clients reviewed: each one can ask the PDS to sign for an audience of its choosing

## Related

- [API Clients](./api-clients.md) — creating clients and choosing a type
- [Linked Repos](./linked-repos.md) — the grant and invite lifecycle
- [Permissions](./permissions.md) — dashboard user permissions, which are separate from OAuth scopes

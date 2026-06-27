---
title: "Glossary"
---

Key terms used throughout the HappyView documentation. For a broader introduction to the atproto, see the [official ATProto glossary](https://atproto.com/guides/glossary).

## atproto terms

**AppView** — A backend service that indexes atproto records and serves them through an API. HappyView is an AppView. See the [ATProto docs](https://atproto.com/guides/glossary#app-view) for more.

**DID** (Decentralized Identifier) — A persistent, globally unique identifier for an account (e.g. `did:plc:abc123`).

**Firehose** — A real-time stream of all record events (creates, updates, deletes) across the atproto network. HappyView consumes a filtered slice of this via [Jetstream](https://github.com/bluesky-social/jetstream).

**Handle** — A human-readable name for an account (e.g. `user.bsky.social`). Handles resolve to a DID via a DNS TXT record or an HTTP `.well-known/atproto-did` lookup.

**Lexicon** — A schema definition for atproto data types and API methods. Lexicons define what records look like, what endpoints exist, and what parameters they accept. See [Lexicons](../guides/lexicons.md).

**NSID** (Namespaced Identifier) — A reverse-DNS identifier for a lexicon (e.g. `xyz.statusphere.status`). The authority is everything except the last segment.

**PDS** (Personal Data Server) — The server that hosts a user's data. Users can be on any PDS — there's no single server. HappyView proxies writes back to each user's PDS.

**PLC directory** — A public service (e.g. `plc.directory`) that maps DIDs to their DID documents, which contain the user's PDS endpoint and other metadata.

**Record** — A single piece of data in an atproto repository, identified by an AT URI (e.g. `at://did:plc:abc/xyz.statusphere.status/abc123`).

**Relay** — A network service that aggregates repository data from many PDSes. HappyView queries the relay during [backfill](../guides/backfill.md) to discover which repos contain records for a given collection, then fetches each repo's records directly from its PDS.

**rkey** (Record Key) — The unique key for a record within a collection and repo. These are most commonly TIDs (timestamp-based) or NSIDs.

**TID** (Timestamp Identifier) — A 13-character sortable identifier used as a record key. Generated from the current timestamp.

**XRPC** — The HTTP-based RPC protocol used by the atproto. Query methods map to GET requests, procedure methods map to POST requests. See [XRPC API](../api-reference/xrpc-api.md).

**Jetstream** — A [filtered firehose](https://github.com/bluesky-social/jetstream) that delivers atproto record commit events as JSON over WebSocket. Not part of the core atproto spec, but widely used. HappyView subscribes to Jetstream with a collection filter built from its indexed record lexicons, and persists a cursor for resume on reconnect.

## HappyView-specific terms

**App Access** — Controls which third-party apps can interact with a space. Either `open` (any app) or `allowList` (only specified apps). Set via `com.atproto.simplespace.updateConfig`.

**Authority DID** — The DID that controls a space. Distinct from the creator DID (who originally created it). Replaces the earlier `owner_did` concept.

**Backfill** — The process of bulk-indexing existing records from the network. HappyView discovers repos via the relay and fetches each repo's records directly from its PDS. Runs when a new record-type lexicon is uploaded or triggered manually. See [Backfill](../guides/backfill.md).

**Delegation Token** — A short-lived JWT (`typ: atproto-space-delegation+jwt`, ES256K, 60-second TTL) that proves a user is a member of a space. Used as step 1 of the credential issuance flow. Obtained via `com.atproto.space.getDelegationToken`.

**LtHash** — A homomorphic set-hash used for per-user repo state in spaces. Uses a 2048-byte state with 1024 little-endian uint16 lanes and BLAKE3 XOF. Supports incremental insert/remove operations.

**Mint Policy** — Controls who can create permissioned repos in a space: `member-list` (only members), `public` (anyone), or `managing-app` (only the managing app).

**Network lexicon** — A lexicon fetched directly from the atproto network via DNS authority resolution, rather than uploaded manually. See [Lexicons - Network lexicons](../guides/lexicons.md#network-lexicons).

**Permission** — A granular access control right that authorizes a specific action in the admin API. HappyView defines 44 permissions organized by category (e.g. `lexicons:create`, `users:read`). See [Permissions](../guides/permissions.md).

**Permissioned Data** — AT Protocol data that is gated by membership in a space, as opposed to public repo data. Defined by AT Protocol Proposal 0016.

**Permission template** — A predefined set of permissions that can be applied when creating a user. Templates are: **Viewer** (read-only access), **Operator** (viewer + backfill and API key management), **Manager** (operator + lexicon, record, spaces, and plugin management), and **Full Access** (all 44 permissions).

**Space** — A container for permissioned data in AT Protocol. Identified by a space DID, type NSID, and space key (skey), forming an `ats://` URI.

**Space Credential** — A short-lived JWT (`typ: atproto-space-credential+jwt`, ES256, 2-hour TTL) for cross-service read access to space data. Signed by the space's P-256 keypair. Obtained by exchanging a delegation token via `com.atproto.space.getSpaceCredential`.

**Super user** — The bootstrapped user created on first login to a fresh HappyView instance. The super user has unrestricted access to all endpoints regardless of permissions, can transfer super status to another user, and cannot be deleted.

**Target collection** — The record collection that a query or procedure lexicon operates on. Set via the `target_collection` field when uploading a lexicon.

---
title: "Lexicons"
---

Lexicons are the core building block of HappyView. They're [atproto schema definitions](https://atproto.com/specs/lexicon) that describe your data model, and HappyView uses them to decide which records to index from the network and what XRPC endpoints to serve.

You don't write route handlers or database queries; you upload a lexicon and HappyView generates the infrastructure from it. There are two ways to add lexicons: uploading them via the [admin API](../api-reference/admin/lexicons.md) or [dashboard](../getting-started/dashboard.md), or fetching them directly from the atproto network via [DNS authority resolution](#network-lexicons).

## Supported lexicon types

| Type          | Effect                                                                                                                                   |
| ------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| `record`      | Adds the collection to the Jetstream subscription filter and indexes records into the database. Supports [record scripts](label-scripts) |
| `query`       | Registers a `GET /xrpc/{nsid}` endpoint that queries indexed records                                                                     |
| `procedure`   | Registers a `POST /xrpc/{nsid}` endpoint that proxies writes to the user's PDS                                                           |
| `definitions` | Stored but does not generate routes or subscriptions                                                                                     |

A typical setup has three lexicons working together: a **record** lexicon that defines the data and triggers indexing, a **query** lexicon that exposes a read endpoint, and a **procedure** lexicon that exposes a write endpoint. The [Statusphere tutorial](../tutorials/statusphere.md) walks through this pattern end-to-end.

## Target collection

Query and procedure lexicons don't store data themselves. They operate on records stored by a record-type lexicon. The `target_collection` field tells HappyView which record collection to read from or write to. Without it, default queries and procedures won't know which DB records to operate on.

For example, a query lexicon `xyz.statusphere.listStatuses` would set `target_collection` to `xyz.statusphere.status` to read from that record collection.

See the [admin API](../api-reference/admin/lexicons.md#upload--upsert-a-lexicon) for how to set `target_collection` when uploading.

<Callout type="info">
The `target_collection` is available in Lua scripts as the `collection` global, but it is not required if your endpoint uses a Lua script.
</Callout>

## Backfill flag

When uploading a record-type lexicon, HappyView automatically creates a backfill job to discover existing records. If you only want to index new records going forward, you can set `backfill` to `false`.

## Jetstream collection filters

When record-type lexicons change (uploaded or deleted), HappyView reconnects to Jetstream with an updated collection filter. HappyView always includes `com.atproto.lexicon.schema` in the filter to track network lexicon updates.

Deleting a lexicon stops live indexing for that collection but does **not** remove previously indexed records from the database. If you want to start fresh, you'll need to delete the records separately (e.g. via the admin API or directly in the database) before re-adding the lexicon and running a [backfill](backfill.md).

## Network lexicons

If a lexicon has already been published, you don't need to upload the JSON manually. Point HappyView at the NSID and it fetches the lexicon directly from the network. Network lexicons are kept updated automatically via the Jetstream subscription. If the publisher updates their schema, your instance will pick up the change.

### NSID authority resolution

Lexicons are stored as records themselves with the `com.atproto.lexicon.schema` NSID and the rkey set to the lexicon's NSID. To find which repo holds a lexicon, HappyView resolves the NSID's authority:

1. Extract the authority from the NSID (all segments except the last). For example, `xyz.statusphere.status` has authority `xyz.statusphere`.
2. Reverse the authority segments to form a domain: `statusphere.xyz`.
3. Look up the DNS TXT record at `_lexicon.{domain}` (e.g. `_lexicon.statusphere.xyz`).
4. Parse the TXT record for a `did=<DID>` value.
5. Resolve the DID to a PDS endpoint via the PLC directory.

<Callout type="info">
The spec states that resolution must be **non-hierarchical**. Each authority requires its own explicit TXT record. If you have multiple levels of authority (e.g. `xyz.statusphere.status` and `xyz.statusphere.actor.profile`), each level must have an explicit TXT record.
</Callout>

### Fetching

Once the authority DID and PDS endpoint are known, HappyView calls `com.atproto.repo.getRecord` with:

- `repo` = the authority DID
- `collection` = `com.atproto.lexicon.schema`
- `rkey` = the NSID

The `value` field of the response is the raw lexicon JSON.

### Live updates via Jetstream

HappyView's Jetstream subscription always includes the `com.atproto.lexicon.schema` collection, so it receives real-time events whenever a lexicon schema record is created, updated, or deleted on the network. When an event arrives, HappyView checks whether the record's DID and rkey (the NSID) match any tracked network lexicon:

- **create/update**: The new schema is parsed and upserted into the `happyview_lexicons` table and the in-memory registry. If it's a record-type lexicon, Jetstream collection filters are updated to include the new collection.
- **delete**: The lexicon is removed from the `happyview_lexicons` table and registry, and collection filters are updated accordingly.

### Startup re-fetch

On every startup, HappyView re-fetches all network lexicons from their respective PDSes. This ensures consistency even if events were missed while offline. Failures are logged as warnings but don't block startup.

## XRPC routing for unknown methods

When a client calls `/xrpc/{method}` and HappyView has a local lexicon for that NSID, the request is handled by the lexicon's Lua script (or HappyView's default behavior if no script is attached). Otherwise, HappyView attempts to proxy the request to the method's **home authority** using the same DNS-based authority resolution described above:

1. Extract the authority from the NSID (all segments except the last). `com.example.foo.getBar` → authority `com.example.foo`.
2. Reverse it to form a domain: `foo.example.com`.
3. Look up the `_lexicon.foo.example.com` TXT record for the authority's DID.
4. Resolve that DID to a PDS endpoint via the PLC directory.
5. Proxy the request to `{pds_endpoint}/xrpc/{method}`.

A few things to note:

- HappyView does **not** proxy to the reversed hostname directly. `foo.example.com` is only the DNS host for the TXT record — the actual XRPC request goes to whatever PDS endpoint the authority DID resolves to.
- Proxying applies equally to queries and procedures. For procedures, HappyView uses the caller's OAuth session to attach a DPoP-bound access token (see [Authentication](../getting-started/authentication.md#proxying-procedures-to-the-users-pds)).
- If authority resolution fails — no TXT record, unresolvable DID, or the target PDS doesn't support the method — the client gets an error back. HappyView does not fall back to any other routing strategy.
- Tracking a network lexicon does **not** make HappyView handle requests for that NSID locally. Network lexicons are only about indexing record collections and keeping the schema up to date. If a client calls a query NSID that you've tracked as a network lexicon but haven't uploaded a local query lexicon for, HappyView still proxies the request out — it won't query your local record table. To serve a method locally, upload a local query or procedure lexicon with a matching `target_collection`.

In short: if you want to serve an XRPC method on your instance, you need a local lexicon for it. Otherwise HappyView attempts to proxy to the method's home authority.

## Next steps

- [Lua Scripting](./lua-scripting.md): Add custom query and procedure logic to your endpoints
- [Record & Label Scripts](label-scripts): Run Lua scripts when records are indexed or labels arrive
- [XRPC API](../api-reference/xrpc-api.md): Understand how the generated endpoints behave
- [Backfill](backfill.md): Learn how historical records are indexed
- [Admin API](../api-reference/admin/admin-api.md): Full reference for lexicon management endpoints

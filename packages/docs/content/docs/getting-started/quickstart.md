---
title: "Quickstart"
---

This page walks you through the fastest path to a working HappyView instance. By the end, you'll have an AppView that indexes records from the atproto network and serves XRPC endpoints.

## 1. Deploy HappyView

Pick whichever option fits your situation:

| Option                                     | Best for                                                                             |
| ------------------------------------------ | ------------------------------------------------------------------------------------ |
| [**Railway**](deployment/railway.md)       | Fastest path — one-click deploy of HappyView + Postgres |
| [**Docker**](deployment/docker.md) | Self-hosting on your own server, from the images published to ghcr.io |
| [**Local development**](deployment/local-development.md) | Running the whole stack locally with hot reloading |
| [**From source**](deployment/other.md)     | Running HappyView with `cargo run` and managing dependencies yourself                |

If you're just trying HappyView for the first time, start with Railway.

## 2. Log in to the dashboard

The built-in [dashboard](dashboard.md) is served at your instance's root URL. Log in with your atproto identity — on a fresh deployment, the first handle to authenticate is automatically bootstrapped as the **super user** with all permissions, so use the handle you want to own the instance.

## 3. Add your first lexicon

Lexicons tell HappyView what data to index and what endpoints to serve. The quickest way to get started is to add one from the network:

1. In the dashboard, go to **Lexicons > Add Lexicon > Network**
2. Enter an NSID (e.g. `xyz.statusphere.status`)
3. HappyView [resolves the schema](https://atproto.com/specs/lexicon#lexicon-publication-and-resolution) from its authority domain records and shows a preview
4. Click **Add**

HappyView starts indexing records for that collection. A backfill job fetches historical records, and new records stream in via Jetstream.

You can also upload lexicons manually via the dashboard or the [admin API](../api-reference/admin/admin-api.md). See [Lexicons](../guides/lexicons.md) for the full details.

## 4. Verify records are being indexed

The dashboard home shows a live record count and a per-collection breakdown. For a deeper look, browse **Records** to inspect individual rows or **Backfill** to watch the historical fetch job drain.

## 5. Query your data

Once you have a record lexicon indexed, add a query lexicon to expose a read endpoint. Go to **Lexicons > Add Lexicon > Local** and create a query lexicon with `target_collection` set to your record collection's NSID. (`target_collection` is a HappyView-specific field that tells a query or procedure which record collection it operates on.)

Without a Lua script, HappyView generates a default query endpoint that supports `limit`, `cursor`, `did`, and `uri` parameters:

```
GET /xrpc/xyz.statusphere.listStatuses?limit=5
```

For custom query logic, attach a [Lua script](../guides/lua-scripting.md).

## Next steps

- [**Statusphere tutorial**](../tutorials/statusphere.md): full walkthrough building a complete AppView with record, query, and procedure lexicons
- [**Lexicons guide**](../guides/lexicons.md): target collections, backfill flag, network lexicons
- [**Lua Scripting**](../guides/lua-scripting.md): custom query and procedure logic
- [**Configuration**](configuration.md): environment variables and tuning
- [**Authentication**](authentication.md): how OAuth works and how to get API tokens

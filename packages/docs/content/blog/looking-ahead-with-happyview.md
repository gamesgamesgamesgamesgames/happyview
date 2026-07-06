---
title: "Looking Ahead with HappyView"
description: "What's next on the roadmap for your favorite AppView software?"
date: 2026-07-06
author:
  name: "Trezy"
  avatar: "/authors/trezy.webp"
tags:
  - announcements
  - roadmap
atUri: "at://did:plc:qneu5uamqs6cug7sjobbaexm/site.standard.document/3mpybucncxh2y"
---

HappyView is a hell of a project. It's ridiculously fast, it's chock full of **big** features, and it does an excellent job of providing the tools you need and then getting out of your way. I'm proud of everything I've built so far, and I am _so excited_ for what's to come.

I'd like to start by taking a look at where we are today, including everything that's been built up to now. After that, I'll dive into what's coming next, and where I'd like HappyView to go in the future.

## HappyView v1

The first version of HappyView got the job done, but it was _so clunky_. Auth was a mess, running an instance of HappyView required running 3 other pieces of software, and there were bugs everywhere. I don't think I even had tests for that first version.

You could upload lexicons, write some basic scripts, and everything would do what you expected! It was already solid, even if it wasn't pretty. But the experience of actually _running_ the thing was rough. You needed HappyView itself, a [Tap](https://github.com/bluesky-social/indigo/tree/main/cmd/tap) server for ingestion, an [AIP](https://github.com/graze-social/aip) server for auth, and a Postgres server that all 3 would share. All of them had to be deployed, configured, and kept in sync. If something broke during a backfill, you were basically on your own. Every time somebody else spun up a HappyView instance I felt like I owed them a handwritten apology.

But v1 proved the idea worked. Upload a lexicon, get a working XRPC API. That was always the promise, and it at least delivered that.

## HappyView v2

v2 is where HappyView actually became _good_.

The companion services got absorbed — no more Tap, no more AIP, no more keeping three separate deployments in sync. Just one binary. SQLite became the default database so you could spin up a fresh instance with zero external dependencies. The barrier to entry dropped from "configure three services and a database" to "download and run."

The scripting system went from one script per lexicon to a proper [trigger-based system](/guides/lua-scripting) with eight different trigger types. Scripts got their own management interface, their own API endpoints, and access to a full suite of Lua APIs for database queries, HTTP requests, XRPC calls, and atproto operations. If scripting in v1 felt like Baby's First AppView, scripting in v2 feels like an AppView that's all grown up.

Auth got a complete rewrite with DPoP-bound tokens. Backfill went from "start it and pray" to a proper two-phase pipeline with pause, resume, cancellation, concurrent fetching, and real progress tracking. The permission system got rebuilt. The docs site got rebuilt. The dashboard got rebuilt.

Uh... I guess almost everything got rebuilt.

Then there's everything that landed in the point releases: [permissioned spaces](/experimental/spaces/), [service identity](/getting-started/service-identity) so your AppView can participate in standard atproto routing, a [background job system](/guides/lua-scripting) for long-running tasks, `db.query` filters so you don't have to write raw SQL for basic lookups, blob utilities for Lua scripts. It's been _a lot_.

I'm really happy with where v2 ended up. It's the version of HappyView I wanted to build from the start.

## HappyView v3

Once the jobs stuff gets merged, I'll be starting on what I expect to be HappyView v3. Aside from cleaning up a bunch of pieces of the system that need attention, there are 4 big things I want to get done for the v3 release.

### Interpreters and libraries as plugins

You'll be able to install Lua, TypeScript, and/or Rust interpreters! This will allow you to start from scratch with writing your scripts in TypeScript, or even install Lua and Rust at the same time so your existing scripts keep working while you migrate. While I'm only planning for Lua, TypeScript, and Rust out-of-the-gate, it should be possible to add support for any of these languages:

- C/C+
- Go
- Python
- C#/.NET
- Kotlin
- Swift
- Zig
- AssemblyScript
- Ruby
- Haskell
- Grain, Moonbit, Virgil

Libraries (e.g. `atproto.*`, `xrpc.*`, `db.*`, etc) will become their own plugins as well, supported by all interpreters. You may only need a subset of these, but the coolest part will be **community libraries**. If you want to write a plugin to issue labels, or integrate with Ollama, or parse Markdown content, or do some PostGIS magic, etc... it'll be substantially easier with v3. I'll be talking to some folks about setting up a proper package registry as well, making it easy to find and install any kind of plugin.

### Filesystem-based management

Having to copy/paste your scripts into the dashboard sucks. I know. I also hate it. That's why I wrote my own system to handle deploying scripts and lexicons via the Admin API with Github Actions. It's still not an awesome experience, tho, because I have to wait for jobs to run and requests to complete.

In v3, I'll be adding support for managing your lexicons and scripts directly from the filesystem. Make changes directly to the files on disk and HappyView will live update the system appropriately. That includes lexicons and scripts, AND this allows me to enable `import`s. If you have a function you use across 45 different scripts (it me, I have that), you'll no longer have to copy/paste that function 45 times. You can finally abstract it into its own file and import where desired. That said, the old systems will still work: you can still deploy via the admin API, and you can write and edit your scripts directly in the dashboard.

### Database optimization

The database currently has one giant table for all records. That means if you're indexing `app.bsky.actor.profile` (40+ million records) but you need to retrieve an `at.youandme.connection` record (~2k records), your query has to page through every single record in your database to get the few you need.

With HappyView v3, each lexicon will get its own table with columns generated from the lexicon structure. Paging through smaller collections will be substantially faster, and `db.raw()` queries will no longer have to do a bunch of `jsonb` voodoo to get the data they need. But don't worry: we'll still store the complete JSON version of the record so you hooligans with non-conforming records can still access all the weird junk you toss in there.

### Dashboard refresh

The current dashboard _works_, but it's also kind of a mess. I want to take a hard look at everything we currently have and make the system make more sense. I feel like the things that should be next to each other aren't, and a lot of the UI feels sparse in a bad way.

If any of y'all know UI/UX people that would be interested in contributing, I'd _love_ to connect with them. Please please _please_ give them to me.

## Get involved

v3 is gonna be a huge shift in the right direction, and hopefully a really, really awesome improvement for everybody. If anybody is interested in contributing or helping shape the next major version of HappyView, please don't hesitate to reach out! I've not been great at onboarding people to HappyView dev, but I'd love to fix that and get some help up in here.

You can find me on [Bluesky](https://bsky.app/profile/trezy.dev), or hop into the `#happyview` channel on the [Cartridge Discord](https://discord.gg/BUPnjaBwRZ). Let's build something cool. ✌️

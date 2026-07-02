---
title: "HappyView v2.5: Permissioned Spaces"
description: "Private groups, invite-only communities, and gated content come to atproto."
date: 2026-05-08
author:
  name: "Trezy"
  avatar: "/authors/trezy.webp"
tags:
  - announcements
atUri: "at://did:plc:qneu5uamqs6cug7sjobbaexm/site.standard.document/3mpokm3iq232d"
---

HappyView 2.5 is out, and it ships with one of the most anticipated features in the [atproto](https://atproto.com) ecosystem:

✨ Permissioned Spaces. ✨

## The problem

ATProto is public by default. That's great for open social graphs, but not so great for private groups, invite-only communities, or gated content. If you wanted access-controlled data, you were mostly out of luck.

Spaces fix that. They give you private, access-controlled data under your existing DID.

## What this means for HappyView

Because HappyView is [lexicon-driven](/guides/getting-started), Spaces integrate naturally with everything you've already built. All of your scripts can query Spaces directly for membership checks, access levels, private record queries, and more. Custom lexicons can build on top of Spaces today without waiting for anything else to land.

## Here be dragons

The whole feature is experimental, and it's gated behind a feature flag. You can enable it from the new "Experimental" page in the dashboard.

![The Experimental page in the HappyView dashboard](/img/blog/happyview-2.5/experimental-page.png)

Give it a try, tell me what breaks, and tell me what you want to work differently.

## Standing on shoulders

All of the work on Permissioned Spaces follows [Daniel Holmgren's](https://bsky.app/profile/dholms.at) Permissioned Data Diaries. Big credit to [Zicklag's](https://bsky.app/profile/zicklag.dev) work on Arbiter and [flo-bit's](https://bsky.app/profile/flo-bit.dev) work on [Contrail](https://flo-bit.dev/contrail). We're actively working to make sure Spaces will be compatible between HappyView and Contrail as things evolve.

## Go get some

Check out [the documentation](https://happyview.dev) and give it a shot!

We've been talking about permissioned data for forever, and now it's here. I'm excited to see what y'all do with it.

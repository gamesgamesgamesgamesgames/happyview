# HappyView

HappyView is the best way to build an AppView for the AT Protocol. Upload your lexicon schemas and get a fully functional AppView, complete with XRPC endpoints, OAuth, real-time network sync, and historical backfill, without writing a single line of server code.

Building an AppView from scratch means wiring up real-time event streams, record storage, XRPC routing, OAuth flows, and PDS write proxying before you can even think about your application. HappyView handles all of that. Define your data model with lexicons, add custom logic with Lua scripts when you need it, and ship your app.

## Features

- **Schema-driven endpoints:** Upload a lexicon and HappyView generates XRPC query and procedure routes, storage, and indexing from it — updatable at runtime with no restart.

- **Network sync built in:** Real-time record streaming via Jetstream, historical backfill from each user's PDS, and atproto OAuth with DPoP-bound proxy writes back to the PDS.

- **Customize with Lua scripts and plugins:** Trigger-keyed Lua scripts for XRPC query/procedure logic and record/label event handling, WASM plugins for external platform integration, and labeler subscriptions for content moderation.

- **Protocol-native:** Works with any PDS, resolves DIDs through the directory, and fetches network lexicons via DNS authority resolution.

- **Full admin surface:** Built-in dashboard and admin API for managing lexicons, users, API keys, API clients, backfill jobs, and plugins.

## Design Principles

- **Schema-first**: Your Lexicons are the source of truth. Upload a schema and HappyView derives endpoints, indexing rules, and network sync from it. You describe _what_ your data looks like; HappyView figures out the rest.

- **Zero boilerplate**: HappyView handles AppView infrastructure (Jetstream, backfill, OAuth, PDS proxying) for you. You should be writing application logic from minute one, not plumbing.

- **Runtime-configurable**: Lexicons can be added, updated, and removed without restarting the server. New endpoints and sync rules take effect immediately, so you can iterate on your data model in real time.

- **Protocol-native**: HappyView works with _any_ PDS, resolves DIDs through the directory, and follows atproto conventions. It's a first-class citizen of the network, not a wrapper around it.

## Contributing

Point git at the tracked hooks once per clone:

```bash
git config core.hooksPath .githooks
```

`core.hooksPath` is local config, so it does not travel with a clone — each contributor runs this themselves.

- **pre-commit** — `cargo fmt --check` plus `cargo clippy` over the lib and bins.
- **pre-push** — `cargo clippy --all-targets`, which additionally type-checks and lints the `#[cfg(test)]` modules in `src/` and the `tests/` binaries. That is ~45 extra compilation units, almost all link steps, so it runs once per push rather than once per commit.

## Documentation

Full documentation is available at [happyview.dev](https://happyview.dev).

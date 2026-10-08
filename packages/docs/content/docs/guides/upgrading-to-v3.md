---
title: "Upgrading to v3"
---

v3 takes the script runtime out of HappyView. In v2 a Lua interpreter was compiled into the binary; in v3 a script runs inside an **interpreter plugin**, and HappyView ships none. An instance with no interpreter installed can neither save nor run a script.

Everything else about running an instance is where you left it: the database upgrades in place, every environment variable you set for v2 still means the same thing, and the admin API keeps its routes. The breaking changes are concentrated in scripts and plugins.

This guide covers the operator-facing changes. The script contract itself — the removed globals, `handle(input, ctx)`, the codemod's rewrites and markers — is in [Migrating Scripts to v3](migrating-scripts.md).

**v3 is in alpha.** Two things an upgrade needs are not finished. No interpreter plugin has a published release, so installing one means building it yourself — [step 1](#1-install-an-interpreter-plugin) is the recipe. And there is no guided upgrade flow that detects a v2 database and walks you through the codemod; every step below is manual.

## Architecture changes

| v2 | v3 |
|----|-----|
| Lua compiled into the HappyView binary | Scripts run in an interpreter plugin; none ships with HappyView |
| One script language, `lua` | `script_type` names whichever language an installed interpreter claims |
| Script bodies checked by the built-in Lua parser | Checked by the interpreter's own `validate` export |
| Plugin manifests at `api_version` `"1"` | `api_version` `"2"`, declaring capabilities |
| Script globals (`db`, `params`, `Record`, `now()`, …) | `handle(input, ctx)` plus `require("internal.*")` / `require("happyview.*")` |
| Instruction limit never fired; no time or memory limit | Instruction limit enforced, plus a request wall clock and a per-run memory ceiling |

## 1. Install an interpreter plugin

Nothing runs a script until one is installed. Boot says so once:

```
no interpreter plugin is installed, so no script can be saved or run;
install one from the plugins page in the dashboard
```

Until then, every script surface refuses in its own way:

| Surface | Behaviour with no interpreter |
|---------|-------------------------------|
| `POST` / `PATCH /admin/scripts` | 400, naming the language |
| XRPC query or procedure backed by a script | 503 `ServerMisconfigured`, naming the language |
| Record event | Dead-lettered on the first attempt, and the record is still indexed |
| Label event | Dead-lettered on the first attempt, and the label still persists unmodified |
| Job | Fails, with the same message in its `error` column |

Record and label events are deliberately let through rather than held: an absent interpreter is an operator's problem, not a reason to stop indexing. The dead-letter rows are the record of what went unprocessed, at **Dead Letters** in the dashboard.

To see what is installed:

```sh
curl "http://127.0.0.1:3000/admin/plugins?type=interpreter" -H "$AUTH"
```

The scripts list also flags each row: `runnable` is `false` when no installed interpreter claims its `script_type`, and the dashboard marks those rows at **Settings → Scripts**.

### Getting the Lua interpreter

The Lua interpreter is a plugin like any other, published from the [plugins](https://github.com/happyproto/plugins) repository. Install its release manifest by any of the [three plugin install routes](plugins.md#installing-plugins) — the dashboard's **Add Plugin**, `PLUGIN_URLS`, or a directory under `./plugins/`:

```
https://github.com/happyproto/plugins/releases/download/happyview-lua-v1.0.0/manifest.json
```

The `.wasm` module is fetched from beside the manifest, so that one URL is the whole install. The repository's releases page carries the current version; each plugin is tagged under its own name.

Building it from source is the other way, and needs more: it vendors PUC Lua, which compiles with a C toolchain targeting WASI — [wasi-sdk](https://github.com/WebAssembly/wasi-sdk) 34 — and the `wasm32-wasip1` Rust target. From the plugins repository's root, with wasi-sdk where its `.cargo/config.toml` expects it:

```sh
rustup target add wasm32-wasip1
cargo build --release -p happyview-lua --target wasm32-wasip1
```

That produces the same two files a release publishes — the manifest, and the `.wasm` its `wasm_file` names — which a URL install needs hosted together.

## 2. Migrate your stored scripts

A script stored before the upgrade stays in the table untouched and fails the first time it reads a removed global. The scripts list reports what each one still references in `needs_migration`, and the codemod rewrites it: from the editor's **Migrate** button, from `POST /admin/scripts/{id}/codemod`, or from the `happyview-codemod` CLI.

[Migrating Scripts to v3](migrating-scripts.md) is the reference for what the rewrite does and what it leaves behind as a `-- codemod:` marker for you to finish by hand.

Two things that matter for the order you do this in:

- **The codemod does not need an interpreter.** It is a source rewrite, so you can migrate every stored script before installing one. Editing a script by hand afterwards does need an interpreter, because the save is validated through it.
- **`happyview-codemod` is not in the published Docker image.** From a container, use `POST /admin/scripts/{id}/codemod` with `{"apply": true}`, or run the CLI from a source checkout against the same `DATABASE_URL`.

Saving is stricter than running: `POST` / `PATCH /admin/scripts` refuse a Lua body that still references a removed global, with the names in `removed_globals`. The codemod's own apply is the one exception, so a script with markers can be stored once you confirm.

## 3. Rebuild or replace your plugins

v2 loaded plugins whose manifest declared `api_version` `"1"`. v3 requires `"2"`, and a manifest below it is refused at load with a message naming the plugin type:

```
auth plugins need an api_version: "2" manifest declaring capabilities
(got api_version "1"); republish the plugin with a capabilities-declaring manifest
```

There is no fallback for any plugin type, auth included. A v2-era manifest also has to declare its `capabilities`, and the loader cross-checks that declaration against the compiled module's import section — a plugin that imports something its manifest does not cover is refused before it runs.

The instance still boots. A refused plugin logs `Failed to install plugin at boot` or `Failed to load plugins from database` and is simply absent, so anything that depended on it — account linking through an auth plugin, say — stops working until you replace it. Check your boot log after the upgrade.

For the official plugins, pick a release whose manifest has `api_version` `"2"`; earlier releases predate capability declarations and are refused.

## 4. Script limits now apply

v2 set a one-million-instruction limit and then never enforced it: the hook was installed on the wrong thread, so the coroutine `handle` ran on never saw it. v3 enforces it, and adds two limits v2 had none of. A script that quietly ran long in v2 can fail in v3.

| Limit | Default | Applies to | Setting |
|-------|---------|------------|---------|
| Instructions per run | 1,000,000 | Every kind but jobs | `script_instruction_limit`, env `SCRIPT_INSTRUCTION_LIMIT`, range 1,000–1,000,000,000 |
| Wall clock per run | 10 s | Every kind but jobs | `script_wall_clock_seconds`, env `SCRIPT_WALL_CLOCK_SECONDS`, range 1–300 |
| Memory per run | 64 MiB | Every kind, jobs included | Not configurable |

Jobs are exempt from the first two — running long is what a job is for, and `ctx.job.should_stop()` is its stop.

Both settings resolve database → environment → default, so the **Instruction limit** and **Request wall clock** fields in **Settings → General** take precedence over the environment. A value outside the range is refused on save; an environment value outside it is warned about and the default stands. A change lands in-process and reaches a second instance on the same database within ten seconds.

The wall clock counts only time the script itself runs. Waiting on the host — an HTTP request, a database read — is not charged to it.

A run that spends either budget fails with `timeout`; one that exhausts its memory fails with `memory`. An XRPC caller sees `{"error": "script_error", "errorType": "timeout"}` at 408, or `"memory"` at 500.

## 5. Error text where you grep it

If you have alerting or log queries over script failures, two columns changed shape.

**Dead-letter and job `error` columns.** v2 wrote a stage prefix naming where in the host the failure happened — `db api:`, `xrpc api:`, `script load failed:`, `missing handle():`, `create sandbox:`. v3 writes the failure's category instead: `syntax:`, `runtime:`, `timeout:`, `memory:` or `missing_handle:`, followed by the interpreter's own text. The old prefixes match nothing.

**An unrecognised `script_type`.** v2 validated the field during deserialisation, so an unknown language was a 422 from the request body. v3 accepts any string there and refuses it in the handler with a 400 naming the language, because what is valid now depends on which interpreters are installed.

## 6. `spaceType` on the plugin and script surfaces

A space's type NSID is named `spaceType` everywhere the protocol names it: the plugin SDK's `SpaceInfo` and `SpacesCreate`, and `ctx.space.spaceType` in a script. It was `type` on the SDK and `type_nsid` on `ctx.space`.

The HTTP surface is unaffected. `createSpace` and `listSpaces` accept `spaceType` and still accept `type`, and a space in a response still carries `type`.

## 7. Library record reads return an envelope

A record read through a library — `require("happyview.db")`'s `get`, `search` and `records(…)` chains, and `require("happyview.backlinks")` — answers `{uri, did, collection, rkey, cid, indexed_at, record}`, with the stored body verbatim under `record`. v2 answered the body with `uri` written onto it. `cid` is null while the row holds a local write's placeholder, and `indexed_at` is null until Jetstream has echoed the record.

The public XRPC query routes keep their flat body-plus-`uri` shape. That is a client contract, and it did not change — a scriptless lexicon query still answers what it answered in v2.

A script the codemod migrated keeps v2's flat rows, through a shim the codemod inlines; [Migrating Scripts to v3](migrating-scripts.md#polyfills) describes it and how to retire it.

## 8. Building from source

A default build compiles no Lua interpreter at all — `mlua` sits behind a non-default `lua-reference` feature. The resulting binary cannot run a script without an interpreter plugin, which is the point.

`lua-reference` is **not** a fallback runtime. It exists so the test suite can compare the interpreter plugin against the implementation it replaced; a build with it enabled still routes every script run through the plugin.

One new environment variable, both optional and additive:

| Variable | Default | Description |
|----------|---------|-------------|
| `PLUGIN_CACHE_DIR` | `plugin-cache` beside the SQLite database, else none | Where compiled plugin modules are kept between restarts. A Postgres instance has no such directory of its own and keeps the cache in memory unless you set this |

## 9. Rolling back

v3's own schema change is one irreversible migration: it drops `happyview_plugin_dedup_keys`, a table nothing read or wrote. Nothing else it adds is destructive.

That is not the same as a return path. A script the codemod rewrote will not run on v2, since the globals it now reaches through `require` were never there. And if you are skipping v2 releases to get here, their migrations come along too — the spaces work in the 2.x line drops columns outright. Take a database backup before the upgrade.

## Checklist

- [ ] Install an interpreter plugin, and confirm boot no longer warns
- [ ] Run the codemod over every stored script, and finish any `-- codemod:` markers by hand
- [ ] Confirm `needs_migration` is empty and `runnable` is true for every script
- [ ] Replace any plugin whose manifest is `api_version` `"1"`, and check the boot log for refusals
- [ ] Review long-running scripts against the instruction limit and the 10-second wall clock
- [ ] Update any alerting that matches the old stage prefixes in dead-letter or job `error` text
- [ ] Rebuild any in-house library plugin against the v3 SDK if it reads records or spaces

## Next steps

- [Migrating Scripts to v3](migrating-scripts.md): the script contract, the codemod, and every marker it emits
- [Plugins](plugins.md): installing and configuring plugins
- [Developing Plugins](developing-plugins.md): the SDK, capabilities, and the library surface
- [Configuration](../getting-started/configuration.md): the full environment variable reference

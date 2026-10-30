# Claims to source: landing.ts

The copy for the happyview.dev landing page, checked against HappyView `alpha`
at f86b819e (tagged v3.0.0-alpha.42) on 2026-10-30, unless a row says
otherwise. Paths are relative to the HappyView repo root.

## The sample

`sample/` is the project `hpy happyview create --name statusboard --no-input
--superadmin <did>` wrote (hpy 1.0.0-alpha.15), plus `hello.test.lua`. The only
edit is the superadmin DID, which is a placeholder here; `create` fills in the
DID you pass. Then, with `hpy happyview dev` on HappyView 3.0.0-alpha.42 and
`happyview-lua` from the `alpha` channel:

```
$ curl -i 'http://127.0.0.1:3000/xrpc/com.example.hello?name=Ada'
HTTP/1.1 200 OK
content-type: application/json

{"message":"Hello, Ada!"}

$ curl 'http://127.0.0.1:3000/xrpc/com.example.hello'
{"message":"Hello, world!"}

$ hpy happyview test
 PASS  src/lexicons/com/example/hello.test.lua (1 tests) 124ms

$ hpy happyview check
ok: 1 lexicon, 1 script, 0 modules
```

Editing the script's greeting and calling again 0.2 s later returned the new
text, with the dev server still running. A call made immediately after the
write still got the old one, so the copy says "save it and call it again"
rather than "the next request".

## landing.ts

| Claim | Source |
|---|---|
| Open source, MIT | `LICENSE.md` |
| "AppView for atproto" | GitHub repo description: "A lexicon-driven AppView for ATProto." |
| Each lexicon becomes an XRPC endpoint; a lexicon's path is its route | The run above; the scaffold's own lexicon description ("The file's path is its route") |
| Scripts in Lua, JavaScript or TypeScript, each an interpreter plugin from the registry | Live registry `at.happyproto.registry.searchProjects`, 2026-10-30: `happyview-lua`, `happyview-javascript`, `happyview-typescript` (all on `alpha` only); `src/script/dispatch.rs:47-48` |
| Indexes the records your app cares about | Jetstream is subscribed with a `collections=` filter (dev log); `crates/happyview-provider/src/source.rs:8` |
| `hpy happyview create` gives this project | The run above |
| Record created/updated/deleted and label scripts | `guides/testing.md` (`record_create`, `record_update`, `record_delete`, `label` triggers); `src/lua/scripts.rs:282-296,492` |
| Subscribing to labelers | `labelers:create` "Subscribe to external labeler services", `src/admin/permissions.rs` |
| Backfill | `backfill:create` "Trigger historical record backfill jobs", `src/admin/permissions.rs` |
| Jobs report progress, can be paused and cancelled, aren't bound by the time limit | `jobs:manage` "Cancel, pause, and resume background jobs"; job `progress` in `guides/testing.md`; jobs exempt from the instruction and wall-clock limits (`guides/migrating-scripts.md`, see the troubleshooting draft's CLAIMS) |
| Plugins are WebAssembly and declare capabilities, checked against imports | `src/plugin/loader.rs:65,71`; `src/plugin/capabilities.rs:1-3` |
| Capabilities are shown before you install | `web/src/app/dashboard/settings/plugins/add/[id]/catalogue-plugin.tsx:17,145` (`visibleCapabilities` on the install page); `CapabilityConsent` on update and channel switch |
| `hpy happyview test` fires queries, procedures, record events and jobs in process, with no network | `guides/testing.md` ("Firing triggers", "What stands in for the outside world"); the run above |
| 52 permissions, four templates (alternate tile) | Generated table in `../glossary.mdx` |
| atproto OAuth sign-in for the dashboard (alternate tile) | `src/auth/`; dashboard login |
| SQLite or Postgres; migrations on startup; Postgres for replicas (alternate tile) | `getting-started/deployment/docker.md` (Compose-file table) |
| Binary for macOS, Linux, Windows | `getting-started/deployment/binary.md`; release v3.0.0-alpha.42 has macOS (arm64, x86_64), Linux (arm64, x86_64; gnu and musl) and Windows (x86_64) archives |
| Docker image | `getting-started/deployment/docker.md` (`ghcr.io/gamesgamesgamesgamesgames/happyview`; prereleases published under their full version). Not checked against the registry: listing the image's tags needs `read:packages` |
| HappyProto instance at `<slug>.hpy.at`, Lua preinstalled | HappyProto provisioner; upgrade guide draft |
| `hpy happyview deploy` | `happyproto/cli` v1.0.0-beta.3 `crates/hpy/src/cli.rs:208-236`. **Not in hpy 1.0.0-alpha.15** (`unrecognized subcommand 'deploy'`) |
| Three plan sizes, from 512 MB / 5 GB | HappyProto plans: Small 512 MB / 5 GB, Medium 1 GB / 10 GB, Large 2 GB / 20 GB. **Depends on provisioner#33** (the shrink) |
| In beta, by invite | Owner-confirmed dates: invites at the very end of October or early November |

## Before this ships

- **CTA hrefs** (`/getting-started/`, `/guides/`, `/getting-started/deployment/`)
  are guesses at the new site's routes. happyview-76 owns the structure.
- **No documented way to install hpy.** "Start a project" leads to
  `hpy happyview create`, but nothing on alpha's docs says how to get hpy, and
  nothing there mentions `hpy happyview create` at all. The getting-started
  page needs both before the primary CTA works.
- **"In beta now, by invite"** is true once invites go out.
- **Plan sizes** wait on provisioner#33.
- **`hpy happyview deploy`** is in the beta channel (1.0.0-beta.3), not in
  alpha.15.

## Found while checking (not fixed here)

- **A query's lexicon constraints aren't enforced on its parameters.**
  `com.example.hello` declares `name` with `maxLength: 64`. A 65-character
  `name` answers `200` over HTTP, and `test.query` with the same input answers
  `200` too. `guides/testing.md` says "Lexicon validation, parameter parsing
  and auth checks all apply" to `query` and `procedure`. Either the guide or
  the router is wrong. The landing copy doesn't claim request validation.

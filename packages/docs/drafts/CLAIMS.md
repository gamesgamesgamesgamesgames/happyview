# Claims to source: troubleshooting.mdx and glossary.mdx

These drafts are for the new docs site (P3/P4 in happyview-76's rebuild). Every
claim was checked against HappyView `alpha` at f86b819e, unless a row says
otherwise. Paths are relative to the repo root. Other repos are named in full.

Regenerate the glossary's permissions table with
`node packages/docs/drafts/generate-permissions.mjs`; `--check` fails when it's
out of date (suitable for CI).

## troubleshooting.mdx

| Claim | Source |
|---|---|
| No-interpreter sentence: `no interpreter installed for '<language>' scripts; install one from the plugins page` | `src/script/dispatch.rs:47-48` (`no_interpreter_message`) |
| A save without an interpreter is refused with 400 `{"error": <sentence>}` | `src/script/checks.rs:152`; `src/admin/scripts.rs:652` (`BadRequest`); body shape `src/error.rs:355,371` |
| A query or procedure without an interpreter answers 503 `{"error":"ServerMisconfigured","message":…}` | `src/error.rs:294-300`; test `src/lua/execute.rs:799` |
| Record and label events dead-letter after one attempt with the same sentence | `src/lua/scripts.rs:282-296` (record), `:492` (label); `attempts: 1` |
| Jobs fail with the same sentence | `src/script/dispatch.rs:39-46` (doc comment lists the five surfaces) |
| Startup warning text | `src/run.rs:567-568` |
| Official interpreter names | Live registry `at.happyproto.registry.searchProjects`, 2026-10-30: `happyview-lua`, `happyview-javascript`, `happyview-typescript` |
| Integrations → Plugins → Add plugin | `web/src/components/app-sidebar.tsx:124,381` (Plugins under the Integrations group) |
| Save with removed globals: 400 `{"error": "script references removed globals: … -- run the codemod first", "removed_globals": [...]}` | `src/script/checks.rs:35-39`; `src/error.rs:302-307` |
| Runtime message for a removed global | HappyView `src/lua/sandbox.rs:99-105`; the same text in `happyproto/plugins` `alpha` `plugins/happyview/lua/src/sandbox.rs:34-41` (the plugin is what runs on v3) |
| A runtime failure is `script_error` with `errorType` `runtime`, status 500 | `src/error.rs:19-27,262-280` |
| 32 removed globals (8 named, plus 24) | `src/codemod/detect.rs:18-51` (`REMOVED_GLOBALS: [&str; 32]`) |
| Codemod: `POST /admin/scripts/{id}/codemod` with `{"apply": true}`, and the `happyview-codemod` CLI | `src/admin/mod.rs:193`; codemod binary per `guides/upgrading-to-v3.md` (not re-checked) |
| "Migrate this script" button on the script's page | `web/src/app/dashboard/settings/scripts/[id]/script-detail.tsx:206-212` |
| Timeout is 408, memory is 500, both `script_error` with `errorType` | `src/error.rs:262-266`; tests `src/error.rs:480-500` |
| Error body fields `error, errorType, message, method, line, file` | `src/error.rs:269-277` |
| Limits: 1,000,000 instructions, 10 s wall clock, 64 MiB memory | `src/lua/limits.rs:17,24`; `src/plugin/runtime.rs:717` |
| Ranges 1,000–1,000,000,000 and 1–300 | `src/lua/limits.rs:279,286` |
| Jobs exempt from instructions and wall clock, not memory; wall clock excludes host waits; `ctx.job.should_stop()` | `guides/upgrading-to-v3.md` and `guides/migrating-scripts.md` on alpha (verified against src by the earlier audit; not re-checked line by line here) |
| Settings `script_instruction_limit` / `script_wall_clock_seconds` and env `SCRIPT_INSTRUCTION_LIMIT` / `SCRIPT_WALL_CLOCK_SECONDS`; the setting wins | `src/admin/settings.rs:36-37`; `src/lua/limits.rs:14-15,251` |
| System → General | `web/src/components/app-sidebar.tsx:139,415` (General under the System group) |
| api_version refusal text | `src/plugin/loader.rs:45-51` (`ApiVersionTooOld`) |
| Install refusal is 400 "Failed to load plugin: …" | `src/admin/plugins.rs:957,965` |
| Startup logs "Failed to load plugin from the registry / from DB / WASM" and the plugin isn't loaded; the instance still starts | `src/plugin/mod.rs:456,473,492` |
| Capabilities are cross-checked against the module's imports | `src/plugin/capabilities.rs:1-3` (module doc), `Capability mismatch` `src/plugin/loader.rs:71` |
| Registry fetch error text `registry request failed: <cause>` | `src/plugin/registry_source.rs:292` |
| Catalogue refresh `official_registry: refresh failed`, every 15 minutes | `src/plugin/official_registry.rs:550-552` |
| Registry installs are refetched and CID-checked at every startup and reload | `src/plugin/mod.rs:446-460`; `guides/plugins.md` |
| Default registry `https://api.happyproto.at`; `PLUGIN_REGISTRY_URL=off` turns the catalogue off | `src/plugin/official_registry.rs:143-180` |
| `PLUGIN_REGISTRY_URL=off` doesn't change where installed plugins fetch from | `guides/upgrading-to-v3.md` (not re-checked in code) |
| Publishing scope refusal texts | `src/lexicon_publishing/target.rs:118-138` |
| Schema-write scope string | `src/lexicon_publishing/status.rs:8-9` |
| A dashboard login replaces the attached session | `src/lexicon_publishing/target.rs:110-114` (doc comment) |
| System → Service Identity, Re-authenticate | `web/src/components/app-sidebar.tsx:157`; the Re-authenticate label is from `guides/publishing-lexicons.md` (not re-checked in web/) |
| 403 `{"error":"InsufficientPermissions","message":"Missing permission: lexicons:publish"}` | `src/error.rs:285-291`; `src/admin/lexicon_publishing.rs:244` |
| Manager template includes `lexicons:publish`; super users always pass | `src/admin/permissions.rs:719`; `src/admin/auth.rs:40` |
| Users created before v3 don't get `lexicons:publish` | `guides/upgrading-to-v3.md` (no migration grants it; not re-checked) |
| Access → Users | `web/src/components/app-sidebar.tsx:91,357` |
| 503 `{"error":"ServiceUnavailable","message":"the record store is unavailable"}` | `src/error.rs:174-182,310-318` |
| Returned by record reads, `/admin/records`, `/admin/stats`, and scripts that read records | `src/error.rs:163-172`; `src/admin/records.rs:265`; `src/admin/stats.rs:90`; `src/lua/execute.rs:81-88,135` |

## glossary.mdx

| Claim | Source |
|---|---|
| `handle(input, ctx)` and `require` | `api-reference/lua/script-contract.md` on alpha; `src/codemod` |
| Plugin types auth / interpreter / library | `src/plugin/types.rs:137-151` |
| Plugins are WebAssembly | `src/plugin/loader.rs:65` (`WASM validation failed`); `src/plugin/runtime.rs` |
| Install from registry, URL or file | `src/plugin/mod.rs:446-500` (sources `url` with registry, `url`, `file`) |
| Interpreter language IDs, and the library support it declares | `src/plugin/loader.rs:77-81`; registry `listVersions` for `happyview-lua` (`languageId: lua`) |
| `internal.` is reserved | `src/plugin/loader.rs:75`, `:21` |
| `require("happyview.db")` | `reference/script-examples/*` on alpha |
| Capability names and risk levels | `src/plugin/capabilities.rs:12-30` and the `PluginCapability` enum |
| `software.release.project` / `.version`; publisher DID plus name | `src/plugin/official_registry.rs:681,699` |
| 15-minute refresh; `PLUGIN_REGISTRY_URL` | `src/plugin/official_registry.rs:143-180,552` |
| Updates only from the installed publisher | `src/plugin/official_registry.rs:388`; `guides/plugins.md` |
| Channels `latest`, `beta`, `alpha` | `src/plugin/registry_source.rs:68-71` |
| Official interpreters on `alpha` only | Live registry, 2026-10-30 (each project's `channels` has only `alpha`, at 1.0.0-alpha.5) |
| Pinning to one version | `guides/plugins.md` on alpha (not re-checked in code) |
| Lifecycle states; `active` and `deprecated` installable | `src/plugin/registry_source.rs:503-506` |
| A retired version keeps running and is marked with the reason | `guides/plugins.md` / `guides/upgrading-to-v3.md` (not re-checked in code) |
| Project layout `src/`, `happyview.toml` | `src/config/project.rs`; `src/config/declarations.rs:149` |
| `happyview.lock` | Written by hpy, not by HappyView (no match in HappyView `src/`); `happyproto/cli` `v1.0.0-beta.3` `crates/hpy/src/cli.rs` ("Add plugins to happyview.toml and happyview.lock") |
| `HAPPYVIEW_PROJECT` | `src/config/mod.rs:282` |
| `project:deploy` | `src/admin/permissions.rs`; `src/admin/project.rs:33` |
| hpy commands | `happyproto/cli` `v1.0.0-beta.3` `crates/hpy/src/cli.rs:23-32` (top level), `:47-55` (registry), `:73-86` (plugin), `:208-236` (happyview) |
| Templates nest Viewer ⊂ Operator ⊂ Manager ⊂ Full Access | `src/admin/permissions.rs:693-747` |
| Super user holds every permission; only the super user can transfer ownership | `src/admin/auth.rs:40`; `reference/troubleshooting.md` (transfer-super) |
| Permission set `dev.happyview.adminFull`, `rpc` on `dev.happyview.admin.manage`, title text | `src/auth/admin_permission_set.json`; `src/auth/admin_scope.rs:1-19` |
| The app acts with the user's own permissions | `src/auth/admin_permission_set.json` (`detail`) |
| hpy signs in with it | `src/auth/cli_client.rs:32` |
| Record source and storage traits, open crate, Jetstream default | `crates/happyview-provider/src/source.rs:8`; `guides/record-providers.md` on alpha |
| Storage-down reads answer 503 | as in troubleshooting |
| Network store: HappyProto's own source and storage | `happyproto/happyview-network-store` `main` README; `crates/network-store/src/source.rs:318`, `storage.rs:652` |
| 52 permissions, their descriptions, categories and template membership | Generated: `generate-permissions.mjs` from `src/admin/permissions.rs` (`Permission::all`, `Permission::info`, `Template::permissions`) |

## Found while checking (not fixed here)

- **Dashboard paths in messages and guides use "Settings →",** but the sidebar groups are Data, Access, Integrations and System (`web/src/components/app-sidebar.tsx:324,357,381,415`). This affects:
  - the removed-global message itself ("Settings → Scripts → Migrate"), in both HappyView and the Lua plugin;
  - `guides/upgrading-to-v3.md`;
  - the HappyProto upgrade guide draft.
  The routes are under `/dashboard/settings/…`, which is probably where the wording comes from.
- **The permission count is 52, not 53.** The earlier audit counted every `x:y` string in `permissions.rs`, which included the test string `"fake:permission"`. The audit is corrected.
- **`script-variables:create`'s description says "for Lua scripts"** (`src/admin/permissions.rs:245`). The generated table inherits it. Fix it in the code rather than the docs.

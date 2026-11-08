---
title: "Developing Plugins"
---

This guide covers how to build your own HappyView WASM plugins. For installing and configuring plugins, see the [Plugins guide](plugins.md).

See the [plugins](https://github.com/happyproto/plugins) repository for examples and the plugin SDK.

## Plugin Manifest

Each plugin has a `manifest.json` that describes its metadata:

```json
{
  "id": "steam",
  "name": "Steam",
  "version": "1.0.0",
  "api_version": "2",
  "description": "Import your Steam game library and playtime data.",
  "icon_url": "https://example.com/steam-icon.png",
  "auth_type": "openid",
  "wasm_file": "steam.wasm",
  "capabilities": ["secrets:read", "network:request"],
  "allowed_hosts": ["api.steampowered.com"],
  "required_secrets": [
    {
      "key": "PLUGIN_STEAM_API_KEY",
      "name": "Steam Web API Key",
      "description": "Get your API key at steamcommunity.com/dev/apikey"
    }
  ]
}
```

| Field              | Description                                           |
| ------------------ | ----------------------------------------------------- |
| `id`               | Unique plugin identifier                              |
| `name`             | Display name                                          |
| `version`          | Semantic version                                      |
| `api_version`      | Plugin API version. Must be `"2"` — the loader refuses anything older (see [Plugin Types](#plugin-types)) |
| `description`      | Brief description shown during install                |
| `icon_url`         | Optional icon URL                                     |
| `auth_type`        | Authentication type: `oauth2`, `openid`, or `api_key` |
| `wasm_file`        | WASM binary filename (default: `plugin.wasm`)         |
| `required_secrets` | Array of secrets the plugin needs                     |

Each `required_secrets` key is a full environment variable name: `PLUGIN_`, then the plugin's `id` upper-cased with every non-alphanumeric character replaced by `_`, then the secret's own name — so a plugin with the id `auth-steam` asks for `PLUGIN_AUTH_STEAM_API_KEY`. The plugin itself reads the part after that prefix: `host_get_secret("API_KEY")`.

The fields above are the `auth` baseline. [Plugin Types](#plugin-types) below covers the fields a `library` or `interpreter` manifest adds: `plugin_type`, `capabilities`, `allowed_hosts`, `publisher`, `dependencies`, `namespace`, and an interpreter's `language_id` and `extensions`.

## Plugin Types

The manifest's `plugin_type` field decides what the host expects the plugin to export and how scripts and other plugins use it. A manifest that omits `plugin_type` defaults to `auth`.

| Type | Description |
| --- | --- |
| `auth` (default) | An external-auth plugin: OAuth/OpenID/API-key login plus profile lookup. The [Plugin Exports](#plugin-exports) and [Host Functions](#host-functions) below are the `auth` contract. |
| `library` | A plugin other plugins and Lua scripts call into, via `host_call_library` or `require(namespace)`. See [Library Plugins](#library-plugins) below. |
| `interpreter` | A script runtime. It runs the scripts written in its language. See [Interpreter manifests](#interpreter-manifests) below. |

Every plugin, `auth` included, needs `api_version` `"2"` and a `capabilities` list covering everything its compiled module imports. The loader refuses an older or missing `api_version` with an error naming the plugin type and the version it found.

A `library` plugin may declare `namespace` — the name scripts use in `require(namespace)`. It defaults to `id` when omitted.

Namespaces are dotted `publisher.name` (`happyview.db`); ids are hyphenated (`happyview-db`). A script's `require("happyview.db")` names the implementation it depends on, since a script is often read without the instance that runs it.

### Interpreter manifests

An `interpreter` manifest names its language and the file extensions it runs:

```json
{
  "id": "happyview-lua",
  "name": "Lua",
  "version": "1.0.0",
  "api_version": "2",
  "plugin_type": "interpreter",
  "language_id": "lua",
  "extensions": ["lua"],
  "capabilities": ["library:call", "script:host", "wasi:clock", "wasi:random", "wasi:stdio"],
  "wasm_file": "happyview-lua.wasm"
}
```

| Field | Description |
| --- | --- |
| `language_id` | The language's name, stored as each script's `script_type`. Lowercase letters, digits, `-` and `_`, starting with a letter. Two installed interpreters cannot share one |
| `extensions` | The file extensions this interpreter runs in a project's `src/`, without the dot. Each is 1 to 16 lowercase letters or digits, none may be `json` (lexicons use it), and none may be listed twice |

A file in a project's `src/` with one of these extensions belongs to this interpreter, which checks it when the project builds and runs it when it is a script. Two installed interpreters cannot claim the same extension: installing the second is refused with `files ending in .<ext> are already run by interpreter '<id>'`. A manifest with no `extensions` claims its older `file_extension` field instead, if it has one.

#### Prepared artifacts and local modules

An interpreter can hand HappyView a prepared form of a script, typically bytecode, so a run skips the parse. It can also let a project script import files that sit beside it in `src/`. Both are optional: an interpreter that does neither still runs every script from source.

**Preparing.** HappyView calls `validate` when a script is saved and for every file when a project builds. Three fields of its input matter here:

| Field | Meaning |
| --- | --- |
| `prepare` | `true` asks for an `artifact` beside the verdict. Absent means a plain check |
| `chunk_name` | The name to give the chunk. For a project file it is the path from the project root, such as `src/lib/format.lua`. Absent means `script` |
| `role` | `handler`, `module` or `test`. Only a `handler` must define `handle`. Absent means `handler` |

`validate` answers with `valid` and `errors`, and may add `artifact` and `imports`:

```json
{
  "valid": true,
  "artifact": { "interpreter": "", "version": "", "engine": "puc-lua-5.4.7", "format": "lua-bytecode", "bytes": "G0x1YVQAGZMNChoKBAgIeFYAAAA=" },
  "imports": [{ "specifier": "../lib/format", "line": 2 }]
}
```

- Set `engine` and `format` yourself. HappyView overwrites `interpreter` and `version` with the build that answered, so an artifact can only be handed back to that build.
- `bytes` is standard base64 (RFC 4648 §4), padded, with no whitespace.
- `imports` lists every import whose specifier is a literal. HappyView checks each relative one when the project builds, and reports a missing file at its line.
- A dashboard script that imports a relative path is refused when it is saved, because it has no files beside it.
- An error's `message` carries no `chunk:line:` prefix. `line` says where, and HappyView names the file beside it.

**Running.** `execute` receives `chunk_name` and, when HappyView holds an artifact this build prepared from that exact source, `artifact`. When `artifact` is present, `source` is empty: load the artifact and name its chunk `chunk_name`. An `error` result may set `chunk` to the chunk `line` is in, in full, since a language may truncate the chunk name inside its own message. HappyView reports it to the caller as the error's `file`.

**Local modules.** For a specifier that starts with `./` or `../`, call `host_module_resolve` (`host::module_resolve` in the SDK, under `script:host`) with the `specifier` and a `referrer`: the importing chunk's name. It answers `{id, source, artifact?}`.

- `id` is the module's path from the project root. Two specifiers naming one file answer the same `id`, so de-duplicate by it, and name the module's chunk with it.
- `source` is always present. `artifact` is present only when it matches this build and that source. Load it when you have it, and the source otherwise.
- Only files in your own language resolve, and only from inside `src/`.

The import fails with one of these codes:

| Code | When |
| --- | --- |
| `NOT_FOUND` | No such file. The message names the specifier, the importer and where HappyView looked, and a same-named file in another language if there is one. Raise it as the script's error |
| `BAD_INPUT` | The specifier is not relative, uses `\`, or climbs out of `src/` |
| `UNSUPPORTED` | The script is a dashboard script, which has no files to import |

Keep a script-facing `load` text-only. Bytecode reaches the interpreter only from HappyView.

#### Running test files

`happyview test` hands each `*.test.<ext>` file to the interpreter that claims `<ext>`, as a run of kind `test`. The project-author side is in the [testing guide](testing.md). This section is the interpreter's half: what a test file's run receives, the five imports it reaches HappyView through, and what each answers.

An interpreter that runs test files declares `script:test` beside `script:host`. The imports below need it, and a module that imports any of them without declaring it is refused at load. A host that predates test runs has no such imports, so a plugin built against them loads only on a host that has them. The SDK wrappers are `host::test_trigger`, `host::test_mock`, `host::test_calls`, `host::test_pds_writes` and `host::test_report`, and the request and response types are in `happyview_plugin_sdk::wire`.

**What `execute` receives.** A test file's `ExecuteInput` differs from a handler's in these fields:

| Field | Value |
| --- | --- |
| `kind` | `"test"` |
| `source` | The file's text, or empty when `artifact` is present. The artifact was prepared by your `validate` with `role` `test`, so it defines no `handle` |
| `chunk_name` | The file's path from the project root, such as `src/lib/format.test.lua` |
| `input` | `null` |
| `context.trigger` | `test:<path>`, the same path |
| `context` | No `caller_did`, `has_pds_auth` is `false`, and there is no `job`, `method`, `collection`, `params` or `space`. `env` is the project's `.env.test` |
| `limits.instructions` | `null`. A test file has no instruction budget; HappyView stops a file whose own code runs past 60 seconds of guest time, and time spent inside an import is not counted |
| `libraries` | As for any run |

Run the file, collect its tests, run them in order, and report each one through `host_test_report` once it has finished, after any `after_each`. Then return; the value returned is ignored. An `error` result fails the file as a whole, placed at its `chunk` and `line`, and so does a file that returns without reporting any test.

An interpreter built on an SDK from before test runs cannot decode `kind: "test"` and answers `BAD_INPUT`. HappyView reports that as an interpreter that predates test runs, naming it.

**The imports.** Each takes a JSON request and answers the usual `{"ok": ...}` or `{"error": {code, message, retryable}}` envelope. None of them traps.

| Import | Request | `ok` |
| --- | --- | --- |
| `host_test_trigger` | `TestTrigger`: `{kind, target, input?, did?, rkey?, as?}` | `TestTriggerResult`: `{status, body}` |
| `host_test_mock` | `TestMock`: `{namespace, responses}` | `null` |
| `host_test_calls` | `TestCalls`: `{namespace}` | `[TestCall]`: `[{method, args}]`, `args` the JSON array the call carried |
| `host_test_pds_writes` | `{}` | `[TestPdsWrite]`: `[{action, repo, collection, rkey, record?, cid?}]`, `record` and `cid` absent for a delete |
| `host_test_report` | `TestReport`: `{path, name, status, message?, file?, line?, duration_ms}` | `null` |

`TestTrigger.kind` is one of `query`, `procedure`, `record_create`, `record_update`, `record_delete`, `job` and `label`. `target` is the method's NSID, the record's collection, the job type or the label's subject URI. `input` is the query's parameters, the procedure's body, the record, the job's input, or the label's `{val, src?, neg?, cts?, exp?}`. `did` is a record's author and `rkey` its record key. `as` is the DID the trigger is forged as.

`TestReport.status` is `pass`, `fail` or `skip`. `name` is the test's full name with its `describe` blocks joined by ` > `. `file` and `line` place a failing assertion, which may be in a module the test file required, and `message` says what failed. HappyView sets `path` to the file being run whatever you send, so `""` is fine.

**What a trigger answers.** Every kind answers `{status, body}`. A handler that fails is not an import error: it shows in `status` and `body`.

| Kind | `status` | `body` |
| --- | --- | --- |
| `query`, `procedure` | The HTTP status of the response | The response's JSON. A body that is not JSON is a string, and an empty one is `null` |
| `record_create`, `record_update`, `record_delete` | `200`, or `500` when a script failed or the outcome is `errored`, `unavailable` or `invalid` | `{outcome, uri, cid, errors}`. `outcome` is `matched`, `skipped`, `schema_event`, `errored`, `unavailable` or `invalid`; `cid` is `null` for a delete; `errors` lists each failed script's message |
| `job` | `200` when the job completed, `500` otherwise | `{id, status, result, error, progress}`, the job's row after its one run |
| `label` | `200`, or `500` when the label script failed or the label could not be stored | `{applied, errors}`. `applied` is the label as stored, or `null` |

**What clears and what persists.** Each test file gets its own database and its own record of calls, writes, mocks and reports. Within a file, `host_test_report` clears the recorded library calls and PDS writes, so each test starts with none. Mocks are not cleared: one lasts for the rest of the file, until the same namespace is mocked again, which replaces it and restarts its lists.

**The mock format.** `responses` maps each method name to the answer for its calls:

- A single value answers every call with that value.
- A list answers one call per element, in order. A call after the last fails with `MOCK_EXHAUSTED`. To answer with a list, wrap it in another: `[[1, 2]]` answers `[1, 2]` once.
- `{"$error": {"code": "TIMEOUT", "message": "slow"}}`, alone or as a list element, fails the call with that code and message. `code` must be a string, and `message`, when present, must be one too; anything else under `$error` is refused when it is set, with `BAD_INPUT` naming the method.

A mocked call never reaches the library, and is recorded for `host_test_calls` whether mocked or not. A namespace that is not mocked runs the real library.

**Error codes.**

| Code | Where | When |
| --- | --- | --- |
| `UNSUPPORTED` | All five imports | The run is not a test file, or the instance is not running `happyview test` |
| `FORBIDDEN` | All five imports | The plugin lacks `script:test` |
| `BAD_INPUT` | All five imports | The request does not decode as the type above |
| `BAD_INPUT` | `host_test_trigger` | The test's own mistake: a query or procedure target that is not an NSID, a query input that is not an object of parameters, a record trigger for a collection with no record lexicon, a missing `rkey` on `record_update` or `record_delete`, a record that is not an object or cannot be encoded as DAG-CBOR, an invalid job type, or a malformed label |
| `HOST_ERROR` | `host_test_trigger` | HappyView could not run the trigger |
| `BAD_INPUT` | `host_test_mock` | No installed library serves the namespace, `responses` is not an object, or an `$error` response is malformed |
| `NOT_MOCKED` | A library call | The namespace is mocked and its mock lists no response for the method. The message names the methods it does list |
| `MOCK_EXHAUSTED` | A library call | The method's list of responses has run out |
| Any `$error` code | A library call | The mock answered with `$error` |

#### Editor declarations

An interpreter can render the types a script sees into declaration files for its language, which `happyview types` writes for editors (see the [editor types guide](editor-types.md)). It is the optional `get_declarations` export, which `interpreter_plugin!` emits when given `declarations:`:

```rust
interpreter_plugin! {
    info: PluginInfo::new("lua", "Lua", "1.0.0"),
    execute: execute,
    validate: validate,
    declarations: declarations,
}

fn declarations(input: &DeclarationsInput) -> Result<Declarations, PluginError> {
    Ok(Declarations { files: vec![DeclarationFile { path: "happyview/db.lua".into(), contents: render(input) }] })
}
```

The input is already parsed, so an interpreter only renders:

| Field | Holds |
| --- | --- |
| `libraries` | Each installed library's `namespace` and its surface as a `TypedSurface` |
| `lexicons` | Each project lexicon's `nsid`, `kind`, and its `main` def (`params`, `input`, `output` or `record`) plus every other def by name |
| `ctx` | The `ctx` object per script kind: `query`, `procedure`, `record`, `label`, `job`, `test` |
| `events` | The `input` of a record-event script (its `record` is a type variable for the record lexicon) and of a label script |

Every type is a `TypeExpr`. A `ref` names a library's named type, or a lexicon def as `nsid#name`; one that names nothing in the input should render as unknown. `ctx` holds the data HappyView sets; functions the interpreter adds itself, such as `ctx.job.progress`, are yours to declare.

Return paths relative to your output directory, with `/`. A path that is absolute, uses `\` or `..`, or is `manifest.json` fails the run. The export takes no host imports, and has 30 seconds. An interpreter without it is skipped.

### Dependencies

A `library` plugin may declare other libraries it calls through `host_call_library`:

```json
{
  "dependencies": [
    { "id": "happyview-db", "version": ">=1.0.0" },
    { "id": "happyview-http" }
  ]
}
```

`version` is a semver requirement (`>=1.0.0`, `^1`, …); it defaults to `*` when omitted. The loader validates the whole dependency graph on every install and uninstall: a missing dependency, an installed version that doesn't satisfy the requirement, a dependency cycle, and a self-dependency are all rejected. Uninstalling a plugin that other libraries still depend on is refused too, unless the request passes `?force=true` (see [Remove a plugin](../api-reference/admin/plugins.md#remove-a-plugin)) to cascade the removal.

`auth` plugins cannot declare dependencies — the loader rejects any `dependencies` entry on a manifest whose `plugin_type` is `auth` (or absent).

## API Endpoints

### Public Endpoints

| Endpoint                                | Description                                    |
| --------------------------------------- | ---------------------------------------------- |
| `GET /external-auth/providers`          | List available auth providers                  |
| `GET /external-auth/accounts`           | List user's linked accounts                    |
| `GET /external-auth/{plugin}/authorize` | Start OAuth flow                               |
| `GET /external-auth/{plugin}/callback`  | OAuth callback handler                         |
| `POST /external-auth/{plugin}/unlink`   | Unlink account                                 |
| `POST /external-auth/{plugin}/connect`  | Connect with API key (for `api_key` auth type) |
| `POST /external-auth/{plugin}/refresh`  | Refresh the caller's tokens if they have expired |

`GET /external-auth/accounts` includes each link's `expires_at` (RFC 3339, or `null` for a token that never expires).

`POST /external-auth/{plugin}/refresh` returns `{"refreshed": bool, "expires_at": string|null}`. Tokens are never returned. It answers `404` when the caller has no link to that plugin, `409` when the token has expired and the provider issued no refresh token (the account must be relinked), and `502` when the plugin's `refresh_tokens` call fails.

### Admin Endpoints

| Endpoint                                | Description                                 |
| --------------------------------------- | ------------------------------------------- |
| `GET /admin/plugins`                    | List installed plugins                      |
| `POST /admin/plugins`                   | Install a plugin                            |
| `POST /admin/plugins/preview`           | Preview plugin before installing            |
| `GET /admin/plugins/official`           | Browse the official plugin registry catalog |
| `DELETE /admin/plugins/{id}`            | Remove a plugin                             |
| `POST /admin/plugins/{id}/reload`       | Reload plugin from source                   |
| `POST /admin/plugins/{id}/check-update` | Check whether a newer version is available  |
| `GET /admin/plugins/{id}/secrets`       | Get configured secrets (masked)             |
| `PUT /admin/plugins/{id}/secrets`       | Update plugin secrets                       |

The dashboard's **Settings > Plugins** page calls `GET /admin/plugins/official` to populate the install browser, and `POST /admin/plugins/{id}/check-update` to display update badges on installed plugins.

## Plugin Exports

Plugins must export these functions:

| Export              | Signature                     | Description                  |
| ------------------- | ----------------------------- | ---------------------------- |
| `alloc`             | `(size: u32) -> u32`          | Allocate memory              |
| `dealloc`           | `(ptr: u32, size: u32)`       | Deallocate memory            |
| `get_authorize_url` | `(ptr: u32, len: u32) -> i64` | Generate OAuth authorize URL |
| `handle_callback`   | `(ptr: u32, len: u32) -> i64` | Handle OAuth callback        |
| `refresh_tokens`    | `(ptr: u32, len: u32) -> i64` | Refresh expired tokens       |
| `get_profile`       | `(ptr: u32, len: u32) -> i64` | Get external profile info    |

The host calls `refresh_tokens` on demand, when a stored token is within 60 seconds of its `expires_at`. A token stored without an expiry is never refreshed. The response may omit `refresh_token`; the host then keeps the previous one.

## Host Functions

Plugins can import these host functions:

| Import              | Description             |
| ------------------- | ----------------------- |
| `host_http_request` | Make HTTP requests      |
| `host_get_secret`   | Read configured secrets |
| `host_log`          | Write to server logs    |
| `host_kv_get`       | Read from KV storage    |
| `host_kv_set`       | Write to KV storage     |
| `host_kv_delete`    | Delete from KV storage  |

## Library Plugins

A `library` plugin exports a callable API surface instead of the `auth` contract above. Other plugins reach it through `host_call_library`; Lua scripts reach it through `require(namespace)`.

### Exports

| Export | Signature | Description |
| ------------------ | ----------------------------- | ------------------------------------------------------------------------ |
| `alloc`             | `(size: u32) -> u32`          | Allocate memory                                                          |
| `dealloc`           | `(ptr: u32, size: u32)`       | Deallocate memory                                                        |
| `plugin_info`       | `() -> i64`                   | Same `PluginInfo` envelope as an `auth` plugin                           |
| `get_api_surface`   | `() -> i64`                   | Returns the JSON-encoded [API surface](#api-surface-format) describing this library's callable functions |
| `call`              | `(ptr: u32, len: u32) -> i64` | Dispatches one function call                                             |

`call`'s input is a JSON envelope:

```json
{
  "function": "get",
  "args": ["https://api.example.com/status", { "headers": { "Accept": "application/json" } }],
  "context": {
    "caller_did": "did:plc:abc123",
    "has_pds_auth": false,
    "db_backend": "sqlite"
  }
}
```

- `function` — the export name being invoked
- `args` — a JSON array of positional arguments
- `context` — who the call is acting as: `caller_did` (nullable; absent for anonymous scripts), `has_pds_auth` (whether the caller currently holds a PDS session), and `db_backend` (`"sqlite"` or `"postgres"`, filled by the host on every call — see [Database access](#database-access)). `context` is threaded unchanged through every nested `host_call_library` hop, so a library can never widen who it's acting as.

### API surface format

`get_api_surface` describes the library's callable functions as data, so `require()` and other interpreters can render them. Here is the `happyview-http` plugin's surface, trimmed to one export:

```json
{
  "namespace": "happyview.http",
  "description": "Outbound HTTP requests",
  "exports": [
    {
      "name": "get",
      "kind": "function",
      "description": "Send a GET request",
      "params": [
        { "name": "url", "type": "string" },
        {
          "name": "opts",
          "type": "object?",
          "description": "Request options",
          "properties": [
            { "name": "headers", "type": "object?", "description": "Header name to value" },
            { "name": "body", "type": "string?", "description": "Request body (ignored for get/head)" }
          ]
        }
      ],
      "returns": {
        "type": "object",
        "properties": [
          { "name": "status", "type": "integer" },
          { "name": "body", "type": "string" },
          { "name": "headers", "type": "object", "description": "Lower-cased header name to value" }
        ]
      }
    }
  ],
  "types": []
}
```

#### Types

Every `type` in a surface is a type string. HappyView parses them all, and an editor's declarations are rendered from what it reads, so a library describes itself once and every interpreter's language gets types for it.

| Write | Means |
| --- | --- |
| `string`, `integer`, `number`, `boolean`, `bytes`, `null`, `blob`, `cid-link` | That value |
| `unknown`, `any` | Anything. `unknown` asks the script to check first |
| `object`, `array` | An object with any keys, a list of anything |
| `'asc'`, `42`, `true` | Exactly that value |
| `T[]` | A list of `T` |
| `T?` | `T` or `null`. On a param or property it also means the caller may leave it out |
| `A \| B` | Either. Parentheses group: `(string \| integer)[]` |
| `Record` | A named type from the surface's `types` |

A param or property is `{name, type, description?, required?, default?}`. `required` defaults to false when the type ends in `?` and true otherwise. A type can also be an object: `{"type": "object", "properties": [...]}` for an object whose keys you list, and `{"type": "array", "items": ...}` or `{"anyOf": [...]}`, which mean `T[]` and `A | B`.

`types` holds the named types, each `{name, description?, type}`:

```rust
ApiSurface::new("happyview.db")
    .named_type(
        "RecordEnvelope",
        "An indexed record and where it came from",
        json!({"type": "object", "properties": [
            {"name": "uri", "type": "string"},
            {"name": "cid", "type": "string?"},
            {"name": "record", "type": "object"}
        ]}),
    )
    .export(ApiExport::function("get").param("uri", "string", "AT URI").returns(json!("RecordEnvelope?")))
```

A method on a constructor's object takes the same `description`, `params` and `returns` as an export, through `ApiMethod::lazy("where").param(...).returns(...)` and `ApiExport::method`. A lazy method that gives no `returns` returns the object it was called on.

HappyView refuses to install a library whose surface does not parse, naming the export and the string: `library db describes itself wrongly: export 'get', returns: 'Recrd' names no type in this surface's types`. Check yours in the plugin's own tests:

```rust
#[test]
fn surface_parses() {
    happyview_plugin_sdk::declarations::assert_surface_parses(&surface());
}
```

Leaving something out is never an error: an export with no `returns` is typed as unknown.

#### Naming

Names are snake_case on the wire and in Lua — `namespace`, export `name`, method names, `library:call`, and documents like the one above all carry the canonical snake_case name a script typed. A JavaScript interpreter renders export and method names to camelCase, mapping back to the canonical name when it builds a call document: `save_local` becomes `saveLocal`. `namespace` is never rendered in any language — `require` resolves it by exact match, so a library published as `happyview.linked_repos` is loaded as `require("happyview.linked_repos")`.

#### Kinds and objects

An export's `kind` is `function`, `constant`, or `constructor`, and defaults to `function`.

`require()` renders `function` exports (as shown above) and `constructor` exports. `constant` is reserved for future interpreters and is not rendered by `require()`. A `constructor` export returns an object. The surface lists the object's `methods`, each `{name, mode}` with mode `lazy` or `immediate`, plus the signature fields described under [Types](#types):

| Method mode | Calling it |
| --- | --- |
| `lazy` | Appends `{name: args}` to the object's accumulated steps and returns the object, so calls chain. |
| `immediate` | Makes one library call named after the constructor, sending `{"args": [...], "steps": [...], "call": {"name": ..., "args": [...]}}` as its single argument, and returns the result. |

Here is `happyview.db`'s `records` export:

```json
{
  "name": "records",
  "kind": "constructor",
  "description": "Build a query against one collection",
  "params": [{ "name": "collection", "type": "string" }],
  "methods": [
    { "name": "where", "mode": "lazy" },
    { "name": "sort", "mode": "lazy" },
    { "name": "limit", "mode": "lazy" },
    { "name": "cursor", "mode": "lazy" },
    { "name": "did", "mode": "lazy" },
    { "name": "run", "mode": "immediate" },
    { "name": "count", "mode": "immediate" },
    { "name": "first", "mode": "immediate" }
  ]
}
```

Calling `run()` after a chain of `where`/`sort`/`limit` sends this document as the library call's single argument:

```json
{
  "args": ["app.bsky.feed.post"],
  "steps": [
    { "where": ["author", "=", "did:plc:abc"] },
    { "where": ["text", "like", "%happyview%"] },
    { "sort": ["createdAt", "desc"] },
    { "limit": [20] }
  ],
  "call": { "name": "run", "args": [] }
}
```

Two limits follow from JSON in, JSON out. An object holds no guest memory between calls, so a library cannot hold an open resource, such as a transaction, across calls. A script cannot hand a library a function, so there are no callbacks, comparators, or subscriptions.

### Host imports for libraries

Beyond the [host functions](#host-functions) above, a plugin declaring `library:call` can import:

| Import | Signature | Description |
| ------------------------ | ------------------------------------------------------------------------ | --------------------------------------------------------------------- |
| `host_call_library`      | `(lib_ptr, lib_len, fn_ptr, fn_len, args_ptr, args_len) -> i64`           | Call another installed library's `function` with a JSON `args` array, acting as the caller's `context` |
| `host_call_library_start` | `(lib_ptr, lib_len, fn_ptr, fn_len, args_ptr, args_len) -> i32`          | Start the same call without waiting for it, and return its handle — see [Concurrent library calls](#concurrent-library-calls) |
| `host_call_library_wait_any` | `(req_ptr, req_len) -> i64`                                           | Wait until any of a JSON array of handles has settled, and return its result |
| `host_get_api_surface`   | `(lib_ptr, lib_len) -> i64`                                               | Fetch another library's API surface (cached after the first call per registration) |

A plugin declaring `database:read` or `database:write` can import:

| Import | Signature | Description | Requires |
| ------------------------ | ------------------------------------------------------------------------ | --------------------------------------------------------------------- | --- |
| `host_db_query`          | `(sql_ptr, sql_len, params_ptr, params_len) -> i64`                       | Run a read query — see [Database access](#database-access)            | `database:read` or `database:write` |
| `host_db_execute`        | `(sql_ptr, sql_len, params_ptr, params_len) -> i64`                       | Run a write statement — see [Database access](#database-access)       | `database:write` |

`host_call_library` calls nest; a library calling a library that calls a third is normal. Depth is capped at **8**: the ninth hop fails with a depth-exceeded error. Every hop, whether from WASM through `host_call_library` or from Lua through `require()`, goes through the same dispatch path (`PluginExecutor::call_library`), so mixing the two does not bypass the limit.

A plugin declaring the listed capability can import the matching function below. Each takes one JSON spec and returns the usual `{ok}`/`{error}` envelope, with error codes `INVALID_SPEC`, `DB_ERROR`, `STORAGE_UNAVAILABLE`, `BAD_INPUT`, or `FORBIDDEN`. `STORAGE_UNAVAILABLE` means the [record storage](record-providers.md) that claims the collection is down; a script that passes it on answers `503`, which a client can retry:

| Import | Capability | Spec |
| --------------------------- | ----------------------------------- | ------------------------------------------------------------------------------- |
| `host_records_query`        | `records:read`                      | `{collection, did?, filter?, sort?, limit?, cursor?}` → `{records, cursor?}`, each record an envelope |
| `host_records_count`        | `records:read`                      | `{collection, did?, filter?}` → integer                                         |
| `host_records_get`          | `records:read`                      | `{uri}` → envelope or null                                                      |
| `host_records_search`       | `records:read`                      | `{collection, field, query, limit?}` → envelopes                                |
| `host_backlinks_query`      | `records:read`                      | `{uri, collection, did?, limit?, cursor?}` → `{records, cursor?}`, each record an envelope |
| `host_table_query`          | `database:read` or `database:write` | `{table, filter?, sort?, limit?, count?}` → rows, or an integer when `count` is set |

An envelope is the record shape every library read answers, described in [Libraries](../api-reference/lua/libraries.md#conventions-every-library-follows).

`filter`, on `host_records_query`, `host_records_count`, and `host_table_query`, is `{field, op, value}` or `{combine: "and"|"or", conditions: [...]}`, nesting capped at 5.

#### Concurrent library calls

`host_call_library` holds the plugin until the call returns, so two calls made with it run one after the other. To run several at once, start each with `host_call_library_start`, which takes the same arguments and returns a handle (a positive integer) immediately, then collect them with `host_call_library_wait_any`.

`host_call_library_wait_any` takes a JSON array of handles, such as `[3, 1, 7]`, and holds the plugin until any one of them has settled. It returns that call's result in the same envelope `host_call_library` returns, with the handle beside it: `{"handle": 3, "ok": ...}` or `{"handle": 3, "error": {"code", "message", "retryable"}}`. If more than one has already settled, the lowest-numbered handle comes back first. A handle can be collected once.

Everything that can go wrong with a call comes back through its handle, including arguments that are not a JSON array (`BAD_INPUT`), a missing `library:call` capability (`FORBIDDEN`), an unknown library, the depth limit, and the library's own errors. `host_call_library_start` only returns `0` when the host cannot read the request from the plugin's memory. A wait the host can never answer — an empty list, or a handle that was never issued or has already been collected — returns `{"error": {"code": "BAD_INPUT", ...}}` with no `handle`, immediately.

At most **8** started calls run at once for each run or library call. A call started beyond that still gets its handle immediately and waits for a free slot before it begins. When a run returns, calls it started and never collected are allowed to finish (for up to the script wall clock) and their results are discarded; a failure among them is written to the plugin's log. When a run fails or times out, its outstanding calls are stopped.

With the SDK, these are `host::call_library_start` and `host::call_library_wait_any`.

#### Database access

`host_db_query` and `host_db_execute` run SQL **untranslated** against whichever backend HappyView is running on, except reads of the record views that external record storage serves (see below) — placeholders are backend-native (`?` on SQLite, `$1`, `$2`, … on Postgres), the same rule as `raw` on [`happyview.sql`](../api-reference/lua/libraries.md). A plugin that supports both backends branches on placeholder syntax itself, using `ctx.db_backend` from the [call envelope](#library-plugins) — the plugin equivalent of `happyview.db`'s `backend()`. Record and table queries go through the imports above; `host_db_query`/`host_db_execute` are for raw SQL only.

- `database:read` permits `host_db_query` only, and only read-only statements: every statement must be a query whose body and every CTE are `SELECT`s. `WITH … INSERT/UPDATE/DELETE` and a data-modifying CTE (`WITH x AS (DELETE FROM t RETURNING uri) SELECT * FROM x`) count as writes and are rejected before they run.
- `database:write` permits both imports for any statement, including `INSERT`, `UPDATE`, `DELETE`, and `DROP`.
- Both share the same protected-table guard as `happyview.sql`: `happyview_*` tables (and `_sqlx_migrations`) are blocked by default, except the same allowlist of public AppView data — `happyview_records`, `happyview_record_refs`, `happyview_records_by_collection`, `happyview_labels`, `happyview_lexicons`, `happyview_jobs`, and the space data tables (`happyview_spaces`, `happyview_space_members`, `happyview_space_records`, `happyview_space_record_oplog`, `happyview_space_notify_registrations`, `happyview_space_dids`). Secrets, tokens, auth/privilege state, trust config, and cryptographic key material stay blocked regardless of which database capability is declared.

`happyview_records` and `happyview_record_refs` are views over HappyView's compact record tables. Reads behave as they did on the old tables, including `json_extract(record, '$.field')` on SQLite and `record::jsonb->>'field'` on Postgres. Writes have some differences: upserts with `INSERT … ON CONFLICT` don't work, `RETURNING` gives back the values you supplied rather than the stored row, writes on SQLite report 0 rows changed, and filtering on `uri` reads every row, so filter by `did`, `collection` and `rkey` instead. To read one collection, use `happyview_records_by_collection` with `hv_collection_id`, ordered by `created_at_us` (see [Reading one collection](database/database-setup.md#reading-one-collection)); filtering `happyview_records` by `collection` alone can read every record. [Raw SQL on the record views](database/database-setup.md#raw-sql-on-the-record-views) lists them all.

**When record storage offers a raw read route.** An instance can keep some collections in external [record storage](record-providers.md). When that storage offers a raw read route, reads of `happyview_records`, `happyview_record_refs` and `happyview_records_by_collection` run there, not on the instance's own database:

- Read-only SQL through `host_db_query` that reads only those two views runs on the route, and so does `host_table_query` on either view. They see the storage's records.
- The route can run a different backend from the instance, so `ctx.db_backend` doesn't tell you its placeholders or dialect. A SQLite instance can have its record views served by Postgres. A failed routed query's error names the backend that ran it.
- A query must read only those two views, or neither. One that joins them with other tables fails with `FORBIDDEN`, as does a read of them that HappyView can't confirm is read-only, such as one with `FOR SHARE`. Split it into two queries.
- While the storage is down, a routed query fails with `STORAGE_UNAVAILABLE`.
- The views on the route show only the storage's records. Records of other collections that this instance holds locally can't be read through them.
- Writes through the views run on the instance's own database as before, and see only local rows, even a write that reads the views, such as `INSERT … SELECT`. Writes to the storage's collections are refused.

### Capabilities

Every host function except `host_log` is gated by a capability. The loader reads the compiled module's import section and refuses to install a plugin whose imports are not all covered by declared capabilities, whatever its `plugin_type`.

| Capability | Risk | Grants |
| --- | --- | --- |
| `secrets:read` | Low | Read the secrets you configure for this plugin. |
| `kv:read` | Low | Read its own key-value storage. |
| `kv:write` | Low | Write to its own key-value storage (1 MB per scope). |
| `records:read` | Medium | Look up indexed AT Protocol records. |
| `network:request` | Medium | Make HTTP requests, only to the hosts it lists. Redirects are not followed. |
| `network:request:defined` | Medium | Make HTTP requests only to the hosts you list for this plugin in its settings. Redirects are not followed. |
| `network:request:unrestricted` | High | Make HTTP requests to any host on the internet, including internal services this server can reach. |
| `library:call` | Medium | Call other installed library plugins, which run with their own permissions (not this plugin's). |
| `database:read` | High | Run arbitrary read-only SQL against indexed records, labels, lexicons, jobs and space data. Internal auth, secret and key tables are blocked. |
| `database:write` | Critical | Run arbitrary SQL, including `INSERT`, `UPDATE`, `DELETE` and `DROP`, against indexed records, labels, lexicons, jobs and space data. This can destroy your index. |
| `caller:read` | Medium | Read from the AT Protocol network as the user who ran the script. |
| `caller:email` | High | Read the email address of the user who ran the script, and whether it is confirmed, from their PDS, whenever their sign-in granted this instance access to it. |
| `caller:write` | High | Create, update and delete records in the user's own repository and upload blobs to it. Each record write is mirrored into the local index at once, so the plugin sees its own write on the next read. |
| `caller:call` | Critical | Call any XRPC procedure as the user, including ones that change their account. A procedure this instance does not serve is forwarded to the NSID's authority without the user's credentials. |
| `records:write` | High | Write to this instance's record index directly, bypassing the network. This can replace the body of a record that arrived from the network while its CID and indexed time stay as the network set them. |
| `atproto:read` | Medium | Resolve any DID's service endpoints, download blobs from any repo on the network, look up labels applied to any URI, and verify this instance's attestation signatures. |
| `attest:sign` | High | Sign records with this instance's attestation key, producing a signature that asserts this instance vouches for the content. |
| `linked_repos:use` | High | Write records and upload blobs through any repo an admin has linked to this instance, within the scopes that admin granted, and call any XRPC method through it, which only that repo's PDS constrains. |
| `jobs:create` | Medium | Enqueue background jobs as the user who ran the script, optionally carrying that user's PDS session into the job. |
| `jobs:read` | Low | Read background jobs the user who ran the script created. |
| `jobs:read_any` | High | Read every background job on this instance, including other users' job input and results. |
| `spaces:read` | High | Read the members and records of every space this instance holds, regardless of each space's read policy. |
| `spaces:write` | High | Create spaces, write records into them, and manage their members and invites, as the user who ran the script and with that user's access. |

`network:request`, `network:request:defined`, and `network:request:unrestricted` are mutually exclusive — declare exactly one. `network:request` requires a non-empty `allowed_hosts`; `network:request:unrestricted` requires `allowed_hosts` to be empty; `network:request:defined` also requires `allowed_hosts` to be empty, because its hosts come from the operator's plugin settings instead of the manifest. `allowed_hosts` entries, wherever they come from, are bare hostnames, optionally prefixed `*.` to allow subdomains — no scheme, path, port, or whitespace:

```json
{
  "capabilities": ["network:request"],
  "allowed_hosts": ["api.example.com", "*.cdn.example.com"]
}
```

Choose `network:request:defined` over `network:request` when the plugin talks to a self-hosted backend whose address belongs to the operator, not to you as the plugin's author — a URL you can't know at publish time and shouldn't have to declare. It's also the better choice over `network:request:unrestricted` in that case: declaring unrestricted network access to reach one operator-chosen server asks for far more than the plugin needs. Until an admin lists hosts for the plugin under its settings, every request fails with "no hosts are configured for this plugin; an admin lists them under the plugin's settings". A plugin reads its own effective list back with `host::allowed_hosts()`: the operator-configured list under `network:request:defined`, the manifest's `allowed_hosts` under `network:request`, or an empty list under `network:request:unrestricted`.

### Acting as the caller

A plugin declaring `caller:read`, `caller:email`, `caller:write`, `caller:call`, or `records:write` can import the matching function below. Each takes one JSON spec and returns the usual `{ok}`/`{error}` envelope:

| Import | Capability | Spec → result |
| --------------------------- | ------------ | ------------------------------------------------------------------------------- |
| `host_caller_xrpc_query`    | `caller:read`  | `{method, params?}` → the response body |
| `host_caller_get_session`   | `caller:read`  | `{}` → `{did, handle?}` from the caller's PDS `getSession` |
| `host_caller_get_email`     | `caller:email` | `{}` → `{email?, email_confirmed?}` from the caller's PDS `getSession`; both absent unless their sign-in granted the address, which the reference PDS does under `transition:email` or `account:email` |
| `host_caller_create_record` | `caller:write` | `{collection, rkey?, repo?, record, validate?}` → `{uri, cid}`; mirrored into the index |
| `host_caller_put_record`    | `caller:write` | `{uri, record, swap_cid?, validate?}` → `{uri, cid}`; mirrored into the index |
| `host_caller_delete_record` | `caller:write` | `{uri}` → nothing; removed from the index |
| `host_caller_upload_blob`   | `caller:write` | `{bytes, mime_type}` → the PDS's blob reference |
| `host_caller_xrpc_procedure`| `caller:call`  | `{method, input, params?}` → the response body |
| `host_records_index_put`    | `records:write`| `{collection, rkey, did?, record, cid?}` → `{uri, cid}` |
| `host_records_index_delete` | `records:write`| `{uri}` → whether a row was removed |
| `host_lexicon_get`          | none           | `{nsid}` → the lexicon's stored JSON, or null if none is uploaded |

`validate` decides how a write is checked against its lexicon. Left out, HappyView checks the record and the PDS validates it when it knows or can resolve the lexicon — `com.atproto.repo.createRecord`'s own default. `true` asks the PDS to validate strictly, which refuses a record whose lexicon it can't resolve; `false` skips both checks.

`host_records_index_put`'s `did` is optional on the wire but required to succeed — the host has no default author for a row and rejects a spec without one.

A library receives the script's session only when the script runner that made the call holds the user's PDS session — a procedure script running with PDS auth, or a job created with `{ auth = true }`. A query, record-event, or label script has no session, and a declared-capability call from one fails with `NO_SESSION` — except `host_caller_xrpc_query`, which needs no session at all, the same as `happyview.xrpc`'s `query`, which it backs. The [AT Protocol reads and attestation](#at-protocol-reads-and-attestation) imports below also need no session, since each acts on a DID, blob, URI, or record named in its own spec rather than on the caller's repo.

A record write, put, or delete targets the caller's own repo, or the repo of the account the script is delegated to act for. Any other repo fails with `WRITABLE_REPO`, since a caller-acting write can never succeed against a repo the instance holds no credentials for.

Every import checks the plugin's declared capability first. Every `host_caller_*` import but one then checks for a session before decoding the spec; `host_caller_xrpc_query` needs no session — it decodes and runs on the capability check alone, falling back to the call context's `caller_did` for claims when there is no session to take them from. The two `host_records_index_*` imports also need no session and go straight from the capability check to decoding, and `host_lexicon_get` needs no capability at all. Errors reaching the guest use the codes `FORBIDDEN` (the capability isn't declared), `NO_SESSION`, `WRITABLE_REPO`, `AUTH_REQUIRED` (the session died), `PDS_ERROR` (the PDS rejected the write), `XRPC_ERROR` (the local handler or proxy rejected the call), `HOST_ERROR` (a failure with no PDS or XRPC status to report), and `BAD_INPUT`.

`host_caller_get_session` and `host_caller_get_email` also refuse a service-auth caller with `NO_SESSION`: the runner lends such a caller the user's own sign-in to this instance, which the calling service was never handed. `caller:email` is High because the address is personal data `caller:read` never covered. A job that returns it, or passes it to `ctx.job.progress`, stores it in the job row, where `jobs:read` can read it.

`caller:call` is Critical because an arbitrary procedure carries the same power as the user's whole session, including ones that change their account. `caller:write` is High because it is bounded to repo writes the PDS's own scope check still governs.

### AT Protocol reads and attestation

A plugin declaring `atproto:read` or `attest:sign` can import the matching function below. Each takes one JSON spec and returns the usual `{ok}`/`{error}` envelope, with error codes `RESOLVE_ERROR`, `BLOB_ERROR`, `NO_SIGNER`, `UNVERIFIABLE`, `FORBIDDEN`, `BAD_INPUT`, and `HOST_ERROR`. `host_attest_sign` is the only one of the five that reads the call context: it signs as `ctx.caller_did` and refuses with `BAD_INPUT` if the call has none, since the DID is part of the signed content and there is no meaningful anonymous signature. `host_atproto_blob_download` refuses a resolved service endpoint that isn't `https`, or that resolves to a loopback, link-local, or private address, before fetching it — a DID document is attacker-controlled input, and the plugin never chose this URL to check it against `allowed_hosts` the way `host_http_request` does:

| Import | Capability | Spec → result |
| --------------------------- | -------------- | ------------------------------------------------------------------------------- |
| `host_atproto_resolve_service` | `atproto:read` | `{did}` → the DID's advertised service endpoint, or null |
| `host_atproto_blob_download`   | `atproto:read` | `{did, cid}` → `{bytes, mime_type, size}` |
| `host_labels_get`              | `atproto:read` | `{uris}` → a map of URI to `[{src, uri, val, cts}]`, every requested URI present |
| `host_attest_sign`             | `attest:sign`  | `{record}` → the signature object just added to it, signed as `ctx.caller_did` |
| `host_attest_verify`           | `atproto:read` | `{record, signature, repo_did}` → boolean |

### Linked repos and jobs

A plugin declaring `linked_repos:use`, `jobs:create`, `jobs:read` or `jobs:read_any` can import the matching function below. Each takes one JSON spec and returns the usual `{ok}`/`{error}` envelope:

| Import | Capability | Spec → result |
| --------------------------------- | ------------------- | ------------------------------------------------------------------------------- |
| `host_linked_repos_list`          | `linked_repos:use`  | `{}` → array of `{id, did, handle, reason, status, scopes}` |
| `host_linked_repo_create_record`  | `linked_repos:use`  | `{did, collection, rkey?, record}` → `{uri, cid}` |
| `host_linked_repo_put_record`     | `linked_repos:use`  | `{did, collection, rkey, record, swap_cid?}` → `{uri, cid}` |
| `host_linked_repo_delete_record`  | `linked_repos:use`  | `{did, collection, rkey}` → nothing |
| `host_linked_repo_upload_blob`    | `linked_repos:use`  | `{did, bytes, mime_type}` → the PDS's blob reference |
| `host_linked_repo_call`           | `linked_repos:use`  | `{did, method, params?, input?}` → the response |
| `host_jobs_create`                | `jobs:create`       | `{job_type, input, auth}` → the job id |
| `host_jobs_get`                   | `jobs:read`         | `{id}` → the job or `null`; only returns the caller's own jobs |
| `host_jobs_get_any`               | `jobs:read_any`     | `{id}` → the job or `null` |
| `host_jobs_list_any`              | `jobs:read_any`     | `{status?: [string], job_type?, limit?}` → jobs, newest first; `limit` defaults to 50, maximum 200, minimum 1 (0 is `BAD_INPUT`) |

The plugin capability `jobs:read` is distinct from the admin user permission of the same name (see [Permissions](./permissions.md)): it controls what a plugin can read, not what an operator can see in the dashboard. Job views never include `inherit_auth`, `api_client_id` or `dpop_key_id`.

Every import needs no caller session, except `host_jobs_create` with `auth` set, which requires the runner to hold a DPoP session.

Errors reaching the guest: linked repos imports use `NOT_LINKED` (no grant for that DID), `SCOPE` (the grant's scopes don't cover the write), `NEEDS_REAUTH` (the grant needs re-authorization), `PDS_ERROR`, `FORBIDDEN`, `BAD_INPUT`, and `HOST_ERROR`. `host_jobs_create` uses `BAD_INPUT` (an invalid or reserved job type, or a call with no caller DID), `NO_SESSION` (`auth` set on a runner with no DPoP session), `FORBIDDEN`, and `HOST_ERROR`.

`host_linked_repo_call` has no scope pre-check; the PDS enforces the token's actual scope.

#### How bytes travel

Wherever an import's spec or answer carries bytes — a blob, an upload, an HTTP body — they're a JSON string when they're valid UTF-8, and the atproto data model's `{"$bytes": "<base64>"}` when they aren't. Scripts see the same two forms, and pass them on as they are. The SDK's `Bytes` type reads and writes both, so a library function that takes or returns content can use it for its own arguments too: `Bytes::from_value(&args[0])` reads one without copying it.

Data the host hands a plugin brings its own fuel, so a library plugin can move a payload as large as the host allows — a multi-megabyte blob, say — without running out of the budget that bounds its computation.

### Spaces

A plugin declaring `spaces:read` or `spaces:write` can import the matching function below. Each takes one JSON spec and returns the usual `{ok}`/`{error}` envelope. A space is `{uri, id, did, authority_did, creator_did, spaceType, skey, display_name, description, read_policy, write_policy, app_access, config, revision, created_at, updated_at}`; a member is `{did, access}`, where `access` is one of `read`, `write`, `read_self`, or `none`; an invite is `{invite_id, token, access, max_uses, expires_at}`; a record is `{uri, collection, rkey, record, cid, author_did}`.

| Import | Capability | Spec → result |
| --- | --- | --- |
| `host_spaces_info` | `spaces:read` | `{uri}` → the space, or null |
| `host_spaces_query` | `spaces:read` | `{uri, collection?, limit?, cursor?}` → `{records, cursor?}`, `cursor` absent on the last page |
| `host_spaces_members` | `spaces:read` | `{uri}` → array of members |
| `host_spaces_access` | `spaces:read` | `{uri, did}` → the DID's access, or null if they aren't a member or the space does not exist |
| `host_spaces_create` | `spaces:write` | `{spaceType, skey, display_name?, description?, read_policy?, write_policy?, app_access?, config?}` → the space |
| `host_spaces_accept_invite` | `spaces:write` | `{token}` → the space |
| `host_spaces_write_record` | `spaces:write` | `{uri, collection, record}` → `{uri, cid}` |
| `host_spaces_put_record` | `spaces:write` | `{uri, collection, rkey, record, swap_cid?}` → `{uri, cid}` |
| `host_spaces_delete_record` | `spaces:write` | `{uri, collection, rkey, swap_cid?}` → nothing |
| `host_spaces_add_member` | `spaces:write` | `{uri, did, access?, read?, write?, is_delegation?}` → the member |
| `host_spaces_set_member` | `spaces:write` | `{uri, did, access?, read?, write?, is_delegation?}` → the member, upserting rather than raising `CONFLICT` on an existing one |
| `host_spaces_remove_member` | `spaces:write` | `{uri, did}` → nothing |
| `host_spaces_update` | `spaces:write` | `{uri, display_name?, description?, read_policy?, write_policy?, app_access?, config?}` → the space |
| `host_spaces_delete` | `spaces:write` | `{uri}` → nothing |
| `host_spaces_create_invite` | `spaces:write` | `{uri, access?, read?, write?, max_uses?, expires_at?}` → the invite |

`spaces:read` needs no caller and enforces no space's read policy — that's why it's High rather than Medium, and its capability description says exactly what it reaches. Every `spaces:write` import acts as `ctx.caller_did` and fails with `BAD_INPUT` when the call has none; a write against a space whose repo has migrated to the member's own PDS then uses that member's session and fails with `NOT_AUTHORIZED` if they have none. On `host_spaces_update`, a key absent from the spec leaves that field unchanged, `false` clears `display_name` or `description`, and a string sets it; `read_policy`, `write_policy`, `app_access`, and `config` replace whole when present.

The three imports that take access take it either as the `read`/`write` pair or as the `access` word, never both. Half a pair is refused too: the word cannot express write without read, so a member or invite that needs that combination must name both booleans, and defaulting the missing half would grant or withdraw access the caller never asked for. A member and an invite are reported back with both — the pair, and the nearest word for it.

Errors reaching the guest: `SPACES_DISABLED` (the `spaces_enabled` feature flag is off), `NOT_FOUND`, `NOT_AUTHORIZED`, `CONFLICT`, `PDS_ERROR`, `BAD_INPUT`, `HOST_ERROR`, and `FORBIDDEN`.

### Using a library from Lua

Installed libraries are available to scripts via `require(namespace)`. Each `function` export becomes an async Lua function, and each `constructor` export becomes a function returning a chainable object:

```lua
local db = require("happyview.db")

function handle(input, ctx)
  local page = db.records("app.bsky.feed.post")
    :where("author", "=", "did:plc:abc")
    :sort("createdAt", "desc")
    :limit(20)
    :run()

  -- each entry is an envelope; the post itself is entry.record
  return page.records
end
```

`where`, `sort`, and `limit` are lazy: each returns the same object, which is why they chain. `run` is immediate: it makes the library call and returns the result. See the library's own README for its full set of methods — `happyview-db`'s, for this example.

`require` resolves `namespace` against every installed `library` plugin's manifest `namespace` (or `id`, if `namespace` is unset), and caches the result for the rest of the script run. Requiring a name with no matching installed library plugin fails with:

```
module '<name>' not found -- is the '<name>' library plugin installed?
```

An empty Lua table argument (`{}`) is encoded as a JSON object, the same convention as `internal.json`'s `encode` — pass it through `json.to_array({})` to send an empty JSON array instead.

### Using the SDK

Everything above — the allocator, the packed-`i64` calling convention, the JSON envelope, and the `env` host imports — is what a plugin would otherwise have to hand-roll behind `extern "C"`. The `happyview-plugin-sdk` crate (`crates/happyview-plugin-sdk` in the HappyView repo) owns all of it, so a plugin crate needs only this one dependency.

- `library_plugin! { info: ..., surface: ..., call: ... }` generates the five ABI exports (`alloc`, `dealloc`, `plugin_info`, `get_api_surface`, `call`) from a `PluginInfo`, an `ApiSurface`-returning function, and a dispatch function — the `export_abi!` macro it builds on is also available directly for lower-level cases.
- `host::*` gives typed, `Result`-returning wrappers over every host import — `host::http_request`, `host::allowed_hosts`, `host::kv_get`/`kv_set`/`kv_delete`, `host::get_secret`, `host::call_library`, `host::call_library_start`/`call_library_wait_any`, `host::library_surface`, `host::db_query`/`db_execute`, `host::lookup_record`, and `host::log`/`debug`/`info`/`warn`/`error`. A native (non-wasm32) build compiles the whole SDK, so a plugin's own logic is testable with `cargo test`; the host wrappers just report `host::HostError::NotWasm` there instead of calling anything.
- Only the imports a plugin actually calls end up in its compiled module; an unused `host::*` wrapper is dropped at link time, so the loader's import check sees exactly what the plugin uses.
- `happyview_plugin_sdk::wire` holds every type that crosses the WASM boundary (`PluginInfo`, `ApiSurface`, `CallInput`, `HttpRequest`/`HttpResponse`, `TokenSet` and the rest). The host re-exports these rather than redefining them, so the two sides cannot drift. Plugins import them from `happyview_plugin_sdk::{PluginInfo, ...}` or `host::HttpRequest`.

The `http` plugin (outbound `get`/`post`/`put`/`patch`/`delete`/`head` through `host::http_request`) is the worked example. HappyView's test fixture at `tests/fixtures/sdk_http` is the same source with a different id.

All plugins, including the standard library ones, live in the [plugins repository](https://github.com/happyproto/plugins) and consume `happyview-plugin-sdk` as an ordinary dependency — HappyView itself ships no plugins, only the SDK crate and, for its own tests, a small SDK-built fixture. `tests/fixtures/test_library` is the one exception: it stays hand-rolled against the raw ABI on purpose, as the conformance fixture the SDK itself is checked against.

## Next steps

- [Official plugins repository](https://github.com/happyproto/plugins) — ready-to-use plugins and the plugin SDK
- [Plugins guide](plugins.md) — install and configure plugins
- [API Keys](./api-keys.md) — authenticate programmatic access to admin endpoints
- [Permissions](./permissions.md) — configure user access to plugin management

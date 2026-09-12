---
title: "Developing Plugins"
---

This guide covers how to build your own HappyView WASM plugins. For installing and configuring plugins, see the [Plugins guide](plugins.md).

See the [happyview-plugins](https://tangled.org/gamesgamesgamesgames.games/happyview-plugins) repository for examples and the plugin SDK.

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

The fields above are the `auth` baseline. [Plugin Types](#plugin-types) below covers the fields a `library` or `interpreter` manifest adds: `plugin_type`, `capabilities`, `allowed_hosts`, `publisher`, `dependencies`, and `namespace`.

## Plugin Types

The manifest's `plugin_type` field decides what the host expects the plugin to export and how scripts and other plugins use it. A manifest that omits `plugin_type` defaults to `auth`.

| Type | Description |
| --- | --- |
| `auth` (default) | An external-auth plugin: OAuth/OpenID/API-key login plus profile lookup. The [Plugin Exports](#plugin-exports) and [Host Functions](#host-functions) below are the `auth` contract. |
| `library` | A plugin other plugins and Lua scripts call into, via `host_call_library` or `require(namespace)`. See [Library Plugins](#library-plugins) below. |
| `interpreter` | Reserved for future non-Lua script runtimes. Not yet implemented. |

Every plugin, `auth` included, needs `api_version` `"2"` and a `capabilities` list covering everything its compiled module imports. The loader refuses an older or missing `api_version` with an error naming the plugin type and the version it found.

A `library` plugin may declare `namespace` — the name scripts use in `require(namespace)`. It defaults to `id` when omitted.

### Dependencies

A `library` plugin may declare other libraries it calls through `host_call_library`:

```json
{
  "dependencies": [
    { "id": "db", "version": ">=1.0.0" },
    { "id": "http" }
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
    "has_pds_auth": false
  }
}
```

- `function` — the export name being invoked
- `args` — a JSON array of positional arguments
- `context` — who the call is acting as: `caller_did` (nullable; absent for anonymous scripts) and `has_pds_auth` (whether the caller currently holds a PDS session). `context` is threaded unchanged through every nested `host_call_library` hop, so a library can never widen who it's acting as.

### API surface format

`get_api_surface` describes the library's callable functions as data, so `require()` and other interpreters can render them. Here is the `http` plugin's surface, trimmed to one export:

```json
{
  "namespace": "http",
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

`kind` defaults to `"function"`, the only kind `require()` renders; other kinds are skipped. `types` holds named types referenced by `params`/`returns`; the host treats it as opaque.

### Host imports for libraries

Beyond the [host functions](#host-functions) above, a plugin declaring `library:call` can import:

| Import | Signature | Description |
| ------------------------ | ------------------------------------------------------------------------ | --------------------------------------------------------------------- |
| `host_call_library`      | `(lib_ptr, lib_len, fn_ptr, fn_len, args_ptr, args_len) -> i64`           | Call another installed library's `function` with a JSON `args` array, acting as the caller's `context` |
| `host_get_api_surface`   | `(lib_ptr, lib_len) -> i64`                                               | Fetch another library's API surface (cached after the first call per registration) |

A plugin declaring `database:read` or `database:write` can import:

| Import | Signature | Description | Requires |
| ------------------------ | ------------------------------------------------------------------------ | --------------------------------------------------------------------- | --- |
| `host_db_query`          | `(sql_ptr, sql_len, params_ptr, params_len) -> i64`                       | Run a read query — see [Database access](#database-access)            | `database:read` or `database:write` |
| `host_db_execute`        | `(sql_ptr, sql_len, params_ptr, params_len) -> i64`                       | Run a write statement — see [Database access](#database-access)       | `database:write` |

`host_call_library` calls nest; a library calling a library that calls a third is normal. Depth is capped at **8**: the ninth hop fails with a depth-exceeded error. Every hop, whether from WASM through `host_call_library` or from Lua through `require()`, goes through the same dispatch path (`PluginExecutor::call_library`), so mixing the two does not bypass the limit.

#### Database access

`host_db_query` and `host_db_execute` run SQL **untranslated** against whichever backend HappyView is running on — placeholders are backend-native (`?` on SQLite, `$1`, `$2`, … on Postgres), the same rule as Lua's [`db.raw`](../api-reference/lua/database-api.md#protected-tables). A plugin that supports both backends has to branch on placeholder syntax itself; there is no plugin equivalent of Lua's `db.backend()`.

- `database:read` permits `host_db_query` only, and only read-only statements: every statement must be a query whose body and every CTE are `SELECT`s. `WITH … INSERT/UPDATE/DELETE` and a data-modifying CTE (`WITH x AS (DELETE FROM t RETURNING uri) SELECT * FROM x`) count as writes and are rejected before they run.
- `database:write` permits both imports for any statement, including `INSERT`, `UPDATE`, `DELETE`, and `DROP`.
- Both share the same protected-table guard as `db.raw`: `happyview_*` tables (and `_sqlx_migrations`) are blocked by default, except the same allowlist of public AppView data — `happyview_records`, `happyview_record_refs`, `happyview_labels`, `happyview_lexicons`, `happyview_jobs`, and the space data tables (`happyview_spaces`, `happyview_space_members`, `happyview_space_records`, `happyview_space_record_oplog`, `happyview_space_notify_registrations`, `happyview_space_dids`). Secrets, tokens, auth/privilege state, trust config, and cryptographic key material stay blocked regardless of which database capability is declared.

### Capabilities

Every host function except `host_log` is gated by a capability. The loader reads the compiled module's import section and refuses to install a plugin whose imports are not all covered by declared capabilities, whatever its `plugin_type`.

| Capability | Risk | Grants |
| --- | --- | --- |
| `secrets:read` | Low | Read the secrets you configure for this plugin. |
| `kv:read` | Low | Read its own key-value storage. |
| `kv:write` | Low | Write to its own key-value storage (1 MB per scope). |
| `records:read` | Medium | Look up indexed AT Protocol records. |
| `network:request` | Medium | Make HTTP requests, only to the hosts it lists. Redirects are not followed. |
| `network:request:unrestricted` | High | Make HTTP requests to any host on the internet, including internal services this server can reach. |
| `library:call` | Medium | Call other installed library plugins, which run with their own permissions (not this plugin's). |
| `database:read` | High | Run arbitrary read-only SQL against indexed records, labels, lexicons, jobs and space data. Internal auth, secret and key tables are blocked. |
| `database:write` | Critical | Run arbitrary SQL, including `INSERT`, `UPDATE`, `DELETE` and `DROP`, against indexed records, labels, lexicons, jobs and space data. This can destroy your index. |

`network:request` and `network:request:unrestricted` are mutually exclusive — declare one or the other, never both. `network:request` requires a non-empty `allowed_hosts`; `network:request:unrestricted` requires `allowed_hosts` to be empty. `allowed_hosts` entries are bare hostnames, optionally prefixed `*.` to allow subdomains — no scheme, path, port, or whitespace:

```json
{
  "capabilities": ["network:request"],
  "allowed_hosts": ["api.example.com", "*.cdn.example.com"]
}
```

### Using a library from Lua

Installed libraries are available to scripts via `require(namespace)`. Each function export becomes an async Lua function:

```lua
local http = require("http")

function handle()
  local response = http.get("https://api.example.com/status", { headers = { Accept = "application/json" } })
  return response.body
end
```

`require` resolves `namespace` against every installed `library` plugin's manifest `namespace` (or `id`, if `namespace` is unset), and caches the result for the rest of the script run. Requiring a name with no matching installed library plugin fails with:

```
module '<name>' not found -- is the '<name>' library plugin installed?
```

An empty Lua table argument (`{}`) is encoded as a JSON object, matching `json.encode`'s convention everywhere else — wrap it in `toarray({})` to send an empty JSON array instead.

### Using the SDK

Everything above — the allocator, the packed-`i64` calling convention, the JSON envelope, and the `env` host imports — is what a plugin would otherwise have to hand-roll behind `extern "C"`. The `happyview-plugin-sdk` crate (`crates/happyview-plugin-sdk` in the HappyView repo) owns all of it, so a plugin crate needs only this one dependency.

- `library_plugin! { info: ..., surface: ..., call: ... }` generates the five ABI exports (`alloc`, `dealloc`, `plugin_info`, `get_api_surface`, `call`) from a `PluginInfo`, an `ApiSurface`-returning function, and a dispatch function — the `export_abi!` macro it builds on is also available directly for lower-level cases.
- `host::*` gives typed, `Result`-returning wrappers over every host import — `host::http_request`, `host::kv_get`/`kv_set`/`kv_delete`, `host::get_secret`, `host::call_library`, `host::library_surface`, `host::db_query`/`db_execute`, `host::lookup_record`, and `host::log`/`debug`/`info`/`warn`/`error`. A native (non-wasm32) build compiles the whole SDK, so a plugin's own logic is testable with `cargo test`; the host wrappers just report `host::HostError::NotWasm` there instead of calling anything.
- Only the imports a plugin actually calls end up in its compiled module; an unused `host::*` wrapper is dropped at link time, so the loader's import check sees exactly what the plugin uses.
- `happyview_plugin_sdk::wire` holds every type that crosses the WASM boundary (`PluginInfo`, `ApiSurface`, `CallInput`, `HttpRequest`/`HttpResponse`, `TokenSet` and the rest). The host re-exports these rather than redefining them, so the two sides cannot drift. Plugins import them from `happyview_plugin_sdk::{PluginInfo, ...}` or `host::HttpRequest`.

The `http` plugin (outbound `get`/`post`/`put`/`patch`/`delete`/`head` through `host::http_request`) is the worked example. HappyView's test fixture at `tests/fixtures/sdk_http` is the same source with a different id.

All plugins, including the standard library ones, live in the [plugins repository](https://tangled.org/gamesgamesgamesgames.games/happyview-plugins) and consume `happyview-plugin-sdk` as an ordinary dependency — HappyView itself ships no plugins, only the SDK crate and, for its own tests, a small SDK-built fixture. `tests/fixtures/test_library` is the one exception: it stays hand-rolled against the raw ABI on purpose, as the conformance fixture the SDK itself is checked against.

## Next steps

- [Official plugins repository](https://tangled.org/gamesgamesgamesgames.games/happyview-plugins) — ready-to-use plugins and the plugin SDK
- [Plugins guide](plugins.md) — install and configure plugins
- [API Keys](./api-keys.md) — authenticate programmatic access to admin endpoints
- [Permissions](./permissions.md) — configure user access to plugin management

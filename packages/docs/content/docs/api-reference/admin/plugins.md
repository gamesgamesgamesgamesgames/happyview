---
title: "Plugins"
---

Plugins extend HappyView with WebAssembly modules sourced from the [official plugin registry](../../guides/plugins.md) or any URL serving a `manifest.json`. Most endpoints take a plugin manifest URL and load (or reload) the plugin in place — no restart needed. Encrypted plugin secrets require `TOKEN_ENCRYPTION_KEY` to be configured.

```ts tab="TypeScript" tab-group="language"
const TOKEN = "hv_..."; // your API key
const headers = { Authorization: `Bearer ${TOKEN}` };
```
```js tab="JavaScript" tab-group="language"
const TOKEN = "hv_..."; // your API key
const headers = { Authorization: `Bearer ${TOKEN}` };
```
```rust tab="Rust" tab-group="language"
let token = "hv_..."; // your API key
```
```go tab="Go" tab-group="language"
token := "hv_..." // your API key
```
```sh tab="cURL" tab-group="language"
# All examples assume $TOKEN is an API key (hv_...)
AUTH="Authorization: Bearer $TOKEN"
```

## List installed plugins

```
GET /admin/plugins
```

Requires `plugins:read`. Returns every loaded plugin with its source, required secrets, configuration status, and any pending updates from the official registry cache.

```ts tab="TypeScript" tab-group="language"
interface RequiredSecret {
  key: string;
  name: string;
  description: string;
}

interface PluginSummary {
  id: string;
  name: string;
  version: string;
  source: string;
  url: string;
  sha256: string | null;
  enabled: boolean;
  auth_type: string;
  required_secrets: RequiredSecret[];
  secrets_configured: boolean;
  loaded_at: string | null;
  update_available: boolean;
  latest_version: string;
  pending_releases: string[];
}

interface PluginsResponse {
  encryption_configured: boolean;
  plugins: PluginSummary[];
}

const response = await fetch("http://127.0.0.1:3000/admin/plugins", {
  headers,
});
const data: PluginsResponse = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/plugins", {
  headers,
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("http://127.0.0.1:3000/admin/plugins")
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/plugins", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/plugins -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "encryption_configured": true,
  "plugins": [
    {
      "id": "steam",
      "name": "Steam",
      "version": "1.2.0",
      "source": "url",
      "url": "https://example.com/plugins/steam/manifest.json",
      "sha256": null,
      "enabled": true,
      "auth_type": "openid",
      "required_secrets": [
        {
          "key": "PLUGIN_STEAM_API_KEY",
          "name": "Steam Web API Key",
          "description": "Get your API key at steamcommunity.com/dev/apikey"
        }
      ],
      "secrets_configured": true,
      "loaded_at": null,
      "update_available": false,
      "latest_version": "1.2.0",
      "pending_releases": []
    }
  ]
}
```

`secrets_configured` is `true` if the plugin has no required secrets, or if a row exists for it in `happyview_plugin_configs`. `update_available` and `pending_releases` are populated from the cached official registry — call `POST /admin/plugins/{id}/check-update` to refresh them.

## Preview a plugin before installing

```
POST /admin/plugins/preview
```

Requires `plugins:create`. Fetches and parses a manifest without installing the plugin, so the dashboard can show what it would register.

```ts tab="TypeScript" tab-group="language"
interface PluginPreview {
  id: string;
  name: string;
  version: string;
  description: string;
  icon_url: string;
  auth_type: string;
  required_secrets: RequiredSecret[];
  manifest_url: string;
  wasm_url: string;
}

const response = await fetch("http://127.0.0.1:3000/admin/plugins/preview", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    url: "https://example.com/plugins/steam/manifest.json",
  }),
});
const data: PluginPreview = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/plugins/preview", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    url: "https://example.com/plugins/steam/manifest.json",
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("http://127.0.0.1:3000/admin/plugins/preview")
    .bearer_auth(token)
    .json(&serde_json::json!({
        "url": "https://example.com/plugins/steam/manifest.json"
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{ "url": "https://example.com/plugins/steam/manifest.json" }`)
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/admin/plugins/preview", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/plugins/preview \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{ "url": "https://example.com/plugins/steam/manifest.json" }'
```

**Response**: `200 OK`

```json
{
  "id": "steam",
  "name": "Steam",
  "version": "1.2.0",
  "description": "Import your Steam game library and playtime data.",
  "icon_url": "https://example.com/steam-icon.png",
  "auth_type": "openid",
  "required_secrets": [
    { "key": "PLUGIN_STEAM_API_KEY", "name": "Steam Web API Key", "description": "..." }
  ],
  "manifest_url": "https://example.com/plugins/steam/manifest.json",
  "wasm_url": "https://example.com/plugins/steam/steam.wasm"
}
```

Returns `400 Bad Request` if the manifest can't be fetched or parsed.

## Install a plugin

```
POST /admin/plugins
```

Requires `plugins:create`. Fetches the manifest, downloads the WASM, registers the plugin, and persists it.

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/plugins", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    url: "https://example.com/plugins/steam/manifest.json",
    sha256: "abc123...",
  }),
});
const data: PluginSummary = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/plugins", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    url: "https://example.com/plugins/steam/manifest.json",
    sha256: "abc123...",
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("http://127.0.0.1:3000/admin/plugins")
    .bearer_auth(token)
    .json(&serde_json::json!({
        "url": "https://example.com/plugins/steam/manifest.json",
        "sha256": "abc123..."
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "url": "https://example.com/plugins/steam/manifest.json",
  "sha256": "abc123..."
}`)
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/admin/plugins", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/plugins \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://example.com/plugins/steam/manifest.json",
    "sha256": "abc123..."
  }'
```

| Field    | Type   | Required | Description                                                                                          |
| -------- | ------ | -------- | ---------------------------------------------------------------------------------------------------- |
| `url`    | string | yes      | URL to the plugin's `manifest.json`                                                                  |
| `sha256` | string | no       | Optional sha256 of the WASM binary. If provided, install fails when the downloaded hash mismatches   |

**Response**: `200 OK` returning the same `PluginSummary` shape as the list endpoint. `secrets_configured` will be `false` if the plugin requires any secrets — call `PUT /admin/plugins/{id}/secrets` to configure them before the plugin can run.

## List official plugins

```
GET /admin/plugins/official
```

Requires `plugins:read`. Returns the cached catalog of plugins from the official registry. The cache is refreshed periodically by the server; use `POST /admin/plugins/{id}/check-update` to force-refresh a single entry.

**Response**: `200 OK`

```json
{
  "last_refreshed_at": "2026-04-13T11:00:00Z",
  "plugins": [
    {
      "id": "steam",
      "name": "Steam",
      "description": "Import your Steam game library and playtime data.",
      "icon_url": "https://example.com/steam-icon.png",
      "latest_version": "1.2.0",
      "manifest_url": "https://example.com/plugins/steam/manifest.json"
    }
  ]
}
```

## Remove a plugin

```
DELETE /admin/plugins/{id}
```

Requires `plugins:delete`. Unregisters the plugin from the runtime and deletes its row from the `happyview_plugins` table. Secrets stay in `happyview_plugin_configs`, so they're reused if you reinstall.

**Response**: `204 No Content`. Returns `404 Not Found` if no plugin with that id is loaded.

## Reload a plugin

```
POST /admin/plugins/{id}/reload
```

Requires `plugins:create`. Re-fetches the plugin from its current source URL and re-registers it. Useful after publishing a new version of a plugin you host yourself.

The body is optional. To point the plugin at a new URL, pass:

```json
{ "url": "https://example.com/plugins/steam/manifest.json" }
```

When a new URL is provided, the stored `sha256` is cleared (the new version has its own hash). File-based plugins cannot be reloaded via this endpoint and return `400 Bad Request`.

**Response**: `200 OK` with the refreshed `PluginSummary`.

## Check for plugin updates

```
POST /admin/plugins/{id}/check-update
```

Requires `plugins:create`. Forces a cache refresh for one plugin from the official registry, then returns the updated `PluginSummary` with `update_available`, `latest_version`, and `pending_releases` reflecting the latest catalog state.

**Response**: `200 OK` with a `PluginSummary`.

## Get plugin secrets

```
GET /admin/plugins/{id}/secrets
```

Requires `plugins:read`. Returns the plugin's configured secrets with values masked (last 4 characters shown for values longer than 8 characters, otherwise fully masked). Requires `TOKEN_ENCRYPTION_KEY` to be configured.

**Response**: `200 OK`

```json
{
  "plugin_id": "steam",
  "secrets": {
    "PLUGIN_STEAM_API_KEY": "********ABCD"
  }
}
```

## Update plugin secrets

```
PUT /admin/plugins/{id}/secrets
```

Requires `plugins:create`. Encrypts the provided secret values with `TOKEN_ENCRYPTION_KEY` (AES-256-GCM) and upserts them into `happyview_plugin_configs`.

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/plugins/steam/secrets", {
  method: "PUT",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    secrets: {
      PLUGIN_STEAM_API_KEY: "your-new-api-key",
    },
  }),
});
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/plugins/steam/secrets", {
  method: "PUT",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    secrets: {
      PLUGIN_STEAM_API_KEY: "your-new-api-key",
    },
  }),
});
```
```rust tab="Rust" tab-group="language"
let response = client
    .put("http://127.0.0.1:3000/admin/plugins/steam/secrets")
    .bearer_auth(token)
    .json(&serde_json::json!({
        "secrets": {
            "PLUGIN_STEAM_API_KEY": "your-new-api-key"
        }
    }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "secrets": {
    "PLUGIN_STEAM_API_KEY": "your-new-api-key"
  }
}`)
req, _ := http.NewRequest("PUT", "http://127.0.0.1:3000/admin/plugins/steam/secrets", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X PUT http://127.0.0.1:3000/admin/plugins/steam/secrets \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{
    "secrets": {
      "PLUGIN_STEAM_API_KEY": "your-new-api-key"
    }
  }'
```

Special handling:

- Values starting with `********` are treated as masked placeholders and the existing encrypted value is preserved (so you can `GET` then `PUT` without re-typing every secret).
- Empty string values are not stored — use them to clear a secret.

**Response**: `204 No Content`

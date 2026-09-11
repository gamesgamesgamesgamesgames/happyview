---
title: "Plugins"
---

HappyView uses WASM plugins to extend its functionality. Plugins can integrate with external platforms, sync data to users' atproto identities, and more. Auth plugins — the first supported plugin type — enable users to link accounts from platforms like Steam, Xbox, itch.io, and others, then sync data like game libraries.

Official plugins for Steam, Xbox, itch.io, and other platforms are available in the [happyview-plugins](https://tangled.org/gamesgamesgamesgames.games/happyview-plugins) repository. Releases built before capability declarations, including earlier official plugin releases, are refused at install; pick a release whose manifest has `api_version` `"2"`.

## Installing Plugins

### Via Dashboard

1. Go to **Settings > Plugins**
2. Click **Add Plugin**
3. Enter the URL to a plugin's `manifest.json` or `.wasm` file
4. Review the plugin details and click **Install Plugin**
5. Configure any required secrets using the settings button

### Via Environment Variables

Set `PLUGIN_URLS` to load plugins at startup:

```
PLUGIN_URLS=steam|https://example.com/plugins/steam/manifest.json
```

Format: `id|url` or `id|url|sha256:hash` (comma-separated for multiple).

### Via File System

Place plugins in the `./plugins/` directory:

```
plugins/
  steam/
    manifest.json
    plugin.wasm
```

## Plugin Configuration

Plugins may require secrets (API keys, client credentials, etc.) to function. There are two ways to configure these:

### Dashboard Configuration

Click the settings icon next to a plugin to enter secrets. These are encrypted using AES-256-GCM and stored in the database.

**Requires:** `TOKEN_ENCRYPTION_KEY` environment variable (base64-encoded 32-byte key).

Generate one with:

```bash
openssl rand -base64 32
```

### Environment Variables

Set secrets as environment variables with the `PLUGIN_<ID>_` prefix:

```bash
PLUGIN_STEAM_API_KEY=your-api-key
PLUGIN_XBOX_CLIENT_ID=your-client-id
PLUGIN_XBOX_CLIENT_SECRET=your-client-secret
```

These are only necessary if you can't configure variables via the dashboard. Dashboard-configured secrets take precedence over environment variables.

## Security

- **Sandboxed execution**: Plugins run in isolated WASM environments
- **Limited host access**: Plugins can only call approved host functions (HTTP requests, KV storage, secrets, logging)
- **Encrypted storage**: OAuth tokens and secrets are encrypted at rest using AES-256-GCM
- **Scoped storage**: Plugin KV storage is isolated per-plugin and per-user
- **No filesystem access**: Plugins cannot access the host filesystem

## Next steps

- [Developing Plugins](developing-plugins.md) — create your own plugins with the WASM plugin API
- [Official plugins repository](https://tangled.org/gamesgamesgamesgames.games/happyview-plugins) — ready-to-use plugins for Steam, Xbox, itch.io, and more
- [API Keys](./api-keys.md) — authenticate programmatic access to admin endpoints
- [Permissions](./permissions.md) — configure user access to plugin management

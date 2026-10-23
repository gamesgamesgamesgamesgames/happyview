---
title: "Plugins"
---

HappyView uses WASM plugins to extend its functionality. Auth plugins let users link accounts from platforms like Steam, Xbox, and itch.io; library plugins expose functions to scripts.

Official plugins for Steam, Xbox, itch.io, and other platforms are built in the [plugins](https://github.com/happyproto/plugins) repository and published to the HappyProto plugin registry. Releases built before capability declarations, including earlier official plugin releases, are refused at install; pick a release whose manifest has `api_version` `"2"`.

## Installing Plugins

### From the registry

HappyView reads its plugin catalogue from a registry of `software.release` records, `https://api.happyproto.at` by default. Each plugin there is a project: a publisher's DID and a project name, such as `did:plc:mo7vq4jbioxwafy5zkcc3eux` and `happyview-lua`. The catalogue is refreshed every 15 minutes.

1. Go to **Settings > Plugins**
2. Click **Add plugin** and pick a plugin from the catalogue
3. Choose a channel, review the plugin details and the capabilities it asks for, and click **Install**
4. Configure any required secrets using the settings button

To install one without the dashboard, see [`POST /admin/plugins`](../api-reference/admin/plugins.md#install-a-plugin) with `registry`.

#### Channels

A project publishes up to three channels: `latest` for releases, and `beta` and `alpha` for prereleases. An install follows a channel, `latest` by default, and is offered an update whenever the channel moves to another version that may be installed. Pin an install to one version instead and it is never offered one. **Switch channel** on an installed plugin moves it to another channel's version and follows that channel from then on.

#### Lifecycle states

The publisher sets every version's state, and can change it after publishing:

| State | Installable | What it means |
|-------|-------------|---------------|
| `active` | yes | The normal state |
| `deprecated` | yes | Still works, but the publisher would rather you moved on; the dashboard shows their reason |
| `unavailable` | no | The publisher no longer distributes it |
| `retired` | no | The publisher no longer supports it |

A version is also refused when the registry doesn't hold its files, or when the registry has flagged it as suspect because its record changed after it was published. A project can be retired as a whole; the catalogue hides it from **Add plugin**.

Nothing turns a plugin off on its own. A plugin whose installed version is later retired keeps running, and the dashboard marks it in red with the publisher's reason. When a channel moves to a version that can't be installed, nothing is offered, and the dashboard says why.

#### Verified by CID

Each registry version names its manifest and its module by CID. HappyView fetches both from the registry and refuses either one whose bytes don't match its CID, naming both CIDs in the error. The CIDs are stored with the install, so a reload or a restart fetches the same bytes and checks them again; if the registry starts serving something else, the plugin isn't loaded. A registry install takes no `sha256`: its CIDs do that job.

#### Capabilities on update

Installing, updating or switching channel each show the capabilities the incoming version would be granted, and ask you to accept them before it replaces what is running. A version that asks for something you didn't accept is refused, and the installed version keeps running.

#### Publishers

An install records its project's publisher. Updates come only from that publisher, even when another publisher's project shares the plugin's name in the catalogue; to move a plugin to another publisher's project, remove it and add it again.

#### Turning the catalogue off

Set `PLUGIN_REGISTRY_URL` to `off`, or to an empty value, to turn the catalogue off. No registry is contacted, **Add plugin** offers only URL installs, and installed plugins are offered no updates. Point it at another URL to read another registry.

### From a URL

**Add plugin** also takes the URL of a plugin's `manifest.json` or `.wasm` file, for a plugin you host yourself or one that isn't in the registry. A URL install is checked against the `sha256` you give it, if any, and is offered updates only to newer versions of the catalogue's `latest` channel.

A plugin installed from a URL, by the dashboard or `PLUGIN_URLS`, is matched to its registry project at the next catalogue refresh: the project with the plugin's id, from its recorded publisher or else the official one. A matched plugin follows that project's `latest` channel from then on, and its next update installs from the registry by CID. One no project matches is marked **Unresolved** and keeps working as a URL install. Each plugin is matched once.

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

Set secrets as environment variables with the `PLUGIN_<ID>_` prefix, where `<ID>` is the plugin's id upper-cased with every non-alphanumeric character replaced by `_` — the plugin `auth-steam` reads `PLUGIN_AUTH_STEAM_API_KEY`:

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
- [Official plugins repository](https://github.com/happyproto/plugins) — the source of the plugins for Steam, Xbox, itch.io, and more
- [Plugins admin API](../api-reference/admin/plugins.md) — install, update and switch plugins by registry project
- [API Keys](./api-keys.md) — authenticate programmatic access to admin endpoints
- [Permissions](./permissions.md) — configure user access to plugin management

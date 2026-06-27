---
title: "Dashboard"
---

HappyView ships with a web dashboard that provides a visual interface for everything the [admin API](../api-reference/admin/admin-api.md) offers. It runs as a separate Next.js application alongside the Rust backend and authenticates via atproto OAuth.

On a fresh deployment with no users in the database, the first person to log in to the dashboard is automatically bootstrapped as the super user with all permissions — so log in with the handle you want to own the instance first.

## Data

### Lexicons

Navigate to **Lexicons** to see all uploaded lexicons. Each entry shows the NSID, type (record, query, procedure), and whether a Lua script is attached.

#### Adding a lexicon

Click **Add Lexicon** and choose **Local** or **Network**.

**Local** lexicons are defined by you. The editor shows two side-by-side panels (stacked on mobile):

- **Lexicon JSON** (left): define your lexicon schema
- **Lua Script** (right): write the handler for query/procedure types

The Lua panel only appears when the lexicon's `defs.main.type` is `query` or `procedure`. For record-type lexicons, only the JSON panel is shown.

HappyView generates a default Lua script when you first set the type to query or procedure. The template updates when the type changes, but once you edit the script your changes are preserved.

Toggle **Enable backfill** to index historical records when uploading a record-type lexicon.

**Network** lexicons are fetched from the atproto network. Enter an NSID (e.g. `xyz.statusphere.status`) and HappyView resolves the schema automatically. If found, the lexicon JSON is displayed in a read-only editor. Click **Add** to track it. Network lexicons are kept up to date via the Jetstream subscription. See [Lexicons - Network lexicons](../guides/lexicons.md#network-lexicons) for how resolution works.

#### JSON editor

The JSON editor provides real-time validation against the atproto Lexicon v1 schema:

- Validation for Lexicon format
- Auto-complete for definition types (`record`, `query`, `procedure`, `subscription`), property types (`string`, `integer`, `boolean`, `ref`, `union`, `blob`, `cid-link`, etc.), and schema structure (`defs`, `main`, `properties`, `required`)
- Enforces the required top-level shape: `lexicon`, `id`, and `defs.main`

#### Lua editor

The Lua editor provides context-aware code completions, including suggestions for the `Record`, `db`, `input`, and `params` APIs as well as Lua keywords, builtins, and standard library functions. It also has snippets for `if`, `for`, `function`, etc.

See [Lua Scripting](../guides/lua-scripting.md) for the full runtime reference and examples.

### Records

Navigate to **Records** to browse all indexed atproto records. Records are grouped by collection and searchable. Each record shows its AT URI, author DID, and the raw record JSON.

### Backfill

Navigate to **Backfill** to view and manage backfill jobs. You can start a new backfill for any record-type lexicon to import historical records from the network. The table shows each job's collection, DID scope, current stage, and start time. Click a row to open a detail sheet with full metadata and a stage-by-stage progress log that updates in real time. Running jobs can be cancelled from the detail sheet — the job transitions to "cancelling" while the worker finishes its current batch, then to "cancelled". See [Backfill](../guides/backfill.md) for how the process works.

### Dead Letters

Navigate to **Dead Letters** to view records that failed to index. Each entry shows the AT URI, error reason, and the raw record payload. You can retry, reindex, or dismiss individual dead letters, or use bulk actions to handle many at once. The sidebar badge shows the count of unresolved dead letters.

## Access

### Users

Navigate to **Users** to manage who can access the admin API and dashboard. You can add users by DID, assign permissions individually or via a template (`viewer`, `operator`, `manager`, `full_access`), and remove users. The super user is highlighted and has all permissions by default. See [Permissions](../guides/permissions.md) for what each permission grants.

### API Keys

Create and revoke admin API keys for automation. Each key is scoped to specific permissions and tied to the creating user. See [API Keys](../guides/api-keys.md) for details.

### API Clients

Register and manage third-party API clients. Each client gets an `hvc_…` client key and `hvs_…` client secret. You can configure the client type (confidential or public), allowed origins, scopes, and per-client rate limits. See [Authentication — API client identification](authentication.md#xrpc-api-client-identification) for how clients are used.

## Integrations

### Plugins

Manage installed plugins and configure plugin secrets. Plugins extend HappyView with additional functionality. Plugin secrets are encrypted at rest when `TOKEN_ENCRYPTION_KEY` is configured. See [Plugins](../guides/plugins.md) for details.

### Labelers

Configure labeler subscriptions for content labeling. See [Labelers](../guides/labelers.md) for details.

## System

### General

Configure instance-level settings: application name, logo, terms of service URL, and privacy policy URL. These values appear on OAuth authorization screens and can also be set via environment variables — dashboard values take precedence.

### XRPC Proxy

Control which unrecognized XRPC methods are forwarded to their resolved authority. Choose from four modes: **Disabled** (block all proxy requests), **Open** (proxy everything — the default), **Allowlist** (only proxy NSIDs matching your patterns), or **Blocklist** (proxy everything except matching patterns). Allowlist and blocklist modes accept NSID patterns with trailing wildcards (e.g. `com.example.*`). Locally registered lexicons are always served regardless of this setting. See [XRPC Proxy](../api-reference/admin/xrpc-proxy.md) for the full API reference.

### ENV Variables

View the current values of all environment variables that affect HappyView's behavior. This is a read-only view — values are set via your deployment environment, not the dashboard.

### Event Logs

View the audit log of admin actions. Events include user creation, lexicon uploads, permission changes, backfill starts, and more. Each entry shows the event type, severity, actor, subject, and timestamp. Events are retained for the number of days configured by `EVENT_LOG_RETENTION_DAYS` (default 30).

### Service Identity

Configure the AT Protocol service identity for your HappyView instance — either a `did:web` derived from your public URL, a `did:plc` you control, or a linked atproto account. This determines the DID that signs service-level interactions on the network.

### Experiments

Toggle experimental feature flags for your instance. Flags like `feature.spaces_enabled` can be enabled here before they are promoted to stable configuration options.

### Scripts

Manage script variables that are injected into Lua scripts at runtime. Variables defined here are available to all scripts and can be used to store shared configuration without hardcoding values in individual scripts.

## About

The **About** page shows the current HappyView version and instance configuration: public URL, database backend, Jetstream URL, relay URL, and PLC directory URL.

## Next steps

- [Lexicons](../guides/lexicons.md) — how lexicons drive HappyView's indexing and routing
- [Lua Scripting](../guides/lua-scripting.md) — write custom query and procedure logic
- [Permissions](../guides/permissions.md) — manage user access to admin features
- [Configuration](configuration.md) — full list of environment variables

---
title: "Instance Settings"
---

Instance settings override environment variables at runtime — things like app name, ToS URL, privacy policy URL, and logo. Settings stored here take precedence over their env var equivalents. All endpoints require the `settings:manage` permission.

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

## List settings

```
GET /admin/settings
```

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/settings", {
  headers,
});
const data = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/settings", {
  headers,
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("http://127.0.0.1:3000/admin/settings")
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/settings", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/settings -H "$AUTH"
```

Returns all key/value pairs stored in the `instance_settings` table, plus any env-var fallback values for keys not stored in the database. Each entry includes a `source` field: `"database"` for stored values, `"env"` for env-var fallbacks.

### Known settings

| Key | Env var | Default | Description |
|-----|---------|---------|-------------|
| `app_name` | `APP_NAME` | --- | Application name shown in sidebar and OAuth consent screen |
| `client_uri` | `CLIENT_URI` | --- | Public URL for this instance, linked from OAuth consent screen |
| `logo_uri` | `LOGO_URI` | --- | External URL to a logo image |
| `tos_uri` | `TOS_URI` | --- | Link to terms of service |
| `policy_uri` | `POLICY_URI` | --- | Link to privacy policy |
| `backfill_concurrent_pds` | `BACKFILL_CONCURRENT_PDS` | `10` | How many PDS servers to fetch from simultaneously during backfill |
| `backfill_concurrent_dids_per_pds` | `BACKFILL_CONCURRENT_DIDS_PER_PDS` | `3` | How many repos to fetch concurrently from each PDS |
| `backfill_concurrent_resolution` | `BACKFILL_CONCURRENT_RESOLUTION` | `100` | How many DID document lookups to run in parallel during PDS resolution |
| `backfill_retention_days` | `BACKFILL_RETENTION_DAYS` | `28` | Days to keep per-repo detail data from completed backfill jobs. `0` = keep indefinitely |
| `verbose_event_logging` | `VERBOSE_EVENT_LOGGING` | `false` | Log every record index, hook execution, and hook skip to the event log. High write volume — recommended only for debugging |
| `feature.spaces_enabled` | `FEATURE_SPACES_ENABLED` | --- | Enables the experimental Permissioned Spaces API. When `"true"`, space endpoints are available. When absent or any other value, space endpoints return `404 FeatureDisabled` |

## Upsert a setting

```
PUT /admin/settings/{key}
```

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/settings/app_name", {
  method: "PUT",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({ value: "My HappyView" }),
});
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/settings/app_name", {
  method: "PUT",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({ value: "My HappyView" }),
});
```
```rust tab="Rust" tab-group="language"
let response = client
    .put("http://127.0.0.1:3000/admin/settings/app_name")
    .bearer_auth(token)
    .json(&serde_json::json!({ "value": "My HappyView" }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{ "value": "My HappyView" }`)
req, _ := http.NewRequest("PUT", "http://127.0.0.1:3000/admin/settings/app_name", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X PUT http://127.0.0.1:3000/admin/settings/app_name \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{ "value": "My HappyView" }'
```

## Delete a setting

```
DELETE /admin/settings/{key}
```

Removes the override; the corresponding environment variable (if any) takes effect again.

## Database info

```
GET /admin/settings/db-info
```

Returns database backend, connection pool sizes, and whether a server restart is recommended to resize the backfill pool for current concurrency settings.

```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/settings/db-info -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "backend": "sqlite",
  "server_max_connections": null,
  "main_pool_size": 32,
  "backfill_pool_size": 64,
  "restart_recommended": false
}
```

| Field | Type | Description |
|-------|------|-------------|
| `backend` | string | `"sqlite"` or `"postgres"` |
| `server_max_connections` | number \| null | Postgres `max_connections` setting. `null` for SQLite |
| `main_pool_size` | number | Current main connection pool size |
| `backfill_pool_size` | number | Current backfill connection pool size |
| `restart_recommended` | boolean | `true` if concurrency settings have changed and a restart would resize the pool |

## Upload / delete logo

```
PUT /admin/settings/logo
DELETE /admin/settings/logo
```

`PUT` accepts a binary image body and stores it as the instance logo (served via the public dashboard). `DELETE` removes the stored logo.

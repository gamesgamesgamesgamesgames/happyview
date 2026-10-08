---
title: "Event Logs"
---

HappyView maintains an internal event log that records system activity — lexicon changes, record operations, Lua script executions and errors, user actions, API key events, backfill jobs, and Jetstream connectivity. Events are stored in the database and queryable via the [admin API](../api-reference/admin/events.md).

## Event types

Events follow a `category.action` naming convention. Each event has a severity level (`info`, `warn`, or `error`), an optional `actor_did` (the user who triggered it), an optional `subject` (what was affected), and a `detail` JSON object with event-specific data.

### Lexicon events

| Event Type        | Severity | Subject      | Detail                             |
| ----------------- | -------- | ------------ | ---------------------------------- |
| `lexicon.created` | info     | Lexicon NSID | `revision`, `has_script`, `source` |
| `lexicon.updated` | info     | Lexicon NSID | `revision`, `has_script`, `source` |
| `lexicon.deleted` | info     | Lexicon NSID | —                                  |

Logged when lexicons are uploaded, updated, or deleted via the [admin API](../api-reference/admin/lexicons.md). The `actor_did` is the user who performed the action.

### Record events

| Event Type       | Severity | Subject       | Detail                                |
| ---------------- | -------- | ------------- | ------------------------------------- |
| `record.created` | info     | Record AT URI | `collection`, `did`, `rkey`           |
| `record.deleted` | info     | Record AT URI | `collection`, `did`, `rkey`           |
| `record.skipped` | info     | Record AT URI | `collection`, `did`, `rkey`, `reason` |

Logged when records are received from Jetstream and stored or removed from the local database, or skipped because a record script returned `nil`. These are system-triggered events (`actor_did` is null). They write one row per record, so successful operations are only logged when [`verbose_event_logging`](../api-reference/admin/settings.md) is on. If a database error occurs, the same event type is logged with `error` severity regardless of that setting, and the error message is included in the detail.

### Script events

| Event Type        | Severity | Subject     | Detail                                                                     |
| ----------------- | -------- | ----------- | -------------------------------------------------------------------------- |
| `script.executed` | info     | Method NSID | `method`, `caller_did`, `duration_ms`, `response_size`, `input`, `response` |
| `script.error`    | error    | Method NSID | `error`, `script_source`, `input`, `caller_did`, `method`                  |

Logged when Lua scripts run for XRPC query or procedure endpoints. `script.executed` is written on every call and stores the full request and response, so it is only logged when [`verbose_event_logging`](../api-reference/admin/settings.md) is on. `script.error` is always logged. Script errors capture the full context needed to reproduce and debug the issue: the error message, the complete Lua script source, the input that triggered it, and the caller's DID.

<Callout type="info">
For query scripts (unauthenticated), `caller_did` is omitted from the detail and `input` is replaced by `params`, since queries don't have an authenticated user or request body.
</Callout>

### User events

| Event Type                 | Severity | Subject               | Detail               |
| -------------------------- | -------- | --------------------- | -------------------- |
| `user.created`             | info     | New user DID          | `template` (if used) |
| `user.deleted`             | info     | Removed user ID       | —                    |
| `user.bootstrapped`        | info     | Bootstrapped user DID | —                    |
| `user.permissions_updated` | info     | User ID               | `granted`, `revoked` |
| `user.super_transferred`   | warn     | New super user ID     | `from_user_id`       |

The `user.bootstrapped` event is logged when the first user is auto-promoted to super user (see [Auth - Auto-bootstrap](../api-reference/admin/admin-api.md#auth)).

### Auth events

| Event Type               | Severity | Subject       | Detail                           |
| ------------------------ | -------- | ------------- | -------------------------------- |
| `auth.permission_denied` | error    | Endpoint path | `required_permission`, `user_id` |

Logged when a user attempts to access an endpoint they don't have permission for.

### Space events

| Event Type                | Severity | Subject                                                           | Detail                                                                  |
| -------------------------- | -------- | -------------------------------------------------------------------- | -------------------------------------------------------------------------- |
| `space.access_granted`     | info     | Space AT URI for a space grant, account DID for an account grant     | `grant_id`, `scope`, `target`, `reason`, `expires_at`, `user_id`           |
| `space.access_revoked`     | info     | Same as `space.access_granted`; the raw space id if the space was deleted | `grant_id`, `revoked_by`, `user_id`                                        |
| `space.moderator_read`     | info     | Space AT URI, or the account DID for an account-records read         | `action`, `grant_id`, `scope`, `user_id`, and what was read (see below)   |
| `space_inspector.enabled`  | warn     | Setting key                                                           | —                                                                            |
| `space_inspector.disabled` | warn     | Setting key                                                           | —                                                                            |

Logged when a moderator requests, uses, or ends access to a space's contents through the [space inspector](../api-reference/admin/spaces.md#access-grants). `space.access_granted` and `space.access_revoked` record the request and the end of a grant; `space_inspector.enabled` and `space_inspector.disabled` record the inspector being turned on or off for the instance (see [Configuration](../getting-started/configuration.md)).

`action` on `space.moderator_read` is `list_records`, `get_blob`, or `list_account_records`, and every such event carries the `grant_id` and `scope` of the grant that allowed it. A `list_records` event includes the `repo` and `collection` filters and the `uris` returned. A `get_blob` event includes the blob `cid` and the `repo` that references it. A `list_account_records` event includes the `repo`, the `space_id` filter if given, the `collection` filter, and the `uris` returned. Listing spaces and viewing their metadata or members is not logged.

### API Key events

| Event Type        | Severity | Subject | Detail                |
| ----------------- | -------- | ------- | --------------------- |
| `api_key.created` | info     | Key ID  | `name`, `permissions` |
| `api_key.revoked` | info     | Key ID  | `name`                |

### Script Variable events

| Event Type                 | Severity | Subject      | Detail |
| -------------------------- | -------- | ------------ | ------ |
| `script_variable.upserted` | info     | Variable key | —      |
| `script_variable.deleted`  | info     | Variable key | —      |

### Hook events

| Event Type             | Severity | Subject                | Detail                                        |
| ---------------------- | -------- | ---------------------- | --------------------------------------------- |
| `script.executed`      | info     | Record or label AT URI | `host_kind`, `host_id`, `trigger`, `attempts` |
| `script.dead_lettered` | error    | Record or label AT URI | `host_kind`, `host_id`, `trigger`, `error`    |

Logged when [record/label scripts](./record-scripts) run. `script.executed` fires once per record or label that reaches a script, so it is only logged when [`verbose_event_logging`](../api-reference/admin/settings.md) is on. Dead-lettered events indicate a script failed all retry attempts. You can manage dead letters from the **Data > Dead Letters** page in the dashboard — see [Dead Letters](#dead-letters) below.

### Backfill events

| Event Type           | Severity | Subject         | Detail                  |
| -------------------- | -------- | --------------- | ----------------------- |
| `backfill.started`   | info     | Collection NSID | `job_id`                |
| `backfill.completed` | info     | Collection NSID | `job_id`, `total_repos` |
| `backfill.failed`    | error    | Collection NSID | `job_id`, `error`       |

See [Backfill](./backfill.md) for background on backfill jobs.

### Audit log events

| Event Type                     | Severity | Subject                                                         | Detail        |
| --------------------------------- | -------- | ------------------------------------------------------------------ | --------------- |
| `event_logs.retention_changed`    | warn     | Setting key (`event_log_retention_days` or `space_access_log_retention_days`) | `from`, `to` |

Logged when a retention setting's effective value changes, env fallback included. An unset or unparseable value counts as its default: 30 days for `event_log_retention_days`, and for `space_access_log_retention_days` 0 (keep forever) while `event_log_retention_days` is 0 and 365 otherwise. Saving a setting back to its current or default value logs nothing. Because protected retention can follow the general setting, one change can log an event for each key. `from` and `to` are the effective day counts. See [Protected events](#protected-events) below.

### Jetstream events

| Event Type               | Severity | Subject | Detail   |
| ------------------------ | -------- | ------- | -------- |
| `jetstream.connected`    | info     | —       | `url`    |
| `jetstream.disconnected` | warn     | —       | `reason` |

Logged when the WebSocket connection to [Jetstream](https://github.com/bluesky-social/jetstream) is established or lost.

## Querying events

Use the admin API to query event logs with filters:

```ts tab="TypeScript" tab-group="language"
const TOKEN = "hv_..."; // your API key
const headers = { Authorization: `Bearer ${TOKEN}` };

interface Event {
  id: string;
  event_type: string;
  severity: string;
  actor_did?: string;
  subject?: string;
  detail: Record<string, unknown>;
  created_at: string;
}

interface EventsResponse {
  events: Event[];
  cursor?: string;
}

// Get all errors
const errors: EventsResponse = await fetch(
  "http://127.0.0.1:3000/admin/events?severity=error",
  { headers },
).then((r) => r.json());

// Get script errors for a specific lexicon
const scriptErrors: EventsResponse = await fetch(
  "http://127.0.0.1:3000/admin/events?event_type=script.error&subject=com.example.feed.like",
  { headers },
).then((r) => r.json());

// Get all lexicon-related events
const lexiconEvents: EventsResponse = await fetch(
  "http://127.0.0.1:3000/admin/events?category=lexicon",
  { headers },
).then((r) => r.json());

// Paginate through results
const page: EventsResponse = await fetch(
  "http://127.0.0.1:3000/admin/events?limit=20&cursor=2026-03-01T11:59:00Z",
  { headers },
).then((r) => r.json());
```

```js tab="JavaScript" tab-group="language"
const TOKEN = "hv_..."; // your API key
const headers = { Authorization: `Bearer ${TOKEN}` };

// Get all errors
const errors = await fetch(
  "http://127.0.0.1:3000/admin/events?severity=error",
  { headers },
).then((r) => r.json());

// Get script errors for a specific lexicon
const scriptErrors = await fetch(
  "http://127.0.0.1:3000/admin/events?event_type=script.error&subject=com.example.feed.like",
  { headers },
).then((r) => r.json());

// Get all lexicon-related events
const lexiconEvents = await fetch(
  "http://127.0.0.1:3000/admin/events?category=lexicon",
  { headers },
).then((r) => r.json());

// Paginate through results
const page = await fetch(
  "http://127.0.0.1:3000/admin/events?limit=20&cursor=2026-03-01T11:59:00Z",
  { headers },
).then((r) => r.json());
```

```rust tab="Rust" tab-group="language"
let client = reqwest::Client::new();
let token = "hv_..."; // your API key

// Get all errors
let errors: serde_json::Value = client
    .get("http://127.0.0.1:3000/admin/events")
    .query(&[("severity", "error")])
    .bearer_auth(token)
    .send()
    .await?
    .json()
    .await?;

// Get script errors for a specific lexicon
let script_errors: serde_json::Value = client
    .get("http://127.0.0.1:3000/admin/events")
    .query(&[
        ("event_type", "script.error"),
        ("subject", "com.example.feed.like"),
    ])
    .bearer_auth(token)
    .send()
    .await?
    .json()
    .await?;

// Get all lexicon-related events
let lexicon_events: serde_json::Value = client
    .get("http://127.0.0.1:3000/admin/events")
    .query(&[("category", "lexicon")])
    .bearer_auth(token)
    .send()
    .await?
    .json()
    .await?;

// Paginate through results
let page: serde_json::Value = client
    .get("http://127.0.0.1:3000/admin/events")
    .query(&[("limit", "20"), ("cursor", "2026-03-01T11:59:00Z")])
    .bearer_auth(token)
    .send()
    .await?
    .json()
    .await?;
```

```go tab="Go" tab-group="language"
token := "hv_..." // your API key

// Get all errors
req, _ := http.NewRequest("GET",
	"http://127.0.0.1:3000/admin/events?severity=error", nil)
req.Header.Set("Authorization", "Bearer "+token)
errors, err := http.DefaultClient.Do(req)

// Get script errors for a specific lexicon
req, _ = http.NewRequest("GET",
	"http://127.0.0.1:3000/admin/events?event_type=script.error&subject=com.example.feed.like", nil)
req.Header.Set("Authorization", "Bearer "+token)
scriptErrors, err := http.DefaultClient.Do(req)

// Get all lexicon-related events
req, _ = http.NewRequest("GET",
	"http://127.0.0.1:3000/admin/events?category=lexicon", nil)
req.Header.Set("Authorization", "Bearer "+token)
lexiconEvents, err := http.DefaultClient.Do(req)

// Paginate through results
req, _ = http.NewRequest("GET",
	"http://127.0.0.1:3000/admin/events?limit=20&cursor=2026-03-01T11:59:00Z", nil)
req.Header.Set("Authorization", "Bearer "+token)
page, err := http.DefaultClient.Do(req)
```

```sh tab="cURL" tab-group="language"
AUTH="Authorization: Bearer hv_..." # your API key

# Get all errors
curl "http://127.0.0.1:3000/admin/events?severity=error" -H "$AUTH"

# Get script errors for a specific lexicon
curl "http://127.0.0.1:3000/admin/events?event_type=script.error&subject=com.example.feed.like" -H "$AUTH"

# Get all lexicon-related events
curl "http://127.0.0.1:3000/admin/events?category=lexicon" -H "$AUTH"

# Paginate through results
curl "http://127.0.0.1:3000/admin/events?limit=20&cursor=2026-03-01T11:59:00Z" -H "$AUTH"
```

See the [Admin API reference](../api-reference/admin/events.md#list-event-logs) for full parameter documentation.

## Retention

Event logs are automatically cleaned up based on the `EVENT_LOG_RETENTION_DAYS` environment variable (default: 30 days). A background task runs hourly to delete events older than the configured retention period.

Set `EVENT_LOG_RETENTION_DAYS=0` to disable automatic cleanup and keep logs indefinitely.

See [Configuration](../getting-started/configuration.md) for all environment variables.

## Protected events

Some event types record access to private space data, or changes to the audit trail itself, and can't be purged by hand:

- `space.access_granted`
- `space.access_revoked`
- `space.moderator_read`
- `space_inspector.enabled`
- `space_inspector.disabled`
- `event_logs.purged`
- `event_logs.retention_changed`

`POST /admin/events/purge` excludes them: a filter naming one of these types, or a category that contains one, returns `400 Bad Request`. `GET /admin/events/count`, the purge preview, never counts them either.

They're removed only on their own schedule, set by `space_access_log_retention_days` (default `365`; `0` keeps them forever; when unset it follows an `event_log_retention_days` of `0`). The events tied to an access grant that still exists, meaning its grant and revocation events and the reads made under it, are kept whatever their age, so a grant's reason and reads last as long as the grant does. Other protected events keep their own schedule. The same schedule deletes access grants once they have been expired or revoked for that long. See [Configuration](../getting-started/configuration.md).

Protected events live in the same database table as every other event log row. This is a safeguard against a routine or accidental purge, not a tamper-proof audit log — anyone with direct access to the database can still delete them.

## Dead Letters

When a record or label script fails after all retry attempts, the event is stored in the dead letters queue. You can manage dead letters from the **Data > Dead Letters** page in the dashboard.

From the dead letters page you can:

- **Retry Script** — replay the stored event through the script (use after fixing the script)
- **Re-index** — fetch the record fresh from the PDS and run it through the full indexing pipeline (use when the record may have changed)
- **Dismiss** — mark the dead letter as resolved without retrying

Bulk actions are available for selected rows or all entries matching the current filters.

## Next steps

- [Admin API — Event Logs](../api-reference/admin/events.md) — full query parameters and response format
- [Permissions](permissions.md) — control which users can read event logs
- [Troubleshooting](../reference/troubleshooting.md) — using event logs to diagnose issues

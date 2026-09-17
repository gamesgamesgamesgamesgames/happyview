---
title: "Event Logs"
---

HappyView logs system events — lexicon changes, record operations, script errors, user actions, and more. See the [Event Logs guide](../../guides/event-logs.md) for details on event types and retention.

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

## List event logs

```
GET /admin/events
```

```ts tab="TypeScript" tab-group="language"
interface EventLog {
  id: string;
  event_type: string;
  severity: string;
  actor_did: string;
  subject: string;
  detail: Record<string, unknown>;
  created_at: string;
}

interface EventLogResponse {
  events: EventLog[];
  cursor: string | null;
}

const response = await fetch(
  "http://127.0.0.1:3000/admin/events?severity=error&limit=10",
  { headers },
);
const data: EventLogResponse = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/events?severity=error&limit=10",
  { headers },
);
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let client = reqwest::Client::new();
let response = client
    .get("http://127.0.0.1:3000/admin/events")
    .query(&[("severity", "error"), ("limit", "10")])
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/events?severity=error&limit=10", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl "http://127.0.0.1:3000/admin/events?severity=error&limit=10" -H "$AUTH"
```

| Param        | Type   | Required | Description                                                           |
| ------------ | ------ | -------- | --------------------------------------------------------------------- |
| `event_type` | string | no       | Filter by exact event type (e.g. `script.error`)                      |
| `category`   | string | no       | Filter by category prefix (e.g. `lexicon` matches all lexicon events) |
| `severity`   | string | no       | Filter by severity: `info`, `warn`, or `error`                        |
| `subject`    | string | no       | Filter by subject (lexicon ID, record URI, admin DID, etc.)           |
| `cursor`     | string | no       | Pagination cursor (ISO 8601 timestamp from previous response)         |
| `limit`      | number | no       | Results per page (default `50`, max `100`)                            |

**Response**: `200 OK`

```json
{
  "events": [
    {
      "id": "550e8400-e29b-41d4-a716-446655440000",
      "event_type": "script.error",
      "severity": "error",
      "actor_did": "did:plc:abc123",
      "subject": "com.example.feed.like",
      "detail": {
        "error": "attempt to index nil value",
        "script_source": "function handle(input, ctx) ... end",
        "input": { "status": "hello" },
        "caller_did": "did:plc:abc123",
        "method": "com.example.feed.like"
      },
      "created_at": "2026-03-01T12:00:00Z"
    }
  ],
  "cursor": "2026-03-01T11:59:00Z"
}
```

Events are returned in reverse chronological order (newest first). Pass the `cursor` value from the response to fetch the next page.

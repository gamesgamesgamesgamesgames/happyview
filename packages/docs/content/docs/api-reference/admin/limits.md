---
title: "Plan limits"
---

What the instance's hosting plan allows. A managed-hosting provider sets these from the instance's plan with the `INSTANCE_LIMIT_*` variables (see [Configuration](../../getting-started/configuration.md)). A self-hosted instance has none set, so every limit is unlimited.

| Limit | Variable | Counts |
| ----- | -------- | ------ |
| `seats` | `INSTANCE_LIMIT_SEATS` | Admin users, the super user included |
| `api_clients` | `INSTANCE_LIMIT_API_CLIENTS` | Active API clients, not counting child clients |
| `log_retention_days` | `INSTANCE_LIMIT_LOG_RETENTION_DAYS` | The longest event logs may be kept, in days |

The limits are read when the instance starts. A plan change restarts a managed instance, so new limits take effect with it.

## Get the limits

```
GET /admin/limits
```

Any signed-in admin may read them. A `max` of `null` is unlimited.

```json
{
  "seats": { "max": 5, "used": 2 },
  "api_clients": { "max": 3, "used": 1 },
  "log_retention_days": { "max": 7, "configured": 30 }
}
```

- `used` can be above `max`: a plan moved down keeps what already exists.
- `configured` is the event log retention the instance is set to, from **Settings → General** or `EVENT_LOG_RETENTION_DAYS`; `0` keeps logs forever.

## When a limit is reached

A route that enforces a limit refuses a request that would pass it with `403`:

```json
{
  "error": "LimitReached",
  "limit": "seats",
  "max": 5,
  "current": 5,
  "message": "this instance's plan allows 5 seats"
}
```

`limit` uses the same names as `GET /admin/limits`. Switch on `error`, not on the message.

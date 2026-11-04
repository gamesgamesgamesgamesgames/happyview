---
title: "Instance limits"
---

Whoever hosts a HappyView instance can limit what it holds: how many admins, how many API clients, whether XRPC answers callers without an API client, and how long logs are kept. A self-hosted instance has no limits unless you set some, so if you run HappyView yourself and set nothing, nothing on this page changes anything for you.

Limits are checked inside the instance, against limits it already holds. A check never calls out to the host, so an instance keeps answering while whatever set its limits is down.

## What each limit refuses

- **`seats`:** adding an admin through the admin API (`POST /admin/users`) when the instance already has `max` admins, the super user included. The owner's own ways in are never refused: the first sign-in, the configured superadmin, and a host setting its owner.
- **`api_clients`:** creating an API client through the admin API (`POST /admin/api-clients`), or turning an inactive one back on, when the instance already has `max` active ones. Only the clients the instance's admins register count: not clients an app registers under its own, nor hpy, which every instance has built in.
- **`anonymous_xrpc`:** with `allowed: false`, a call to a lexicon's query or procedure, or one proxied onward, from a caller who isn't signed in and has no API client that checks out (none, an unknown key, or the wrong Origin or secret). A signed-in caller is never refused, whatever their client key, nor is a call with another service's service auth. A space credential counts only on the space routes, which verify it and keep their own access rules; the other XRPC methods treat a call that carries only a space credential as anonymous. HappyView's built-in XRPC routes, such as `app.bsky.actor.getProfile`, `com.atproto.repo.uploadBlob` and `dev.happyview.*`, need a signed-in caller anyway. The admin API, OAuth and `/health` are never affected.
- **`log_retention_days`:** a ceiling on `event_log_retention_days` and `backfill_retention_days`. Setting either above `max`, or to `0` (forever), is refused. Whatever they're set to, the retention sweeps keep event logs, job logs, stored script metrics and backfill history for at most `max` days. The space access log isn't held to it: it's a security record, kept as its own setting says. Its `max` must be at least 1, since a retention of 0 means forever: a limits file or variable that sets 0 is refused, at boot or, for a changed file, by keeping the last good limits.

## The limits

| Limit | Kind | What it limits |
| --- | --- | --- |
| `seats` | a number | Admin users, the super user included |
| `api_clients` | a number | API clients that can call the instance |
| `anonymous_xrpc` | allowed or not | XRPC calls made without an API client |
| `log_retention_days` | a ceiling | How many days the retention settings can keep logs. A setting of `0`, which keeps logs forever, becomes the ceiling |

A limit that's left out is unlimited, or for `anonymous_xrpc`, allowed. Lowering a limit below what's already there takes nothing away: the instance keeps what it has, and refuses to add more.

## Setting them

Use one of these, not both. Boot stops if both are set.

### A limits file

Set `HAPPYVIEW_LIMITS_FILE` to the path of a JSON file:

```json
{
  "seats": {
    "max": 5,
    "message": "Your plan includes 5 seats.",
    "actionUrl": "https://host.example/billing",
    "actionLabel": "Add a seat"
  },
  "api_clients": { "max": 3 },
  "anonymous_xrpc": { "allowed": false },
  "log_retention_days": { "max": 30 }
}
```

- `max` is a whole number. `allowed` is `true` or `false`.
- `message`, `actionUrl` and `actionLabel` are optional, and are your words for when the limit is reached. Without a `message`, HappyView says its own sentence, such as "This instance allows 5 admins." An action is shown only when it has both a URL and a label, and the URL is `http` or `https`.
- An unknown key is refused, so a misspelt limit can't quietly read as unlimited.

HappyView reads the file at boot, and boot stops if it's missing or not valid. After that it looks at the file every 2 seconds and takes any change, so a limit can go up or down without a restart. If a changed file isn't valid, or the file goes away, the instance keeps the limits it last read and logs a warning.

### Environment variables

Without a file, `HAPPYVIEW_LIMIT_SEATS`, `HAPPYVIEW_LIMIT_API_CLIENTS`, `HAPPYVIEW_LIMIT_ANONYMOUS_XRPC` and `HAPPYVIEW_LIMIT_LOG_RETENTION_DAYS` set the limits (see [Configuration](../getting-started/configuration.md)). They're read once at boot, and carry no wording.

## When a limit is reached

The admin API and XRPC answer `403` with one shape:

```json
{
  "error": "LimitReached",
  "limit": "seats",
  "max": 5,
  "current": 5,
  "message": "Your plan includes 5 seats.",
  "actionUrl": "https://host.example/billing",
  "actionLabel": "Add a seat"
}
```

- `limit` is one of the names above.
- `max` and `current` are there for a number; `max` alone for a ceiling; neither for `anonymous_xrpc`.
- `message` is always there: your wording, or HappyView's.
- `actionUrl` and `actionLabel` come together, or not at all.

The dashboard shows the message, with the action as a button. `hpy` prints the message and the action's URL.

## Where the limits come from

The limits come from a `LimitsProvider`, a trait in the open `happyview-provider` crate, alongside the [record providers](record-providers.md)' contract. HappyView asks it for a snapshot each time it checks, so it answers from what it already holds. The file and the variables above are the provider HappyView ships with.

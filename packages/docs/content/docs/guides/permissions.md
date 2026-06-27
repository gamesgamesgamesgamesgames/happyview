---
title: "Permissions"
---

HappyView uses a granular permission system to control access to the admin API. Each user has a set of permissions that determine which endpoints they can access. Permissions can be assigned individually, via templates, or both.

## Permission list

HappyView defines 44 permissions organized by category:

### Lexicons

| Permission        | Description                          |
| ----------------- | ------------------------------------ |
| `lexicons:create` | Upload and register new lexicon schemas |
| `lexicons:read`   | View registered lexicon schemas      |
| `lexicons:delete` | Remove lexicon schemas               |

### Records

| Permission                  | Description                             |
| --------------------------- | --------------------------------------- |
| `records:read`              | Browse indexed AT Protocol records      |
| `records:delete`            | Delete individual records from the index |
| `records:delete-collection` | Bulk-delete all records in a collection |

### Scripts

| Permission       | Description                                       |
| ---------------- | ------------------------------------------------- |
| `scripts:read`   | View trigger-keyed scripts                        |
| `scripts:manage` | Create, update, and delete trigger-keyed scripts  |

### Script Variables

| Permission                | Description                                        |
| ------------------------- | -------------------------------------------------- |
| `script-variables:create` | Add or update environment variables for Lua scripts |
| `script-variables:read`   | View script environment variable keys and values   |
| `script-variables:delete` | Remove script environment variables                |

### Users

| Permission     | Description                            |
| -------------- | -------------------------------------- |
| `users:create` | Add new dashboard users                |
| `users:read`   | View the user list and their permissions |
| `users:update` | Modify user permissions                |
| `users:delete` | Remove dashboard users                 |

### API Keys

| Permission        | Description                              |
| ----------------- | ---------------------------------------- |
| `api-keys:create` | Generate new API keys for admin access   |
| `api-keys:read`   | View existing API keys                   |
| `api-keys:delete` | Revoke existing API keys                 |

### Backfill

| Permission        | Description                               |
| ----------------- | ----------------------------------------- |
| `backfill:create` | Trigger historical record backfill jobs   |
| `backfill:read`   | View backfill job status and progress     |

### Labelers

| Permission        | Description                           |
| ----------------- | ------------------------------------- |
| `labelers:create` | Subscribe to external labeler services |
| `labelers:read`   | View subscribed labeler services      |
| `labelers:delete` | Unsubscribe from labeler services     |

### Settings

| Permission        | Description                                         |
| ----------------- | --------------------------------------------------- |
| `settings:manage` | Modify instance settings, logo, and configuration   |

### Plugins

| Permission        | Description                                  |
| ----------------- | -------------------------------------------- |
| `plugins:read`    | View installed plugins and their configuration |
| `plugins:create`  | Install and configure new plugins            |
| `plugins:delete`  | Uninstall plugins                            |

### API Clients

| Permission           | Description                              |
| -------------------- | ---------------------------------------- |
| `api-clients:view`   | View registered OAuth API clients        |
| `api-clients:create` | Register new OAuth API clients           |
| `api-clients:edit`   | Modify API client settings and credentials |
| `api-clients:delete` | Remove registered API clients            |

### Dead Letters

| Permission            | Description                            |
| --------------------- | -------------------------------------- |
| `dead-letters:read`   | View failed hook executions            |
| `dead-letters:manage` | Retry, re-index, or dismiss dead letters |

### Spaces

| Permission                  | Description                                |
| --------------------------- | ------------------------------------------ |
| `spaces:create`             | Create new permissioned data spaces        |
| `spaces:read`               | View space details and metadata            |
| `spaces:update`             | Modify space settings                      |
| `spaces:delete`             | Remove spaces and their data               |
| `spaces:manage-members`     | Add or remove space members and roles      |
| `spaces:manage-invites`     | Create and revoke space invitations        |
| `spaces:manage-records`     | Read and write records within spaces       |
| `spaces:manage-credentials` | Issue and revoke space access credentials  |

### System

| Permission   | Description                              |
| ------------ | ---------------------------------------- |
| `stats:read` | View collection statistics and record counts |
| `events:read` | View the event log                      |

## Permission templates

Templates are predefined sets of permissions that simplify user creation. Pass a `template` value when creating a user via `POST /admin/users`.

### Viewer

Read-only access. Can browse lexicons, records, scripts, stats, events, dead letters, and user lists but cannot modify anything.

Includes: `lexicons:read`, `records:read`, `scripts:read`, `script-variables:read`, `users:read`, `api-keys:read`, `backfill:read`, `stats:read`, `events:read`, `dead-letters:read`

### Operator

Everything in Viewer, plus the ability to run backfill jobs, manage API keys, and manage dead letters.

Adds: `backfill:create`, `api-keys:create`, `api-keys:delete`, `dead-letters:manage`

### Manager

Everything in Operator, plus the ability to manage lexicons, records, scripts, labelers, settings, plugins, API clients, and spaces.

Adds: `lexicons:create`, `lexicons:delete`, `scripts:manage`, `script-variables:create`, `script-variables:delete`, `records:delete`, `labelers:create`, `labelers:read`, `labelers:delete`, `settings:manage`, `plugins:read`, `plugins:create`, `plugins:delete`, `api-clients:view`, `api-clients:create`, `api-clients:edit`, `api-clients:delete`, `spaces:create`, `spaces:read`, `spaces:update`, `spaces:delete`, `spaces:manage-members`, `spaces:manage-invites`, `spaces:manage-records`, `spaces:manage-credentials`

### Full Access

All 44 permissions. Equivalent to granting every permission individually (but still not a super user).

## Super user

The super user is a special user created automatically when the first person logs in to a fresh HappyView instance. The super user:

- Has unrestricted access to all endpoints, regardless of which permissions are assigned
- Is the only user who can call `POST /admin/users/transfer-super`
- Cannot be deleted
- Cannot have their permissions modified by other users

There is always exactly one super user. Super status can be transferred to another user via the dashboard or transfer endpoint in the Admin API.

## Escalation guards

HappyView prevents privilege escalation:

- When creating a user or API key, you can only grant permissions that you yourself have. Attempting to grant a permission you lack returns `403 Forbidden`.
- When updating a user's permissions, the same rule applies — you cannot grant permissions beyond your own.

## Self-modification guards

Users cannot modify their own account in destructive ways:

- You cannot delete yourself
- You cannot revoke your own permissions

These guards prevent accidental lockout.

## API key permissions

API keys have their own set of permissions, specified at creation time. The effective permissions of an API key are the **intersection** of:

1. The permissions assigned to the key
2. The permissions of the user who owns the key

This means if a user's permissions are later reduced, any API keys they created are also effectively reduced — even though the key's own permission list doesn't change.

For example, if a user with `lexicons:create` and `lexicons:read` creates a key with both permissions, and the user later loses `lexicons:create`, the key can only use `lexicons:read`.

## Managing permissions

### Via the dashboard

Go to **Settings > Users** to view and manage user permissions. Click on a user to see their current permissions and modify them. You can also assign templates when creating new users.

### Via the API

- `POST /admin/users` — create a user with a template or explicit permissions
- `PATCH /admin/users/{id}/permissions` — grant or revoke individual permissions
- `POST /admin/users/transfer-super` — transfer super user status (super user only)

See the [Admin API — Users](../api-reference/admin/users.md) for full details.

## Next steps

- [Admin API reference](../api-reference/admin/admin-api.md) — endpoint documentation with required permissions
- [API Keys](api-keys.md) — creating scoped API keys
- [Event Logs](event-logs.md) — permission-denied events are logged for auditing

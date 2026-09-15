---
title: "API Keys"
---

API keys let you authenticate with the admin API without going through the OAuth flow. They're useful for CI/CD pipelines, scripts, and any automation that needs to manage your HappyView instance programmatically.

## How API keys work

Each API key is a 35-character token with the format `hv_` followed by 32 random hex characters:

```
hv_a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4
```

When you create a key, HappyView shows the full token **once**. After that, only the prefix (`hv_a1b2c3d4`) is stored for display — the key itself is SHA-256 hashed before being saved to the database. This means nobody (including you) can retrieve the full key after creation.

Each API key has its own set of **scoped permissions**. When you create a key, you specify which permissions it should have. The key's effective permissions are the **intersection** of the permissions assigned to the key and the permissions of the user who created it — a key can never have more access than its creator. A revoked key immediately stops working.

## Creating a key

1. Go to **Settings > API Keys** in the dashboard sidebar
2. Click **Create API Key**
3. Enter a descriptive name (e.g., "CI Deploy", "Monitoring Script")
4. Select the permissions the key should have
5. Copy the full key from the confirmation dialog — you won't see it again

## Using a key

Pass the key as a Bearer token in the `Authorization` header:

```ts tab="TypeScript" tab-group="language"
const TOKEN = "hv_a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4";

const response = await fetch("http://127.0.0.1:3000/admin/lexicons", {
  headers: { Authorization: `Bearer ${TOKEN}` },
});

interface LexiconsResponse {
  lexicons: Array<{
    id: string;
    nsid: string;
    revision: number;
  }>;
}

const data: LexiconsResponse = await response.json();
```

```js tab="JavaScript" tab-group="language"
const TOKEN = "hv_a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4";

const response = await fetch("http://127.0.0.1:3000/admin/lexicons", {
  headers: { Authorization: `Bearer ${TOKEN}` },
});

const data = await response.json();
```

```rust tab="Rust" tab-group="language"
let client = reqwest::Client::new();
let token = "hv_a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4";

let response = client
    .get("http://127.0.0.1:3000/admin/lexicons")
    .bearer_auth(token)
    .send()
    .await?;

let data: serde_json::Value = response.json().await?;
```

```go tab="Go" tab-group="language"
token := "hv_a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4"

req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/lexicons", nil)
req.Header.Set("Authorization", "Bearer "+token)

resp, err := http.DefaultClient.Do(req)
```

```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/lexicons \
  -H "Authorization: Bearer hv_a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4"
```

This works for all [admin API](../api-reference/admin/admin-api.md) endpoints that the key has permissions for. Unlike OAuth tokens which carry the user's full permissions, API keys are limited to the specific permissions assigned at creation time.

## Revoking a key

1. Go to **Settings > API Keys**
2. Click **Revoke** next to the key
3. Confirm in the dialog

Revoked keys are soft-deleted: they remain visible in the table (with a strikethrough) for audit purposes but can no longer authenticate. Any service using the key will immediately receive `401 Unauthorized` responses.

## Tracking usage

The **Last Used** column in the API Keys table shows when each key was last used to authenticate a request. Keys that have never been used show "Never". This helps you identify unused keys that can be safely revoked.

## Security considerations

- **Treat API keys like passwords.** Anyone with the key can access your HappyView instance with the key's permissions.
- **Use the principle of least privilege.** Only grant the permissions a key actually needs. A CI deploy key probably only needs `lexicons:create` and `backfill:create`, not full access.
- **Use descriptive names** so you can identify which service uses which key.
- **Revoke keys you no longer need.** If a key is compromised, revoke it immediately.
- **Don't commit keys to version control.** Use environment variables or secret managers instead.

## Next steps

- [Admin API reference](../api-reference/admin/admin-api.md) — full endpoint documentation
- [Scripting](./lua-scripting.md) — automate record processing with Lua scripts
- [Record & label scripts](./record-scripts) — react to record changes and label events

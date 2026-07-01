---
title: "Service Identity"
---

Manage the service identity configuration: the DID and identity mode that HappyView uses to identify itself on the AT Protocol network. All endpoints require the `settings:manage` permission.

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

## Get service identity

```
GET /admin/service-identity
```

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/service-identity", {
  headers,
});
const data = await response.json();
```

```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/service-identity", {
  headers,
});
const data = await response.json();
```

```rust tab="Rust" tab-group="language"
let response = client
    .get("http://127.0.0.1:3000/admin/service-identity")
    .bearer_auth(token)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```

```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET", "http://127.0.0.1:3000/admin/service-identity", nil)
req.Header.Set("Authorization", "Bearer "+token)
resp, err := http.DefaultClient.Do(req)
```

```sh tab="cURL" tab-group="language"
curl http://127.0.0.1:3000/admin/service-identity -H "$AUTH"
```

Returns the current service identity configuration, or `null` if no identity has been configured.

**Response:**

| Field                  | Type        | Description                                       |
| ---------------------- | ----------- | ------------------------------------------------- |
| `mode`                 | string      | Identity mode (see below)                         |
| `did`                  | string/null | The service DID                                   |
| `signing_key_enc`      | string/null | Encrypted signing key (present for `did_plc`)     |
| `attached_account_did` | string/null | Linked account DID (present for `attach_account`) |
| `setup_complete`       | boolean     | Whether identity setup has been completed         |
| `created_at`           | string      | ISO 8601 timestamp                                |
| `updated_at`           | string      | ISO 8601 timestamp                                |

### Identity modes

| Mode             | Description                                          |
| ---------------- | ---------------------------------------------------- |
| `did_web`        | HappyView derives a `did:web` from its public URL    |
| `did_plc`        | HappyView manages its own `did:plc` identity         |
| `attach_account` | HappyView uses an existing AT Protocol account's DID |
| `not_exposed`    | No service identity is exposed on the network        |

## Update service identity

```
PUT /admin/service-identity
```

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/service-identity", {
  method: "PUT",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    mode: "did_web",
  }),
});
```

```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/service-identity", {
  method: "PUT",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    mode: "did_web",
  }),
});
```

```rust tab="Rust" tab-group="language"
let response = client
    .put("http://127.0.0.1:3000/admin/service-identity")
    .bearer_auth(token)
    .json(&serde_json::json!({
        "mode": "did_web"
    }))
    .send()
    .await?;
```

```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{"mode": "did_web"}`)
req, _ := http.NewRequest("PUT", "http://127.0.0.1:3000/admin/service-identity", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```

```sh tab="cURL" tab-group="language"
curl -X PUT http://127.0.0.1:3000/admin/service-identity \
  -H "$AUTH" \
  -H 'Content-Type: application/json' \
  -d '{"mode": "did_web"}'
```

**Input:**

| Field                  | Type   | Required | Description                                              |
| ---------------------- | ------ | -------- | -------------------------------------------------------- |
| `mode`                 | string | Yes      | `did_web`, `did_plc`, `attach_account`, or `not_exposed` |
| `did`                  | string | No       | Service DID (required for `did_plc`)                     |
| `signing_key_enc`      | string | No       | Encrypted signing key (for `did_plc`)                    |
| `rotation_key_enc`     | string | No       | Encrypted rotation key (for `did_plc`)                   |
| `attached_account_did` | string | No       | Account DID to attach (for `attach_account`)             |

**Response**: `204 No Content`

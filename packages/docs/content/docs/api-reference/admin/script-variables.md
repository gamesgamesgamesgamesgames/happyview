---
title: "Script Variables"
---

Script variables are encrypted key/value pairs available to Lua scripts via the `env` global. Use them for secrets like API tokens.

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

## List script variables

```
GET /admin/script-variables
```

Requires `script-variables:read`. Returns a list of variable keys (values are not returned).

## Upsert a script variable

```
POST /admin/script-variables
```

Requires `script-variables:create`.

```ts tab="TypeScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/script-variables", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({ key: "ALGOLIA_API_KEY", value: "..." }),
});
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("http://127.0.0.1:3000/admin/script-variables", {
  method: "POST",
  headers: {
    ...headers,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({ key: "ALGOLIA_API_KEY", value: "..." }),
});
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("http://127.0.0.1:3000/admin/script-variables")
    .bearer_auth(token)
    .json(&serde_json::json!({ "key": "ALGOLIA_API_KEY", "value": "..." }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{ "key": "ALGOLIA_API_KEY", "value": "..." }`)
req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/admin/script-variables", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/script-variables \
  -H "$AUTH" \
  -H "Content-Type: application/json" \
  -d '{ "key": "ALGOLIA_API_KEY", "value": "..." }'
```

The value is encrypted at rest using `TOKEN_ENCRYPTION_KEY`.

## Delete a script variable

```
DELETE /admin/script-variables/{key}
```

Requires `script-variables:delete`.

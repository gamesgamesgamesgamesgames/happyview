---
title: "Labelers"
---

Labelers are external services that apply content labels to records. They operate out-of-band — labeler data does not appear in repos or flow through relays. HappyView can subscribe to labelers and store the labels they emit, making them available on records in the admin dashboard and via Lua scripts.

## How labelers work

A labeler is identified by its DID. When you subscribe to a labeler, HappyView connects directly to the labeler's WebSocket and streams label events in real time. Each label targets a specific record URI and carries a value like `nudity`, `spam`, or any custom string the labeler defines.

Labels are stored in a `happyview_labels` table in the database. HappyView tracks a cursor per labeler subscription so it can resume from where it left off after a restart.

Records can also have **self-labels** — labels applied by the record's author and embedded directly in the record's `labels.values` array. These are not managed by external labelers but are displayed alongside external labels in the dashboard.

## Adding a labeler

1. Go to **Settings > Labelers** in the dashboard sidebar
2. Click **Add Labeler**
3. Enter the labeler's DID (e.g., `did:plc:ar7c4by46qjdydhdevvrndac`)
4. Click **Add**

HappyView begins consuming labels from the labeler immediately. The subscription appears in the table with an `active` status.

You can also add a labeler via the API:

```ts tab="TypeScript" tab-group="language"
const TOKEN = "hv_..."; // your API key
const headers = {
  Authorization: `Bearer ${TOKEN}`,
  "Content-Type": "application/json",
};

const response = await fetch("http://127.0.0.1:3000/admin/labelers", {
  method: "POST",
  headers,
  body: JSON.stringify({ did: "did:plc:ar7c4by46qjdydhdevvrndac" }),
});
```
```js tab="JavaScript" tab-group="language"
const TOKEN = "hv_..."; // your API key
const headers = {
  Authorization: `Bearer ${TOKEN}`,
  "Content-Type": "application/json",
};

const response = await fetch("http://127.0.0.1:3000/admin/labelers", {
  method: "POST",
  headers,
  body: JSON.stringify({ did: "did:plc:ar7c4by46qjdydhdevvrndac" }),
});
```
```rust tab="Rust" tab-group="language"
let client = reqwest::Client::new();
let token = "hv_..."; // your API key

let response = client
    .post("http://127.0.0.1:3000/admin/labelers")
    .bearer_auth(token)
    .json(&serde_json::json!({ "did": "did:plc:ar7c4by46qjdydhdevvrndac" }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
token := "hv_..." // your API key
body := bytes.NewBufferString(`{"did": "did:plc:ar7c4by46qjdydhdevvrndac"}`)

req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/admin/labelers", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")

resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/labelers \
  -H "Authorization: Bearer hv_..." \
  -H "Content-Type: application/json" \
  -d '{ "did": "did:plc:ar7c4by46qjdydhdevvrndac" }'
```

## Pausing and resuming

You can pause a labeler subscription to temporarily stop consuming labels without losing your cursor position. Click the pause icon next to the labeler in the table, or use the API:

```ts tab="TypeScript" tab-group="language"
const TOKEN = "hv_..."; // your API key

const response = await fetch(
  "http://127.0.0.1:3000/admin/labelers/did:plc:ar7c4by46qjdydhdevvrndac",
  {
    method: "PATCH",
    headers: {
      Authorization: `Bearer ${TOKEN}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ status: "paused" }),
  },
);
```
```js tab="JavaScript" tab-group="language"
const TOKEN = "hv_..."; // your API key

const response = await fetch(
  "http://127.0.0.1:3000/admin/labelers/did:plc:ar7c4by46qjdydhdevvrndac",
  {
    method: "PATCH",
    headers: {
      Authorization: `Bearer ${TOKEN}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ status: "paused" }),
  },
);
```
```rust tab="Rust" tab-group="language"
let response = client
    .patch("http://127.0.0.1:3000/admin/labelers/did:plc:ar7c4by46qjdydhdevvrndac")
    .bearer_auth(token)
    .json(&serde_json::json!({ "status": "paused" }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{"status": "paused"}`)

req, _ := http.NewRequest("PATCH",
  "http://127.0.0.1:3000/admin/labelers/did:plc:ar7c4by46qjdydhdevvrndac", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")

resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X PATCH http://127.0.0.1:3000/admin/labelers/did:plc:ar7c4by46qjdydhdevvrndac \
  -H "Authorization: Bearer hv_..." \
  -H "Content-Type: application/json" \
  -d '{ "status": "paused" }'
```

Resume by clicking the play icon or sending `{ "status": "active" }`.

## Deleting a labeler

Deleting a labeler removes the subscription **and all labels it has emitted**. This cannot be undone.

1. Click the trash icon next to the labeler
2. Confirm in the dialog

Or via the API:

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/labelers/did:plc:ar7c4by46qjdydhdevvrndac",
  {
    method: "DELETE",
    headers: { Authorization: `Bearer ${TOKEN}` },
  },
);
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "http://127.0.0.1:3000/admin/labelers/did:plc:ar7c4by46qjdydhdevvrndac",
  {
    method: "DELETE",
    headers: { Authorization: `Bearer ${TOKEN}` },
  },
);
```
```rust tab="Rust" tab-group="language"
let response = client
    .delete("http://127.0.0.1:3000/admin/labelers/did:plc:ar7c4by46qjdydhdevvrndac")
    .bearer_auth(token)
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("DELETE",
  "http://127.0.0.1:3000/admin/labelers/did:plc:ar7c4by46qjdydhdevvrndac", nil)
req.Header.Set("Authorization", "Bearer "+token)

resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X DELETE http://127.0.0.1:3000/admin/labelers/did:plc:ar7c4by46qjdydhdevvrndac \
  -H "Authorization: Bearer hv_..."
```

## Labels on records

Labels appear in the **Labels** column on the Records page as color-coded badges:

- **Red** — content warnings: `nudity`, `sexual`, `graphic-media`, `violence`, `gore`
- **Amber** — moderation labels: `spam`, `impersonation`
- **Neutral** — everything else

Self-labels (applied by the record author) use an outline badge style to distinguish them from external labels. Hover over a badge to see the source labeler's DID.

Labels are also available in the records API response and in Lua scripts via the [`atproto.get_labels` and `atproto.get_labels_batch`](../api-reference/lua/atproto-api.md#atprotoget_labels) functions.

## Using labels in your AppView

Labeler subscriptions give your AppView access to content moderation signals without building your own moderation system. Some ways to use them:

- **Content filtering**: Use labels in query scripts to exclude or down-rank flagged content. Check labels with `atproto.get_labels` and filter results before returning them.
- **Moderation dashboards**: Display labels alongside records in your admin dashboard to review flagged content. Labels appear automatically on the Records page once a labeler is subscribed.
- **Custom labelers**: You can subscribe to any labeler that implements the atproto labeler spec, including community-run labelers or one you operate yourself for domain-specific moderation (e.g. labeling game content by age rating).

## Permissions

| Action              | Permission        |
| ------------------- | ----------------- |
| View labeler list   | `labelers:read`   |
| Add or pause/resume | `labelers:create` |
| Delete a labeler    | `labelers:delete` |

## Next steps

- [Admin API — Labelers](../api-reference/admin/labelers.md) — full endpoint documentation
- [atproto API](../api-reference/lua/atproto-api.md) — access labels in Lua scripts with `get_labels` and `get_labels_batch`
- [Permissions](./permissions.md) — manage user access to labeler operations

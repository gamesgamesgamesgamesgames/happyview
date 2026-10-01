---
title: "Write Notifications"
---

<Callout type="error" title="Experimental">
This API is experimental and will change. See the [Permissioned Spaces overview](../spaces.md) for context.
</Callout>

Write notifications tell syncers when a repo in a space changes. A syncer registers its service identifier for a space, and HappyView calls `com.atproto.space.notifyWrite` on that service after every commit to any repo in the space. The syncer then reads the changed repo with a [space credential](./credentials.md).

Notifications flow in two directions:

- **Outbound**: HappyView notifies registered syncers when a repo in the space advances, whether HappyView hosts the repo or the author's PDS does.
- **Inbound**: a PDS hosting a repo in the space notifies HappyView, as the space authority, when that repo changes.

Registrations cover the whole space and expire after 24 hours. Registering again renews the registration and replaces the previous one for the same service.

## Registering for notifications

Requires an OAuth session or a space credential. With a credential, the request's audience is the space authority's DID.

```ts tab="TypeScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.space.registerNotify", {
  method: "POST",
  headers: {
    ...(await signSpaceRequest(`Atproto-Space ${SPACE_CREDENTIAL}`, "did:web:happyview.example.com")),
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "at://did:web:happyview.example.com/space/com.example.forum/main",
    service: "did:web:syncer.example.com#atproto_space_syncer",
  }),
});
interface RegisterNotifyResponse {
  expiresAt: string;
}
const data: RegisterNotifyResponse = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.space.registerNotify", {
  method: "POST",
  headers: {
    ...(await signSpaceRequest(`Atproto-Space ${SPACE_CREDENTIAL}`, "did:web:happyview.example.com")),
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "at://did:web:happyview.example.com/space/com.example.forum/main",
    service: "did:web:syncer.example.com#atproto_space_syncer",
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("https://happyview.example.com/xrpc/com.atproto.space.registerNotify")
    .headers(sign_space_request(
        &signing_key,
        &format!("Atproto-Space {space_credential}"),
        Some("did:web:happyview.example.com"),
    ))
    .json(&serde_json::json!({
        "space": "at://did:web:happyview.example.com/space/com.example.forum/main",
        "service": "did:web:syncer.example.com#atproto_space_syncer"
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "space": "at://did:web:happyview.example.com/space/com.example.forum/main",
  "service": "did:web:syncer.example.com#atproto_space_syncer"
}`)
req, _ := http.NewRequest("POST",
  "https://happyview.example.com/xrpc/com.atproto.space.registerNotify", body)
signSpaceRequest(req, key, "Atproto-Space "+spaceCredential, "did:web:happyview.example.com")
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST 'https://happyview.example.com/xrpc/com.atproto.space.registerNotify' \
  -H 'Authorization: Atproto-Space <credential>' \
  -H 'Atproto-Space-Audience: did:web:happyview.example.com' \
  -H 'Signature-Input: atproto-space=("authorization" "atproto-space-audience")' \
  -H 'Signature: atproto-space=:<base64 signature>:' \
  -H 'Content-Type: application/json' \
  -d '{
    "space": "at://did:web:happyview.example.com/space/com.example.forum/main",
    "service": "did:web:syncer.example.com#atproto_space_syncer"
  }'
```

The `signSpaceRequest` helper is defined in [Signing requests](./credentials.md#signing-requests).

**Input:**

| Field     | Type   | Required | Description |
| --------- | ------ | -------- | ----------- |
| `space`   | string | Yes      | Space URI (`at://...`) |
| `service` | string | Yes      | Service identifier of the syncer: a DID with an optional fragment naming a service entry in its DID document. A bare DID means the account's `#atproto_pds` service. |

HappyView resolves the service identifier to its endpoint when the syncer registers. An identifier that does not resolve fails with `400 ServiceNotResolvable`.

**Response (200):**

```json
{
  "expiresAt": "2026-10-01T12:00:00Z"
}
```

## Unregistering

`com.atproto.space.unregisterNotify` withdraws a service's registration. It takes the same authentication as `registerNotify`.

**Input:**

| Field     | Type   | Required | Description |
| --------- | ------ | -------- | ----------- |
| `space`   | string | Yes      | Space URI |
| `service` | string | Yes      | The service identifier to unregister |

The caller must be the registered service or the space's creator. The call is idempotent.

**Response (200):**

```json
{
  "removed": 1
}
```

## Receiving notifications

For each commit to a repo in the space, HappyView POSTs to `<endpoint>/xrpc/com.atproto.space.notifyWrite` on every registered service. Each call carries service auth from HappyView: a Bearer JWT whose `iss` is HappyView's instance DID, whose `aud` is the registered service identifier, and whose `lxm` is `com.atproto.space.notifyWrite`.

```json
{
  "space": "at://did:web:happyview.example.com/space/com.example.forum/main",
  "repo": "did:plc:author456",
  "repoRev": "3l2tkbx7225co",
  "rev": "3l2tkbx7225co",
  "hash": { "$bytes": "q83vEjRWeJC..." },
  "spaceRev": "3l2tkbx7a3k2s",
  "prevSpaceRev": "3l2tkbwz5xq2c"
}
```

| Field          | Type    | Description |
| -------------- | ------- | ----------- |
| `space`        | string  | The space URI |
| `repo`         | string  | DID of the repo that changed |
| `repoRev`      | string  | The repo's new revision (TID) |
| `rev`          | string  | The same value as `repoRev`, under the alpha lexicon's name. Sent until v3. |
| `hash`         | bytes   | The repo's new LtHash digest |
| `spaceRev`     | string  | The space revision this update was assigned |
| `prevSpaceRev` | string? | The space revision before it. Absent on the first update in the space. |

Every accepted update advances the space revision, a TID that increases across all repos in the space. A syncer that receives a `prevSpaceRev` newer than the last `spaceRev` it saw has missed a notification. It catches up by calling [`listRepos`](./records.md#listing-repos) with `cursor` set to the last space revision it processed.

Delivery is best effort. HappyView does not retry a failed call.

## Sending notifications to HappyView

A PDS hosting a repo in the space calls `com.atproto.space.notifyWrite` on HappyView after each commit to that repo. HappyView then pulls the new commit from the PDS and indexes it.

The call requires service auth signed by the account that wrote. The token's `aud` is HappyView's instance DID with a service fragment, and its `lxm` must be `com.atproto.space.notifyWrite`.

```ts tab="TypeScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.space.notifyWrite", {
  method: "POST",
  headers: {
    "Authorization": `Bearer ${SERVICE_AUTH_TOKEN}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "at://did:web:happyview.example.com/space/com.example.forum/main",
    repo: "did:plc:author456",
    repoRev: "3l2tkbx7225co",
    hash: { $bytes: "q83vEjRWeJC..." },
  }),
});
const data = await response.json();
// { "success": true }
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.space.notifyWrite", {
  method: "POST",
  headers: {
    "Authorization": `Bearer ${SERVICE_AUTH_TOKEN}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    space: "at://did:web:happyview.example.com/space/com.example.forum/main",
    repo: "did:plc:author456",
    repoRev: "3l2tkbx7225co",
    hash: { $bytes: "q83vEjRWeJC..." },
  }),
});
const data = await response.json();
// { "success": true }
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("https://happyview.example.com/xrpc/com.atproto.space.notifyWrite")
    .header("Authorization", format!("Bearer {}", service_auth_token))
    .json(&serde_json::json!({
        "space": "at://did:web:happyview.example.com/space/com.example.forum/main",
        "repo": "did:plc:author456",
        "repoRev": "3l2tkbx7225co",
        "hash": { "$bytes": "q83vEjRWeJC..." }
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{
  "space": "at://did:web:happyview.example.com/space/com.example.forum/main",
  "repo": "did:plc:author456",
  "repoRev": "3l2tkbx7225co",
  "hash": {"$bytes": "q83vEjRWeJC..."}
}`)
req, _ := http.NewRequest("POST",
  "https://happyview.example.com/xrpc/com.atproto.space.notifyWrite", body)
req.Header.Set("Authorization", "Bearer "+serviceAuthToken)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST 'https://happyview.example.com/xrpc/com.atproto.space.notifyWrite' \
  -H 'Authorization: Bearer <service auth token>' \
  -H 'Content-Type: application/json' \
  -d '{
    "space": "at://did:web:happyview.example.com/space/com.example.forum/main",
    "repo": "did:plc:author456",
    "repoRev": "3l2tkbx7225co",
    "hash": {"$bytes": "q83vEjRWeJC..."}
  }'
```

**Input:**

| Field   | Type   | Required | Description |
| ------- | ------ | -------- | ----------- |
| `space` | string | Yes      | Space URI (`at://...`) |
| `repo`  | string | Yes      | DID of the repo that changed. Must match the service auth issuer. |
| `repoRev` | string | Yes    | The repo's new revision (TID). `rev`, the alpha lexicon's name, is accepted in its place until v3. |
| `hash`  | bytes  | Yes      | The repo's new LtHash digest |

HappyView handles the notification as follows:

- The writer must pass the space's [write policy](./managing-spaces.md#policies). Otherwise the call fails with `403`.
- A `repoRev` that is not newer than the last one recorded for the repo is accepted and ignored.
- A `repoRev` more than 5 minutes in the future fails with `400 FutureRev`.
- A newer `repoRev` joins the repo to the space's writer set, advances the space revision, and is forwarded to registered syncers.

**Response (200):**

```json
{
  "success": true
}
```

HappyView also checks every repo hosted on its author's PDS every 5 minutes, so a write whose notification was lost is indexed on the next check.

## Notifying space deletion

When a space is deleted, HappyView tells every service registered for it. A service registered by identifier receives `com.atproto.space.notifySpaceDeleted` at its endpoint, with service auth from HappyView and the body `{ "space": "<space URI>" }`. A legacy webhook receives `{ "space": "<space id>" }`. Delivery is best effort.

The space's creator or a HappyView super admin can also send the same notification for a space that still exists by calling `com.atproto.space.notifySpaceDeleted` with `{ "space": "<space URI>" }`. HappyView responds with `{ "success": true }`.

## Legacy webhooks

The following forms are deprecated and kept until v3.

**Webhook registration.** `registerNotify` with `serviceDid` and `endpoint` in place of `service` registers a webhook URL. The response includes the registration `id`:

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "expiresAt": "2026-10-01T12:00:00Z"
}
```

HappyView POSTs a JSON payload to the endpoint for each record created, updated, or deleted in the space, without authentication:

| Field        | Type          | Description |
| ------------ | ------------- | ----------- |
| `space`      | string        | Internal space ID |
| `did`        | string        | DID of the author |
| `collection` | string (NSID) | Collection of the record |
| `rkey`       | string        | Record key |
| `cid`        | string?       | CID of the new record value, `null` for deletes |

**Per-record notifyWrite.** `notifyWrite` also accepts `{space, did, collection, rkey, cid}` from the author, the space's creator, or a super admin. HappyView syncs the author's repo and sends the per-record payload to webhook registrations.

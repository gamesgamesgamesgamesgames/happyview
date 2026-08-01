---
title: "Node Client"
---

Server-side OAuth client for authenticating with a HappyView instance using atproto. Built on top of [`@happyview/oauth-client`](../oauth-client/overview.md), matching the API surface of [`@atproto/oauth-client-node`](https://www.npmjs.com/package/@atproto/oauth-client-node).

## Installation

```bash
npm install @happyview/oauth-client-node
```

## Setup

```typescript
import { HappyViewNodeClient } from "@happyview/oauth-client-node";

const client = new HappyViewNodeClient({
  instanceUrl: "https://happyview.example.com",
  clientId: "https://example.com/oauth-client-metadata.json",
  clientKey: "hvc_your_client_key",
  redirectUri: "https://example.com/oauth/callback",
  storage: myStorageAdapter,
});
```

| Option         | Required | Description                                                                   |
| -------------- | -------- | ----------------------------------------------------------------------------- |
| `instanceUrl`  | Yes      | The HappyView instance URL                                                    |
| `clientId`     | Yes      | URL where your app serves its [OAuth client metadata](#oauth-client-metadata) |
| `clientKey`    | Yes      | API client key from the HappyView admin dashboard                             |
| `redirectUri`  | Yes      | OAuth callback URL                                                            |
| `storage`      | Yes      | Storage adapter for persisting sessions and auth state                        |
| `clientSecret` | No       | Secret for confidential clients                                               |
| `scopes`       | No       | OAuth scopes to request. Defaults to `"atproto"`                              |
| `sessionHooks` | No       | Event hooks for session lifecycle events                                      |
| `fetch`        | No       | Custom fetch implementation                                                   |

### Storage

You must provide a `StorageAdapter`. The built-in `MemoryStorage` works for development but won't survive restarts:

```typescript
import { MemoryStorage } from "@happyview/oauth-client-node";

const client = new HappyViewNodeClient({
  // ...
  storage: new MemoryStorage(),
});
```

For production, implement the `StorageAdapter` interface backed by your database or cache:

```typescript
interface StorageAdapter {
  get(key: string): Promise<string | null>;
  set(key: string, value: string): Promise<void>;
  delete(key: string): Promise<void>;
}
```

## Authorize

Generate an authorization URL and redirect the user:

```typescript
const url = await client.authorize("alice.bsky.social");
res.redirect(url.href);
```

Options:

```typescript
const url = await client.authorize("alice.bsky.social", {
  scope: "atproto transition:generic",
  redirect_uri: "https://example.com/alt-callback",
  state: "my-custom-state",
  prompt: "login",
  display: "page",
});
```

| Option                   | Description                                                                  |
| ------------------------ | ---------------------------------------------------------------------------- |
| `scope`                  | OAuth scopes for this request (overrides constructor default)                |
| `scopes`                 | Deprecated alias for `scope`. `scope` takes priority if both passed.         |
| `state`                  | Custom state value. Defaults to a random hex string.                         |
| `redirect_uri`           | Override the redirect URI for this request                                   |
| `signal`                 | `AbortSignal` for cancellation                                               |
| `display`                | Display hint: `"page"`, `"popup"`, `"touch"`, or `"wap"`                     |
| `prompt`                 | Prompt mode (e.g. `"login"` to force re-authentication)                      |
| `nonce`                  | OIDC nonce value                                                             |
| `max_age`                | Max elapsed seconds since last active authentication                         |
| `ui_locales`             | Space-separated locale tags (e.g. `"en fr"`)                                 |
| `dpop_jkt`               | DPoP JWK thumbprint                                                          |
| `claims`                 | OIDC claims request object                                                   |
| `authorization_details`  | RFC 9396 authorization details                                               |
| `id_token_hint`          | Previous ID token hint                                                       |

### Abort a pending request

If you need to cancel a pending authorization (e.g., the user navigates away), pass the URL returned from `authorize()`:

```typescript
const url = await client.authorize("alice.bsky.social");
// ...later, if the user cancels:
await client.abortRequest(url);
```

This cleans up the stored pending auth state.

## Callback

On your callback route, process the OAuth response:

```typescript
app.get("/oauth/callback", async (req, res) => {
  const params = new URLSearchParams(req.url.split("?")[1]);
  const { session, state } = await client.callback(params);

  // session.did is the authenticated user's DID
  // state is the value passed to authorize() (or the auto-generated one)
});
```

You can override the redirect URI for this specific callback:

```typescript
const { session } = await client.callback(params, {
  redirect_uri: "https://other.example.com/callback",
});
```

## Restore session

Restore a session by DID:

```typescript
const session = await client.restore("did:plc:abc123");
```

The `did` parameter is required in the node client (unlike the browser client, there's no "last active" session concept on the server).

A second `refresh` parameter is accepted for API compatibility with upstream (`restore(did, refresh?)`). HappyView manages token refresh server-side, so this parameter is accepted but ignored.

## Session

### Authenticated requests

The session's `fetchHandler` attaches DPoP proof headers automatically:

```typescript
const response = await session.fetchHandler(
  "/xrpc/com.example.getStuff?limit=10",
  { method: "GET" },
);

const data = await response.json();
```

Pass a relative path (prepends the HappyView instance URL) or a full URL (used as-is).

### Token info

```typescript
const info = session.getTokenInfo();
// { sub, scope, iss, aud, expiresAt?, expired? }
```

Returns available metadata about the session. `expiresAt` and `expired` are always `undefined` since HappyView manages token lifecycle server-side.

### Properties

| Property | Type     | Description                              |
| -------- | -------- | ---------------------------------------- |
| `did`    | `string` | The authenticated user's DID             |
| `sub`    | `string` | Alias for `did` (matches upstream naming) |

### Sign out

Sessions can self-revoke:

```typescript
await session.signOut();
```

This is equivalent to calling `client.revoke(session.did)`.

## Confidential vs public clients

Clients created with a `clientSecret` are confidential — they can hold secrets safely on the server. Clients without a secret are public. Use `client.isConfidential` to check:

```typescript
const client = new HappyViewNodeClient({
  // ...
  clientSecret: "hvs_your_secret",
});
client.isConfidential; // true
```

Public clients use PKCE to secure the DPoP key provisioning step. Confidential clients authenticate with their secret instead.

## Session event hooks

React to session lifecycle events with `sessionHooks`:

```typescript
const client = new HappyViewNodeClient({
  // ...
  sessionHooks: {
    onSessionUpdate(did) {
      console.log(`Session created/updated for ${did}`);
    },
    onSessionDelete(did) {
      console.log(`Session deleted for ${did}`);
    },
  },
});
```

- `onSessionUpdate(did)` fires after a new session is registered (from `callback()`).
- `onSessionDelete(did)` fires after a session is revoked (from `revoke()` or `session.signOut()`).

## Error handling

Callback errors are always wrapped in `OAuthCallbackError`, which carries the original callback params and state:

```typescript
import { OAuthCallbackError } from "@happyview/oauth-client-node";

try {
  const { session } = await client.callback(params);
} catch (err) {
  if (err instanceof OAuthCallbackError) {
    console.log(err.state);           // the state from the callback
    console.log(err.params.get("error")); // e.g. "access_denied"
    console.log(err.cause);           // the underlying error, if any
  }
}
```

If the authorization server returns an error (e.g., the user denied access), the `params` contain the `error` and `error_description` fields from the server response. If the token exchange fails, the underlying `TokenExchangeError` is available as `err.cause`.

## Using with @atproto/api

`HappyViewSession` is directly compatible with `@atproto/api`'s `Agent`:

```typescript
import { Agent } from "@atproto/api";

const session = await client.restore("did:plc:abc123");
const agent = new Agent(session);

const profile = await agent.getProfile({ actor: agent.did });
await agent.like(postUri, postCid);
```

This works because `HappyViewSession` implements the `SessionManager` interface that `Agent` expects.

## Revoke session

From the client:

```typescript
await client.revoke("did:plc:abc123");
```

Or from the session itself:

```typescript
await session.signOut();
```

## Validate client metadata

Verify that your OAuth client metadata is served correctly:

```typescript
const metadata = await HappyViewNodeClient.fetchMetadata({
  clientId: "https://example.com/oauth-client-metadata.json",
});
console.log(metadata.client_name);
```

## Identity resolution

The client exposes its handle and DID resolvers for advanced use:

```typescript
const did = await client.handleResolver.resolve("alice.bsky.social");
const doc = await client.didResolver.resolve(did);
```

## OAuth client metadata

Your app must serve an OAuth client metadata JSON document at the URL you pass as `clientId`. The PDS fetches this during authorization.

For a confidential Node.js server:

```typescript
app.get("/oauth-client-metadata.json", (req, res) => {
  const origin = `${req.protocol}://${req.get("host")}`;
  res.json({
    client_id: `${origin}/oauth-client-metadata.json`,
    client_name: "My Server App",
    client_uri: origin,
    redirect_uris: [`${origin}/oauth/callback`],
    token_endpoint_auth_method: "none",
    grant_types: ["authorization_code", "refresh_token"],
    scope: "atproto",
    application_type: "web",
    dpop_bound_access_tokens: true,
  });
});
```

## Re-exports

This package re-exports everything from `@happyview/oauth-client`, `@atproto-labs/handle-resolver`, and `@atproto-labs/did-resolver`. You don't need to install these packages separately:

```typescript
import {
  // From @happyview/oauth-client
  HappyViewNodeClient,
  HappyViewSession,
  MemoryStorage,
  ApiError,
  OAuthCallbackError,
  Key,
  type SessionEventHooks,
  type StorageAdapter,
  type TokenInfo,
  type Jwk,

  // From @atproto-labs/handle-resolver
  AtprotoDohHandleResolver,

  // From @atproto-labs/did-resolver
  DidResolverCommon,
  type DidDocument,
} from "@happyview/oauth-client-node";
```

## Differences from upstream

The HappyView SDK matches the upstream `@atproto/oauth-client-node` public API but differs architecturally:

| Area | Upstream | HappyView |
|------|----------|-----------|
| DPoP keys | Generated client-side | Provisioned from HappyView instance |
| Token refresh | Client-side with `refresh` param | Server-side (HappyView manages lifecycle) |
| `restore(did, refresh?)` | `refresh` controls token refresh behavior | `refresh` accepted but ignored |
| `session.getTokenInfo()` | Includes `expiresAt`/`expired` | These fields are `undefined` |
| `jwks` | Returns client's public keyset | Not applicable (no client keypairs) |

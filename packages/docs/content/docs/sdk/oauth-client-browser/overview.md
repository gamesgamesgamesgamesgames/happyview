---
title: "Browser Client"
---

The browser client handles the full OAuth redirect flow for browser apps authenticating with a HappyView instance. It wraps the [OAuth Client](../oauth-client/overview.md) with Web Crypto, localStorage, and atproto handle/DID resolution.

If you're starting a new app, consider using [`@happyview/lex-agent`](../lex-agent/overview.md) with `@atproto/lex` instead — it provides type-safe XRPC calls and is the recommended way to interact with HappyView. This package is primarily useful if your app already uses `@atproto/oauth-client-browser` and you want to add HappyView authentication alongside it.

## Installation

```bash
npm install @happyview/oauth-client-browser
```

## Setup

```typescript
import { HappyViewBrowserClient } from "@happyview/oauth-client-browser";

const client = new HappyViewBrowserClient({
  instanceUrl: "https://happyview.example.com",
  clientId: "https://example.com/oauth-client-metadata.json",
  clientKey: "hvc_your_client_key",
});
```

| Option        | Required | Description                                                                   |
| ------------- | -------- | ----------------------------------------------------------------------------- |
| `instanceUrl` | Yes      | The HappyView instance URL                                                    |
| `clientId`    | Yes      | URL where your app serves its [OAuth client metadata](#oauth-client-metadata) |
| `clientKey`   | Yes      | API client key from the HappyView admin dashboard                             |
| `redirectUri` | No       | OAuth callback URL. Defaults to `${window.location.origin}/oauth/callback`    |
| `scopes`      | No       | OAuth scopes to request. Defaults to `"atproto"`                              |
| `storage`     | No       | Custom storage adapter. Defaults to localStorage                              |
| `sessionHooks`| No       | Event hooks for session lifecycle events                                      |
| `fetch`       | No       | Custom fetch implementation                                                   |

The client uses localStorage by default. You can override it:

```typescript
const client = new HappyViewBrowserClient({
  instanceUrl: "https://happyview.example.com",
  clientId: "https://example.com/oauth-client-metadata.json",
  clientKey: "hvc_your_client_key",
  storage: myCustomStorageAdapter,
});
```

<Callout type="info">
The API client must be registered as a **public** client (no secret) with your app's origin in `allowed_origins`. See [Authentication — API clients](../../getting-started/authentication.md#api-clients-confidential-vs-public).
</Callout>

## Sign in

`signIn()` resolves the user's handle, discovers their PDS, provisions a DPoP key, and redirects the browser to the PDS authorization server:

```typescript
await client.signIn("alice.bsky.social");
// Browser redirects — code stops here
```

To sign in via a popup window instead:

```typescript
const session = await client.signIn("alice.bsky.social", {
  display: "popup",
});
```

Or use the explicit methods:

```typescript
// Full-page redirect (equivalent to signIn without display option)
await client.signInRedirect("alice.bsky.social");

// Popup window
const session = await client.signInPopup("alice.bsky.social");
```

If you need the authorization URL without redirecting (e.g., for a custom UI), use `prepareLogin()`:

```typescript
const { authorizationUrl, did, state } =
  await client.prepareLogin("alice.bsky.social");
```

<Callout type="info">
`login()` still works as an alias for `signInRedirect()`.
</Callout>

### What happens during sign in

1. The handle is resolved to a DID via `resolveHandleToDid`.
2. The DID document is fetched to find the PDS URL.
3. The PDS's OAuth authorization server metadata is fetched.
4. A DPoP key is provisioned from HappyView.
5. PKCE challenge/verifier pairs are generated (one for HappyView's DPoP provisioning, one for the PDS authorization server).
6. The pending auth state is stored in localStorage.
7. The browser is redirected to the PDS authorization endpoint (or a popup is opened).

## Initialization

On page load, call `init()` to automatically handle both session restoration and OAuth callbacks:

```typescript
const result = await client.init();
if (result) {
  const { session, state } = result;
  // session is ready to use
}
```

`init()` checks the URL for OAuth callback parameters. If found, it processes the callback and returns `{ session, state }`. Otherwise, it tries to restore the last active session from localStorage.

For more control, use the specific methods:

```typescript
// Restore only — ignores callback params in the URL
const result = await client.initRestore();
if (result) {
  const { session } = result;
}

// Callback only — throws if no callback params are present
const { session, state } = await client.initCallback();
```

### Restoring a specific session

To restore a specific user's session by DID:

```typescript
const session = await client.restore("did:plc:abc123");
```

Calling `restore()` with no arguments returns the last active session, or `null` if none is found.

<Callout type="info">
`callback()` still works as a standalone method that processes the OAuth callback and returns a session directly.
</Callout>

## Detecting callback params

`readCallbackParams()` checks the current URL for OAuth callback parameters without processing them. This is useful when your app uses client-side routing and needs to detect callbacks before the router changes the URL:

```typescript
const params = client.readCallbackParams();
if (params) {
  // URL contains OAuth callback params — process them
  const { session } = await client.initCallback();
}
```

## Checking approved scopes

After sign in or session restoration, you can check which scopes were approved:

```typescript
console.log(session.scopes);
// ["atproto", "transition:generic"]
```

To fetch the latest scopes from the server:

```typescript
const info = await client.getSession("did:plc:abc123");
console.log(info.scopes);
// ["atproto", "transition:generic"]
```

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

## Revoke session

```typescript
await client.revoke(session.did);
```

<Callout type="info">
`logout()` still works as an alias for `revoke()`.
</Callout>

The stored session is always cleared, even when the server refuses the revocation. `404`, `401`, and `403` are treated as a completed logout — there is no live credential left to revoke. `5xx` and network errors still throw so you know revocation may not have reached the server, but they throw after the local session is gone, so the user is never left signed in by a failed logout.

To clear a session locally without contacting the server at all:

```typescript
await client.forgetSession(session.did);
```

## Resolution utilities

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

## Session event hooks

React to session lifecycle events with `sessionHooks`:

```typescript
const client = new HappyViewBrowserClient({
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
- `onSessionDelete(did)` fires after a session is revoked (from `revoke()`, `logout()`, or `session.signOut()`).

## Error handling

Callback errors are always wrapped in `OAuthCallbackError`, which carries the original callback params and state:

```typescript
import { OAuthCallbackError } from "@happyview/oauth-client-browser";

try {
  const session = await client.callback();
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

`HappyViewSession` is directly compatible with `@atproto/api`'s `Agent`. Pass it as the session manager:

```typescript
import { Agent } from "@atproto/api";

const result = await client.init();
if (result) {
  const agent = new Agent(result.session);

  // Use the full @atproto/api surface
  const profile = await agent.getProfile({ actor: agent.did });
  await agent.like(postUri, postCid);
}
```

This works because `HappyViewSession` implements the `SessionManager` interface that `Agent` expects — it has `did` and a `fetchHandler` that attaches DPoP authentication headers and prepends the HappyView instance URL.

## Revoke session

From the client:

```typescript
await client.revoke(session.did);
```

Or from the session itself:

```typescript
await session.signOut();
```

<Callout type="info">
`logout()` still works as an alias for `revoke()`.
</Callout>

## Identity resolution

The client exposes its handle and DID resolvers for advanced use:

```typescript
const did = await client.handleResolver.resolve("alice.bsky.social");
const doc = await client.didResolver.resolve(did);
```

## Validate client metadata

Verify that your OAuth client metadata is served correctly:

```typescript
import { HappyViewBrowserClient } from "@happyview/oauth-client-browser";

const metadata = await HappyViewBrowserClient.fetchMetadata({
  clientId: "https://example.com/oauth-client-metadata.json",
});
console.log(metadata.client_name);
```

## OAuth client metadata

Your app must serve an OAuth client metadata JSON document at the URL you pass as `clientId`. The PDS fetches this during authorization to validate the redirect URI and display your app's information.

Example for a Next.js app:

```typescript
// src/app/oauth-client-metadata.json/route.ts
import { type NextRequest } from "next/server";

export function GET(request: NextRequest) {
  const origin = request.nextUrl.origin;

  return Response.json({
    client_id: `${origin}/oauth-client-metadata.json`,
    client_name: "My App",
    client_uri: origin,
    redirect_uris: [`${origin}/oauth/callback`],
    token_endpoint_auth_method: "none",
    grant_types: ["authorization_code", "refresh_token"],
    scope: "atproto",
    application_type: "web",
    dpop_bound_access_tokens: true,
  });
}
```

For a static site, serve a plain JSON file at `/oauth-client-metadata.json`.

The `redirect_uris` array must include the `redirectUri` your client is configured with (defaults to `${origin}/oauth/callback`).

## Local development

For local development with atproto's loopback client ID convention, use `buildLoopbackClientId`:

```typescript
import { buildLoopbackClientId } from "@happyview/oauth-client-browser";

const clientId = buildLoopbackClientId(window.location);
// → "http://localhost?redirect_uri=http%3A%2F%2F127.0.0.1%3A3000%2F"
```

This builds a client ID that authorization servers recognize as a local development app. The `redirect_uri` is encoded in the client ID URL query string.

## Cleanup

The browser client implements `AsyncDisposable` for use with `await using`:

```typescript
await using client = new HappyViewBrowserClient({ ... });
// client.dispose() called automatically when scope exits
```

Or call `dispose()` manually:

```typescript
client.dispose();
```

## Re-exports

This package re-exports everything from `@happyview/oauth-client`, `@atproto-labs/handle-resolver`, and `@atproto-labs/did-resolver`. You don't need to install these packages separately:

```typescript
import {
  // From @happyview/oauth-client
  HappyViewBrowserClient,
  HappyViewSession,
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
} from "@happyview/oauth-client-browser";
```

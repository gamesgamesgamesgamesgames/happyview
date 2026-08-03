---
title: "OAuth Client"
---

The core OAuth client handles DPoP key provisioning, session registration, and session restoration against a HappyView instance. It's platform-agnostic — you provide a `CryptoAdapter` and optional `StorageAdapter` for your environment.

If you're building a browser app, use the [Browser Client](../oauth-client-browser/overview.md) instead. It wraps this package with Web Crypto, localStorage, and a complete OAuth redirect flow.

## Installation

```bash
npm install @happyview/oauth-client
```

## Setup

```typescript
import { HappyViewOAuthClient } from "@happyview/oauth-client";

const client = new HappyViewOAuthClient({
  instanceUrl: "https://happyview.example.com",
  clientKey: "hvc_your_client_key",
  clientSecret: "hvs_your_secret", // optional, for confidential clients
  crypto: myCryptoAdapter,
  storage: myStorageAdapter, // optional, defaults to in-memory
});
```

The `clientSecret` parameter makes this a **confidential client**. Omit it for public clients (browser apps), which use PKCE instead. See [Authentication — API clients](../../getting-started/authentication.md#api-clients-confidential-vs-public) for details.

## DPoP key provisioning

Request a DPoP keypair from the HappyView instance. This is the first step of the [DPoP key provisioning flow](../../getting-started/authentication.md#dpop-key-provisioning-for-third-party-apps).

```typescript
const { provisionId, dpopKey, pkceVerifier } =
  await client.provisionDpopKey();
```

For public clients, `pkceVerifier` is included and must be passed back when registering the session. For confidential clients it will be `undefined`.

Use the returned `dpopKey` (a private JWK) as your DPoP keypair during your atproto OAuth flow with the user's PDS.

## Session registration

After completing OAuth authorization with the user's PDS, register the session with HappyView:

```typescript
const session = await client.registerSession({
  provisionId,
  pkceVerifier,       // required for public clients
  did: "did:plc:abc123",
  accessToken: tokens.access_token,
  refreshToken: tokens.refresh_token,
  scopes: "atproto",
  pdsUrl: "https://bsky.social",
  issuer: tokens.iss,
  dpopKey,
});
```

The returned `HappyViewSession` is ready to make authenticated requests. The session data is also persisted to the `StorageAdapter` for later restoration.

The response includes the scopes that were approved by the authorization server, available on the session:

```typescript
console.log(session.scopes);
// ["atproto", "transition:generic"]
```

## Retrieving session info

To fetch the current session's approved scopes from the server (e.g., after restoring from storage):

```typescript
const info = await client.getSession("did:plc:abc123");
console.log(info.scopes);
// ["atproto", "transition:generic"]
```

## Making authenticated requests

`HappyViewSession.fetchHandler` works like `fetch` but automatically attaches DPoP proof, authorization, and client key headers:

```typescript
// Relative path — prepends the HappyView instance URL
const response = await session.fetchHandler(
  "/xrpc/com.example.getStuff?limit=10",
  { method: "GET" },
);

// Absolute URL — used as-is
const response = await session.fetchHandler(
  "https://other-service.example.com/xrpc/test.method",
  { method: "GET" },
);
```

## Session restoration

Restore a previously stored session without re-authenticating:

```typescript
// Restore the last active session
const session = await client.restore();

// Restore a specific user's session
const session = await client.restoreSession("did:plc:abc123");
```

Returns `null` if no stored session is found.

## Logout

```typescript
await client.deleteSession("did:plc:abc123");
```

This deletes the session from both HappyView and local storage.

**The local cleanup always happens.** A `404`, `401`, or `403` from the server is treated as a completed logout — the session is either already gone or the credential is no longer usable, so there is nothing left to revoke. A `5xx` or a network error still throws, because the server may genuinely still hold a live session and you should know that, but it throws *after* the local session has been cleared. Either way the user ends up logged out on this device, and calling `deleteSession` again is safe.

### Forgetting a session locally

```typescript
await client.forgetSession("did:plc:abc123");
```

Clears the stored session without contacting the server. Use it when revocation is impossible — an unreachable instance, or a credential the server has already rejected. Nothing is revoked, so the instance may still consider the session live until it expires naturally.

### Storage keys

`STORAGE_PREFIX` (`"happyview:session:"`) and `LAST_ACTIVE_KEY` (`"happyview:last-active-did"`) are exported, so tooling that needs to inspect or clear stored sessions directly can do so without hardcoding the format:

```typescript
import { STORAGE_PREFIX, LAST_ACTIVE_KEY } from "@happyview/oauth-client";
```

## Adapters

### CryptoAdapter

Implement this interface for your platform's cryptographic primitives:

```typescript
interface CryptoAdapter {
  generatePkceVerifier(): Promise<string>;
  computePkceChallenge(verifier: string): Promise<string>;
  signEs256(privateKey: JsonWebKey, payload: Uint8Array): Promise<Uint8Array>;
  sha256(data: Uint8Array): Promise<Uint8Array>;
  getRandomValues(length: number): Uint8Array;
}
```

### StorageAdapter

Implement this interface to persist sessions:

```typescript
interface StorageAdapter {
  get(key: string): Promise<string | null>;
  set(key: string, value: string): Promise<void>;
  delete(key: string): Promise<void>;
}
```

If no `StorageAdapter` is provided, sessions are stored in memory and won't survive page reloads or process restarts.

<Callout type="info">
The built-in `MemoryStorage` is exported for testing. In production, always provide a persistent storage adapter.
</Callout>

## Error handling

All errors extend `HappyViewError`:

| Error | When |
| --- | --- |
| `ApiError` | HappyView API returned a non-OK response (has `status` and `body`) |
| `AuthenticationError` | Authentication failed (default status 401) |
| `InvalidStateError` | Missing or invalid OAuth state |
| `TokenExchangeError` | Token exchange with the PDS failed (has `status` and `body`) |
| `ResolutionError` | Handle or DID resolution failed |

```typescript
import { ApiError } from "@happyview/oauth-client";

try {
  await client.registerSession(params);
} catch (err) {
  if (err instanceof ApiError) {
    console.error(`API error ${err.status}:`, err.body);
  }
}
```

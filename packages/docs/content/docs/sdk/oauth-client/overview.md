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

## Client assertions

If your app is a confidential atproto OAuth client — its `client_id_url` document publishes `token_endpoint_auth_method: "private_key_jwt"` and a `jwks_uri` pointing back at HappyView — the PDS requires a signed `private_key_jwt` assertion at two points during the OAuth flow: the pushed authorization request (PAR) and the token exchange. HappyView holds the signing key on your behalf, so ask it to sign each one:

```typescript
const { clientAssertion, clientAssertionType } =
  await client.getClientAssertion(pdsIssuer);
```

`pdsIssuer` is the `issuer` field from the PDS authorization server's metadata. Attach `clientAssertion` and `clientAssertionType` as the `client_assertion` and `client_assertion_type` form parameters on the request.

<Callout type="warn">
Call this once for the PAR and once more for the token exchange — never reuse a single assertion for both. Each one is valid for only 60 seconds and carries a unique `jti`, so an app that mints one and reuses it fails at whichever call comes second.
</Callout>

Public clients don't need this — PKCE is their proof of possession, and `pkceVerifier` from `provisionDpopKey` covers it. `getClientAssertion` is unrelated to `isConfidential` on this class, which only describes whether this SDK instance authenticates *to HappyView* with a client secret, not whether your app is a confidential OAuth client to a user's PDS.

See [API Clients — Confidential clients: attaching the client assertion](../../guides/api-clients.md#confidential-clients-attaching-the-client-assertion) for the endpoint this wraps and the full manual flow.

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

This deletes the session from HappyView and local storage, and revokes it at the user's PDS.

That last part matters: without it the session stays listed under the account's active sessions on their PDS until its refresh token expires — up to two years — even though they logged out. Revocation is best-effort, so a PDS that is unreachable or does not implement RFC 7009 will not make the logout fail.

### Signing in again replaces the previous session

Each login runs a full OAuth authorization and so creates a *new* session on the user's PDS; the DPoP key is minted fresh and the old key is overwritten in storage. Left alone, that means every re-login strands the previous session on their account with no way to reach it.

`registerSession` therefore retires this client's previous session for the same account as part of completing a login — revoking it at the PDS while the credentials to do so still exist. It is scoped to the account being signed into, so sessions for other accounts, and sessions belonging to the same account on other devices, are untouched. No `onSessionDelete` hook fires: the user is signing in, not out.

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

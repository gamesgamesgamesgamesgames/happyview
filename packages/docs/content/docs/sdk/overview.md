---
title: "Overview"
---

HappyView provides JavaScript packages for building third-party apps that authenticate with a HappyView instance and make XRPC requests on behalf of users.

| Package                                                                                       | Purpose                                                                                                                    |
| --------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| [`@happyview/lex-agent`](https://npmx.dev/package/@happyview/lex-agent)                       | Recommended — type-safe XRPC via [`@atproto/lex`](https://npmx.dev/package/@atproto/lex) `Client` with HappyView DPoP auth |
| [`@happyview/oauth-client`](https://npmx.dev/package/@happyview/oauth-client)                 | Platform-agnostic core — DPoP key provisioning, session management, authenticated fetch                                    |
| [`@happyview/oauth-client-browser`](https://npmx.dev/package/@happyview/oauth-client-browser) | Browser OAuth wrapper for apps already using `@atproto/oauth-client-browser`                                               |
| [`@happyview/oauth-client-node`](https://npmx.dev/package/@happyview/oauth-client-node)       | Node.js OAuth client for server-side apps, matching `@atproto/oauth-client-node`                                           |

## Which package do I need?

**Starting a new app?** Use `@happyview/lex-agent` with `@atproto/lex`. It gives you type-safe XRPC calls through a `Client` that routes requests to your HappyView instance with DPoP authentication. This is the recommended way to interact with HappyView from JavaScript.

**Already using `@atproto/api`?** `HappyViewSession` works directly as a session manager for `@atproto/api`'s `Agent` — just pass it to `new Agent(session)`. See [Using with @atproto/api](./oauth-client-browser/overview.md#using-with-atprotoapi).

**Already using `@atproto/oauth-client-browser`?** Add `@happyview/oauth-client-browser` to get a `HappyViewBrowserClient` that handles the HappyView-specific DPoP key provisioning and session registration on top of the standard atproto OAuth flow.

**Building a server-side (Node.js) app?** Use `@happyview/oauth-client-node` — it handles handle resolution, DID resolution, PDS discovery, and the full OAuth flow server-side. Matches the API surface of `@atproto/oauth-client-node`.

**Building something more custom?** Use `@happyview/oauth-client` directly and provide your own `CryptoAdapter` and `StorageAdapter`.

## How it works

Third-party apps authenticate using HappyView's [DPoP key provisioning](../getting-started/authentication.md#dpop-key-provisioning-for-third-party-apps) flow:

1. The SDK requests a DPoP keypair from the HappyView instance.
2. Your app runs a standard atproto OAuth flow with the user's PDS using that keypair.
3. The SDK registers the resulting tokens with HappyView.
4. All subsequent XRPC requests are authenticated with DPoP proofs — HappyView handles its own lexicons locally and proxies standard atproto writes to the user's PDS.

If your app is a confidential atproto OAuth client, step 2 also needs a signed `private_key_jwt` assertion for its PAR and token-exchange requests. `@happyview/oauth-client`'s `getClientAssertion()` mints one — see [OAuth Client — Client assertions](./oauth-client/overview.md#client-assertions).

## Quick start

```bash
npm install @happyview/lex-agent @happyview/oauth-client-browser @atproto/lex
```

```typescript
import { Client } from "@atproto/lex";
import { HappyViewBrowserClient } from "@happyview/oauth-client-browser";
import { createAgent } from "@happyview/lex-agent";

// Set up the OAuth client
const oauthClient = new HappyViewBrowserClient({
  instanceUrl: "https://happyview.example.com",
  clientId: "https://example.com/oauth-client-metadata.json",
  clientKey: "hvc_your_client_key",
});

// Sign in — redirects to the user's PDS
await oauthClient.signIn("alice.bsky.social");

// On page load — restore session or process callback
const result = await oauthClient.init();
const session = result?.session;

// Create a type-safe Lex client
const agent = createAgent(session);
const lex = new Client(agent);

// Make type-safe XRPC calls
const result = await lex.xrpc(myLexicons.com.example.getGame, {
  params: { slug: "celeste" },
});
```

## Next steps

- [Lex Agent](./lex-agent/overview.md): type-safe XRPC with `@atproto/lex`
- [OAuth Client](./oauth-client/overview.md): platform-agnostic core client
- [Browser Client](./oauth-client-browser/overview.md): browser OAuth redirect flow
- [Node Client](./oauth-client-node/overview.md): server-side OAuth flow
- [Authentication](../getting-started/authentication.md): full details on DPoP key provisioning and API client types

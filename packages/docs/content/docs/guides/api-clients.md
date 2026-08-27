---
title: "API Clients"
---

API clients identify your application to a HappyView instance. Every XRPC request — even unauthenticated queries — must include a client key. This guide walks through creating a client, choosing between public and confidential types, and authenticating users.

For the admin CRUD endpoints, see the [API reference](../api-reference/admin/api-clients.md). For the JavaScript SDK, see the [SDK docs](../sdk/overview.md).

## Concepts

An API client represents **your application**, not individual users. Create one client for your app and use the same client key everywhere. Users authenticate separately via OAuth — the client key identifies _who built the app_, not _who is using it_.

Each client has:

- An `hvc_`-prefixed **client key** — included in every request to identify your app
- An `hvs_`-prefixed **client secret** — used by server-side apps to prove ownership (confidential clients only)
- **Rate limits** — a token bucket that controls how many requests your app can make
- **Scopes** — which lexicons your app is allowed to access

## Public vs. confidential clients

Choose based on where your code runs:

|                        | Confidential                               | Public                                      |
| ---------------------- | ------------------------------------------ | ------------------------------------------- |
| **Use when**           | Server-side apps, CLI tools, bots          | Browser apps, mobile apps                   |
| **Authentication**     | `X-Client-Key` + `X-Client-Secret` headers | `X-Client-Key` + `Origin` header + PKCE     |
| **Can keep a secret?** | Yes                                        | No                                          |
| **Origin validation**  | No                                         | Yes — `Origin` must match `allowed_origins` |
| **PKCE required?**     | No                                         | Yes (S256)                                  |

<Callout type="idea">
If your app has a backend that can securely store the client secret, use a confidential client even if the frontend is a browser app. The backend can proxy OAuth operations.
</Callout>

## Creating a client

### From the dashboard

Go to **Settings > API Clients > New client** and fill in:

- **Client type** — `confidential` (default) or `public`
- **Name** — a human-readable label (e.g. "My atproto Client")
- **Client ID URL** — URL to your published [OAuth client metadata](https://drafts.aaronpk.com/draft-parecki-oauth-client-id-metadata-document/draft-parecki-oauth-client-id-metadata-document.html) document
- **Client URI** — your app's root domain (e.g. https://example.com)
- **Redirect URIs** — where the PDS should redirect after authorization
- **Allowed origins** — (public clients only) which `Origin` headers to accept
- **Scopes** — `atproto` is always included; add custom scopes if your instance uses them

**Save the client secret immediately.** It is only shown once and is hashed before storage.

### From the API

```ts tab="TypeScript" tab-group="language"
const TOKEN = "hv_..."; // your API key

interface ClientResponse {
  id: string;
  client_key: string;
  client_secret?: string;
  name: string;
  client_id_url: string;
  client_uri: string;
  redirect_uris: string[];
  client_type: string;
  allowed_origins: string[];
}

const response = await fetch("http://127.0.0.1:3000/admin/api-clients", {
  method: "POST",
  headers: {
    Authorization: `Bearer ${TOKEN}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    name: "My atproto Client",
    client_id_url: "https://example.com/client-metadata.json",
    client_uri: "https://example.com",
    redirect_uris: ["https://example.com/oauth/callback"],
    client_type: "public",
    allowed_origins: ["https://example.com"],
  }),
});

const client: ClientResponse = await response.json();
```
```js tab="JavaScript" tab-group="language"
const TOKEN = "hv_..."; // your API key

const response = await fetch("http://127.0.0.1:3000/admin/api-clients", {
  method: "POST",
  headers: {
    Authorization: `Bearer ${TOKEN}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    name: "My atproto Client",
    client_id_url: "https://example.com/client-metadata.json",
    client_uri: "https://example.com",
    redirect_uris: ["https://example.com/oauth/callback"],
    client_type: "public",
    allowed_origins: ["https://example.com"],
  }),
});

const client = await response.json();
```
```rust tab="Rust" tab-group="language"
let client = reqwest::Client::new();
let token = "hv_..."; // your API key

let response = client
    .post("http://127.0.0.1:3000/admin/api-clients")
    .bearer_auth(token)
    .json(&serde_json::json!({
        "name": "My atproto Client",
        "client_id_url": "https://example.com/client-metadata.json",
        "client_uri": "https://example.com",
        "redirect_uris": ["https://example.com/oauth/callback"],
        "client_type": "public",
        "allowed_origins": ["https://example.com"]
    }))
    .send()
    .await?;

let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
token := "hv_..." // your API key

body := bytes.NewBufferString(`{
  "name": "My atproto Client",
  "client_id_url": "https://example.com/client-metadata.json",
  "client_uri": "https://example.com",
  "redirect_uris": ["https://example.com/oauth/callback"],
  "client_type": "public",
  "allowed_origins": ["https://example.com"]
}`)

req, _ := http.NewRequest("POST", "http://127.0.0.1:3000/admin/api-clients", body)
req.Header.Set("Authorization", "Bearer "+token)
req.Header.Set("Content-Type", "application/json")

resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST http://127.0.0.1:3000/admin/api-clients \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "My atproto Client",
    "client_id_url": "https://example.com/client-metadata.json",
    "client_uri": "https://example.com",
    "redirect_uris": ["https://example.com/oauth/callback"],
    "client_type": "public",
    "allowed_origins": ["https://example.com"]
  }'
```

See the [API reference](../api-reference/admin/api-clients.md#create-an-api-client) for all fields.

## Using your client key

Every XRPC request must include the client key. HappyView looks for it in this order:

1. `X-Client-Key` request header (preferred)
2. `client_key` query parameter

### Unauthenticated queries

For public queries that don't need a user identity:

```ts tab="TypeScript" tab-group="language"
const CLIENT_KEY = "hvc_a1b2c3..."; // your API client key

const response = await fetch(
  "https://happyview.example.com/xrpc/com.example.feed.getHot",
  { headers: { "X-Client-Key": CLIENT_KEY } },
);
```
```js tab="JavaScript" tab-group="language"
const CLIENT_KEY = "hvc_a1b2c3..."; // your API client key

const response = await fetch(
  "https://happyview.example.com/xrpc/com.example.feed.getHot",
  { headers: { "X-Client-Key": CLIENT_KEY } },
);
```
```rust tab="Rust" tab-group="language"
let client = reqwest::Client::new();
let client_key = "hvc_a1b2c3..."; // your API client key

let response = client
    .get("https://happyview.example.com/xrpc/com.example.feed.getHot")
    .header("X-Client-Key", client_key)
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
clientKey := "hvc_a1b2c3..." // your API client key

req, _ := http.NewRequest("GET",
  "https://happyview.example.com/xrpc/com.example.feed.getHot", nil)
req.Header.Set("X-Client-Key", clientKey)

resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl 'https://happyview.example.com/xrpc/com.example.feed.getHot' \
  -H 'X-Client-Key: hvc_a1b2c3...'
```

Server-side callers should also include the secret (since there's no origin to authenticate):

```ts tab="TypeScript" tab-group="language"
const CLIENT_SECRET = "hvs_d4e5f6..."; // your API client secret

const response = await fetch(
  "https://happyview.example.com/xrpc/com.example.feed.getHot",
  {
    headers: {
      "X-Client-Key": "hvc_a1b2c3...",
      "X-Client-Secret": CLIENT_SECRET,
    },
  },
);
```
```js tab="JavaScript" tab-group="language"
const CLIENT_SECRET = "hvs_d4e5f6..."; // your API client secret

const response = await fetch(
  "https://happyview.example.com/xrpc/com.example.feed.getHot",
  {
    headers: {
      "X-Client-Key": "hvc_a1b2c3...",
      "X-Client-Secret": CLIENT_SECRET,
    },
  },
);
```
```rust tab="Rust" tab-group="language"
let client_secret = "hvs_d4e5f6..."; // your API client secret

let response = client
    .get("https://happyview.example.com/xrpc/com.example.feed.getHot")
    .header("X-Client-Key", client_key)
    .header("X-Client-Secret", client_secret)
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
clientSecret := "hvs_d4e5f6..." // your API client secret

req, _ := http.NewRequest("GET",
  "https://happyview.example.com/xrpc/com.example.feed.getHot", nil)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("X-Client-Secret", clientSecret)

resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl 'https://happyview.example.com/xrpc/com.example.feed.getHot' \
  -H 'X-Client-Key: hvc_a1b2c3...' \
  -H 'X-Client-Secret: hvs_d4e5f6...'
```

### Authenticated requests (user identity)

Procedures — and queries whose scripts need to know who the caller is — require a user's OAuth session. This uses [DPoP authentication](../getting-started/authentication.md#dpop-key-provisioning-for-third-party-apps), where each request includes a cryptographic proof that the caller holds the right key.

```ts tab="TypeScript" tab-group="language"
const CLIENT_KEY = "hvc_..."; // your API client key
const ACCESS_TOKEN = "..."; // DPoP access token
const DPOP_PROOF = "..."; // DPoP proof JWT

const response = await fetch(
  "https://happyview.example.com/xrpc/com.example.createPost",
  {
    method: "POST",
    headers: {
      "X-Client-Key": CLIENT_KEY,
      Authorization: `DPoP ${ACCESS_TOKEN}`,
      DPoP: DPOP_PROOF,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ text: "Hello world" }),
  },
);
```
```js tab="JavaScript" tab-group="language"
const CLIENT_KEY = "hvc_..."; // your API client key
const ACCESS_TOKEN = "..."; // DPoP access token
const DPOP_PROOF = "..."; // DPoP proof JWT

const response = await fetch(
  "https://happyview.example.com/xrpc/com.example.createPost",
  {
    method: "POST",
    headers: {
      "X-Client-Key": CLIENT_KEY,
      Authorization: `DPoP ${ACCESS_TOKEN}`,
      DPoP: DPOP_PROOF,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ text: "Hello world" }),
  },
);
```
```rust tab="Rust" tab-group="language"
let client_key = "hvc_..."; // your API client key
let access_token = "..."; // DPoP access token
let dpop_proof = "..."; // DPoP proof JWT

let response = client
    .post("https://happyview.example.com/xrpc/com.example.createPost")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", dpop_proof)
    .json(&serde_json::json!({ "text": "Hello world" }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
clientKey := "hvc_..."  // your API client key
accessToken := "..."    // DPoP access token
dpopProof := "..."      // DPoP proof JWT

body := bytes.NewBufferString(`{"text": "Hello world"}`)

req, _ := http.NewRequest("POST",
  "https://happyview.example.com/xrpc/com.example.createPost", body)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
req.Header.Set("Content-Type", "application/json")

resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST 'https://happyview.example.com/xrpc/com.example.createPost' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <access_token>' \
  -H 'DPoP: <proof_jwt>' \
  -H 'Content-Type: application/json' \
  -d '{"text": "Hello world"}'
```

## Authenticating users

### Using the JavaScript SDK

The SDK handles the entire DPoP flow. A complete browser example:

```typescript
import { HappyViewBrowserClient } from "@happyview/oauth-client-browser";

const client = new HappyViewBrowserClient({
  instanceUrl: "https://happyview.example.com",
  clientKey: "hvc_your_client_key",
});

// Sign in — redirects to the user's PDS
await client.signIn("alice.bsky.social");
```

On page load, restore a session or process the OAuth callback:

```typescript
const result = await client.init();
if (result) {
  const { session } = result;

  // Make authenticated requests
  const response = await session.fetchHandler(
    "/xrpc/com.example.getStuff?limit=10",
    { method: "GET" },
  );
}
```

For server-side Node.js apps, use the core [`@happyview/oauth-client`](../sdk/oauth-client.md) package with a confidential client. For type-safe XRPC calls, pair either client with [`@happyview/lex-agent`](../sdk/lex-agent.md).

### Manual DPoP flow

If you're not using JavaScript, or want to understand the protocol, the DPoP flow has four phases.

#### Phase 1: Provision a DPoP key

Ask HappyView for an ES256 keypair that will be shared between your app and the instance.

**Confidential client:**

```http
POST /oauth/dpop-keys
X-Client-Key: hvc_...
X-Client-Secret: hvs_...
Content-Type: application/json

{}
```

**Public client:**

```http
POST /oauth/dpop-keys
X-Client-Key: hvc_...
Origin: https://example.com
Content-Type: application/json

{"pkce_challenge": "<base64url-encoded S256 challenge>"}
```

**Response:**

```json
{
  "provision_id": "hvp_...",
  "dpop_key": {
    "kty": "EC",
    "crv": "P-256",
    "x": "...",
    "y": "...",
    "d": "..."
  }
}
```

The `dpop_key` is the full private JWK. Store it securely — you'll use it to sign DPoP proofs.

#### Phase 2: OAuth with the user's PDS

Run a standard atproto OAuth flow with the user's PDS authorization server, using the provisioned DPoP key as your keypair.

1. Resolve the user's handle to a DID
2. Resolve the DID document to find the PDS URL
3. Fetch the PDS's OAuth authorization server metadata
4. Redirect the user to the PDS authorization endpoint
5. Exchange the authorization code for tokens (using DPoP proofs signed with the provisioned key)

HappyView is not involved in any of this — unless your `client_id_url` document declares `private_key_jwt`, in which case it holds your app's signing key and needs to be asked for a signed assertion at two separate points below.

##### Confidential clients: attaching the client assertion

Publishing `token_endpoint_auth_method: "private_key_jwt"` (and a `jwks_uri` pointing back at HappyView) on your `client_id_url` document makes your app a confidential atproto OAuth client, which the PDS trusts with much longer-lived sessions than a public client gets. HappyView can hold the signing key for you — generate one from **Settings > API Clients > (your client) > AT Protocol Client Auth** in the dashboard, or via `POST /admin/api-clients/{id}/auth-key` — and use `POST /oauth/client-assertion` to sign on your behalf.

<Callout type="warn">
The assertion is required **at both the pushed authorization request (PAR) and the token exchange** — these are two separate requests to the PDS's authorization server, each needing its own freshly signed assertion (they expire after 60 seconds). Signing once and reusing it for both calls will be rejected by the PDS.
</Callout>

```http
POST /oauth/client-assertion
X-Client-Key: hvc_...
X-Client-Secret: hvs_...
Content-Type: application/json

{"issuer": "https://bsky.social"}
```

**Response:**

```json
{
  "client_assertion": "eyJhbG...",
  "client_assertion_type": "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
  "expires_in": 60
}
```

`issuer` is the `issuer` field from the PDS authorization server's metadata (step 3 above). Call this endpoint once before submitting the PAR request, attaching `client_assertion` and `client_assertion_type` as form parameters alongside your other PAR fields, and again before the token exchange request, attaching them there too. Public clients skip this entirely — PKCE is their proof of possession.

There is no flag anywhere marking your client confidential — the PDS decides by reading your `client_id_url` document, and `/oauth/client-assertion` will sign for you regardless of whether that document is actually correct yet. If it isn't, the PDS ends up treating your app as public even though you're sending an assertion, which is confusing to debug from the outside. Check the "AT Protocol Client Auth" card in the dashboard (or `POST /admin/api-clients/{id}/auth-key/recheck`) — it reads the same document and reports in plain language exactly what's wrong with it.

#### Phase 3: Register the session

After the OAuth callback, register the token set with HappyView so it can proxy requests on behalf of the user.

**Confidential client:**

```http
POST /oauth/sessions
X-Client-Key: hvc_...
X-Client-Secret: hvs_...
Content-Type: application/json

{
  "provision_id": "hvp_...",
  "did": "did:plc:user123",
  "access_token": "...",
  "refresh_token": "...",
  "expires_at": "2026-04-17T00:00:00Z",
  "scopes": "atproto transition:generic",
  "pds_url": "https://bsky.social",
  "issuer": "https://bsky.social"
}
```

**Public client** — omit the secret, include the PKCE verifier:

```http
POST /oauth/sessions
X-Client-Key: hvc_...
Content-Type: application/json

{
  "provision_id": "hvp_...",
  "pkce_verifier": "...",
  "did": "did:plc:user123",
  "access_token": "...",
  "refresh_token": "...",
  "expires_at": "2026-04-17T00:00:00Z",
  "scopes": "atproto transition:generic",
  "pds_url": "https://bsky.social",
  "issuer": "https://bsky.social"
}
```

#### Phase 4: Make authenticated XRPC requests

With a registered session, sign each request with a DPoP proof:

```ts tab="TypeScript" tab-group="language"
const CLIENT_KEY = "hvc_..."; // your API client key
const ACCESS_TOKEN = "..."; // DPoP access token
const DPOP_PROOF = "..."; // DPoP proof JWT

const response = await fetch(
  "https://happyview.example.com/xrpc/com.example.createPost",
  {
    method: "POST",
    headers: {
      "X-Client-Key": CLIENT_KEY,
      Authorization: `DPoP ${ACCESS_TOKEN}`,
      DPoP: DPOP_PROOF,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ text: "Hello world" }),
  },
);
```
```js tab="JavaScript" tab-group="language"
const CLIENT_KEY = "hvc_..."; // your API client key
const ACCESS_TOKEN = "..."; // DPoP access token
const DPOP_PROOF = "..."; // DPoP proof JWT

const response = await fetch(
  "https://happyview.example.com/xrpc/com.example.createPost",
  {
    method: "POST",
    headers: {
      "X-Client-Key": CLIENT_KEY,
      Authorization: `DPoP ${ACCESS_TOKEN}`,
      DPoP: DPOP_PROOF,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ text: "Hello world" }),
  },
);
```
```rust tab="Rust" tab-group="language"
let client_key = "hvc_..."; // your API client key
let access_token = "..."; // DPoP access token
let dpop_proof = "..."; // DPoP proof JWT

let response = client
    .post("https://happyview.example.com/xrpc/com.example.createPost")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", dpop_proof)
    .json(&serde_json::json!({ "text": "Hello world" }))
    .send()
    .await?;
```
```go tab="Go" tab-group="language"
clientKey := "hvc_..."  // your API client key
accessToken := "..."    // DPoP access token
dpopProof := "..."      // DPoP proof JWT

body := bytes.NewBufferString(`{"text": "Hello world"}`)

req, _ := http.NewRequest("POST",
  "https://happyview.example.com/xrpc/com.example.createPost", body)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
req.Header.Set("Content-Type", "application/json")

resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST 'https://happyview.example.com/xrpc/com.example.createPost' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <access_token>' \
  -H 'DPoP: <proof_jwt>' \
  -H 'Content-Type: application/json' \
  -d '{"text": "Hello world"}'
```

HappyView validates the proof, looks up the stored session, and proxies writes to the user's PDS using the shared DPoP key.

#### Logout

**Confidential:**

```http
DELETE /oauth/sessions/did:plc:user123
X-Client-Key: hvc_...
X-Client-Secret: hvs_...
```

**With a DPoP proof** (any client type — proves key possession, revokes just that device's session):

```http
DELETE /oauth/sessions/did:plc:user123
X-Client-Key: hvc_...
Authorization: DPoP <access_token>
DPoP: <proof_jwt>
```

The proof's `htu` must equal the URL as sent, including any percent-encoding in the DID.

### DPoP proof format

If you're implementing the flow without the SDK, a DPoP proof JWT looks like this:

**Header:**

```json
{
  "alg": "ES256",
  "typ": "dpop+jwt",
  "jwk": {
    "kty": "EC",
    "crv": "P-256",
    "x": "...",
    "y": "..."
  }
}
```

**Payload:**

```json
{
  "htm": "POST",
  "htu": "https://happyview.example.com/xrpc/com.example.createPost",
  "iat": 1745452800,
  "ath": "<base64url SHA-256 of the access token>",
  "jti": "<unique identifier>"
}
```

Validation rules:

- `htm` must match the HTTP method (case-insensitive)
- `htu` must match the request URL (scheme + host + path, no query string)
- `iat` must be within 5 minutes of the server's clock
- `ath` must be the base64url-encoded SHA-256 hash of the access token
- The JWK thumbprint (RFC 7638, SHA-256) must match the key used during provisioning
- The signature must verify against the embedded public JWK

## Scopes

By default, a client's scopes are just `atproto`. You can add custom scopes when creating or updating the client.

### Permission sets

HappyView supports an `include:` directive that expands permission sets defined in lexicons. For example, if your instance has a lexicon `com.example.authBasic` with a `permissions` array in its definition, you can set the client's scopes to:

```
atproto include:com.example.authBasic
```

This expands to include all RPC methods and repository actions defined in that permission set.

## Rate limiting

Each API client has its own token bucket for rate limiting:

- **Capacity** — maximum tokens in the bucket
- **Refill rate** — tokens added per second

If not set on the client, the instance defaults apply (`DEFAULT_RATE_LIMIT_CAPACITY` and `DEFAULT_RATE_LIMIT_REFILL_RATE`).

Rate limit state is returned in response headers:

| Header                | Description                                 |
| --------------------- | ------------------------------------------- |
| `RateLimit-Limit`     | Bucket capacity                             |
| `RateLimit-Remaining` | Tokens remaining                            |
| `RateLimit-Reset`     | Unix timestamp when the bucket will be full |
| `Retry-After`         | Seconds to wait (only on `429` responses)   |

Adjust per-client rate limits via the dashboard or the [admin API](../api-reference/admin/api-clients.md#update-an-api-client).

## Security notes

- Client secrets are SHA-256 hashed before storage — HappyView never stores the plaintext.
- DPoP private keys and OAuth tokens are encrypted at rest with AES-256-GCM using the `TOKEN_ENCRYPTION_KEY` environment variable.
- Re-authenticating the same user with the same client upserts the session. The old DPoP key is cleaned up automatically.
- Multiple clients can have active sessions for the same user — sessions are isolated per client.

## Next steps

- [Authentication](../getting-started/authentication.md) — full protocol details and security model
- [JavaScript SDK](../sdk/overview.md) — get started with the SDK
- [Admin API — API Clients](../api-reference/admin/api-clients.md) — CRUD endpoints
- [Permissions](./permissions.md) — control who can manage API clients

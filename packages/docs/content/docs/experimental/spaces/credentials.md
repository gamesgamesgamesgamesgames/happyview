---
title: "Credentials"
---

<Callout type="error" title="Experimental">
This API is experimental and will change. See the [Permissioned Spaces overview](../spaces.md) for context.
</Callout>

Space credentials are short-lived JWTs for cross-service access to space data. A member requests a delegation token to prove their membership, exchanges the token for a credential JWT, then passes it to an external service that needs to read the space's records.

## How credentials work

Credential issuance is a two-step process. The delegation token is a short-lived proof of membership (60-second TTL), and the credential is the bearer token used for cross-service access (2-hour TTL).

```mermaid
sequenceDiagram
    participant App as Client App
    participant HV as HappyView
    participant Svc as External Service

    App->>HV: GET com.atproto.space.getDelegationToken<br/>(DPoP auth, must be a member)
    HV->>HV: Verify membership
    HV-->>App: delegation token + expiresAt

    App->>HV: POST com.atproto.space.getSpaceCredential<br/>(DPoP auth, delegation token)
    HV->>HV: Verify delegation token<br/>Check app access<br/>Sign credential with space keypair
    HV-->>App: credential JWT + expiresAt

    App->>Svc: Request with Authorization: Bearer credential
    Svc->>HV: Read space records<br/>(Bearer credential)
    HV->>HV: Verify credential signature<br/>via space DID doc
    HV-->>Svc: Record data
```

Credentials are ES256 JWTs signed with a P-256 keypair unique to each space. The keypair is generated on first credential request and stored encrypted (AES-256-GCM).

## Step 1: Get a delegation token

The caller must be an authenticated member of the space. The delegation token is a short-lived proof of membership (60-second TTL).

Note: this endpoint is a GET request (not POST). The previous `getMemberGrant` endpoint (POST) is available as a legacy alias via `dev.happyview.space.getMemberGrant`.

```ts tab="TypeScript" tab-group="language"
const params = new URLSearchParams({
  space: "ats://did:plc:abc123/com.example.forum/main",
});
const response = await fetch(`https://happyview.example.com/xrpc/com.atproto.space.getDelegationToken?${params}`, {
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
  },
});
interface DelegationTokenResponse {
  delegationToken: string;
  expiresAt: string;
}
const data: DelegationTokenResponse = await response.json();
```
```js tab="JavaScript" tab-group="language"
const params = new URLSearchParams({
  space: "ats://did:plc:abc123/com.example.forum/main",
});
const response = await fetch(`https://happyview.example.com/xrpc/com.atproto.space.getDelegationToken?${params}`, {
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
  },
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("https://happyview.example.com/xrpc/com.atproto.space.getDelegationToken")
    .query(&[("space", "ats://did:plc:abc123/com.example.forum/main")])
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET",
  "https://happyview.example.com/xrpc/com.atproto.space.getDelegationToken?space=ats%3A%2F%2Fdid%3Aplc%3Aabc123%2Fcom.example.forum%2Fmain",
  nil)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl 'https://happyview.example.com/xrpc/com.atproto.space.getDelegationToken?space=ats%3A%2F%2Fdid%3Aplc%3Aabc123%2Fcom.example.forum%2Fmain' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>'
```

**Response:**

```json
{
  "delegationToken": "eyJhbGciOiJFUzI1NktFWSJ9...",
  "expiresAt": "2026-05-09T12:01:00Z"
}
```

## Step 2: Get a space credential

Exchange the delegation token for a space credential JWT. The credential is signed by the space's keypair and has a 2-hour TTL.

```ts tab="TypeScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.space.getSpaceCredential", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    grant: "eyJhbGciOiJFUzI1NktFWSJ9...",
  }),
});
interface CredentialResponse {
  credential: string;
  expiresAt: string;
}
const data: CredentialResponse = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch("https://happyview.example.com/xrpc/com.atproto.space.getSpaceCredential", {
  method: "POST",
  headers: {
    "X-Client-Key": CLIENT_KEY,
    "Authorization": `DPoP ${ACCESS_TOKEN}`,
    "DPoP": DPOP_PROOF,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    grant: "eyJhbGciOiJFUzI1NktFWSJ9...",
  }),
});
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .post("https://happyview.example.com/xrpc/com.atproto.space.getSpaceCredential")
    .header("X-Client-Key", client_key)
    .header("Authorization", format!("DPoP {}", access_token))
    .header("DPoP", &dpop_proof)
    .json(&serde_json::json!({
        "grant": "eyJhbGciOiJFUzI1NktFWSJ9..."
    }))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
body := bytes.NewBufferString(`{"grant": "eyJhbGciOiJFUzI1NktFWSJ9..."}`)
req, _ := http.NewRequest("POST",
  "https://happyview.example.com/xrpc/com.atproto.space.getSpaceCredential", body)
req.Header.Set("X-Client-Key", clientKey)
req.Header.Set("Authorization", "DPoP "+accessToken)
req.Header.Set("DPoP", dpopProof)
req.Header.Set("Content-Type", "application/json")
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl -X POST 'https://happyview.example.com/xrpc/com.atproto.space.getSpaceCredential' \
  -H 'X-Client-Key: hvc_...' \
  -H 'Authorization: DPoP <token>' \
  -H 'DPoP: <proof>' \
  -H 'Content-Type: application/json' \
  -d '{
    "grant": "eyJhbGciOiJFUzI1NktFWSJ9..."
  }'
```

**Response:**

```json
{
  "credential": "eyJhbGciOiJFUzI1NiJ9...",
  "expiresAt": "2026-05-09T14:00:00Z"
}
```

### Credential claims

The JWT payload contains:

| Claim | Description |
|---|---|
| `iss` | The space authority's DID (who signed it) |
| `sub` | The full `ats://` space URI |
| `iat` | Issued at (Unix timestamp) |
| `exp` | Expiry (Unix timestamp) |
| `jti` | Random nonce for replay protection |

## Using a credential

Pass the credential as a standard Bearer token in the `Authorization` header. HappyView distinguishes space credentials from other tokens by checking the JWT header's `typ` field (`atproto-space-credential+jwt`).

```ts tab="TypeScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/com.atproto.space.getRecord?space=...&collection=...&rkey=...",
  {
    headers: {
      "Authorization": `Bearer ${SPACE_CREDENTIAL}`,
    },
  },
);
const data = await response.json();
```
```js tab="JavaScript" tab-group="language"
const response = await fetch(
  "https://happyview.example.com/xrpc/com.atproto.space.getRecord?space=...&collection=...&rkey=...",
  {
    headers: {
      "Authorization": `Bearer ${SPACE_CREDENTIAL}`,
    },
  },
);
const data = await response.json();
```
```rust tab="Rust" tab-group="language"
let response = client
    .get("https://happyview.example.com/xrpc/com.atproto.space.getRecord")
    .query(&[("space", "..."), ("collection", "..."), ("rkey", "...")])
    .header("Authorization", format!("Bearer {}", space_credential))
    .send()
    .await?;
let data: serde_json::Value = response.json().await?;
```
```go tab="Go" tab-group="language"
req, _ := http.NewRequest("GET",
  "https://happyview.example.com/xrpc/com.atproto.space.getRecord?space=...&collection=...&rkey=...",
  nil)
req.Header.Set("Authorization", "Bearer "+spaceCredential)
resp, err := http.DefaultClient.Do(req)
```
```sh tab="cURL" tab-group="language"
curl 'https://happyview.example.com/xrpc/com.atproto.space.getRecord?space=...&collection=...&rkey=...' \
  -H 'Authorization: Bearer eyJhbGciOiJFUzI1NiIsInR5cCI6InNwYWNlX2NyZWRlbnRpYWwifQ...'
```

No DPoP auth or client key is needed when authenticating via space credential — the credential itself is sufficient. The `sub` claim identifies the space being accessed.

HappyView verifies the credential by resolving the issuer's DID document, extracting the `#atproto_space` signing key, and validating the JWT signature and expiry. If valid, the request is granted read access to the space identified by `sub`.

## App access control

Before issuing a credential, HappyView checks whether the calling app (identified by its DPoP client key) is allowed to access the space:

- **`open` (default)**: any app can get credentials
- **`allowList`**: only apps whose client metadata URL appears in the `allowed` array can get credentials

For `open` spaces, requests without a client key are allowed. For `allowList` spaces, a client key is required — requests without one are rejected.

## External credential verification

HappyView can also verify credentials issued by *other* HappyView instances or space-aware services. When a Bearer space credential is presented, HappyView:

1. Decodes the JWT without verification to extract the `iss` (issuer DID)
2. Resolves the issuer's DID document
3. Extracts the `#atproto_space` signing key from the DID doc
4. Verifies the JWT signature and expiry
5. Checks that the `sub` claim matches the requested space

A credential issued by one instance can be used to read from another instance that hosts the same space's data.

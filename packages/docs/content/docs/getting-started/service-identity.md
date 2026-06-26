---
title: "Service Identity"
---

An AT Protocol service identity lets your AppView authenticate itself to other services on the network. When a user's PDS routes a request, it verifies the destination by resolving the AppView's DID — without a service identity, standard AT Protocol app routing won't reach your instance.

HappyView can operate without a service identity using its built-in auth, but configuring one is recommended for full network compatibility.

## Identity modes

HappyView supports three ways to establish a service identity during setup.

### Domain identity (did:web)

Your domain name becomes your identity. HappyView generates a signing keypair and serves a [DID document](https://atproto.com/specs/did#did-web) at `/.well-known/did.json` automatically.

This is the simplest option — no external registration is needed. The identity is tied to your domain: if you change domains, you'll need to reconfigure.

### Network identity (did:plc)

HappyView registers a new identity in the [PLC directory](https://atproto.com/specs/did#did-plc), a public registry that maps DIDs to their metadata. This is the most durable option — the identity survives domain changes because it isn't tied to any single hostname.

During registration, HappyView generates two keypairs:

- **Signing key** — Used to authenticate requests from your AppView. Stored encrypted on the server and managed automatically.
- **Rotation key** — Used to recover or update the identity if the signing key is lost or the server goes down. This key is generated once and must be downloaded immediately — it cannot be retrieved later.

Store the rotation key file somewhere safe and offline (e.g. a password manager, encrypted USB drive, or secure backup). You will need it if you ever need to migrate your identity to a new server or recover from data loss.

### Linked account

Link your AppView to an existing AT Protocol account you control. HappyView verifies ownership by redirecting you to sign in through that account's PDS, then uses the account's existing DID as the service identity.

## Choosing an identity mode

| | Domain (did:web) | Network (did:plc) | Linked account |
|---|---|---|---|
| Setup complexity | Automatic | Requires key backup | Requires existing account |
| Domain independence | No — tied to your domain | Yes — survives domain changes | Depends on the linked account |
| Key management | Automatic | You must back up the rotation key | Managed by the linked account's PDS |
| Best for | Single-domain deployments | Long-lived production instances | Operators who already have an AT Protocol presence |

## Skipping setup

You can skip service identity configuration during setup. Your AppView will work with HappyView's built-in authentication, but standard AT Protocol service-to-service routing won't be available. You can configure a service identity later from **Settings > Service Identity** in the dashboard.

## Further reading

- [AT Protocol identity specification](https://atproto.com/guides/identity)
- [DID methods in AT Protocol](https://atproto.com/specs/did)
- [AT Protocol glossary](https://atproto.com/guides/glossary)

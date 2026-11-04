---
title: "Rotating the encryption key"
---

This guide replaces `TOKEN_ENCRYPTION_KEY` on a running instance without losing anything it stored. It takes two restarts. Between them, HappyView keeps the old key as `TOKEN_ENCRYPTION_KEY_PREVIOUS` and re-seals every stored value under the new key on its own.

It covers a self-hosted instance. An instance run by a managed-hosting provider is rotated by that provider. To set a key on an instance that never had one, set `TOKEN_ENCRYPTION_KEY` and restart: HappyView seals the plaintext values it holds at the next start, and no rotation is needed.

## What the key protects

`TOKEN_ENCRYPTION_KEY` seals, with AES-256-GCM: DPoP keys and sessions, OAuth client keys, sessions and pending sign-in state, linked repository sessions, external account tokens, the service identity key, verification method keys, space DID keys, the attestation and delegation keys, script variables, and plugin secrets. A value sealed under one key opens only with that key.

## Before you start

- An API key with `settings:manage`, or a super user's, to read the rotation status:

  ```sh
  TOKEN="hv_..."
  BASE="https://happyview.example.com"
  ```

- A copy of the current key, kept somewhere other than the instance. You need it for step 1, and again to restore any backup taken before the rotation (see [Backups](#backups)).

## Steps

1. Generate a new key:

   ```sh
   openssl rand -base64 32
   ```

2. Set the new key as `TOKEN_ENCRYPTION_KEY`, and the current key as `TOKEN_ENCRYPTION_KEY_PREVIOUS`:

   ```sh
   TOKEN_ENCRYPTION_KEY=<new key>
   TOKEN_ENCRYPTION_KEY_PREVIOUS=<current key>
   ```

   Set both before the restart. Changing `TOKEN_ENCRYPTION_KEY` alone stops the instance booting, because it can no longer open its OAuth client key (see [Troubleshooting](#troubleshooting)).

3. Restart HappyView. At startup it counts the values only the previous key opens and starts a `happyview.reseal` background job to re-seal them under the new key. Writes made in the meantime are sealed under the new key, and a value changed while the job runs is left as written.

4. Wait for the re-seal to finish:

   ```sh
   curl -s "$BASE/admin/encryption/rotation" -H "Authorization: Bearer $TOKEN"
   ```

   It's finished when `previous_key_can_go` is `true`:

   ```json
   {
     "key_configured": true,
     "previous_key_configured": true,
     "previous_key_can_go": true,
     "remaining": 0,
     "unsealed": 0,
     "unreadable": 0,
     "stores": [ ... ],
     "job": null
   }
   ```

   The status reads every stored value each time it's called, so poll it every few seconds, not continuously. An instance with a handful of sealed values finishes within a second of starting; one with many takes as long as rewriting each sealed row, 200 at a time. See [Encryption rotation](../api-reference/admin/encryption.md) for each field.

5. Remove `TOKEN_ENCRYPTION_KEY_PREVIOUS` and restart HappyView. The instance boots with the new key alone.

## Verify it worked

After the second restart, the status shows the new key alone, with nothing left under the old one and nothing unreadable:

```sh
curl -s "$BASE/admin/encryption/rotation" -H "Authorization: Bearer $TOKEN"
```

```json
{ "key_configured": true, "previous_key_configured": false, "previous_key_can_go": false, "remaining": 0, "unsealed": 0, "unreadable": 0, ... }
```

## Rolling back

Until step 5, put the old key back as `TOKEN_ENCRYPTION_KEY` and the new one as `TOKEN_ENCRYPTION_KEY_PREVIOUS`, then restart. HappyView re-seals under the old key the same way.

After step 5, the old key no longer opens anything the instance holds. Rolling back is then a rotation of its own, from the new key back to the old one.

## Backups

A backup holds values sealed under whichever key was current when it was taken, so keep every retired key for as long as you keep backups taken under it.

To restore a backup taken before a rotation, start the restored instance with the current key as `TOKEN_ENCRYPTION_KEY` and the key the backup was sealed under as `TOKEN_ENCRYPTION_KEY_PREVIOUS`. HappyView re-seals the restored values under the current key, then continue from step 4.

## What rotation doesn't protect against

Rotation limits what an old key can still open. It doesn't help when the database and the key it's sealed under leak together: whoever has both can read every value from before the rotation.

## Troubleshooting

### The instance won't start after changing the key

**Symptom**: `Failed to load OAuth client authentication key: Internal("failed to decrypt client key: Decryption failed")`

**Cause**: `TOKEN_ENCRYPTION_KEY` changed without the old key set as `TOKEN_ENCRYPTION_KEY_PREVIOUS`, or the old key was removed before the re-seal finished.

**Fix**: set `TOKEN_ENCRYPTION_KEY_PREVIOUS` to the old key and restart, then follow the steps from step 4.

### The instance refuses to start with only the previous key set

**Symptom**: `TOKEN_ENCRYPTION_KEY_PREVIOUS is set without TOKEN_ENCRYPTION_KEY, which seals every value; set TOKEN_ENCRYPTION_KEY to the new key`

**Fix**: set `TOKEN_ENCRYPTION_KEY` to the new key.

### `unreadable` isn't 0

**Cause**: some stored values open with neither key. They were sealed under a third key, or damaged. The re-seal can't recover them, and the instance logs `stored values open under no configured TOKEN_ENCRYPTION_KEY and cannot be re-sealed` at each start.

**Fix**: if you have the key they were sealed under, set it as `TOKEN_ENCRYPTION_KEY_PREVIOUS` and restart; HappyView re-seals them under the current key. `stores` shows which store holds them. Otherwise the affected values (a script variable, a plugin secret, a session) have to be set again.

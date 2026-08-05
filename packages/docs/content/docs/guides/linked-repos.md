---
title: "Linked Repos"
---

Linked repos give your AppView durable write access to a specific atproto repo. Once an admin links a repo, any Lua script can write to it through the `linked_repos` global. You can think of these repos like service accounts: an account that your AppView may post on behalf of, a partner repo into which you mirror data, or an operator account your instance maintains on its own.

This is different from writing on behalf of the user making a request. Those writes use the caller's own OAuth session and only work while that session is live. A linked repo belongs to the instance: HappyView stores the session, handles token refreshes in the background, and allows scripts to act as the linked repo rather than the caller.

## How it works

1. An admin adds a linked repo from the dashboard (Access > Linked Repos), choosing which scopes it needs.
2. Either the admin runs the OAuth flow directly, or forwards an invite link to someone else with the ability to login to the repo.
3. Scripts call `linked_repos.get(did)` and write through the returned handle. A background worker keeps the session alive so rarely used repos stay working.

## Adding a linked repo

In the dashboard, go to **Access > Linked Repos** and choose **Link a repo** to open the side panel.

The **handle or DID** is optional, but it decides who is allowed to complete the authorization:

- If a handle or DID is set, the grant is **pinned**: that is the only account that can be connected.
- If a handle or DID is **not set**, the grant is **open**. Whoever follows the invite gets to decide which account is linked.

In \*_most scenarios_, you should set the identifier to avoid confusion and thwart bad actors that get their hands on the link.

The **reason** explains why you're asking for access. It's shown to whoever opens the invite link: it's how a user will decide whether to complete the auth.

You can limit the **scopes** that are requested when a user follows the invite link into the auth flow. It is best practice to use the minimum necessary scopes.

Scopes are **immutable**. There is no changing what a grant can do after it's been sent. If you make a mistake, you must delete and recreate a new one.

## Authorizing

### Inline

If the grant names a handle or DID, **Authorize** takes you straight to the OAuth flow. Afterward it will return you to the Linked Repos page. Use this for accounts you control.

### By invite link

**Copy link** mints a single-use invite and puts the URL in your clipboard. Send it to whoever has credentials for the repo.

The URL is returned **once** and is unrecoverable afterwards. If you lose it, revoke the invite and mint another. The dashboard shows outstanding invites and their expiry in the **Invites** column, so you can check whether one is still live and revoke it if you sent it to the wrong person.

An invite is single-use. The invite is retired once a repo is linked, at which point every outstanding invite for that grant is cleared.

### What the recipient sees

The recipient of an invite link **is not required** to be added to your HappyView instance, and they never see your dashboard. They get a page naming your instance, your stated reason, and the access being requested in plain language, then a prompt for their handle (or, for a pinned grant, confirmation of which account the link is for). After approving at their own PDS they land on a result page confirming what was linked and reminding them they can revoke it from their own account settings at any time.

## Grant status

| Status         | Meaning                                                                  |
| -------------- | ------------------------------------------------------------------------ |
| `pending`      | Created but never authorized                                             |
| `active`       | Authorized and usable                                                    |
| `needs_reauth` | The stored session failed to refresh; the grant must be authorized again |

A background worker refreshes each active grant's session periodically, so a rarely-used grant doesn't go stale. If a refresh fails, the grant flips to `needs_reauth` and the error is recorded and shown in the dashboard. A script hitting a dead grant triggers the same transition, so failures surface either way.

## Using a linked repo from a script

The `linked_repos` global is available in every script context: procedures, queries, record scripts, label scripts, and job scripts.

```lua
function handle()
  local repo = linked_repos.get("did:plc:abc123")

  repo:create_record{
    collection = "com.example.note",
    record = { text = "posted by the AppView" },
  }

  return { ok = true }
end
```

Scope checks happen locally, before any network call. If a grant lacks the scope an operation needs, the call fails immediately with a message naming the grant and the missing scope, so you never have to work backwards from an opaque 403.

Every method re-reads the grant from the database. If a grant is revoked or flips to `needs_reauth` while a long-running script is working, the next call it makes is refused.

See the [Linked Repos Lua API reference](../api-reference/lua/linked-repos-api.md) for the full surface.

<Callout type="warn">
Any script can use any linked repo. Scripts are already admin-authored code with database, HTTP, and environment access, and linked repos sit at the same trust level. Be deliberate about scripts that write to a linked repo in response to unauthenticated requests.
</Callout>

## Permissions

| Permission            | Grants                                                       |
| --------------------- | ------------------------------------------------------------ |
| `linked-repos:read`   | View grants, their status, and their outstanding invites     |
| `linked-repos:create` | Create grants, start authorizations, mint and revoke invites |
| `linked-repos:delete` | Delete a grant and its stored session                        |

Revoking an invite uses `linked-repos:create` rather than `linked-repos:delete`, because minting and revoking are part of the same lifecycle: whoever can send a link should be able to delete it. `linked-repos:delete` is reserved for destroying a grant and the session behind it.

## Removing a linked repo

Deleting the grant for a linked repo also removes its stored OAuth session. Scripts writing to that repo will fail immediately.

Deleting the grant on HappyView's side doesn't revoke the authorization at the PDS. The person who granted access can also revoke it from their own account settings, which is what causes a grant to land in `needs_reauth`.

## See also

- [Linked Repos admin API](../api-reference/admin/linked-repos.md): Drive the whole lifecycle headlessly
- [Linked Repos Lua API](../api-reference/lua/linked-repos-api.md): The full script surface
- [Lua Scripting](./lua-scripting.md): Script contexts and triggers
- [Permissions](./permissions.md): The full permission list

---
title: "Publishing Lexicons"
---

Other apps can only resolve your lexicons if they're published on the network. HappyView can publish the lexicons it owns as `com.atproto.lexicon.schema` records, into either the instance's attached account or a [linked repo](./linked-repos.md), and can keep them up to date automatically.

Only lexicons you uploaded by hand or declared in your project files can be published. A network lexicon was fetched from someone else's repo, so it's never offered for publishing.

## How a lexicon gets resolved

A published lexicon is a record in the publisher's repo, in the `com.atproto.lexicon.schema` collection, with the NSID as its record key. The body is your lexicon document with `"$type": "com.atproto.lexicon.schema"` added; nothing else is changed.

Finding that record from an NSID takes a DNS lookup:

1. Take the NSID and drop its last segment: `com.example.notes.getNote` becomes `com.example.notes`.
2. Reverse what's left into a domain: `notes.example.com`.
3. Look up the TXT record at `_lexicon.notes.example.com`. Its value is `did=<did>`.
4. Fetch the schema record from that DID's repo.

<Callout type="warn">
A published record only resolves when the `_lexicon` TXT record names the account it was written to. HappyView can't set DNS for you. It checks the record and shows you exactly what to add.
</Callout>

The lookup isn't hierarchical. `_lexicon.notes.example.com` and `_lexicon.example.com` are different names, and one doesn't cover the other. If you publish `com.example.notes.*` and `com.example.billing.*`, you need a TXT record for each authority.

For example, publishing `com.example.notes.getNote` to `did:plc:abc123` needs:

```
_lexicon.notes.example.com.  TXT  "did=did:plc:abc123"
```

The publishing page runs this check for you and shows the record to add whenever the DNS is missing or names a different DID.

## Targets

Each default sends lexicons to one of two places.

### The instance's account

Choose **Instance account** to publish into the account attached to your instance. This target only exists when the instance identity is in **attach account** mode (see [Service Identity](../api-reference/admin/service-identity.md)). With `did:web`, `did:plc`, or an identity that isn't exposed, there's no repo to write to, and the target is shown as unavailable with that reason.

Publishing uses the attached account's dashboard sign-in. If that session has expired, publishing fails with a message asking you to sign in again.

### A linked repo

Choose a linked repo to publish into that repo. The grant needs permission to write schema records. Add the linked repo with this scope:

```
repo:com.atproto.lexicon.schema?action=create&action=update
```

Add `&action=delete` as well if you want to be able to unpublish from it:

```
repo:com.atproto.lexicon.schema?action=create&action=update&action=delete
```

Both `create` and `update` are required, because publishing is an upsert. The **Link a repo** dialog has a preset that adds all three.

Scopes on a grant are immutable. If a grant is missing one, the target shows as unavailable and says what to add; delete the grant and link the repo again with the right scopes. A linked repo that is pending, or needs reauthorization, is unavailable until it's active again.

A PDS can approve fewer scopes than a grant asks for, and HappyView judges the target by what was approved. If it shows as unavailable because the PDS granted fewer scopes, the PDS probably cached HappyView's client details from before the grant existed; link the repo again later.

## Publishing defaults

A publishing default is a rule that says where a set of lexicons goes. Manage them at **Lexicons > Publishing** (`/dashboard/lexicons/publishing`).

Each default has:

- A **pattern**: `*` for everything, a prefix such as `com.example.*`, or one full NSID.
- A **target**: the instance's account or a linked repo.
- **Auto-publish**: whether matching lexicons are published without a click.

`com.example.*` matches anything under `com.example`, at any depth, but not `com.example` itself. Prefixes are case-insensitive and stored lowercased. Whitespace is trimmed; a trailing `.`, `**`, a wildcard in the middle (`com.*.foo`), and a bare single segment are refused.

### Most specific wins

When several defaults match a lexicon, the most specific one decides:

1. An exact NSID.
2. The longest matching prefix.
3. `*`.

So with `*` pointing at the instance and `com.example1.*` pointing at repo A, a lexicon under `com.example1` goes to repo A and everything else goes to the instance.

### If a linked repo is removed

Deleting a linked repo doesn't delete the defaults that used it, and those defaults don't fall through to a broader one. Their lexicons show as **target unavailable** until you point the default at something else. Publishing them somewhere you never chose would be worse than not publishing them.

## Status

Every publishable lexicon has one status:

| Status               | Meaning                                                                                   |
| -------------------- | ----------------------------------------------------------------------------------------- |
| `no_rule`            | No default matches                                                                        |
| `target_unavailable` | A default matches, but its target can't publish right now; the reason is shown            |
| `unpublished`        | Never successfully published                                                              |
| `withdrawn`          | Unpublished by hand; auto-publish leaves it alone until you publish it again               |
| `moved`              | Published to a different account than its default points at now                           |
| `outdated`           | Published to the right account, but the lexicon has changed since                         |
| `current`            | The last published record matches what HappyView would publish today                      |

The last error, if any, is shown beside the status. Each lexicon's own page has a **Publishing** panel with the same information: which default applies and why, the status, the DNS record to add, and **Publish** and **Unpublish** buttons.

## Auto-publish

With auto-publish on, HappyView publishes any matching lexicon that is `unpublished`, `moved`, or `outdated`. A `withdrawn` lexicon is skipped. It runs after the server starts, after you upload a lexicon, and after you create or change a default. There's no polling loop, so a failed publish is retried at the next of those, or when you publish by hand.

Only one run happens at a time. A trigger that arrives mid-run doesn't start a second run: it marks the work pending and the running pass goes round once more, so a change made during a run isn't missed.

## Unpublishing

**Nothing ever unpublishes automatically.** Deleting a lexicon, deleting a default, or pointing a default somewhere else leaves published records where they are. A published schema is a public contract that other apps resolve, and removing one can break them.

To remove a record, use **Unpublish**. It deletes the record from the account it was published to, using whichever credential holds that DID: the attached account, or the linked repo bound to it. If nothing HappyView holds can act as that account any more, unpublishing is refused and names the DID.

Unpublishing also withdraws the lexicon from auto-publish: its status becomes `withdrawn`, and no auto-publish run puts the record back, even under a default with auto-publish on. Publishing it by hand clears that and returns it to its default's normal handling.

## Permissions

Reading the publishing page needs `lexicons:read`. Creating, changing, or deleting defaults, and publishing or unpublishing, needs `lexicons:publish`.

The **Manager** template includes `lexicons:publish` for users created from it going forward. Existing non-super users don't get it automatically: grant it to them explicitly from **Access > Users**. Super users can always publish.

## See also

- [Publishing admin API](../api-reference/admin/lexicon-publishing.md): Drive all of this headlessly
- [Lexicons](./lexicons.md): Uploading lexicons and how NSIDs resolve
- [Linked Repos](./linked-repos.md): Granting access to another account's repo
- [Permissions](./permissions.md): The full permission list

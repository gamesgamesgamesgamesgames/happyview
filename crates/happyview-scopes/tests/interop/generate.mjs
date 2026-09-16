// Regenerates the vendored interop corpus from the reference implementation.
//
//   npm install @atproto/oauth-scopes@0.3.1
//   node generate.mjs > corpus.json
//
// The corpus is what pins this crate's behaviour; the Rust port is not trusted
// on its own. Re-run this when bumping the pinned reference version and review
// the diff — a change here is a change in the protocol's semantics.
import {
  RepoPermission, RpcPermission, BlobPermission,
  IdentityPermission, AccountPermission, IncludeScope,
  ScopePermissions, ScopePermissionsTransition,
} from '@atproto/oauth-scopes'

const PARSERS = {
  repo: RepoPermission, rpc: RpcPermission, blob: BlobPermission,
  identity: IdentityPermission, account: AccountPermission, include: IncludeScope,
}

const SCOPES = [
  // repo
  'repo:com.example.post', 'repo:*', 'repo:com.example.post?action=create',
  'repo:com.example.post?action=create&action=update',
  'repo:com.example.post?action=create,update',
  'repo:com.example.post?action=', 'repo:com.example.post?action=frobnicate',
  'repo:com.example.post?foo=bar', 'repo:com.example.post?collection=com.other.x',
  'repo?collection=com.example.post', 'repo?collection=a.b.c&collection=d.e.f',
  'repo?collection=com.example.post&action=delete', 'repo:', 'repo',
  'repo:notannsid', 'repo:com', 'repo:pics.2bit.feed.photo',
  // rpc
  'rpc:com.example.a', 'rpc:com.example.a?aud=*', 'rpc:*?aud=*',
  'rpc:com.example.a?aud=did:web:example.com%23svc',
  'rpc:com.example.a?aud=did:plc:6msi3pj7krzih5qxqtryxlzw%23atproto_pds',
  'rpc:com.example.a?aud=did:plc:abc%23svc',
  'rpc:com.example.a?aud=did:web:example.com',
  'rpc:com.example.a?aud=did:web:example.com%23',
  'rpc:com.example.a?aud=did:foo:bar%23svc',
  'rpc:com.example.a?aud=did:web:example.com%3A3000%23s',
  'rpc?lxm=com.example.a&lxm=com.example.b&aud=*',
  'rpc:com.example.a?aud=*&aud=*',
  // blob
  'blob:*/*', 'blob:image/*', 'blob:image/png', 'blob:image', 'blob:',
  'blob:image/png?accept=text/plain', 'blob?accept=image/png&accept=text/plain',
  'blob:*/png',
  // identity / account
  'identity:handle', 'identity:*', 'identity:email', 'identity:',
  'account:email', 'account:repo', 'account:status', 'account:bogus',
  'account:email?action=manage', 'account:email?action=read&action=manage',
  'account:email?action=read,manage', 'account:email?action=bogus',
  // include
  'include:com.example.authBasic', 'include:com.example.authBasic?aud=did:web:x.com%23svc',
  'include:com.example.authBasic?aud=notadid', 'include:notannsid', 'include:',
]

const parse = SCOPES.map((scope) => {
  const prefix = scope.split(/[:?]/, 1)[0]
  const Parser = PARSERS[prefix]
  const parsed = Parser ? Parser.fromString(scope) : null
  return { scope, prefix, valid: !!parsed, canonical: parsed ? parsed.toString() : null }
})

// Matching questions, evaluated with transitional scopes honoured (which is
// how a PDS evaluates a real token).
const GRANTS = [
  'atproto', 'atproto transition:generic', 'atproto transition:chat.bsky',
  'atproto transition:email', 'atproto transition:generic transition:chat.bsky',
  'atproto repo:com.example.post', 'atproto repo:com.example.post?action=create',
  'atproto repo:*', 'atproto blob:image/*', 'atproto blob:*/*',
  'atproto rpc:com.example.a?aud=*', 'atproto rpc:*?aud=did:web:x.com%23svc',
  'atproto identity:handle', 'atproto identity:*',
  'atproto account:email', 'atproto account:email?action=manage',
]
const REPO_Q = [['com.example.post','create'],['com.example.post','delete'],['com.other.x','create'],['chat.bsky.x.y','create']]
const RPC_Q = [['com.example.a','did:web:x.com#svc'],['com.example.b','did:web:x.com#svc'],['chat.bsky.convo.listConvos','did:web:x.com#svc']]
const BLOB_Q = ['image/png','video/mp4','image/*','notamime']
const ID_Q = ['handle','*']
const ACC_Q = [['email','read'],['email','manage'],['status','read'],['repo','manage']]

const matches = []
for (const grant of GRANTS) {
  const p = new ScopePermissionsTransition(grant)
  for (const [collection, action] of REPO_Q)
    matches.push({ grant, kind: 'repo', collection, action, allowed: p.allowsRepo({ collection, action }) })
  for (const [lxm, aud] of RPC_Q)
    matches.push({ grant, kind: 'rpc', lxm, aud, allowed: p.allowsRpc({ lxm, aud }) })
  for (const mime of BLOB_Q)
    matches.push({ grant, kind: 'blob', mime, allowed: p.allowsBlob({ mime }) })
  for (const attr of ID_Q)
    matches.push({ grant, kind: 'identity', attr, allowed: p.allowsIdentity({ attr }) })
  for (const [attr, action] of ACC_Q)
    matches.push({ grant, kind: 'account', attr, action, allowed: p.allowsAccount({ attr, action }) })
}

// include: expansion, including the authority-containment cases.
const INCLUDES = [
  ['include:com.example.authBasic', { permissions: [
      { resource: 'repo', collection: ['com.example.profile'] },
      { resource: 'repo', collection: ['app.bsky.feed.post'] },
      { resource: 'repo', collection: ['*'] }]}],
  ['include:com.example.authBasic', { permissions: [
      { resource: 'rpc', lxm: ['com.example.getFeed'] },
      { resource: 'rpc', lxm: ['com.example.getFeed'], aud: '*' },
      { resource: 'rpc', lxm: ['com.example.getFeed'], inheritAud: true }]}],
  ['include:com.example.authBasic?aud=did:web:x.com%23svc', { permissions: [
      { resource: 'rpc', lxm: ['com.example.getFeed'], inheritAud: true },
      { resource: 'rpc', lxm: ['com.example.getFeed'], aud: 'did:web:evil.com#svc' },
      { resource: 'rpc', lxm: ['app.bsky.feed.getFeed'], inheritAud: true }]}],
  ['include:com.example.authBasic', { permissions: [
      { resource: 'account', attr: 'email' },
      { resource: 'repo', collection: ['com.example.a','com.example.b'], action: ['create'] }]}],
]
const includes = INCLUDES.map(([scope, set]) => ({
  scope, set, expanded: IncludeScope.fromString(scope).toScopes(set),
}))

console.log(JSON.stringify({ reference: '@atproto/oauth-scopes@0.3.1', parse, matches, includes }, null, 2))

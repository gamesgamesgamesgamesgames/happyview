// Generates interop vectors for atproto space commits.
//
// Mirrors packages/space/src/repo-commit.ts and packages/crypto/src/hmac.ts
// from bluesky-social/atproto @ permissioned-data-alpha. Uses @noble/hashes,
// the library @atproto/crypto builds on, so the vectors come from the reference
// primitives rather than from HappyView's own code.
//
// The repo root is a bun workspace, so `npm i` there fails on the
// `workspace:*` protocol. Install and run from a scratch directory instead:
//
//   mkdir -p /tmp/vecgen && cd /tmp/vecgen
//   echo '{"name":"vecgen","private":true,"type":"module"}' > package.json
//   npm i @noble/hashes@1
//   cp <repo>/scripts/gen-space-vectors.mjs .
//   node gen-space-vectors.mjs > <repo>/tests/fixtures/space_commit_vectors.json
import { expand } from '@noble/hashes/hkdf'
import { hmac } from '@noble/hashes/hmac'
import { sha256 } from '@noble/hashes/sha256'

const DOMAIN_PREFIX = new TextEncoder().encode('atproto-space-v1')

const encodeCommitCtx = (space, author, rev, ikm) => {
  const enc = new TextEncoder()
  const fields = [enc.encode(space), enc.encode(author), enc.encode(rev), ikm]
  let size = DOMAIN_PREFIX.length
  for (const f of fields) size += 2 + f.length
  const out = new Uint8Array(size)
  out.set(DOMAIN_PREFIX)
  let o = DOMAIN_PREFIX.length
  for (const f of fields) {
    out[o++] = (f.length >>> 8) & 0xff
    out[o++] = f.length & 0xff
    out.set(f, o)
    o += f.length
  }
  return out
}

const hex = (u8) => Buffer.from(u8).toString('hex')
const fill = (n, b) => new Uint8Array(n).fill(b)

const cases = [
  {
    space: 'at://did:plc:abc/space/com.example.forum/main',
    author: 'did:plc:testuser',
    rev: '3k2rev1',
    ikm: fill(32, 0xaa),
    hash: fill(32, 0xcc),
  },
  {
    space: 'at://did:plc:xyz/space/app.bsky.group/self',
    author: 'did:plc:alice',
    rev: '3l9zzz9',
    ikm: fill(32, 0x00),
    hash: fill(32, 0xff),
  },
  {
    space: 'at://did:web:example.com/space/my.bulletin.board/self',
    author: 'did:web:bob.example.com',
    rev: '3m1aaaa',
    ikm: fill(32, 0x5a),
    hash: fill(32, 0x01),
  },
]

const vectors = cases.map(({ space, author, rev, ikm, hash }) => {
  const context = encodeCommitCtx(space, author, rev, ikm)
  const derivedKey = expand(sha256, ikm, context, 32)
  const mac = hmac(sha256, derivedKey, hash)
  return {
    space,
    author,
    rev,
    ikm_hex: hex(ikm),
    hash_hex: hex(hash),
    context_hex: hex(context),
    derived_key_hex: hex(derivedKey),
    mac_hex: hex(mac),
  }
})

console.log(JSON.stringify({ vectors }, null, 2))

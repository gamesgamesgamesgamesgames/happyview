// Generates interop vectors for proposal 0016's space MACs: the KEM shared
// secret, the commit MAC, and the two headers that carry them.
//
// Unlike gen-space-vectors.mjs this runs the reference code itself rather than
// mirroring it, because the KEM has enough steps (RFC 9180 labels, point
// encodings, a full HKDF) that a mirror could repeat our own mistake. The
// published @atproto/space alpha declares `workspace:` dependencies and cannot
// be installed from npm, so build it from a checkout instead:
//
//   git clone --depth 1 --branch permissioned-data-alpha \
//     https://github.com/bluesky-social/atproto.git /tmp/atproto-ref
//   cd /tmp/atproto-ref
//   pnpm install --ignore-scripts --frozen-lockfile --filter "@atproto/space..."
//   (cd packages/space && pnpm exec tsc --build tsconfig.build.json)
//   node <repo>/scripts/gen-space-mac-vectors.mjs /tmp/atproto-ref \
//     > <repo>/tests/fixtures/space_mac_vectors.json
import path from 'node:path'
import { pathToFileURL } from 'node:url'

const root = process.argv[2]
if (!root) {
  console.error('usage: gen-space-mac-vectors.mjs <atproto checkout>')
  process.exit(2)
}
const load = (p) => import(pathToFileURL(path.join(root, p)).href)

const { P256Keypair } = await load('packages/crypto/dist/index.js')
const space = await load('packages/space/dist/index.js')

const hex = (u8) => Buffer.from(u8).toString('hex')
const fill = (n, b) => new Uint8Array(n).fill(b)

const cases = [
  {
    author: 0x11,
    ephemeral: 0x22,
    commit: {
      ver: 1,
      repo: 'did:plc:author',
      space: 'ats://did:plc:auth/com.example.forum/main',
      rev: '3krevaaaaaaaa',
      data: fill(32, 0xaa),
    },
  },
  {
    author: 0x5c,
    ephemeral: 0x07,
    commit: {
      ver: 1,
      repo: 'did:web:alice.example.com',
      space: 'ats://did:web:space.example.com/com.example.chat/général',
      rev: '3lzq2b3k4c22a',
      data: fill(32, 0x01),
    },
  },
]

const vectors = []
for (const c of cases) {
  const author = await P256Keypair.import(fill(32, c.author))
  const ephemeral = await P256Keypair.import(fill(32, c.ephemeral))
  const sharedSecret = space.decapsulate(author, ephemeral.did())
  const mac = space.computeCommitMac(sharedSecret, c.commit)
  vectors.push({
    author_kem_private_hex: hex(fill(32, c.author)),
    author_kem_did: author.did(),
    ephemeral_private_hex: hex(fill(32, c.ephemeral)),
    ephemeral_did: ephemeral.did(),
    shared_secret_hex: hex(sharedSecret),
    commit: { ...c.commit, data_hex: hex(c.commit.data), data: undefined },
    commit_mac_hex: hex(mac),
    want_mac_header: space.formatWantMacHeader(space.COMMIT_MAC_TYPE, ephemeral.did()),
    mac_header: space.formatMacHeader(space.COMMIT_MAC_TYPE, mac),
  })
}

console.log(JSON.stringify({ vectors }, null, 2))

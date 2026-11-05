# Agent and contributor notes

## This is the v2 line

`release/v2` ships HappyView 2.17.x fixes only. Its documentation is the v2 docs site, so a doc change for a change on this branch goes to [happyproto/happyview-docs-legacy](https://github.com/happyproto/happyview-docs-legacy).

## Docs aren't in this repo

HappyView's documentation lives in its own repositories:

- v3 docs (https://happyview.dev/docs/): [happyproto/happyview-docs](https://github.com/happyproto/happyview-docs)
- v2 docs (https://v2.happyview.dev): [happyproto/happyview-docs-legacy](https://github.com/happyproto/happyview-docs-legacy)

When a change alters documented behaviour, open a companion PR in the repository for that line (happyview-docs-legacy, for this branch), and link it from the "Docs" checkbox in this PR's description. Write `n/a` there when nothing documented changes.

Never add `packages/docs/` or a top-level `docs/` to this repo. The `docs-moved` CI check fails any PR that adds or modifies a file under either path.

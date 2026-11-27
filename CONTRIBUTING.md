# Contributing to HappyView

The contributor guide is on the documentation site:

- [Develop](https://happyview.dev/docs/contributing/develop): choosing the base branch, setting up a checkout, the development stack, signing off commits, commit messages, and what a pull request needs to merge.
- [Test](https://happyview.dev/docs/contributing/test): every CI check and the local commands that correspond to it.
- [Release](https://happyview.dev/docs/contributing/release): how CI cuts releases from `main`, `beta` and `release/v2`.

The docs aren't in this repo. v3 docs live in [happyproto/happyview-docs](https://github.com/happyproto/happyview-docs), and v2 docs in [happyproto/happyview-docs-legacy](https://github.com/happyproto/happyview-docs-legacy). A change to documented behaviour needs a companion PR there; link it from the Docs checkbox in the pull request's description.

In short:

- Open pull requests against `beta`, where v3 is developed. `alpha` is parked and takes no pull requests.
- A fix that v2 needs too lands on `beta` first. It is then cherry-picked with `git cherry-pick -x` onto a `<topic>-v2` branch, in a pull request into `release/v2`, which ships 2.17.x fixes only. Its `v2-guard` check refuses a `feat` or breaking change, v3 history, and a migration that isn't on `beta` first.
- Sign off every commit with `git commit -s`. The `DCO` check is required.
- Write commit messages as [Conventional Commits](https://www.conventionalcommits.org/); releases are versioned from them.
- Into `beta`, open the pull request as a draft while you iterate, and mark it ready when it passes locally. The costly checks run only on a ready pull request.

This file keeps what applies to maintainers and to the provider contract.

## Pull requests from forks

CI runs a fork's PR without the repository's secrets, and every required check is built to pass without them. PR images are pushed by a separate workflow that runs from the default branch and never executes the PR's code. What a maintainer does on a fork's PR:

1. **Approve the workflow run** for a first-time contributor, from the banner on the PR's Checks tab. Read the diff first, especially anything under `.github/`: the run executes the PR's code on our runners.
2. **Check the commit trailers before merging.** Pull requests into `beta` and `release/v2` merge with a merge commit, so every commit lands as written, trailers included, and the merge dialog can't remove one. If a commit carries a trailer we don't carry, such as an AI co-author line, ask the contributor to amend it before merging, and keep their `Signed-off-by`.

## The provider contract

The `happyview-provider` crate is the contract that record sources and stores implement, and it is licensed MIT OR Apache-2.0 so providers can be built under any terms. A change to it is a contract change. If the change breaks existing providers, bump `CONTRACT_VERSION` in `crates/happyview-provider/src/types.rs`. See [Record providers](https://happyview.dev/docs/guides/record-providers).

## License of contributions

HappyView is licensed under the MIT License ([LICENSE.md](LICENSE.md)). By contributing, you agree that your contribution is licensed under the same terms as the file or package it changes, and your `Signed-off-by` line certifies the [Developer Certificate of Origin](https://developercertificate.org/) for it.

## Code of conduct

Participation in this project is covered by the [Code of Conduct](CODE_OF_CONDUCT.md).

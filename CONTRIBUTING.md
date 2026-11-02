# Contributing to HappyView

The contributor guide is on the documentation site:

- [Develop](https://happyview.dev/docs/contributing/develop): choosing the base branch, setting up a checkout, the development stack, signing off commits, commit messages, and what a pull request needs to merge.
- [Test](https://happyview.dev/docs/contributing/test): every CI check and the local commands that correspond to it.
- [Release](https://happyview.dev/docs/contributing/release): how CI cuts releases from `main`, `beta` and `alpha`.

In short:

- Open pull requests against `beta`, or against `alpha` for a change that belongs to the next major line.
- Sign off every commit with `git commit -s`. The `DCO` check is required.
- Write commit messages as [Conventional Commits](https://www.conventionalcommits.org/); releases are versioned from them.
- Into `alpha`, open the pull request as a draft while you iterate, and mark it ready when it passes locally.

This file keeps what applies to maintainers and to the provider contract.

## Pull requests from forks

CI runs a fork's PR without the repository's secrets, and every required check is built to pass without them. PR images are pushed by a separate workflow that runs from the default branch and never executes the PR's code. What a maintainer does on a fork's PR:

1. **Approve the workflow run** for a first-time contributor, from the banner on the PR's Checks tab. Read the diff first, especially anything under `.github/`: the run executes the PR's code on our runners.
2. **Authorize the docs preview**, if it's useful for the PR, from the link in Vercel's comment. That runs the docs build from the PR's branch on Vercel. A preview never publishes to the docs' atproto account; only the production deploy does. It also builds without fresh changelogs if GitHub's API can't be reached, and uses the committed ones.
3. **Check the commit trailers before merging.** Pull requests into `alpha` and `beta` merge with a merge commit, so every commit lands as written, trailers included, and the merge dialog can't remove one. If a commit carries a trailer we don't carry, such as an AI co-author line, ask the contributor to amend it before merging, and keep their `Signed-off-by`.

## The provider contract

The `happyview-provider` crate is the contract that record sources and stores implement, and it is licensed MIT OR Apache-2.0 so providers can be built under any terms. A change to it is a contract change. If the change breaks existing providers, bump `CONTRACT_VERSION` in `crates/happyview-provider/src/types.rs`. See [Record providers](https://happyview.dev/docs/guides/record-providers).

## License of contributions

HappyView is licensed under the MIT License ([LICENSE.md](LICENSE.md)). By contributing, you agree that your contribution is licensed under the same terms as the file or package it changes, and your `Signed-off-by` line certifies the [Developer Certificate of Origin](https://developercertificate.org/) for it.

## Code of conduct

Participation in this project is covered by the [Code of Conduct](CODE_OF_CONDUCT.md).

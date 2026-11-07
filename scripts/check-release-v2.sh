#!/usr/bin/env bash
# Refuses a change to release/v2 that would take it off the 2.17.x line:
#   - a head branch that is main, beta or alpha
#   - history that carries a v3 tag, which means it contains v3 commits
#   - a feat or breaking commit, since release/v2 ships fixes only, and the
#     same in the pull request's title and body, which a squash merge makes
#     the message of the commit that lands
#   - a migration that shipped in v2.17.0 changing or disappearing
#   - a new migration that is not listed in migrations/v2-backports.txt, or
#     that beta does not hold byte for byte (migrations land on beta first)
#
# Usage: scripts/check-release-v2.sh <base> <head>
# HEAD_REF, PR_TITLE and PR_BODY, when set, are the pull request's head branch,
# title and body. Tags and origin/beta must have been fetched.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
[ $# -eq 2 ] || { echo "usage: $0 <base> <head>" >&2; exit 2; }
base=$1
head=$2
floor=v2.17.0
backports=migrations/v2-backports.txt

for ref in "$floor" origin/beta; do
  git rev-parse --verify --quiet "$ref^{commit}" >/dev/null || {
    echo "missing $ref: fetch it first" >&2
    exit 2
  }
done

failed=0
fail() {
  echo "release/v2: $*" >&2
  failed=1
}

case "${HEAD_REF:-}" in
  main | beta | alpha)
    fail "$HEAD_REF cannot merge into release/v2. Cherry-pick the fix onto a <topic>-v2 branch instead."
    ;;
esac

v3=$(git tag --merged "$head" --list 'v3.*' | head -n 1)
[ -z "$v3" ] || fail "$head carries the tag $v3, so it contains v3 history."

# What semantic-release would read from a commit message: "breaking", "feat"
# or nothing.
release_type() {
  local message=$1
  local subject=${message%%$'\n'*}
  if [[ $subject =~ ^[a-z]+(\([^\)]*\))?!: ]] || grep -qE '^BREAKING[ -]CHANGE:' <<<"$message"; then
    echo breaking
  elif [[ $subject =~ ^feat(\([^\)]*\))?: ]]; then
    echo feat
  fi
}

refuse_release_type() {
  local what=$1 message=$2
  case $(release_type "$message") in
    breaking) fail "$what is a breaking change." ;;
    feat) fail "$what is a feat, and release/v2 ships fixes only." ;;
  esac
}

while read -r sha; do
  [ -n "$sha" ] || continue
  refuse_release_type "$(git log -1 --format='%h %s' "$sha")" "$(git log -1 --format=%B "$sha")"
done < <(git rev-list --no-merges "$base..$head")

if [ -n "${PR_TITLE:-}" ]; then
  refuse_release_type "The pull request title \"$PR_TITLE\"" "$PR_TITLE"$'\n\n'"${PR_BODY:-}"
fi

# The list is read from the head being checked, not from the working tree,
# which need not be that head.
listed() {
  git show "$head:$backports" 2>/dev/null | grep -vE '^[[:space:]]*(#|$)' | grep -qxF "$1"
}

for backend in sqlite postgres; do
  dir=migrations/$backend/

  while IFS=$'\t' read -r meta path; do
    blob=${meta##* }
    if ! now=$(git rev-parse --verify --quiet "$head:$path"); then
      fail "$path shipped in $floor and is gone."
    elif [ "$now" != "$blob" ]; then
      fail "$path shipped in $floor and has changed."
    fi
  done < <(git ls-tree "$floor" "$dir")

  while IFS=$'\t' read -r meta path; do
    git rev-parse --verify --quiet "$floor:$path" >/dev/null && continue
    blob=${meta##* }
    if ! listed "$path"; then
      fail "$path is new. A backported migration is listed in $backports."
    elif [ "$(git rev-parse --verify --quiet "origin/beta:$path" || true)" != "$blob" ]; then
      fail "$path is not on beta byte for byte. Migrations land on beta first."
    fi
  done < <(git ls-tree "$head" "$dir")
done

[ "$failed" -eq 0 ] && echo "release/v2: $base..$head stays on 2.17.x"
exit "$failed"

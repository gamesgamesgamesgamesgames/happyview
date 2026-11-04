#!/usr/bin/env bash
# Every migration a stable branch ships must be on this branch, byte for byte,
# because a database that ran it can only boot here if the migrator finds it.
# A file may sit at another version only when migrations/renumbered.tsv says
# so; boot moves such a database's record to that version before migrating.
#
# Usage: scripts/check-migration-drift.sh [branch ...]   (default: beta main)
# Compares against origin/<branch>, which must have been fetched.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
branches=("$@")
[ ${#branches[@]} -gt 0 ] || branches=(beta main)

# The version this branch carries a stable version's file at, if renumbered.
renumbered() {
  awk -F'\t' -v v="$1" '$0 !~ /^#/ && $1 == v { print $2 }' migrations/renumbered.tsv
}

failed=0
for branch in "${branches[@]}"; do
  ref="origin/$branch"
  git rev-parse --verify --quiet "$ref" >/dev/null || {
    echo "missing $ref: fetch it first" >&2
    exit 2
  }
  for backend in sqlite postgres; do
    while read -r _ _ blob name; do
      version=${name%%_*}
      here="migrations/$backend/$name"
      to=$(renumbered "$version")
      if [ -n "$to" ]; then
        here="migrations/$backend/${to}_${name#*_}"
      fi
      if [ ! -f "$here" ]; then
        echo "$branch: migrations/$backend/$name is not on this branch" >&2
        failed=1
      elif [ "$(git hash-object "$here")" != "$blob" ]; then
        echo "$branch: $here differs from $branch's migrations/$backend/$name" >&2
        failed=1
      fi
    done < <(git ls-tree "$ref" "migrations/$backend/" | sed 's|migrations/[a-z]*/||')
  done
done

if [ "$failed" -ne 0 ]; then
  echo "A stable branch has migrations this branch lacks or has changed." >&2
  echo "Carry each file over unchanged; if its version is taken here, add it to migrations/renumbered.tsv." >&2
  exit 1
fi
echo "Migrations match ${branches[*]}."

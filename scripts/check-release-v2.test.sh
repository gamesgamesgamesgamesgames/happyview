#!/usr/bin/env bash
# Exercises scripts/check-release-v2.sh against a throwaway repository: a
# v2.17.0 tag, an origin/beta, and pull requests built on top of them, each
# expected to pass or to be refused.
#
# Usage: scripts/check-release-v2.test.sh
set -euo pipefail

script="$(cd "$(dirname "$0")" && pwd)/check-release-v2.sh"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work"

git init -q -b release/v2
git config user.name test
git config user.email test@example.com
git config commit.gpgsign false
git config tag.gpgsign false

mkdir -p migrations/sqlite migrations/postgres
echo "create table a (id int);" >migrations/sqlite/20260101000000_a.sql
echo "create table a (id int);" >migrations/postgres/20260101000000_a.sql
git add -A
git commit -q -m "chore: v2.17.0"
git tag v2.17.0
base=$(git rev-parse HEAD)

# beta carries one migration that release/v2 may take as a backport.
git switch -q -c beta
echo "create table b (id int);" >migrations/sqlite/20261201000000_b.sql
git add -A
git commit -q -m "fix: add b on beta"
git update-ref refs/remotes/origin/beta HEAD
git switch -q release/v2

failures=0

# expect <pass|refuse> <name> <head> [VAR=value ...]
expect() {
  local want=$1 name=$2 head=$3
  shift 3
  local got
  if env -u HEAD_REF -u PR_TITLE -u PR_BODY "$@" bash "$script" "$base" "$head" >/dev/null 2>"$work/err"; then
    got=pass
  else
    got=refuse
  fi
  if [ "$got" = "$want" ]; then
    echo "ok   $name"
  else
    echo "FAIL $name: wanted $want, got $got" >&2
    sed 's/^/     /' "$work/err" >&2
    failures=$((failures + 1))
  fi
}

# commit <branch> <subject> [body]: a branch off v2.17.0 holding one empty
# commit, echoing the branch name.
commit() {
  git switch -q -c "$1" "$base"
  git commit -q --allow-empty -m "$2" ${3:+-m "$3"}
  git switch -q release/v2
  echo "$1"
}

fix=$(commit fix "fix: correct a thing")
expect pass "a fix with a fix title" "$fix" PR_TITLE="fix: correct a thing"
expect pass "a fix with no title given" "$fix"
expect refuse "a feat title over fix commits" "$fix" PR_TITLE="feat: add a thing"
expect refuse "a scoped feat title" "$fix" PR_TITLE="feat(web): add a thing"
expect refuse "a breaking title" "$fix" PR_TITLE="fix!: change a thing"
expect refuse "a scoped breaking title" "$fix" PR_TITLE="fix(api)!: change a thing"
expect refuse "a breaking change in the body" "$fix" PR_TITLE="fix: change a thing" \
  PR_BODY=$'Some context.\n\nBREAKING CHANGE: the response shape moved'
expect refuse "a hyphenated breaking change in the body" "$fix" PR_TITLE="fix: change a thing" \
  PR_BODY="BREAKING-CHANGE: the response shape moved"
expect pass "a body that only mentions a breaking change" "$fix" PR_TITLE="fix: change a thing" \
  PR_BODY="This is not a BREAKING CHANGE: it only says so mid-line."
expect refuse "a breaking footer with a space before the colon" "$fix" PR_TITLE="fix: change a thing" \
  PR_BODY="BREAKING CHANGE : the response shape moved"
expect refuse "an indented breaking footer" "$fix" PR_TITLE="fix: change a thing" \
  PR_BODY="  BREAKING CHANGE: the response shape moved"
expect refuse "a lowercase breaking footer" "$fix" PR_TITLE="fix: change a thing" \
  PR_BODY="breaking change: the response shape moved"

feat=$(commit feat "feat: add a thing")
expect refuse "a feat commit under a fix title" "$feat" PR_TITLE="fix: add a thing"

breaking=$(commit breaking "fix: change a thing" "BREAKING CHANGE: the response shape moved")
expect refuse "a breaking commit" "$breaking"
spaced=$(commit spaced "fix: change a thing" "BREAKING CHANGE : the response shape moved")
expect refuse "a breaking commit footer with a space before the colon" "$spaced"

expect refuse "beta as the head branch" "$fix" HEAD_REF=beta

git switch -q -c v3 "$base"
git commit -q --allow-empty -m "fix: on v3"
git tag v3.0.0-alpha.1
git switch -q release/v2
expect refuse "history carrying a v3 tag" v3

git switch -q -c backport "$base"
git show beta:migrations/sqlite/20261201000000_b.sql >migrations/sqlite/20261201000000_b.sql
echo "migrations/sqlite/20261201000000_b.sql" >migrations/v2-backports.txt
git add -A
git commit -q -m "fix: backport b"
git switch -q release/v2
expect pass "a listed migration that beta holds" backport

git switch -q -c unlisted "$base"
git show beta:migrations/sqlite/20261201000000_b.sql >migrations/sqlite/20261201000000_b.sql
git add -A
git commit -q -m "fix: add b without listing it"
git switch -q release/v2
expect refuse "an unlisted migration" unlisted

git switch -q -c edited "$base"
echo "-- edited" >>migrations/postgres/20260101000000_a.sql
git commit -q -am "fix: edit a shipped migration"
git switch -q release/v2
expect refuse "an edited shipped migration" edited

if [ "$failures" -gt 0 ]; then
  echo "$failures case(s) failed" >&2
  exit 1
fi
echo "all cases passed"

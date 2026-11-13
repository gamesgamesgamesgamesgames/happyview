#!/usr/bin/env bash
# Prints HappyView's past releases as the JSON array happyproto/actions
# release-publish takes in its `releases` input: one entry per release,
# oldest first, built by release-inputs.sh.
#
#   scripts/registry/backfill-list.sh [--include-legacy] > releases.json
#
# Scope by default: 2.x and later stable releases, and the 3.x and later
# alpha and beta prereleases. --include-legacy adds the 1.x releases and
# the 2.x -dev, -beta and -next prereleases, which no channel follows and no
# build of which checks for updates.
#
# Only each channel's newest version carries that channel; every other entry
# carries none. Entries are oldest first, so a run stopped part-way leaves
# every channel where it was or on a newer version, and the action never
# moves a channel backwards anyway.
#
# Dates: a software.release version has no date field. Each entry's
# published-at is the GitHub release's publishedAt, which the action turns
# into the manifest's record key.
#
# Reads with `gh` (GH_TOKEN); writes nothing. REGISTRY_LATEST_MAJOR as for
# channels.sh.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="${HAPPYVIEW_REPO:-gamesgamesgamesgamesgames/happyview}"
legacy=false
for arg in "$@"; do
    case "$arg" in
        --include-legacy) legacy=true ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done
latest_major="${REGISTRY_LATEST_MAJOR:-}"
[[ "$latest_major" =~ ^[0-9]+$ ]] \
    || { echo "REGISTRY_LATEST_MAJOR must be set to the stable major latest follows" >&2; exit 2; }

in_scope() {
    local v="$1"
    if [[ "$v" =~ ^([0-9]+)\.[0-9]+\.[0-9]+(-(alpha|beta)\.[0-9]+)?$ ]]; then
        local major="${BASH_REMATCH[1]}" pre="${BASH_REMATCH[3]}"
        [ "$legacy" = true ] && return 0
        if [ -z "$pre" ]; then [ "$major" -ge 2 ]; else [ "$major" -ge 3 ]; fi
        return
    fi
    [ "$legacy" = true ] && [[ "$v" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]
}

# Oldest first, by semver: a release sorts after its own prereleases, which
# `sort -V` alone gets backwards.
semver_sort() {
    awk '{
        split($0, a, "-"); core = a[1]; pre = substr($0, length(core) + 2)
        split(core, c, ".")
        key = sprintf("%06d.%06d.%06d", c[1], c[2], c[3])
        if (pre == "") key = key ".1"; else {
            m = split(pre, p, ".")
            key = key ".0." p[1]
            for (i = 2; i <= m; i++) key = key "." sprintf("%06d", p[i])
        }
        print key "\t" $0
    }' | sort | cut -f2
}

versions="$(
    gh release list -R "$repo" -L 1000 --exclude-drafts --json tagName --jq '.[].tagName' \
        | sed -n 's/^v\([0-9].*\)$/\1/p' \
        | while IFS= read -r v; do if in_scope "$v"; then echo "$v"; fi; done \
        | semver_sort
)"
[ -n "$versions" ] || { echo "no releases in scope" >&2; exit 1; }

# "<channel> <version>" for each channel's newest version: the last one seen.
newest="$(
    while IFS= read -r v; do
        bash "$here/channels.sh" "$v" "$latest_major" | sed "s/\$/ $v/"
    done <<<"$versions" | awk '{ n[$1] = $2 } END { for (c in n) print c, n[c] }' | sort
)"

while IFS= read -r v; do
    entry="$(bash "$here/release-inputs.sh" "v$v" --no-channels)"
    mine="$(awk -v v="$v" '$2 == v { print $1 }' <<<"$newest" | jq -R . | jq -s -c .)"
    jq -c --argjson c "$mine" '.channels = $c' <<<"$entry"
done <<<"$versions" | jq -s .

echo "$(wc -l <<<"$versions" | tr -d ' ') releases, oldest first; channels:" >&2
sed 's/^/  /; s/ \([^ ]*\)$/ -> \1/' <<<"$newest" >&2

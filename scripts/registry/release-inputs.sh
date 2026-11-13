#!/usr/bin/env bash
# Prints one HappyView release as the JSON entry happyproto/actions
# release-publish takes, built from what the release already is on GitHub:
#
#   scripts/registry/release-inputs.sh v2.17.4 [--no-channels]
#
#   { "version", "repository", "commit", "tag", "changelog-url",
#     "published-at", "channels" }
#
# The same tag always gives the same entry, which is what lets the action
# finish a release it already has instead of refusing it as a different one.
# `changelog-url` is the GitHub release page, and `published-at` its
# publishedAt, which the action turns into the manifest's record key.
#
# Channels come from channels.sh with REGISTRY_LATEST_MAJOR. --no-channels
# leaves them empty, for the backfill, which moves each channel once at the
# end.
#
# Reads with `gh` (GH_TOKEN); writes nothing.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
tag=""
channels=true
for arg in "$@"; do
    case "$arg" in
        --no-channels) channels=false ;;
        -*) echo "unknown flag: $arg" >&2; exit 2 ;;
        *) [ -z "$tag" ] || { echo "one tag at a time" >&2; exit 2; }; tag="$arg" ;;
    esac
done
[ -n "$tag" ] || { echo "usage: $0 <tag> [--no-channels]" >&2; exit 2; }
repo="${HAPPYVIEW_REPO:-gamesgamesgamesgamesgames/happyview}"

version="${tag#v}"
semver='^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'
[[ "$version" =~ $semver ]] || { echo "not a release tag: $tag" >&2; exit 1; }

release="$(gh release view "$tag" -R "$repo" --json publishedAt,url,isDraft)"
if [ "$(jq -r .isDraft <<<"$release")" = true ]; then
    echo "$tag is a draft release; publish it on GitHub first" >&2
    exit 1
fi
published_at="$(jq -r '.publishedAt // empty' <<<"$release")"
release_url="$(jq -r '.url // empty' <<<"$release")"
[ -n "$published_at" ] && [[ "$release_url" == https://* ]] \
    || { echo "$tag: GitHub gave no publishedAt or https url" >&2; exit 1; }

commit="$(gh api "repos/$repo/commits/$tag" --jq .sha)"
[[ "$commit" =~ ^[0-9a-f]{40}$ ]] || { echo "$tag: not a commit: $commit" >&2; exit 1; }

list="[]"
if [ "$channels" = true ]; then
    latest_major="${REGISTRY_LATEST_MAJOR:-}"
    [[ "$latest_major" =~ ^[0-9]+$ ]] \
        || { echo "REGISTRY_LATEST_MAJOR must be set to the stable major latest follows" >&2; exit 2; }
    list="$(bash "$here/channels.sh" "$version" "$latest_major" | jq -R . | jq -s -c .)"
fi

jq -n -c \
    --arg version "$version" \
    --arg repository "https://github.com/$repo.git" \
    --arg commit "$commit" \
    --arg tag "$tag" \
    --arg changelog "$release_url" \
    --arg published_at "$published_at" \
    --argjson channels "$list" \
    '{ version: $version, repository: $repository, commit: $commit, tag: $tag,
       "changelog-url": $changelog, "published-at": $published_at, channels: $channels }'

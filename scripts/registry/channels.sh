#!/usr/bin/env bash
# Prints the channels a HappyView version is published on, one per line.
#
#   scripts/registry/channels.sh <version> <latest-major>
#
#   3.0.0-alpha.4  v3-alpha
#   3.0.0-beta.2   v3-beta
#   2.17.4         v2, and latest too when <latest-major> is 2
#
# Each line has a channel of its own, so an instance reads exactly one: a
# stable 2.x follows v2, a 3.x beta v3-beta. <latest-major> is the stable
# major `latest` follows (2 until 3.0.0 ships, then 3), from the
# REGISTRY_LATEST_MAJOR repository variable: both majors cut stable releases
# for a while, and a 2.17.x fix cut after 3.0.0 must leave `latest` on 3.x.
#
# Anything else (the old -dev.N and -next.N prereleases, a malformed version)
# follows no channel, and the script prints nothing.
set -euo pipefail

version="${1:-}"
latest_major="${2:-}"
[ -n "$version" ] && [[ "$latest_major" =~ ^[0-9]+$ ]] \
    || { echo "usage: $0 <version> <latest-major>" >&2; exit 2; }
version="${version#v}"

if [[ "$version" =~ ^([0-9]+)\.[0-9]+\.[0-9]+$ ]]; then
    major="${BASH_REMATCH[1]}"
    echo "v$major"
    if [ "$major" = "$latest_major" ]; then echo latest; fi
elif [[ "$version" =~ ^([0-9]+)\.[0-9]+\.[0-9]+-(alpha|beta)\.[0-9]+$ ]]; then
    echo "v${BASH_REMATCH[1]}-${BASH_REMATCH[2]}"
fi

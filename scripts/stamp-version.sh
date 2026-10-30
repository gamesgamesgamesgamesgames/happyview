#!/usr/bin/env bash
# Cargo.toml's package version is deliberately not bumped per release, so
# CARGO_PKG_VERSION reports 0.1.0 unless a release build stamps the tag in
# first (see src/version.rs). The lockfile entry moves with it so a --locked
# build agrees with the manifest.
set -euo pipefail

v="${1-}"
v="${v#v}"
cd "${STAMP_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"

if [ -z "$v" ]; then
    echo "no version given; keeping repo version"
    exit 0
fi

# The value is spliced into sed and awk programs and into a manifest, so
# anything that is not a semver version is refused rather than escaped.
semver='^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$'
[[ "$v" =~ $semver ]] \
    || { echo "not a version: $v" >&2; exit 1; }

test "$(grep -c '^version = ' Cargo.toml)" = 1 \
    || { echo "Cargo.toml: expected exactly one top-level 'version =' line" >&2; exit 1; }

# A temporary file rather than sed -i, whose flags differ between GNU and BSD.
sed "s|^version = .*|version = \"$v\"|" Cargo.toml > Cargo.toml.stamp
mv Cargo.toml.stamp Cargo.toml
grep -qx "version = \"$v\"" Cargo.toml \
    || { echo "Cargo.toml: version stamp failed" >&2; exit 1; }

awk -v v="$v" '
    prev == "name = \"happyview\"" && /^version = / { print "version = \"" v "\""; prev = ""; next }
    { print; prev = $0 }
' Cargo.lock > Cargo.lock.stamp
mv Cargo.lock.stamp Cargo.lock
grep -A1 -x 'name = "happyview"' Cargo.lock | grep -qx "version = \"$v\"" \
    || { echo "Cargo.lock: version stamp failed" >&2; exit 1; }

echo "stamped version = $v"

#!/usr/bin/env bash
# Runs stamp-version.sh against copies of the real manifest and lockfile.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cp "$root/Cargo.toml" "$root/Cargo.lock" "$work/"

check_stamped() {
    grep -qx "version = \"$1\"" "$work/Cargo.toml" \
        || { echo "Cargo.toml was not stamped with $1" >&2; exit 1; }
    grep -A1 -x 'name = "happyview"' "$work/Cargo.lock" | grep -qx "version = \"$1\"" \
        || { echo "Cargo.lock was not stamped with $1" >&2; exit 1; }
    # Only the package's own entry moves.
    test "$(grep -c "version = \"$1\"" "$work/Cargo.lock")" = 1 \
        || { echo "Cargo.lock: more than one entry changed" >&2; exit 1; }
}

STAMP_ROOT="$work" bash "$here/stamp-version.sh" v3.1.0-alpha.7
check_stamped 3.1.0-alpha.7

# The tag form with no leading v stamps the same way.
STAMP_ROOT="$work" bash "$here/stamp-version.sh" 1.2.3
check_stamped 1.2.3

cp "$root/Cargo.toml" "$root/Cargo.lock" "$work/"
if STAMP_ROOT="$work" bash "$here/stamp-version.sh" 'v1.2.3|evil' 2>/dev/null; then
    echo "an invalid version was accepted" >&2; exit 1
fi
for f in Cargo.toml Cargo.lock; do
    cmp -s "$root/$f" "$work/$f" \
        || { echo "an invalid version changed $f" >&2; exit 1; }
done

STAMP_ROOT="$work" bash "$here/stamp-version.sh" "" | grep -q 'keeping repo version'
cmp -s "$root/Cargo.toml" "$work/Cargo.toml" \
    || { echo "an empty version changed Cargo.toml" >&2; exit 1; }

echo "stamp-version: ok"

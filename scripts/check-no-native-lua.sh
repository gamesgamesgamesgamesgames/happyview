#!/usr/bin/env bash
# A default build must link no Lua interpreter, so that the interpreter plugin
# is the only path a script run can take. A grep over source text cannot say
# that; the resolved dependency graph can.
#
# The second assertion is what stops the first from passing for the wrong
# reason: a renamed or misspelled package would be absent from both graphs and
# the check would report success having looked for nothing.
set -euo pipefail

cd "$(dirname "$0")/.."

normal_deps() {
    cargo tree --quiet --edges normal --prefix none --format '{p}' "$@" | sort -u
}

if normal_deps | grep -q '^mlua '; then
    echo "mlua is in the default dependency graph" >&2
    cargo tree --edges normal --invert mlua >&2
    exit 1
fi

if ! normal_deps --features lua-reference | grep -q '^mlua '; then
    echo "mlua is absent under --features lua-reference, so this check sees nothing" >&2
    exit 1
fi

echo "no mlua in the default dependency graph; present under --features lua-reference"

#!/usr/bin/env bash
#
# Whether every plugin wasm fixture's module is where the tests open it.
#
# Tests that load a fixture skip when its module is absent rather than
# panicking, so a build step that stopped producing one would otherwise leave
# them skipping inside a green run. This is what turns that back into a
# failure in the job that builds them.
#
#   --require [name...]
#               exit non-zero on anything missing, for a job that builds
#               fixtures and runs the tests needing them. Named fixtures
#               narrow it to those, for a job that needs some and not all; a
#               name matching no entry is itself a failure, so a typo cannot
#               quietly check nothing.
#   (default)   report and exit zero, for the job that deliberately builds none
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
cd "$here/.." || exit 1

# shellcheck source=scripts/plugin-fixtures.sh
. "$here/plugin-fixtures.sh"

require=0
if [ "${1:-}" = "--require" ]; then
  require=1
  shift
fi

status=0

# A newline-delimited list rather than an array, because an empty array is an
# unbound variable under `set -u` in the bash a developer's machine may still
# have, and the empty case is reachable: every name given matched nothing.
wanted=$(plugin_fixtures_select "$@") || status=1
wanted_count=$(printf '%s' "$wanted" | grep -c . || true)

# A fixture added without an entry in the shared list would be built by
# nothing and skipped by everything, which is the same silence one level up.
for dir in tests/fixtures/*/; do
  [ -f "$dir/Cargo.toml" ] || continue
  name=$(basename "$dir")
  if ! printf '%s\n' "${PLUGIN_FIXTURES[@]}" | grep -q "^tests/fixtures/$name/"; then
    echo "fixture is unlisted, so nothing checks it: $name"
    echo "  add its artefact path to plugin-fixtures.sh"
    status=1
  fi
done

missing=0
while IFS= read -r artefact; do
  [ -n "$artefact" ] || continue
  if [ ! -f "$artefact" ]; then
    echo "not built, so tests needing it skip: $artefact"
    missing=$((missing + 1))
    status=1
  fi
done <<EOF
$wanted
EOF

if [ "$missing" = 0 ] && [ "$status" = 0 ]; then
  echo "all $wanted_count of the plugin fixtures asked about are built"
  exit 0
fi

if [ "$require" = 1 ]; then
  exit "$status"
fi

echo "(not a failure here: this job builds no fixtures and those tests skip)"
exit 0

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
#
# Paths are listed rather than derived from the directory name. An artefact is
# named after the crate's `[lib] name`, or its package name when it declares
# none, so a rename would move the file without moving the directory that the
# build step names — and a derived path would follow the rename and call a
# built module missing.
set -uo pipefail

cd "$(dirname "$0")/.."

ARTEFACTS=(
  tests/fixtures/differential_library/target/wasm32-unknown-unknown/release/differential_library.wasm
  tests/fixtures/interpreter_echo/target/wasm32-unknown-unknown/release/interpreter_echo.wasm
  tests/fixtures/sdk_atproto/target/wasm32-unknown-unknown/release/sdk_atproto.wasm
  tests/fixtures/sdk_auth/target/wasm32-unknown-unknown/release/sdk_auth.wasm
  tests/fixtures/sdk_caller/target/wasm32-unknown-unknown/release/sdk_caller.wasm
  tests/fixtures/sdk_http/target/wasm32-unknown-unknown/release/sdk_http.wasm
  tests/fixtures/sdk_linked_repos/target/wasm32-unknown-unknown/release/sdk_linked_repos.wasm
  tests/fixtures/sdk_objects/target/wasm32-unknown-unknown/release/sdk_objects.wasm
  tests/fixtures/sdk_spaces/target/wasm32-unknown-unknown/release/sdk_spaces.wasm
  tests/fixtures/test_library/target/wasm32-unknown-unknown/release/test_library.wasm
  tests/fixtures/test_plugin/target/wasm32-unknown-unknown/release/test_plugin.wasm
  tests/fixtures/wasi_forbidden/target/wasm32-wasip1/release/wasi_forbidden.wasm
  tests/fixtures/wasi_probe/target/wasm32-wasip1/release/wasi_probe.wasm
)

require=0
if [ "${1:-}" = "--require" ]; then
  require=1
  shift
fi

status=0

# The artefacts this invocation is about. Every one by default; otherwise the
# ones under each named fixture directory. A newline-delimited list rather
# than an array, because an empty array is an unbound variable under `set -u`
# in the bash a developer's machine may still have, and the empty case is
# reachable: every name given matched nothing.
wanted=$(printf '%s\n' "${ARTEFACTS[@]}")
wanted_count=${#ARTEFACTS[@]}
if [ "$#" -gt 0 ]; then
  wanted=""
  wanted_count=0
  for name in "$@"; do
    matched=$(printf '%s\n' "${ARTEFACTS[@]}" | grep "^tests/fixtures/$name/" || true)
    if [ -z "$matched" ]; then
      echo "no listed artefact belongs to fixture: $name"
      status=1
      continue
    fi
    wanted="${wanted}${matched}
"
    wanted_count=$((wanted_count + $(printf '%s\n' "$matched" | wc -l)))
  done
fi

# A fixture added without an entry here would be built by nothing and skipped
# by everything, which is the same silence one level up.
for dir in tests/fixtures/*/; do
  [ -f "$dir/Cargo.toml" ] || continue
  name=$(basename "$dir")
  if ! printf '%s\n' "${ARTEFACTS[@]}" | grep -q "^tests/fixtures/$name/"; then
    echo "fixture is unlisted, so nothing checks it: $name"
    echo "  add its artefact path to $(basename "$0") and a build step to ci.yml"
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

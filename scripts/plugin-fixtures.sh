# shellcheck shell=bash
#
# The plugin wasm fixtures, as the paths the tests open their modules at.
#
# Sourced by both the script that builds them and the script that checks they
# are built, so a job cannot build one set and check another. With the set
# spelled out in a workflow's own steps instead, each job that wanted fixtures
# restated it and drifted from it in silence — the drift reaching a reader only
# as a test panicking about a module it could not open.
#
# Paths are listed rather than derived from the directory name. An artefact is
# named after the crate's `[lib] name`, or its package name when it declares
# none, so a rename would move the file without moving the directory that
# holds the manifest — and a derived path would follow the rename and call a
# built module missing. The manifest and the target travel the other way and
# are read back out of the path.
#
# wasip1 is only for the two fixtures that link preview-1 imports; every other
# fixture is a bare wasm32 module.
PLUGIN_FIXTURES=(
  tests/fixtures/differential_library/target/wasm32-unknown-unknown/release/differential_library.wasm
  tests/fixtures/interpreter_echo/target/wasm32-unknown-unknown/release/interpreter_echo.wasm
  tests/fixtures/sdk_atproto/target/wasm32-unknown-unknown/release/sdk_atproto.wasm
  tests/fixtures/sdk_auth/target/wasm32-unknown-unknown/release/sdk_auth.wasm
  tests/fixtures/sdk_blobs/target/wasm32-unknown-unknown/release/sdk_blobs.wasm
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

# The artefacts belonging to the fixtures named, or all of them when none is
# named, one per line on stdout.
#
# A name matching no entry is reported and makes the call non-zero, so a typo
# in a job that wants a subset cannot quietly select nothing. Every name is
# tried before returning, so one run names them all. Errors go to stderr, so a
# caller may read stdout as paths alone.
plugin_fixtures_select() {
  if [ "$#" -eq 0 ]; then
    printf '%s\n' "${PLUGIN_FIXTURES[@]}"
    return 0
  fi

  local status=0 name matched
  for name in "$@"; do
    matched=$(printf '%s\n' "${PLUGIN_FIXTURES[@]}" | grep "^tests/fixtures/$name/" || true)
    if [ -z "$matched" ]; then
      echo "no listed artefact belongs to fixture: $name" >&2
      status=1
      continue
    fi
    printf '%s\n' "$matched"
  done
  return "$status"
}

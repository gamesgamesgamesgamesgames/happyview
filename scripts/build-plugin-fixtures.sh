#!/usr/bin/env bash
#
# Build the plugin wasm fixtures the tests load.
#
# Every job that runs a fixture-loading test builds through here, so a fixture
# added to the shared list reaches all of them at once.
#
#   [name...]   build only the fixtures named, for a job that needs some and
#               not all. A name matching no entry is a failure.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
cd "$here/.."

# shellcheck source=scripts/plugin-fixtures.sh
. "$here/plugin-fixtures.sh"

artefacts=$(plugin_fixtures_select "$@")

names=()
targets=()
while IFS= read -r artefact; do
  [ -n "$artefact" ] || continue
  names+=("$(echo "$artefact" | cut -d/ -f3)")
  targets+=("$(echo "$artefact" | cut -d/ -f5)")
done <<EOF
$artefacts
EOF

# The targets come out of the same list as the fixtures, so one built for a
# target a job's own step never installed cannot fail there. Skipped when
# rustup is absent, since a toolchain installed by something else manages its
# targets itself and the build below then names the missing one plainly.
if command -v rustup >/dev/null 2>&1; then
  rust_targets=()
  while IFS= read -r target; do
    rust_targets+=("$target")
  done < <(printf '%s\n' "${targets[@]}" | sort -u)
  rustup target add "${rust_targets[@]}"
fi

for i in "${!names[@]}"; do
  cargo build \
    --manifest-path "tests/fixtures/${names[$i]}/Cargo.toml" \
    --target "${targets[$i]}" \
    --release
done

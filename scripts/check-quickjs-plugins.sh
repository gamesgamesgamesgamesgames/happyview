#!/usr/bin/env bash
#
# Whether the real JavaScript and TypeScript interpreter plugins are available
# to tests/quickjs_interpreter_plugins.rs, which skips without them.
#
# They are built artefacts of another repository, so nothing in this one can
# produce them. libtest captures a passing test's skip line, so without this a
# green run would not say that every test in that target made no assertion.
#
#   --require   exit non-zero when either plugin is absent, for a runner that
#               is meant to have them
#   (default)   report and exit zero
set -uo pipefail

missing=0

# check <variable> <language>
check() {
  local var=$1 language=$2
  local dir=${!var:-}

  if [ -z "$dir" ]; then
    echo "$var is unset, so the real $language interpreter is absent"
    missing=1
    return
  fi
  if [ ! -f "$dir/manifest.json" ]; then
    echo "$var names $dir, which holds no manifest.json"
    exit 1
  fi
  # One key off one line, rather than a JSON parser this has no other need
  # for, as check-lua-plugin.sh does.
  local wasm
  wasm=$(sed -n 's/.*"wasm_file"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$dir/manifest.json")
  if [ -z "$wasm" ]; then
    echo "the manifest under $dir names no wasm_file"
    exit 1
  fi
  if [ ! -f "$dir/$wasm" ]; then
    echo "the manifest under $dir names $wasm, which is not there"
    exit 1
  fi
  echo "the real $language interpreter is at $dir"
}

check HAPPYVIEW_JAVASCRIPT_PLUGIN JavaScript
check HAPPYVIEW_TYPESCRIPT_PLUGIN TypeScript

if [ "$missing" = 0 ]; then
  exit 0
fi

cat <<'MESSAGE'

so these did not run:

  tests/quickjs_interpreter_plugins.rs   every test: that a TypeScript
                                         artifact runs without its source and
                                         reports TypeScript lines, that a
                                         script reaches only modules in its
                                         own language, and that validate
                                         reports imports for the host to check

CI fetches pinned releases of both and runs them, so this gap is a local one.
To close it here, fetch those same releases:

  scripts/fetch-quickjs-plugins.sh
  export HAPPYVIEW_JAVASCRIPT_PLUGIN=tests/fixtures/javascript-plugin
  export HAPPYVIEW_TYPESCRIPT_PLUGIN=tests/fixtures/typescript-plugin
MESSAGE

if [ "${1:-}" = "--require" ]; then
  exit 1
fi

echo "(not a failure here: nothing in this repository can build them)"
exit 0

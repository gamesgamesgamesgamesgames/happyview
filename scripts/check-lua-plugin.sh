#!/usr/bin/env bash
#
# Whether the real Lua interpreter plugin is available to the targets that
# load it.
#
# It is a built artefact of another repository and needs a C toolchain, so
# nothing in this repository can produce it and the targets needing it skip
# wherever it was not supplied. libtest captures a passing test's skip line,
# so without this a green run would not say which assertions it did not make
# — and among them are the three refusal messages an operator reads when a
# script will not save.
#
#   --require   exit non-zero when the plugin is absent, for a runner that is
#               meant to have it
#   (default)   report and exit zero
set -uo pipefail

if [ -n "${HAPPYVIEW_LUA_PLUGIN:-}" ]; then
  dir=$HAPPYVIEW_LUA_PLUGIN
  if [ ! -f "$dir/manifest.json" ]; then
    echo "HAPPYVIEW_LUA_PLUGIN names $dir, which holds no manifest.json"
    exit 1
  fi
  # One key off one line, rather than a JSON parser this has no other need
  # for. A directory carrying a manifest without the module it names loads
  # nothing, and saying so here beats the same absence surfacing mid-suite as
  # a test panicking about a file it could not open.
  wasm=$(sed -n 's/.*"wasm_file"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$dir/manifest.json")
  if [ -z "$wasm" ]; then
    echo "the manifest under $dir names no wasm_file"
    exit 1
  fi
  if [ ! -f "$dir/$wasm" ]; then
    echo "the manifest under $dir names $wasm, which is not there"
    exit 1
  fi
  echo "the real Lua interpreter is at $dir"
  if [ -z "${HAPPYVIEW_LUA_SRC:-}" ]; then
    echo "HAPPYVIEW_LUA_SRC is unset, so nothing checks that it was built from the current source"
  fi
  exit 0
fi

cat <<'MESSAGE'
HAPPYVIEW_LUA_PLUGIN is unset, so the real Lua interpreter is absent and
these did not run:

  tests/lua_interpreter_plugin.rs   every test: that the plugin loads, that
                                    its declared capabilities are the ones
                                    its imports need, and that a script runs
                                    through it
  tests/lua_differential.rs         the codemod's rewritten corpus, the
                                    editor templates and the bridge cases,
                                    each against the native runner
  tests/admin_scripts_validate.rs   five of its ten: that a save refuses a
                                    missing `handle`, a body that will not
                                    parse and a file-scope read of a removed
                                    global, each in the words the operator
                                    reads; that a v3 body saves; and that all
                                    five editor templates save

What still ran, so the gap is only the real interpreter's half of it:

  the reference validator's own tests pin those three sentences
  (src/lua/sandbox.rs); src/admin/scripts.rs pins how a refusal is rendered
  from each kind an interpreter can report; and the other five tests in
  tests/admin_scripts_validate.rs, plus tests/admin_scripts_codemod.rs and
  tests/e2e_scripts.rs, pin the save path through the interpreter_echo
  fixture, whose messages are its own.

CI fetches a pinned release of the plugin and runs all of the above against
it, so this gap is a local one.

To close it here, fetch that same release. The download needs no
credentials, and the script holds the pinned version CI uses:

  scripts/fetch-lua-plugin.sh
  export HAPPYVIEW_LUA_PLUGIN=tests/fixtures/lua-plugin

HAPPYVIEW_LUA_SRC is worth setting only for a module built by hand, where it
points at the plugin crate's src/ and turns a stale artefact into a failure
rather than a figure describing neither the code nor a comparison. Building
one is the harder path and needs the happyproto/plugins repository with
wasi-sdk on CC_wasm32_wasip1, for the wasm32-wasip1 target; a release asset
needs neither.
MESSAGE

if [ "${1:-}" = "--require" ]; then
  exit 1
fi

echo "(not a failure here: nothing in this repository can build it)"
exit 0

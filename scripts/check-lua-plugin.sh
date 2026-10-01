#!/usr/bin/env bash
#
# Whether the real Lua interpreter plugin is available to the targets that
# load it.
#
# It is a built artefact of another repository and needs a C toolchain, so
# nothing here can produce it and the targets needing it skip. libtest
# captures a passing test's skip line, so without this a green run would not
# say which assertions it did not make — and among them are the three refusal
# messages an operator reads when a script will not save.
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
  tests/lua_differential.rs         the 84-file corpus and the 34 bridge
                                    cases against the native runner
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

To make them run, point HAPPYVIEW_LUA_PLUGIN at a directory holding the
plugin's manifest.json beside the .wasm it names, and HAPPYVIEW_LUA_SRC at
the plugin crate's src/ so a stale artefact fails rather than reporting.
The plugin is built from the happyview-plugins repository, not this one:
its crate needs wasi-sdk on CC_wasm32_wasip1 and builds for
wasm32-wasip1. Copy its manifest.json and the built .wasm into one
directory and point the variable there.
MESSAGE

if [ "${1:-}" = "--require" ]; then
  exit 1
fi

echo "(not a failure here: nothing in this repository can build it)"
exit 0

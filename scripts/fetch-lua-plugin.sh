#!/usr/bin/env bash
#
# Fetch the pinned release of the Lua interpreter plugin into a directory the
# host can load: its `manifest.json` beside the `.wasm` that manifest names.
#
# The version and hash below are the single pin. CI reads them from here, the
# browser stack's bind mount is filled from here, and a developer pointing
# HAPPYVIEW_LUA_PLUGIN at the result is loading the module CI ran against — so
# "the pinned version" means one thing across all three.
#
# The hash is half of the pin: a tag's release assets stay replaceable, so a
# version on its own would name whichever module was uploaded there most
# recently.
#
#   $1   destination directory (default: tests/fixtures/lua-plugin)
set -euo pipefail

LUA_PLUGIN_VERSION="1.0.0"
LUA_PLUGIN_SHA256="9496eb497590e44c5c8a66441156499c63dd6f9c8b9ec01bf59c9b8a5e11382b"

REPO="happyproto/plugins"
BASE="https://github.com/$REPO/releases/download/happyview-lua-v$LUA_PLUGIN_VERSION"

dest=${1:-tests/fixtures/lua-plugin}

# Linux ships one, macOS the other.
hash_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# The module is named by the manifest rather than spelled twice here, so a
# release whose manifest and assets disagree fails at the second fetch.
read_wasm_file() {
  sed -n 's/.*"wasm_file"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$1"
}

if [ -f "$dest/manifest.json" ]; then
  have=$(read_wasm_file "$dest/manifest.json")
  if [ -n "$have" ] && [ -f "$dest/$have" ] &&
    [ "$(hash_of "$dest/$have")" = "$LUA_PLUGIN_SHA256" ]; then
    echo "happyview-lua v$LUA_PLUGIN_VERSION already at $dest"
    exit 0
  fi
fi

mkdir -p "$dest"
curl -fsSL -o "$dest/manifest.json" "$BASE/manifest.json"

wasm=$(read_wasm_file "$dest/manifest.json")
if [ -z "$wasm" ]; then
  echo "the release manifest names no wasm_file" >&2
  exit 1
fi

curl -fsSL -o "$dest/$wasm" "$BASE/$wasm"

got=$(hash_of "$dest/$wasm")
if [ "$got" != "$LUA_PLUGIN_SHA256" ]; then
  echo "$dest/$wasm hashes $got, and the pin names $LUA_PLUGIN_SHA256" >&2
  exit 1
fi

echo "happyview-lua v$LUA_PLUGIN_VERSION fetched to $dest ($wasm)"

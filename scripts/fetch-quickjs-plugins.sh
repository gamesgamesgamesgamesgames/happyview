#!/usr/bin/env bash
#
# Fetch the pinned releases of the JavaScript and TypeScript interpreter
# plugins, each into a directory the host can load: its `manifest.json` beside
# the `.wasm` that manifest names.
#
# The versions and hashes below are the single pin, as in
# fetch-lua-plugin.sh: CI reads them from here, and a developer pointing
# HAPPYVIEW_JAVASCRIPT_PLUGIN and HAPPYVIEW_TYPESCRIPT_PLUGIN at the result is
# loading the modules CI ran against.
#
# The hash is half of the pin: a tag's release assets stay replaceable, so a
# version on its own would name whichever module was uploaded there most
# recently.
#
#   $1   directory the two plugin directories go under
#        (default: tests/fixtures), as javascript-plugin and typescript-plugin
set -euo pipefail

JAVASCRIPT_PLUGIN_VERSION="1.0.0-alpha.5"
JAVASCRIPT_PLUGIN_SHA256="6c814ecd1a4ce52877cfab33e48464b0db27540f20ce62425edf4ddde5f00f0d"

TYPESCRIPT_PLUGIN_VERSION="1.0.0-alpha.6"
TYPESCRIPT_PLUGIN_SHA256="9232e0614fec929a1323b24cbfb2a02dc6538812c643d7d4d80f5de60b72172d"

REPO="happyproto/plugins"

root=${1:-tests/fixtures}

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

# fetch <language> <version> <sha256>
fetch() {
  local language=$1 version=$2 sha256=$3
  local dest="$root/$language-plugin"
  local base="https://github.com/$REPO/releases/download/happyview-$language-v$version"

  if [ -f "$dest/manifest.json" ]; then
    local have
    have=$(read_wasm_file "$dest/manifest.json")
    if [ -n "$have" ] && [ -f "$dest/$have" ] &&
      [ "$(hash_of "$dest/$have")" = "$sha256" ]; then
      echo "happyview-$language v$version already at $dest"
      return
    fi
  fi

  mkdir -p "$dest"
  curl -fsSL -o "$dest/manifest.json" "$base/manifest.json"

  local wasm
  wasm=$(read_wasm_file "$dest/manifest.json")
  if [ -z "$wasm" ]; then
    echo "the happyview-$language release manifest names no wasm_file" >&2
    exit 1
  fi

  curl -fsSL -o "$dest/$wasm" "$base/$wasm"

  local got
  got=$(hash_of "$dest/$wasm")
  if [ "$got" != "$sha256" ]; then
    echo "$dest/$wasm hashes $got, and the pin names $sha256" >&2
    exit 1
  fi

  echo "happyview-$language v$version fetched to $dest ($wasm)"
}

fetch javascript "$JAVASCRIPT_PLUGIN_VERSION" "$JAVASCRIPT_PLUGIN_SHA256"
fetch typescript "$TYPESCRIPT_PLUGIN_VERSION" "$TYPESCRIPT_PLUGIN_SHA256"

#!/usr/bin/env bash
# A release binary has to link statically on musl and build on Windows with
# no system OpenSSL, so nothing in the graph may pull it in. Checked across
# every target, because native-tls only reaches OpenSSL off Apple and Windows.
#
# The second assertion stops the first from passing for the wrong reason: if
# `cargo tree` resolved nothing, rustls would be absent too.
set -euo pipefail

cd "$(dirname "$0")/.."

normal_deps() {
    cargo tree --quiet --edges normal --target all --prefix none --format '{p}' | sort -u
}

deps="$(normal_deps)"

if grep -qE '^(openssl-sys|native-tls) ' <<<"$deps"; then
    echo "OpenSSL is in the dependency graph:" >&2
    cargo tree --edges normal --target all --invert openssl-sys >&2 || true
    exit 1
fi

if ! grep -q '^rustls ' <<<"$deps"; then
    echo "rustls is absent from the graph, so this check resolved nothing" >&2
    exit 1
fi

echo "no OpenSSL in the dependency graph on any target"

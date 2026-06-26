#!/bin/sh
set -e

TUNNEL_URL_FILE="${TUNNEL_URL_FILE:-}"

if [ -n "$TUNNEL_URL_FILE" ]; then
  rm -f "$TUNNEL_URL_FILE"

  echo "Waiting for tunnel URL..."
  elapsed=0
  while [ ! -f "$TUNNEL_URL_FILE" ] && [ "$elapsed" -lt 30 ]; do
    sleep 1
    elapsed=$((elapsed + 1))
  done

  if [ -f "$TUNNEL_URL_FILE" ]; then
    url=$(cat "$TUNNEL_URL_FILE")
    export PUBLIC_URL="$url"
    echo "Using tunnel URL as PUBLIC_URL: $url"
  else
    echo "Tunnel URL not found after 30s, using PUBLIC_URL from env: $PUBLIC_URL"
  fi
fi

if ! command -v cargo-watch >/dev/null 2>&1; then
  echo "Installing cargo-watch..."
  cargo install cargo-watch
fi

exec "$@"

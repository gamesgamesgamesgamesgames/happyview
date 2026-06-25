#!/bin/sh
set -e

# Install cloudflared if not present
if ! command -v cloudflared >/dev/null 2>&1; then
  ARCH=$(uname -m)
  case "$ARCH" in
    x86_64|amd64) CF_ARCH="amd64" ;;
    aarch64|arm64) CF_ARCH="arm64" ;;
    armv7l) CF_ARCH="arm" ;;
    *) echo "Unsupported architecture: $ARCH"; exit 1 ;;
  esac
  echo "Installing cloudflared ($CF_ARCH)..."
  wget -q -O /usr/local/bin/cloudflared \
    "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-$CF_ARCH"
  chmod +x /usr/local/bin/cloudflared
fi

UPSTREAM="${TUNNEL_UPSTREAM:-http://happyview:3000}"
URL_FILE="${TUNNEL_URL_FILE:-/shared/tunnel-url}"

if [ -n "$CLOUDFLARE_TUNNEL_TOKEN" ]; then
  if [ -n "$TUNNEL_HOSTNAME" ]; then
    mkdir -p "$(dirname "$URL_FILE")"
    echo "https://$TUNNEL_HOSTNAME" > "$URL_FILE"
  fi
  echo "══════════════════════════════════════════════════════════════"
  echo "  Starting named Cloudflare tunnel"
  echo "  Hostname: ${TUNNEL_HOSTNAME:-<configured in Cloudflare>}"
  echo "  Upstream: $UPSTREAM"
  echo "══════════════════════════════════════════════════════════════"
  exec cloudflared tunnel run --token "$CLOUDFLARE_TUNNEL_TOKEN"
fi

rm -f "$URL_FILE"

echo "══════════════════════════════════════════════════════════════"
echo "  Starting quick Cloudflare tunnel"
echo "  Upstream: $UPSTREAM"
echo "══════════════════════════════════════════════════════════════"

cloudflared tunnel --url "$UPSTREAM" 2>&1 | while IFS= read -r line; do
  echo "$line"
  case "$line" in
    *trycloudflare.com*)
      url=$(echo "$line" | grep -o 'https://[a-zA-Z0-9._-]*trycloudflare\.com' | head -1)
      if [ -n "$url" ]; then
        mkdir -p "$(dirname "$URL_FILE")"
        echo "$url" > "$URL_FILE"
        echo "══════════════════════════════════════════════════════════════"
        echo "  Tunnel URL written to $URL_FILE"
        echo "══════════════════════════════════════════════════════════════"
      fi
      ;;
  esac
done

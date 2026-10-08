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

# A locally-managed tunnel: `cloudflared tunnel create` writes <tunnel-id>.json
# into the mounted credentials directory, and the ingress rules are generated
# here from TUNNEL_HOSTNAME and the upstream instead of living in the
# Cloudflare dashboard.
CREDENTIALS_DIR="${TUNNEL_CREDENTIALS_DIR:-/etc/cloudflared}"
CREDENTIALS_FILE=$(ls "$CREDENTIALS_DIR"/*.json 2>/dev/null | head -1 || true)

if [ -n "$CREDENTIALS_FILE" ]; then
  if [ -z "$TUNNEL_HOSTNAME" ]; then
    echo "Found $CREDENTIALS_FILE but TUNNEL_HOSTNAME is not set."
    echo "Set TUNNEL_HOSTNAME in .env to the hostname routed to this tunnel."
    exit 1
  fi

  TUNNEL_ID=$(sed -n 's/.*"TunnelID" *: *"\([^"]*\)".*/\1/p' "$CREDENTIALS_FILE")
  if [ -z "$TUNNEL_ID" ]; then
    echo "$CREDENTIALS_FILE has no TunnelID. Is it a tunnel credentials file?"
    exit 1
  fi

  CONFIG_FILE=/tmp/cloudflared.yml
  cat > "$CONFIG_FILE" <<EOF
tunnel: $TUNNEL_ID
credentials-file: $CREDENTIALS_FILE
ingress:
  - hostname: $TUNNEL_HOSTNAME
    service: $UPSTREAM
  - service: http_status:404
EOF

  mkdir -p "$(dirname "$URL_FILE")"
  echo "https://$TUNNEL_HOSTNAME" > "$URL_FILE"
  echo "══════════════════════════════════════════════════════════════"
  echo "  Starting locally-managed Cloudflare tunnel"
  echo "  Tunnel:   $TUNNEL_ID"
  echo "  Hostname: $TUNNEL_HOSTNAME"
  echo "  Upstream: $UPSTREAM"
  echo "══════════════════════════════════════════════════════════════"
  exec cloudflared tunnel --config "$CONFIG_FILE" run
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

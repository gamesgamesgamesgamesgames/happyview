#!/bin/sh
set -e

# Wait for Caddy's internal CA certificate to appear in the shared volume,
# then install it so reqwest (native-tls / OpenSSL) trusts TLS connections
# proxied through Caddy (e.g. the PDS OAuth endpoints).
CA_CERT=/caddy-data/caddy/pki/authorities/local/root.crt
if [ -d /caddy-data ]; then
  echo "Waiting for Caddy CA certificate..."
  while [ ! -f "$CA_CERT" ]; do sleep 0.5; done
  cp "$CA_CERT" /usr/local/share/ca-certificates/caddy-local.crt
  update-ca-certificates 2>/dev/null
  echo "Caddy CA certificate installed."
fi

exec /entrypoint.sh

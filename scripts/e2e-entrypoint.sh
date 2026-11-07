#!/bin/sh
set -e

# Wait for Caddy's internal CA certificate to appear in the shared volume,
# then install it into the system store, which reqwest (rustls with native
# roots) reads, so it trusts TLS connections proxied through Caddy (e.g. the
# PDS OAuth endpoints).
#
# Reading Caddy's CA (its pki/ tree is root-owned, mode 0700 to protect the CA
# key) and installing it into the system trust store both require root, so the
# e2e compose runs this container as root. The production image also runs as
# root (see the Dockerfile note on the reverted non-root user), so after the CA
# bootstrap we run the server directly — matching production, no privilege drop.
CA_CERT=/caddy-data/caddy/pki/authorities/local/root.crt
if [ -d /caddy-data ]; then
  echo "Waiting for Caddy CA certificate..."
  while [ ! -f "$CA_CERT" ]; do sleep 0.5; done
  cp "$CA_CERT" /usr/local/share/ca-certificates/caddy-local.crt
  update-ca-certificates 2>/dev/null
  echo "Caddy CA certificate installed."
fi

exec /entrypoint.sh "$@"

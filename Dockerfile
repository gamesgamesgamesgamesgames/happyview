FROM node:22-alpine AS frontend

WORKDIR /app/web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ .
ENV NEXT_PUBLIC_BASE_PATH=/__HAPPYVIEW_BP__
RUN npm run build

FROM rust:1.96.1-bookworm AS builder

WORKDIR /app

# Build dependencies first (cached until Cargo.toml/Cargo.lock change)
# Every workspace member needs its manifest and a stub source here, or cargo
# cannot resolve the workspace and the dependency-cache layer fails outright.
# Adding a crate under crates/ means adding it to this list too.
COPY Cargo.toml Cargo.lock ./
COPY crates/happyview-nsid/Cargo.toml crates/happyview-nsid/
COPY crates/happyview-plc/Cargo.toml crates/happyview-plc/
COPY crates/happyview-scopes/Cargo.toml crates/happyview-scopes/
RUN mkdir -p crates/happyview-nsid/src crates/happyview-plc/src crates/happyview-scopes/src \
    && touch crates/happyview-nsid/src/lib.rs crates/happyview-plc/src/lib.rs crates/happyview-scopes/src/lib.rs
RUN mkdir -p src/bin && echo "fn main() {}" > src/main.rs && touch src/lib.rs && echo "fn main() {}" > src/bin/migrate_lua_sql.rs && echo "fn main() {}" > src/bin/migrate_space_cids.rs
ENV SQLX_OFFLINE=true
RUN cargo build --release && rm -rf src target/release/.fingerprint/happyview-*

# Build application code
COPY src/ src/
COPY crates/ crates/
COPY migrations/ migrations/
ARG HAPPYVIEW_VERSION
ENV HAPPYVIEW_VERSION=$HAPPYVIEW_VERSION
RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# NOTE: The container runs as root. A previous attempt to run as a non-root user
# (uid 10001) broke upgrades for existing SQLite deployments — mounted data
# volumes (e.g. Railway volumes, root-owned bind mounts) were not writable by the
# non-root user, causing "attempt to write a readonly database" on the first
# migration. Running as root sidesteps volume-ownership entirely. Non-root will be
# reintroduced as a documented breaking change in a future major release, with the
# entrypoint chowning the actual (operator-configurable) data directory before
# dropping privileges. See the L5 note in the security review.
WORKDIR /app

COPY --from=builder /app/target/release/happyview /usr/local/bin/happyview
RUN chmod +x /usr/local/bin/happyview
COPY migrations/ /app/migrations
COPY --from=frontend /app/web/out /srv/static
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh && touch /srv/static/.base-path-pending

ENV STATIC_DIR=/srv/static

EXPOSE 3000

ENTRYPOINT ["/entrypoint.sh"]

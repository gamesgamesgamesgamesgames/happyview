#!/usr/bin/env bash
#
# Backfill a fixed set of repos into a fresh SQLite database and report what
# it cost on disk: the write-ahead log's peak size, the database's final size,
# and the bytes the server process wrote.
#
#   scripts/measure-backfill-storage.sh [dids-file] [collection]
#
# Defaults: scripts/measure-backfill-dids.txt and app.bsky.feed.post. Run it on
# a checkout before a change and on one after it, then compare the summaries.
# It reaches the server only through the HTTP API, so any version can be
# measured. Bytes written come from /proc/<pid>/io and are reported on Linux
# only; elsewhere write_bytes reads "unavailable". Needs cargo, curl, jq and
# sqlite3, and reaches the real PLC directory and PDSes. Jetstream is pointed
# at a closed port so live ingest stays out of the numbers.
#
#   MEASURE_PORT      port for the throwaway server (default 3917)
#   MEASURE_PROFILE   cargo profile to build and run (default release)
#
# The WAL is sampled every 0.2s, so its peak is a lower bound.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
dids_file=${1:-"$here/measure-backfill-dids.txt"}
collection=${2:-app.bsky.feed.post}
port=${MEASURE_PORT:-3917}
profile=${MEASURE_PROFILE:-release}

for tool in cargo curl jq sqlite3; do
  command -v "$tool" >/dev/null || { echo "missing required tool: $tool" >&2; exit 1; }
done
if command -v sha256sum >/dev/null; then
  sha256() { sha256sum | cut -d' ' -f1; }
else
  sha256() { shasum -a 256 | cut -d' ' -f1; }
fi

# A DID list with nothing in it would make the API run a network-wide
# backfill, so refuse it here.
dids_json=$(grep -v '^[[:space:]]*#' "$dids_file" | grep -v '^[[:space:]]*$' | jq -R . | jq -s .)
if [[ "$(jq length <<<"$dids_json")" -eq 0 ]]; then
  echo "no DIDs in $dids_file" >&2
  exit 1
fi

work=$(mktemp -d)
db="$work/happyview.db"
log="$work/server.log"
token="hv_measure_$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"
token_hash=$(printf '%s' "$token" | sha256)
base="http://127.0.0.1:$port"
pid=""

cleanup() {
  if [[ -n "$pid" ]]; then kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi
  echo "work dir kept for inspection: $work" >&2
}
trap cleanup EXIT

size_of() { if [[ -f "$1" ]]; then wc -c <"$1" | tr -d ' '; else echo 0; fi; }

proc_write_bytes() { awk '/^write_bytes/ {print $2}' "/proc/$pid/io"; }

# The binary runs directly from the main shell, not a subshell, so $! is the
# server's own pid and /proc/$pid/io describes the server.
start_server() {
  cd "$root"
  DATABASE_URL="sqlite://$db?mode=rwc" \
    PUBLIC_URL="$base" HOST=127.0.0.1 PORT="$port" \
    SESSION_SECRET="measure-$(printf 'x%.0s' {1..64})" \
    JETSTREAM_URL="ws://127.0.0.1:9" \
    RUST_LOG="happyview=info" \
    "$bin" >>"$log" 2>&1 &
  pid=$!
  for _ in $(seq 1 120); do
    curl -fsS "$base/health" >/dev/null 2>&1 && return 0
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.5
  done
  echo "server did not become healthy; see $log" >&2
  exit 1
}

stop_server() {
  kill "$pid"
  wait "$pid" 2>/dev/null || true
  pid=""
}

echo "building $profile binary..." >&2
cargo_profile=(--release)
[[ "$profile" == release ]] || cargo_profile=(--profile "$profile")
cargo build "${cargo_profile[@]}" --bin happyview --manifest-path "$root/Cargo.toml" >&2
profile_dir=$profile
[[ "$profile" != dev ]] || profile_dir=debug
bin="${CARGO_TARGET_DIR:-$root/target}/$profile_dir/happyview"

# First boot creates the schema; then seed a super user, an API key and the
# record lexicon directly, with the server stopped.
start_server
stop_server
sqlite3 "$db" <<SQL
INSERT INTO happyview_users (id, did, is_super, created_at)
  VALUES ('measure-user', 'did:plc:measureadmin', 1, datetime('now'));
INSERT INTO happyview_api_keys (id, user_id, name, key_hash, key_prefix, permissions)
  VALUES ('measure-key', 'measure-user', 'measure', '$token_hash', 'hv_measure', '["backfill:create","backfill:read"]');
INSERT INTO happyview_lexicons (id, lexicon_json, backfill)
  VALUES ('$collection', '{"lexicon":1,"id":"$collection","defs":{"main":{"type":"record","key":"tid"}}}', 0);
SQL

# Measure from a clean WAL.
sqlite3 "$db" "PRAGMA wal_checkpoint(TRUNCATE);" >/dev/null
start_server
write_bytes_before=0
if [[ -r "/proc/$pid/io" ]]; then
  write_bytes_before=$(proc_write_bytes)
fi

job_id=$(curl -fsS -X POST "$base/admin/backfill" \
  -H "Authorization: Bearer $token" -H 'content-type: application/json' \
  -d "{\"collection\": \"$collection\", \"dids\": $dids_json}" | jq -r .id)
echo "backfill job $job_id started" >&2

started=$(date +%s)
wal_peak=0
status=running
tick=0
while [[ "$status" == running || "$status" == pausing || "$status" == cancelling ]]; do
  wal=$(size_of "$db-wal")
  (( wal > wal_peak )) && wal_peak=$wal
  if (( tick % 5 == 0 )); then
    status=$(curl -fsS "$base/admin/backfill/status" -H "Authorization: Bearer $token" \
      | jq -r --arg id "$job_id" '.[] | select(.id == $id) | .status')
  fi
  tick=$(( tick + 1 ))
  sleep 0.2
done
seconds=$(( $(date +%s) - started ))

write_bytes="unavailable (no /proc/<pid>/io on this OS)"
if [[ -r "/proc/$pid/io" ]]; then
  write_bytes=$(( $(proc_write_bytes) - write_bytes_before ))
fi
wal_at_end=$(size_of "$db-wal")
records=$(sqlite3 "$db" "SELECT COUNT(*) FROM happyview_records WHERE collection = '$collection';")
stop_server

cat <<REPORT
job_status=$status
records=$records
seconds=$seconds
wal_peak_bytes=$wal_peak
wal_bytes_at_end=$wal_at_end
db_bytes=$(size_of "$db")
write_bytes=$write_bytes
REPORT

#!/usr/bin/env bash
# Runs the release-record scripts against a stub gh, so no network.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
fail() { echo "registry.test: $*" >&2; exit 1; }

# --- channels.sh ---------------------------------------------------------------
channels() { bash "$here/registry/channels.sh" "$@" | paste -sd' ' -; }
[ "$(channels 3.0.0-alpha.4 2)" = "v3-alpha" ] || fail "alpha"
[ "$(channels v3.0.0-beta.12 2)" = "v3-beta" ] || fail "beta"
[ "$(channels 2.17.4 2)" = "v2 latest" ] || fail "current stable major"
[ "$(channels 2.17.5 3)" = "v2" ] || fail "an older stable major must leave latest alone"
[ "$(channels 3.0.0 3)" = "v3 latest" ] || fail "new major"
[ "$(channels 2.17.0-dev.2 2)" = "" ] || fail "a dev prerelease follows no channel"
[ "$(channels 2.11.0-next.1 2)" = "" ] || fail "a next prerelease follows no channel"
if bash "$here/registry/channels.sh" 2.17.4 2>/dev/null; then fail "no latest major accepted"; fi

# --- release-inputs.sh -----------------------------------------------------------
mkdir "$work/bin"
cat > "$work/bin/gh" <<'STUB'
#!/usr/bin/env bash
case "$1 $2" in
  "release view")
    case "$3" in v[0-9]*) ;; *) exit 1 ;; esac
    draft=false; [ "$3" = v9.9.9 ] && draft=true
    echo '{"publishedAt":"2026-11-01T12:00:00Z","url":"https://github.com/gamesgamesgamesgamesgames/happyview/releases/tag/'"$3"'","isDraft":'"$draft"'}' ;;
  "release list")
    printf '%s\n' v3.0.0-alpha.10 v3.0.0-beta.2 v2.17.4 v3.0.0-alpha.9 \
      v2.17.0-dev.2 v2.16.0-beta.1 v2.1.0 v1.0.0 @happyview/oauth-client-v1.4.3 ;;
  "api repos/gamesgamesgamesgamesgames/happyview/commits/"*)
    echo 0123456789abcdef0123456789abcdef01234567 ;;
  *) echo "unexpected gh $*" >&2; exit 1 ;;
esac
STUB
chmod +x "$work/bin/gh"
export PATH="$work/bin:$PATH"

e="$(REGISTRY_LATEST_MAJOR=2 bash "$here/registry/release-inputs.sh" v2.17.4)"
[ "$(jq -r .version <<<"$e")" = 2.17.4 ] || fail "version"
[ "$(jq -r .tag <<<"$e")" = v2.17.4 ] || fail "tag"
[ "$(jq -r .commit <<<"$e")" = 0123456789abcdef0123456789abcdef01234567 ] || fail "commit"
[ "$(jq -r .repository <<<"$e")" = https://github.com/gamesgamesgamesgamesgames/happyview.git ] || fail "repository"
[ "$(jq -r '."changelog-url"' <<<"$e")" = https://github.com/gamesgamesgamesgamesgames/happyview/releases/tag/v2.17.4 ] || fail "changelog"
[ "$(jq -r '."published-at"' <<<"$e")" = 2026-11-01T12:00:00Z ] || fail "published-at"
[ "$(jq -c .channels <<<"$e")" = '["v2","latest"]' ] || fail "channels: $(jq -c .channels <<<"$e")"
# Only pointers: nothing in the entry is a blob.
[ "$(jq '[.. | objects | select(."$type" == "blob" or has("ref"))] | length' <<<"$e")" = 0 ] || fail "blob"
# The same tag, the same entry.
[ "$e" = "$(REGISTRY_LATEST_MAJOR=2 bash "$here/registry/release-inputs.sh" v2.17.4)" ] || fail "not deterministic"

n="$(bash "$here/registry/release-inputs.sh" v3.0.0-beta.2 --no-channels)"
[ "$(jq -c .channels <<<"$n")" = '[]' ] || fail "--no-channels"

if REGISTRY_LATEST_MAJOR=2 bash "$here/registry/release-inputs.sh" v9.9.9 2>/dev/null; then fail "a draft release was accepted"; fi
if bash "$here/registry/release-inputs.sh" 'v1.0.0|x' 2>/dev/null; then fail "a bad tag was accepted"; fi
if bash "$here/registry/release-inputs.sh" v2.17.4 2>/dev/null; then fail "channels without REGISTRY_LATEST_MAJOR"; fi

# --- backfill-list.sh -------------------------------------------------------------
REGISTRY_LATEST_MAJOR=2 bash "$here/registry/backfill-list.sh" >"$work/list.json" 2>"$work/plan" \
    || { cat "$work/plan" >&2; fail "backfill-list failed"; }
order="$(jq -r 'map(.version) | join(" ")' "$work/list.json")"
[ "$order" = "2.1.0 2.17.4 3.0.0-alpha.9 3.0.0-alpha.10 3.0.0-beta.2" ] || fail "order: $order"
# Only each channel's newest version carries that channel.
[ "$(jq -c 'map({(.version): .channels}) | add' "$work/list.json")" \
    = '{"2.1.0":[],"2.17.4":["latest","v2"],"3.0.0-alpha.9":[],"3.0.0-alpha.10":["v3-alpha"],"3.0.0-beta.2":["v3-beta"]}' ] \
    || fail "channels: $(jq -c 'map({(.version): .channels}) | add' "$work/list.json")"
grep -q "5 releases" "$work/plan" || fail "summary"

REGISTRY_LATEST_MAJOR=2 bash "$here/registry/backfill-list.sh" --include-legacy >"$work/legacy.json" 2>/dev/null
[ "$(jq length "$work/legacy.json")" = 8 ] || fail "legacy scope: $(jq length "$work/legacy.json")"
# A 2.x beta is in v2-beta, never v3-beta.
[ "$(jq -c '.[] | select(.version == "2.16.0-beta.1") | .channels' "$work/legacy.json")" = '["v2-beta"]' ] \
    || fail "2.x beta channel"

echo "registry scripts: ok"

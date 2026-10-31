#!/usr/bin/env bash
# Prints the `cargo test` target arguments for one shard of the e2e suite:
#
#   cargo test $(bash scripts/e2e-shard.sh 1 2)
#
# The shards together are exactly what `cargo test --tests` runs for the
# `happyview` package: its lib, its bins and every integration test without
# `required-features`. Targets are read from `cargo metadata`, never listed by
# hand, so a new target always lands in some shard.
#
# Assignment is longest-first onto the lightest shard. `weight` holds rough
# seconds for the targets that dominate a run (measured on CI); anything not
# named counts as DEFAULT_WEIGHT. A stale weight only unbalances the shards.
#
# `--list` prints one `<shard>\t<weight>\t<target>` line per target instead.
set -euo pipefail

usage() {
  echo "usage: $0 <shard> <count> | --list <count>" >&2
  exit 2
}

[[ $# -eq 2 ]] || usage
if [[ $1 == --list ]]; then
  list=1
  shard=0
else
  list=0
  shard=$1
fi
count=$2
[[ $count =~ ^[1-9][0-9]*$ ]] || usage
if [[ $list -eq 0 ]] && { ! [[ $shard =~ ^[1-9][0-9]*$ ]] || ((shard > count)); }; then
  usage
fi

DEFAULT_WEIGHT=5
weight() {
  case $1 in
    lib:happyview) echo 180 ;;
    test:jetstream_live) echo 200 ;;
    test:test_runner) echo 140 ;;
    test:test_runner_imports) echo 135 ;;
    test:record_label_script_runners) echo 45 ;;
    test:script_starters) echo 45 ;;
    test:quickjs_interpreter_plugins) echo 45 ;;
    test:jetstream_replay) echo 40 ;;
    test:external_storage) echo 35 ;;
    test:ingest_pipeline) echo 35 ;;
    test:plugin_interpreter) echo 27 ;;
    test:lua_interpreter_plugin) echo 25 ;;
    *) echo "$DEFAULT_WEIGHT" ;;
  esac
}

root=$(cd "$(dirname "$0")/.." && pwd)
targets=$(
  cargo metadata --no-deps --format-version 1 --manifest-path "$root/Cargo.toml" |
    jq -r '.packages[] | select(.name == "happyview") | .targets[]
      | select(.test and ((.["required-features"] // []) | length == 0))
      | select(.kind[0] == "lib" or .kind[0] == "bin" or .kind[0] == "test")
      | "\(.kind[0]):\(.name)"'
)
[[ -n $targets ]] || { echo "$0: cargo metadata listed no targets" >&2; exit 1; }

# Heaviest first, ties by name, so every machine computes the same split.
sorted=$(
  while read -r t; do
    printf '%s\t%s\n' "$(weight "$t")" "$t"
  done <<<"$targets" | sort -t$'\t' -k1,1nr -k2,2
)

load=()
for ((i = 1; i <= count; i++)); do load[i]=0; done

args=()
while IFS=$'\t' read -r weight target; do
  best=1
  for ((i = 2; i <= count; i++)); do
    ((load[i] < load[best])) && best=$i
  done
  load[best]=$((load[best] + weight))
  if [[ $list -eq 1 ]]; then
    printf '%s\t%s\t%s\n' "$best" "$weight" "$target"
  elif [[ $best -eq $shard ]]; then
    kind=${target%%:*}
    name=${target#*:}
    case $kind in
      lib) args+=(--lib) ;;
      bin) args+=(--bin "$name") ;;
      test) args+=(--test "$name") ;;
    esac
  fi
done <<<"$sorted"

[[ $list -eq 1 ]] || echo "${args[*]}"

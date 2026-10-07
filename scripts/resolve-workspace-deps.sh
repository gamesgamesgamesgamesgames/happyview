#!/usr/bin/env bash
#
# Rewrite a package's `workspace:` dependency ranges to published versions, so
# a consumer installing the tarball can resolve what it depends on.
#
#   resolve-workspace-deps.sh <package-dir>
#
# The channel is $GITHUB_REF_NAME. A prerelease channel's dist-tag is its
# branch name, which is also what semantic-release passes to `npm publish
# --tag`, so the two agree without a list to keep in step; the release branch
# publishes to npm's own `latest`.
#
# The dist-tag for this channel need not exist, and a job must not assume it
# does. A dependency whose commits warrant no release publishes nothing, so a
# channel carries a dist-tag for it only once something has actually shipped
# there — which the first release on a newly opened channel meets on every
# dependency at once. The newest channel that has released it is then the
# version to depend on, so the tags are tried in turn, newest first: the
# channels up to this one, read out of the package's own release config so
# their order is stated once rather than restated here.
#
# The embedded node scripts are single-quoted so that their template literals
# reach node rather than the shell; every value they read comes in as argv.
# shellcheck disable=SC2016
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
cd "$here/.." || exit 1

pkg_dir=${1:-}
if [ -z "$pkg_dir" ]; then
  echo "usage: $(basename "$0") <package-dir>" >&2
  exit 2
fi

manifest="$pkg_dir/package.json"
release_config="$pkg_dir/.releaserc.json"
for required in "$manifest" "$release_config"; do
  if [ ! -f "$required" ]; then
    echo "not a released package, so there is no channel to resolve against: $pkg_dir" >&2
    echo "  missing $required" >&2
    exit 2
  fi
done

branch=${GITHUB_REF_NAME:-}
if [ -z "$branch" ]; then
  echo "GITHUB_REF_NAME names the channel to resolve against, and is unset" >&2
  exit 2
fi

# Space-separated, so the caller may walk it with word splitting.
chain=$(node -e '
  const fs = require("fs");
  const [configPath, branch] = process.argv.slice(1);
  const branches = (JSON.parse(fs.readFileSync(configPath, "utf8")).branches || [])
    .map((b) => (typeof b === "string" ? { name: b } : b));
  const at = branches.findIndex((b) => b.name === branch);
  if (at < 0) {
    console.error(`${configPath} releases no channel from branch ${branch}`);
    process.exit(1);
  }
  // Prereleases default their channel to the branch name; the release branch
  // has no channel of its own and takes the npm default.
  const tagFor = (b) =>
    typeof b.channel === "string" ? b.channel : b.prerelease ? b.name : "latest";
  console.log(branches.slice(0, at + 1).reverse().map(tagFor).join(" "));
' "$release_config" "$branch") || exit 1

echo "$pkg_dir on $branch resolves through dist-tags: $chain"

workspace_deps=$(node -e '
  const fs = require("fs");
  const deps = JSON.parse(fs.readFileSync(process.argv[1], "utf8")).dependencies || {};
  for (const [name, range] of Object.entries(deps)) {
    if (String(range).startsWith("workspace:")) console.log(name);
  }
' "$manifest") || exit 1

status=0

while IFS= read -r dep; do
  [ -n "$dep" ] || continue

  version=""
  for tag in $chain; do
    candidate=$(npm view "$dep@$tag" version 2>/dev/null | tail -n 1)
    # A version npm declined to print is not one to depend on: it would reach
    # the manifest as an empty range, which resolves to anything.
    if [[ "$candidate" =~ ^[0-9]+\.[0-9]+\.[0-9]+ ]]; then
      version="$candidate"
      echo "  $dep -> ^$version (dist-tag $tag)"
      break
    fi
  done

  if [ -z "$version" ]; then
    echo "$dep is published under none of: $chain" >&2
    echo "  nothing this channel falls back to has published it, so no range installs it" >&2
    status=1
    continue
  fi

  node -e '
    const fs = require("fs");
    const [manifest, dep, version] = process.argv.slice(1);
    const pkg = JSON.parse(fs.readFileSync(manifest, "utf8"));
    pkg.dependencies[dep] = `^${version}`;
    fs.writeFileSync(manifest, JSON.stringify(pkg, null, 2) + "\n");
  ' "$manifest" "$dep" "$version" || status=1
done <<EOF
$workspace_deps
EOF

exit "$status"

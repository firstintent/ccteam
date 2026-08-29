#!/usr/bin/env bash
# Build one `@ccteam/engine-<os>-<cpu>` tarball from a prebuilt ccteam binary.
#
#   plugins/engine-packages/pack-engine.sh <os-cpu> <binary> <version> [outdir]
#
# The four platform packages are how `dsh plugin add @ccteam/ccteam-ui` brings
# an engine with it (PRD v0.10.5 D2). They carry `os`/`cpu`, so npm and pnpm
# install exactly the one that matches the machine and skip the other three,
# and they carry NO lifecycle script — pnpm 10 blocks postinstall by default,
# which is precisely why the binary rides a package instead of a download hook.
#
# The template in `<os-cpu>/package.json` is the shape; this script stamps the
# version onto a copy. Nothing writes back into the template: the version in
# git stays `0.0.0-template`, so no tag can be implied by the checkout.
#
# The release pipeline (PLUG-5) runs this once per platform with the binary
# that tag built. Locally it takes any ccteam binary, which is what makes the
# whole install path testable without publishing anything.
set -euo pipefail

usage() {
  echo "usage: $(basename "$0") <os-cpu> <binary> <version> [outdir]" >&2
  echo "  os-cpu: linux-x64 | linux-arm64 | darwin-x64 | darwin-arm64" >&2
  exit 2
}

[ $# -ge 3 ] || usage
tuple="$1"
binary="$2"
version="$3"
outdir="${4:-$PWD}"

here="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
template="$here/$tuple/package.json"
[ -f "$template" ] || { echo "pack-engine: no template for platform '$tuple'" >&2; usage; }
[ -f "$binary" ] || { echo "pack-engine: no such binary: $binary" >&2; exit 1; }

mkdir -p "$outdir"
outdir="$(cd -- "$outdir" && pwd)"
staging="$(mktemp -d)"
trap 'rm -rf "$staging"' EXIT

mkdir -p "$staging/bin"
# `install -m 0755` is the point: npm preserves the executable bit from the
# archive, and a 644 `bin/ccteam` would install a binary nothing can run.
install -m 0755 "$binary" "$staging/bin/ccteam"
node -e '
  const fs = require("node:fs")
  const pkg = JSON.parse(fs.readFileSync(process.argv[1], "utf8"))
  pkg.version = process.argv[3]
  fs.writeFileSync(process.argv[2], JSON.stringify(pkg, null, 2) + "\n")
' "$template" "$staging/package.json" "$version"

(cd "$staging" && npm pack --pack-destination "$outdir" >/dev/null)
tgz="$(find "$outdir" -maxdepth 1 -name "ccteam-engine-$tuple-$version.tgz" -print -quit)"
[ -n "$tgz" ] || { echo "pack-engine: npm pack produced no tarball for $tuple" >&2; exit 1; }

# The one failure this script exists to catch: a tarball whose engine is not
# executable installs a plugin that can never start a daemon.
mode="$(tar tvzf "$tgz" | awk '$NF == "package/bin/ccteam" { print $1 }')"
case "$mode" in
  *x*) ;;
  *) echo "pack-engine: $tgz ships package/bin/ccteam without the executable bit ($mode)" >&2; exit 1 ;;
esac

echo "$tgz"
sha256sum "$tgz" 2>/dev/null || shasum -a 256 "$tgz"

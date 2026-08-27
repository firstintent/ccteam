#!/usr/bin/env bash
# Rebuild the embedded plugin tarballs that `execution/dsh_acp/materialize.rs`
# ships with `include_bytes!`.
#
#   plugins/ccteam-client -> crates/ccteam-harness/src/execution/dsh_acp/assets/ccteam-client.tgz
#   plugins/ccteam-ui  -> crates/ccteam-harness/src/execution/dsh_acp/assets/ccteam-ui.tgz
#
# Rust builds must never require npm or node (same rule as the checked-in Pi
# bridge asset), so the tarballs are produced here and committed alongside the
# Rust change.
#
# `npm pack` is byte-reproducible for a given (source, dependency tree, npm
# version), which is what makes the sha256 cache key in materialize.rs stable.
#
# The guard below is the point of this script: `npm pack` folds
# `bundledDependencies` in from `node_modules`, so packing WITHOUT installing
# first silently produces a tarball whose plugin cannot resolve
# `@deepseek-ai/schemastery` at runtime — and every Rust test still passes,
# because they only assert the archive extracts. Refuse to publish such a
# tarball instead.
#
# Usage: plugins/pack-assets.sh [plugin ...]   (default: both)
set -euo pipefail

here="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd -- "$here/.." && pwd)"
assets="$repo/crates/ccteam-harness/src/execution/dsh_acp/assets"

plugins=("$@")
if [ ${#plugins[@]} -eq 0 ]; then
  plugins=(ccteam-client ccteam-ui)
fi

mkdir -p "$assets"
staging="$(mktemp -d)"
trap 'rm -rf "$staging"' EXIT

for plugin in "${plugins[@]}"; do
  src="$repo/plugins/$plugin"
  [ -d "$src" ] || { echo "pack-assets: no such plugin: $plugin" >&2; exit 1; }
  echo "==> $plugin"

  (cd "$src" && npm ci --no-audit --no-fund && npm run build)

  out="$staging/$plugin"
  mkdir -p "$out"
  (cd "$src" && npm pack --pack-destination "$out" >/dev/null)
  tgz="$(find "$out" -maxdepth 1 -name '*.tgz' -print -quit)"
  [ -n "$tgz" ] || { echo "pack-assets: $plugin produced no tarball" >&2; exit 1; }

  listing="$out/listing.txt"
  tar tzf "$tgz" > "$listing"
  grep -qx 'package/package.json' "$listing" \
    || { echo "pack-assets: $plugin tarball has no package/package.json" >&2; exit 1; }

  # Every bundledDependency must have landed inside the tarball.
  bundled="$(node -e 'const p=require(process.argv[1]);for(const d of p.bundledDependencies??p.bundleDependencies??[])console.log(d)' "$src/package.json")"
  while IFS= read -r dep; do
    [ -n "$dep" ] || continue
    grep -qx "package/node_modules/$dep/package.json" "$listing" || {
      echo "pack-assets: $plugin tarball is missing bundled dependency $dep" >&2
      echo "             (run npm ci in $src first — an un-bundled tarball ships a plugin that cannot boot)" >&2
      exit 1
    }
  done <<< "$bundled"

  install -m 0644 "$tgz" "$assets/$plugin.tgz"
  sha256sum "$assets/$plugin.tgz"
done

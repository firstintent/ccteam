#!/usr/bin/env bash
# Rebuild the embedded plugin tarball that `execution/dsh_acp/materialize.rs`
# ships with `include_bytes!`.
#
#   plugins/ccteam-ui -> crates/ccteam-harness/src/execution/dsh_acp/assets/ccteam-ui.tgz
#
# Rust builds must never require npm or node (same rule as the checked-in Pi
# bridge asset), so the tarballs are produced here and committed alongside the
# Rust change.
#
# `npm pack` is byte-reproducible for a given (source, dependency tree, npm
# version) — INCLUDING across checkouts at different paths, which is what makes
# the sha256 cache key in materialize.rs stable and a committed tarball
# reviewable. That property is not free: rolldown prints module ids into
# `//#region` comments, so a build whose virtual ids were absolute embedded the
# checkout path (found by the DSH2-MERGE checker: the committed tarball hashed
# 7bddf3… while a fresh pack from another worktree hashed abac17…). The ids are
# package-relative now (plugins/ccteam-ui/build/css-plugins.ts), and the
# two-path check below is what keeps them that way: it rebuilds and re-packs
# each plugin from a second, differently-named directory and refuses to publish
# a tarball whose bytes depend on where the repo happens to live.
#
# The guard below is the point of this script: `npm pack` folds
# `bundledDependencies` in from `node_modules`, so packing WITHOUT installing
# first silently produces a tarball whose plugin cannot resolve
# `@deepseek-ai/schemastery` at runtime — and every Rust test still passes,
# because they only assert the archive extracts. Refuse to publish such a
# tarball instead.
#
# Usage: plugins/pack-assets.sh [plugin ...]   (default: every embedded plugin)
set -euo pipefail

here="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd -- "$here/.." && pwd)"
assets="$repo/crates/ccteam-harness/src/execution/dsh_acp/assets"

plugins=("$@")
if [ ${#plugins[@]} -eq 0 ]; then
  plugins=(ccteam-ui)
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

  # Two-path reproducibility gate: same source, different directory, same bytes.
  mirror="$staging/mirror-$plugin"
  rm -rf "$mirror"
  cp -al "$src" "$mirror" 2>/dev/null || cp -a "$src" "$mirror"
  (cd "$mirror" && npm run build >/dev/null)
  mirror_out="$staging/$plugin-mirror"
  mkdir -p "$mirror_out"
  (cd "$mirror" && npm pack --pack-destination "$mirror_out" >/dev/null)
  mirror_tgz="$(find "$mirror_out" -maxdepth 1 -name '*.tgz' -print -quit)"
  a="$(sha256sum "$tgz" | cut -d' ' -f1)"
  b="$(sha256sum "$mirror_tgz" | cut -d' ' -f1)"
  if [ "$a" != "$b" ]; then
    echo "pack-assets: $plugin is not path-reproducible" >&2
    echo "             $src -> $a" >&2
    echo "             $mirror -> $b" >&2
    echo "             (something in the build output carries the checkout path;" >&2
    echo "              diff the two extracted trees to find it)" >&2
    exit 1
  fi
  echo "    two-path reproducible: $a"

  install -m 0644 "$tgz" "$assets/$plugin.tgz"
  sha256sum "$assets/$plugin.tgz"
done

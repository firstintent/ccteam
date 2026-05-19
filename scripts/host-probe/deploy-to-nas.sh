#!/usr/bin/env bash
#
# scripts/host-probe/deploy-to-nas.sh
#
# V0.6.0 Wave 4 host-probe deploy step. Ships the current main of
# ccteam to the real probe host (`nas-box005`, 192.168.1.19) at
# `/home/rob/nasworkspace/ccteam`, fetches origin, hard-resets to the
# requested ref, and runs a release build so probes start with a clean
# tree.
#
# This is the **deploy** step only — it does *not* run probes. Use
# `run-probes.sh` after deploy completes for the 5 preset + 3 Codex
# scenarios.
#
# Usage:
#   scripts/host-probe/deploy-to-nas.sh [REF]
#
#   REF — git ref to deploy. Defaults to `origin/main`.
#
# Env overrides:
#   CCTEAM_NAS_HOST       — ssh alias / hostname (default: nas-box005)
#   CCTEAM_NAS_PATH       — remote ccteam checkout (default:
#                           /home/rob/nasworkspace/ccteam)
#   CCTEAM_NAS_WIPE_HOME  — when "1", `rm -rf ~/.ccteam` on the remote
#                           before deploy (host probe wants a clean
#                           credentials / state dir; user confirms
#                           via the team-lead handshake first).
#
# Required on the remote:
#   - git (origin set to the upstream repo)
#   - rustup with stable toolchain matching this repo's
#     `rust-toolchain.toml`
#   - claude (>= 2.1.x) on PATH if Claude probes will run
#   - codex (>= 0.131.0) on PATH + ChatGPT auth if Codex probes will run
#
# Exit codes:
#   0   — deploy + build ok
#   1   — pre-flight failure (ssh unreachable, missing tooling)
#   2   — build failure on remote
#
# Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>

set -euo pipefail

REF="${1:-origin/main}"
NAS_HOST="${CCTEAM_NAS_HOST:-nas-box005}"
NAS_PATH="${CCTEAM_NAS_PATH:-/home/rob/nasworkspace/ccteam}"

log() { printf '[%s] %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }
die() { printf 'ERROR: %s\n' "$*" >&2; exit "${2:-1}"; }

log "pre-flight: ssh $NAS_HOST"
ssh -o BatchMode=yes -o ConnectTimeout=5 "$NAS_HOST" 'true' \
    || die "ssh $NAS_HOST unreachable" 1

log "pre-flight: remote tooling"
ssh "$NAS_HOST" 'command -v git && command -v cargo && command -v rustc' \
    >/dev/null || die "remote missing git / cargo / rustc" 1

if [[ "${CCTEAM_NAS_WIPE_HOME:-0}" == "1" ]]; then
    log "wiping ~/.ccteam on $NAS_HOST (user-confirmed clean slate)"
    ssh "$NAS_HOST" 'rm -rf ~/.ccteam'
fi

log "fetching + hard-resetting $NAS_PATH to $REF"
# shellcheck disable=SC2087
ssh "$NAS_HOST" bash -s <<EOF
set -euo pipefail
cd "$NAS_PATH"
git fetch --prune origin
git reset --hard "$REF"
git clean -fdx -- target/  # keep registry cache, drop stale build
git log -1 --oneline
EOF

log "release build on $NAS_HOST (this may take a few minutes on first run)"
ssh "$NAS_HOST" bash -s <<EOF
set -euo pipefail
cd "$NAS_PATH"
env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy \
    cargo build --workspace --locked --release
EOF
build_rc=$?
[[ $build_rc -eq 0 ]] || die "remote cargo build failed (rc=$build_rc)" 2

log "deploy ok — remote tree pinned to $REF, release binaries built"
log "next: scripts/host-probe/run-probes.sh"

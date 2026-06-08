#!/usr/bin/env bash
# v0.8.10 short real-machine smoke driver.
#
# This script is intentionally a release-validation helper, not a product
# command. It runs the real probes that can be automated from the repo and
# leaves host suspend / network-drop checks to the paired checklist:
# docs/versions/v0-8-10/nas-box005-short-smoke-checklist.md

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOG_DIR="${TMPDIR:-/tmp}/ccteam-v0-8-10-real-short"
mkdir -p "$LOG_DIR"

usage() {
  cat <<'EOF'
usage: scripts/smoke-v0-8-10-real-short.sh [--skip-rmux] [--skip-im]

Runs the automated part of the v0.8.10 nas-box005 short smoke:
  1. real rmux daemon smoke
  2. real IM WebSocket dual-harness smoke with restart + fault injection

Manual host suspend / netdrop checks remain in:
  docs/versions/v0-8-10/nas-box005-short-smoke-checklist.md

Required on the target box:
  - tmux
  - claude
  - codex
  - a built ccteam binary in target/debug/ccteam
EOF
}

run_rmux=1
run_im=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-rmux)
      run_rmux=0
      shift
      ;;
    --skip-im)
      run_im=0
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "smoke-v0-8-10-real-short: unknown argument: $1" >&2
      usage >&2
      exit 64
      ;;
  esac
done

run_logged() {
  local name="$1"
  shift
  local log="$LOG_DIR/${name}.log"
  echo "==> $name"
  (
    cd "$ROOT"
    "$@"
  ) >"$log" 2>&1 || {
    local code=$?
    echo "FAILED: $name (log: $log)" >&2
    tail -100 "$log" >&2 || true
    exit "$code"
  }
  tail -20 "$log"
}

if [[ "$run_rmux" -eq 1 ]]; then
  run_logged real_rmux scripts/rmux-smoke.sh
else
  echo "==> real_rmux: skipped"
fi

if [[ "$run_im" -eq 1 ]]; then
  run_logged real_im_ws_restart_faults env \
    CCTEAM_REAL_IM_WS=1 \
    CCTEAM_REAL_IM_WS_RESTART=1 \
    CCTEAM_REAL_IM_WS_FAULTS=1 \
    scripts/smoke-im.sh --real
else
  echo "==> real_im_ws_restart_faults: skipped"
fi

cat <<EOF
==> automated real short smoke: PASS

Next required manual record on nas-box005:
  docs/versions/v0-8-10/nas-box005-short-smoke-checklist.md

This script does not prove host suspend or real netdrop recovery by itself.
EOF

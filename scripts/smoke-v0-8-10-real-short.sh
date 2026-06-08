#!/usr/bin/env bash
# v0.8.10 short real-machine smoke driver.
#
# This script is intentionally a release-validation helper, not a product
# command. It runs the real probes that can be automated from the repo and
# records host-fault evidence in the paired checklist:
# docs/versions/v0-8-10/nas-box005-short-smoke-checklist.md

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOG_DIR="${TMPDIR:-/tmp}/ccteam-v0-8-10-real-short"
mkdir -p "$LOG_DIR"

usage() {
  cat <<'EOF'
usage: scripts/smoke-v0-8-10-real-short.sh [--skip-rmux] [--skip-im] [--preflight-only]

Runs the automated part of the v0.8.10 target-host short smoke:
  1. real rmux daemon smoke
  2. real IM WebSocket dual-harness smoke with restart + host-fault +
     pane-death fault injection

The host-fault leg is local and repeatable:
  - SIGSTOP/SIGCONT freezes the daemon test process.
  - WebSocket disconnect/reconnect covers the IM/web client network boundary.
  - Set CCTEAM_REAL_IM_WS_HOST_FAULT_STOP_SECS to override the freeze length.

Checklist record:
  docs/versions/v0-8-10/nas-box005-short-smoke-checklist.md

Required on the target box:
  - tmux
  - claude
  - codex only for opt-in Codex probes
  - cargo
  - a built ccteam binary in target/debug/ccteam

Set CCTEAM_REAL_SMOKE_HOST to the hostname that is allowed to record the smoke.
It defaults to nas-box005 for the original release target; this wave also
supports recording on the local workstation when the operator explicitly sets
that variable to the local hostname.

The script also requires a clean worktree and HEAD to match origin/dev so the
checklist records the exact pushed commit under test.
EOF
}

run_rmux=1
run_im=1
preflight_only=0
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
    --preflight-only)
      preflight_only=1
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

require_cmd() {
  local cmd="$1"
  command -v "$cmd" >/dev/null 2>&1 || {
    echo "smoke-v0-8-10-real-short: required command not found: $cmd" >&2
    exit 69
  }
}

needs_real_codex() {
  [[ "${CCTEAM_REAL_CODEX_RPC:-0}" == "1" ]] \
    || [[ "${CCTEAM_REAL_IM_WS_CODEX:-0}" == "1" ]] \
    || [[ "${CCTEAM_REAL_IM_WS_NL:-}" == "1" ]] \
    || [[ "${CCTEAM_REAL_IM_WS_NL:-}" == "codex" ]]
}

preflight() {
  local expected_host="${CCTEAM_REAL_SMOKE_HOST:-nas-box005}"
  local current_host
  current_host="$(hostname -s 2>/dev/null || hostname)"
  if [[ "$current_host" != "$expected_host" && "${CCTEAM_ALLOW_NON_NAS_SMOKE:-}" != "1" ]]; then
    cat >&2 <<EOF
smoke-v0-8-10-real-short: refusing to run on '$current_host'.
Expected host: '$expected_host'.

Set CCTEAM_REAL_SMOKE_HOST=$current_host to record this host intentionally, or
set CCTEAM_ALLOW_NON_NAS_SMOKE=1 for a rehearsal that must not be marked PASS.
EOF
    exit 78
  fi

  require_cmd git
  require_cmd cargo
  local head
  local origin_dev
  head="$(git -C "$ROOT" rev-parse HEAD)"
  origin_dev="$(git -C "$ROOT" rev-parse origin/dev)"
  if [[ "$head" != "$origin_dev" ]]; then
    cat >&2 <<EOF
smoke-v0-8-10-real-short: HEAD does not match origin/dev.
HEAD:       $head
origin/dev: $origin_dev

Fetch/pull the pushed dev commit before recording nas-box005 smoke results.
EOF
    exit 78
  fi
  if [[ -n "$(git -C "$ROOT" status --porcelain)" ]]; then
    cat >&2 <<'EOF'
smoke-v0-8-10-real-short: worktree is dirty.

Commit, stash, or clean local changes before recording nas-box005 smoke
results. The release smoke must run against an exact pushed commit.
EOF
    exit 78
  fi

  if [[ "$run_rmux" -eq 1 ]]; then
    require_cmd tmux
  fi
  if [[ "$run_im" -eq 1 ]]; then
    require_cmd claude
    if needs_real_codex; then
      require_cmd codex
    fi
    if [[ ! -x "$ROOT/target/debug/ccteam" ]]; then
      echo "smoke-v0-8-10-real-short: target/debug/ccteam is missing or not executable" >&2
      echo "Build it first on the target host: cargo build --workspace" >&2
      exit 69
    fi
  fi

  echo "==> preflight"
  echo "host: $current_host"
  echo "head: $head"
  echo "origin/dev: $origin_dev"
  echo "log dir: $LOG_DIR"
}

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

preflight

if [[ "$preflight_only" -eq 1 ]]; then
  echo "==> preflight-only: PASS"
  exit 0
fi

if [[ "$run_rmux" -eq 1 ]]; then
  run_logged real_rmux scripts/rmux-smoke.sh
else
  echo "==> real_rmux: skipped"
fi

if [[ "$run_im" -eq 1 ]]; then
  run_logged real_im_ws_restart_faults env \
    CCTEAM_REAL_IM_WS=1 \
    CCTEAM_REAL_IM_WS_RESTART=1 \
    CCTEAM_REAL_IM_WS_HOST_FAULTS=1 \
    CCTEAM_REAL_IM_WS_HOST_FAULT_STOP_SECS="${CCTEAM_REAL_IM_WS_HOST_FAULT_STOP_SECS:-600}" \
    CCTEAM_REAL_IM_WS_FAULTS=1 \
    scripts/smoke-im.sh --real
else
  echo "==> real_im_ws_restart_faults: skipped"
fi

cat <<EOF
==> automated real short smoke: PASS

Checklist record:
  docs/versions/v0-8-10/nas-box005-short-smoke-checklist.md

Host-fault scope: SIGSTOP/SIGCONT daemon freeze plus WebSocket client
disconnect/reconnect. It does not claim full ACPI system suspend or
system-level outbound network blocking.
EOF

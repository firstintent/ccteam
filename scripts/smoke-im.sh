#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOG_DIR="${TMPDIR:-/tmp}/ccteam-smoke-im"
mkdir -p "$LOG_DIR"
MODE="fake"

usage() {
  cat <<'EOF'
usage: scripts/smoke-im.sh [--real]

  --real  Fail loud unless real claude/codex binaries and the Codex
          app-server socket are present. This mode is the v8.2 guard
          against accidentally treating fake gateway tests as a real
          IM smoke.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --real)
      MODE="real"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "smoke-im: unknown argument: $1" >&2
      usage >&2
      exit 64
      ;;
  esac
done

run_cargo() {
  if command -v rtk >/dev/null 2>&1; then
    rtk cargo "$@"
  else
    cargo "$@"
  fi
}

run_test() {
  local name="$1"
  local package="$2"
  shift 2
  local log="$LOG_DIR/${name}.log"
  echo "==> $name"
  (
    cd "$ROOT"
    run_cargo test -p "$package" "$@" >"$log" 2>&1
  )
  grep -E '^(test result|cargo test:)' "$log" | tail -10
}

resolve_bin() {
  local env_name="$1"
  local default_name="$2"
  local configured="${!env_name:-}"
  if [[ -n "$configured" ]]; then
    if [[ -x "$configured" ]]; then
      printf '%s\n' "$configured"
      return 0
    fi
    if command -v "$configured" >/dev/null 2>&1; then
      command -v "$configured"
      return 0
    fi
    echo "smoke-im --real: $env_name is set but not executable/on PATH: $configured" >&2
    return 1
  fi
  command -v "$default_name" 2>/dev/null
}

run_version_probe() {
  local name="$1"
  local bin="$2"
  local log="$LOG_DIR/real-${name}-version.log"
  echo "==> real_${name}_version"
  timeout 20 "$bin" --version >"$log" 2>&1
  tail -5 "$log"
}

run_real_preflight() {
  local missing=0
  local claude_bin codex_bin codex_socket
  if ! claude_bin="$(resolve_bin CCTEAM_CLAUDE_BIN claude)"; then
    missing=1
  fi
  if ! codex_bin="$(resolve_bin CCTEAM_CODEX_BIN codex)"; then
    missing=1
  fi
  if ! command -v tmux >/dev/null 2>&1; then
    echo "smoke-im --real: tmux is required for ClaudeTuiAdapter" >&2
    missing=1
  fi
  codex_socket="${CCTEAM_CODEX_APP_SERVER_SOCKET:-${CODEX_HOME:-$HOME/.codex}/app-server-control/app-server-control.sock}"
  if [[ ! -S "$codex_socket" ]]; then
    echo "smoke-im --real: Codex app-server socket not found: $codex_socket" >&2
    echo "smoke-im --real: start it with: codex app-server daemon start" >&2
    missing=1
  fi
  if [[ "$missing" -ne 0 ]]; then
    exit 2
  fi

  run_version_probe claude "$claude_bin"
  run_version_probe codex "$codex_bin"
  echo "smoke-im --real: real binary preflight PASS"
}

if [[ "$MODE" == "real" ]]; then
  run_real_preflight
fi

run_test init_scaffold ccteam-cli run_init_fresh_install_scaffolds_and_registers
run_test gateway_pair ccteam-im gateway_pair_starts_default_session
run_test gateway_routes ccteam-im daemon_routes_gateway_inbound_to_submit_turn_and_outbound
run_test gateway_ws ccteam-im daemon_routes_ws_channel_to_gateway_over_real_socket
run_test gateway_ws_restart ccteam-im daemon_restart_preserves_ws_gateway_session
run_test gateway_switch ccteam-im gateway_commands_switch_project_and_session
run_test gateway_resume ccteam-im gateway_persistence_restores_routes_and_sessions
run_test start_gateway ccteam-cli --test start_with_imd_test start_spawns_imd_supervisor_unless_no_imd_set

echo "smoke-im: PASS"

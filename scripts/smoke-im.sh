#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOG_DIR="${TMPDIR:-/tmp}/ccteam-smoke-im"
mkdir -p "$LOG_DIR"
MODE="fake"

usage() {
  cat <<'EOF'
usage: scripts/smoke-im.sh [--real]

  --real  Fail loud unless real claude/codex binaries and a Codex
          app-server transport are present. This mode is the v8.2 guard
          against accidentally treating fake gateway tests as a real
          IM smoke.

Set CCTEAM_REAL_CODEX_RPC=1 with --real to also probe Codex app-server
thread/start. The default preflight only proves binaries + transport availability.
Set CCTEAM_REAL_IM_WS=1 with --real to also run the real WebSocket
dual-harness smoke (Codex app-server + Claude tmux).
Set CCTEAM_REAL_IM_WS_NL=codex|claude|1 with CCTEAM_REAL_IM_WS=1
to require true natural-language replies from Codex, Claude, or both.
Set CCTEAM_REAL_IM_WS_RESTART=1 with CCTEAM_REAL_IM_WS=1 to kill and
restart the daemon mid-smoke, then require both sessions to continue.
Set CCTEAM_REAL_IM_WS_FAULTS=1 with CCTEAM_REAL_IM_WS=1 to inject a
real Claude tmux-session death plus a Codex app-server disconnect, and
require user-visible gateway errors.
Set CCTEAM_REAL_IM_TELEGRAM=1 with --real plus CCTEAM_TELEGRAM_BOT_TOKEN
and CCTEAM_TELEGRAM_CHAT_ID to run an opt-in real Telegram send/listen
round trip. The test sends a unique code and waits for that exact reply.
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
  local code=0
  (
    cd "$ROOT"
    run_cargo test -p "$package" "$@" >"$log" 2>&1
  ) || code=$?
  if [[ "$code" -ne 0 ]]; then
    grep -E '^(test result|cargo test:)' "$log" | tail -10 || true
    tail -80 "$log"
    exit "$code"
  fi
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

ensure_codex_app_server_socket() {
  local codex_bin="$1"
  local configured="${CCTEAM_CODEX_APP_SERVER_SOCKET:-}"
  local socket="${configured:-${CODEX_HOME:-$HOME/.codex}/app-server-control/app-server-control.sock}"

  # F10: transport is single-axis on CCTEAM_CODEX_APP_SERVER_SOCKET —
  # setting it selects the UDS (Socket) override; leaving it unset selects
  # the default stdio child-spawn.
  if [[ -S "$socket" ]]; then
    export CCTEAM_CODEX_APP_SERVER_SOCKET="$socket"
    return 0
  fi

  local daemon_log="$LOG_DIR/real-codex-app-server-daemon-start.log"
  timeout 30 "$codex_bin" app-server daemon start >"$daemon_log" 2>&1 || true
  if [[ -S "$socket" ]]; then
    export CCTEAM_CODEX_APP_SERVER_SOCKET="$socket"
    return 0
  fi

  if [[ -z "$configured" ]]; then
    # npm-managed Codex exposes a raw JSONL app-server on stdio. Its
    # foreground unix:// listener is not the standalone daemon control
    # socket protocol the harness UDS client speaks, so fall back to the
    # default stdio transport (no socket env) when the managed daemon is
    # unavailable.
    unset CCTEAM_CODEX_APP_SERVER_SOCKET
    export CCTEAM_CODEX_BIN="$codex_bin"
    return 0
  fi

  echo "smoke-im --real: Codex app-server socket not found: $socket" >&2
  echo "smoke-im --real: daemon start log: $daemon_log" >&2
  tail -20 "$daemon_log" >&2 || true
  return 1
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
  if [[ "$missing" -eq 0 ]] && ! ensure_codex_app_server_socket "$codex_bin"; then
    missing=1
  fi
  if [[ "$missing" -ne 0 ]]; then
    exit 2
  fi

  run_version_probe claude "$claude_bin"
  run_version_probe codex "$codex_bin"
  if [[ -z "${CCTEAM_CODEX_APP_SERVER_SOCKET:-}" ]]; then
    echo "smoke-im --real: Codex app-server transport stdio (default)"
  else
    echo "smoke-im --real: Codex app-server socket $CCTEAM_CODEX_APP_SERVER_SOCKET"
  fi
  export CCTEAM_REAL_CODEX_APP_SERVER=1
  echo "smoke-im --real: real binary preflight PASS"
}

if [[ "$MODE" == "real" ]]; then
  run_real_preflight
  if [[ "${CCTEAM_REAL_CODEX_RPC:-0}" == "1" ]]; then
    run_test real_codex_app_server ccteam-harness real_codex_app_server_start_thread_smoke
  else
    echo "smoke-im --real: skipping Codex RPC probe (set CCTEAM_REAL_CODEX_RPC=1)"
  fi
  if [[ "${CCTEAM_REAL_IM_WS:-0}" == "1" ]]; then
    run_test real_ws_dual_harness ccteam-im real_ws_dual_harness_smoke
  else
    echo "smoke-im --real: skipping real WS dual-harness probe (set CCTEAM_REAL_IM_WS=1)"
  fi
  if [[ "${CCTEAM_REAL_IM_TELEGRAM:-0}" == "1" ]]; then
    if [[ -z "${CCTEAM_TELEGRAM_BOT_TOKEN:-${CCTEAM_TELEGRAM_TOKEN:-}}" || -z "${CCTEAM_TELEGRAM_CHAT_ID:-}" ]]; then
      echo "smoke-im --real: CCTEAM_REAL_IM_TELEGRAM=1 requires CCTEAM_TELEGRAM_BOT_TOKEN and CCTEAM_TELEGRAM_CHAT_ID" >&2
      exit 2
    fi
    run_test real_telegram_channel ccteam-im real_telegram_channel_roundtrip_smoke
  else
    echo "smoke-im --real: skipping real Telegram probe (set CCTEAM_REAL_IM_TELEGRAM=1)"
  fi
fi

run_test init_scaffold ccteam-cli run_init_fresh_install_scaffolds_and_registers
run_test gateway_pair ccteam-im gateway_pair_starts_default_session
run_test gateway_routes ccteam-im daemon_routes_gateway_inbound_to_submit_turn_and_outbound
run_test gateway_ws ccteam-im daemon_routes_ws_channel_to_gateway_over_real_socket
run_test gateway_ws_restart ccteam-im daemon_restart_preserves_ws_gateway_session
run_test gateway_outbound_replay ccteam-im daemon_replays_queued_durable_outbound_to_mock_channel
run_test gateway_outbound_replay_idempotent ccteam-im daemon_replays_queued_durable_outbound_idempotently_once
run_test gateway_ws_replay ccteam-im daemon_replays_ws_outbound_when_client_reconnects
run_test gateway_sid_routing ccteam-im same_role_submit_to_sid_routes_to_each_thread
run_test gateway_start_failure ccteam-im daemon_surfaces_start_failure_to_im_and_ledger
run_test gateway_submit_failure ccteam-im daemon_surfaces_submit_failure_to_im_and_ledger
run_test gateway_turn_timeout ccteam-im daemon_surfaces_turn_timeout_to_im_and_ledger
run_test gateway_switch ccteam-im gateway_commands_switch_project_and_session
run_test gateway_resume ccteam-im gateway_persistence_restores_routes_and_sessions
run_test start_gateway ccteam-cli --test start_with_imd_test start_spawns_imd_supervisor_unless_no_imd_set

echo "smoke-im: PASS"

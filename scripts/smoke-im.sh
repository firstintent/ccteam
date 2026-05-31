#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOG_DIR="${TMPDIR:-/tmp}/ccteam-smoke-im"
mkdir -p "$LOG_DIR"

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

run_test init_scaffold ccteam-cli run_init_fresh_install_scaffolds_and_registers
run_test gateway_pair ccteam-im gateway_pair_starts_default_session
run_test gateway_routes ccteam-im daemon_routes_gateway_inbound_to_submit_turn_and_outbound
run_test gateway_switch ccteam-im gateway_commands_switch_project_and_session
run_test gateway_resume ccteam-im gateway_persistence_restores_routes_and_sessions
run_test start_gateway ccteam-cli --test start_with_imd_test start_spawns_imd_supervisor_unless_no_imd_set

echo "smoke-im: PASS"

#!/usr/bin/env bash
# Demo 2: Team Sprint — /ccteam-team 3 "fix TS errors" (3 parallel teammates).
# Target ~30s (60s compressed), 90x30.
set -eu
cd "$(dirname "$0")"
source ./_lib.sh
hide_cursor
clear

header "Claude Code  ·  ccteam-demo  ·  mode-1 fan-out (3 teammates)"
log ""
pause 0.5

user_prompt "/ccteam-team 3 'tsc 报 12 个 error,分三人并行修,最后我审'"
pause 0.5
log ""
log "${C_TOOL}● workflow_create${C_RESET} ${C_DIMK}→ team=ts-fix, fan-out=3${C_RESET}"
pause 0.3
log "${C_TOOL}● workflow_invoke${C_RESET} ${C_DIMK}→ spawning 3 teammates…${C_RESET}"
pause 0.6
log ""
log "${C_AGENT}fixer-1${C_RESET}  ${C_DIMK}claim: src/api/*${C_RESET}    ${C_OK}● started${C_RESET}"
pause 0.15
log "${C_AGENT}fixer-2${C_RESET}  ${C_DIMK}claim: src/ui/*${C_RESET}     ${C_OK}● started${C_RESET}"
pause 0.15
log "${C_AGENT}fixer-3${C_RESET}  ${C_DIMK}claim: src/lib/*${C_RESET}    ${C_OK}● started${C_RESET}"
pause 0.8
log ""
log "${C_DIMK}─ progress.jsonl (live tail) ─${C_RESET}"
pause 0.3
log "${C_AGENT}fixer-1${C_RESET} ▸ Edit src/api/client.ts        ${C_DIMK}(implicit any → unknown)${C_RESET}"
pause 0.3
log "${C_AGENT}fixer-2${C_RESET} ▸ Edit src/ui/Modal.tsx          ${C_DIMK}(missing key prop)${C_RESET}"
pause 0.3
log "${C_AGENT}fixer-3${C_RESET} ▸ Edit src/lib/cache.ts          ${C_DIMK}(strictNullCheck)${C_RESET}"
pause 0.3
log "${C_AGENT}fixer-1${C_RESET} ▸ Edit src/api/auth.ts           ${C_DIMK}(fix Promise<void>)${C_RESET}"
pause 0.3
log "${C_AGENT}fixer-3${C_RESET} ▸ Bash tsc --noEmit             ${C_DIMK}(verifying…)${C_RESET}"
pause 0.4
log "${C_AGENT}fixer-2${C_RESET} ▸ ${C_OK}✓ done${C_RESET}                          ${C_DIMK}3 edits, 0 errors${C_RESET}"
pause 0.3
log "${C_AGENT}fixer-1${C_RESET} ▸ ${C_OK}✓ done${C_RESET}                          ${C_DIMK}5 edits, 0 errors${C_RESET}"
pause 0.3
log "${C_AGENT}fixer-3${C_RESET} ▸ ${C_OK}✓ done${C_RESET}                          ${C_DIMK}4 edits, 0 errors${C_RESET}"
pause 0.6
log ""
log "${C_TOOL}● ccteam${C_RESET} merge results → ${C_OK}12/12 fixed, tsc clean${C_RESET}"
log "  ${C_DIMK}duration 11s wall  ·  cost \$0.08 mocked  ·  /fleetview to inspect${C_RESET}"
pause 1.2
log ""
log "${C_DIMK}─ end of demo (mode 1 fan-out, in-proc Task subagents) ─${C_RESET}"
pause 1.0
